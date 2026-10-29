//! A chat's execution journal, next to its file (`<chat>.jsonl.run`): what the running turn has
//! started, appended as it happens, so a restart can tell what was lost with the process.
//!
//! While a turn runs, the process running it holds an exclusive lock on the journal. A journal
//! that can be locked but records a turn without an end therefore belongs to a process that is
//! gone: that turn was interrupted, and its tools that started without finishing have an
//! unknown outcome. The chat shows them and resends nothing by itself.
//!
//! The journal also keeps what can safely be picked up again: the messages queued behind the
//! turn (never sent, so sending them later repeats nothing), and the ids of recent prompt
//! requests, so a client retransmitting one is recognized instead of starting a second turn.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Request ids remembered per chat.
const MAX_RECEIPTS: usize = 32;
/// Longest tool argument summary kept.
const MAX_ARGS: usize = 300;

/// What a tool call may do, as far as a restart is concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    /// Only reads: repeating it is harmless.
    Read,
    /// Changes files.
    Write,
    /// Runs a command: anything may have happened.
    Shell,
    /// Outside oxi's view (MCP and other tools).
    External,
}

impl Effect {
    /// Classify a tool by name (oxi's own tools) or by the kind an agent reported (ACP).
    pub fn of(name: &str, kind: Option<&str>) -> Self {
        match kind {
            Some("read" | "search" | "fetch" | "think") => Self::Read,
            Some("edit" | "delete" | "move") => Self::Write,
            Some("execute") => Self::Shell,
            Some(_) => Self::External,
            None => match crate::agent::tools::tool_side_effect(name) {
                crate::agent::tools::ToolSideEffect::ReadOnly => Self::Read,
                crate::agent::tools::ToolSideEffect::WorkspaceMutation => Self::Write,
                crate::agent::tools::ToolSideEffect::Shell => Self::Shell,
                crate::agent::tools::ToolSideEffect::UnknownExternal => Self::External,
            },
        }
    }

    /// Whether an unfinished call may have left changes behind.
    pub fn may_have_changed_things(self) -> bool {
        self != Self::Read
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum Record {
    Start {
        turn: String,
        at: u64,
        prompt: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request: Option<String>,
    },
    Tool {
        turn: String,
        id: String,
        name: String,
        args: String,
        effect: Effect,
    },
    ToolDone {
        turn: String,
        id: String,
        error: bool,
    },
    End {
        turn: String,
        outcome: String,
    },
    Queue {
        items: Vec<String>,
    },
    Receipt {
        request: String,
    },
}

/// A tool call that started and never reported back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartedTool {
    pub id: String,
    pub name: String,
    pub args: String,
    pub effect: Effect,
}

/// A turn whose process went away before it ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Interrupted {
    /// Unix seconds it started.
    pub started: u64,
    /// The opening of its prompt.
    pub prompt: String,
    pub unfinished: Vec<StartedTool>,
}

/// What a journal left by an earlier process holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Recovery {
    pub interrupted: Option<Interrupted>,
    /// Messages queued behind the turn, never sent.
    pub queued: Vec<String>,
    /// Ids of recent prompt requests.
    pub receipts: Vec<String>,
}

pub fn path_for(session_file: &str) -> PathBuf {
    PathBuf::from(format!("{session_file}.run"))
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn line(record: &Record) -> String {
    let mut text = serde_json::to_string(record).unwrap_or_default();
    text.push('\n');
    text
}

fn cut(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &text[..i]),
        None => text.to_string(),
    }
}

fn open(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn read_records(file: &mut File) -> Vec<Record> {
    if file.seek(SeekFrom::Start(0)).is_err() {
        return Vec::new();
    }
    BufReader::new(&*file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str(&l).ok())
        .collect()
}

/// Replace the file's contents with what outlives a turn: receipts and the queue. Returns
/// whether that is nothing (the caller removes the file once it let go of it).
fn compact(file: &mut File, path: &Path, receipts: &[String], queued: &[String]) -> bool {
    let start = receipts.len().saturating_sub(MAX_RECEIPTS);
    let mut text = String::new();
    for request in &receipts[start..] {
        text.push_str(&line(&Record::Receipt {
            request: request.clone(),
        }));
    }
    if !queued.is_empty() {
        text.push_str(&line(&Record::Queue {
            items: queued.to_vec(),
        }));
    }
    let written = file
        .set_len(0)
        .and_then(|()| file.seek(SeekFrom::Start(0)))
        .and_then(|_| file.write_all(text.as_bytes()))
        .and_then(|()| file.sync_data());
    if let Err(e) = written {
        log::warn!("run journal {}: {e}", path.display());
    }
    text.is_empty()
}

/// Compact the journal at `path`, then remove it if nothing is left. The file is closed (and
/// so unlocked) first: Windows does not delete a file that is still open.
fn compact_and_close(mut file: File, path: &Path, receipts: &[String], queued: &[String]) {
    let empty = compact(&mut file, path, receipts, queued);
    drop(file);
    if empty {
        let _ = std::fs::remove_file(path);
    }
}

fn summarize(records: Vec<Record>) -> Recovery {
    let mut recovery = Recovery::default();
    let mut open_turn: Option<(String, Interrupted)> = None;
    for record in records {
        match record {
            Record::Start {
                turn,
                at,
                prompt,
                request,
            } => {
                if let Some(request) = request {
                    recovery.receipts.push(request);
                }
                open_turn = Some((
                    turn,
                    Interrupted {
                        started: at,
                        prompt,
                        unfinished: Vec::new(),
                    },
                ));
            }
            Record::Tool {
                turn,
                id,
                name,
                args,
                effect,
            } => {
                if let Some((current, interrupted)) = open_turn.as_mut()
                    && *current == turn
                    && !interrupted.unfinished.iter().any(|t| t.id == id)
                {
                    interrupted.unfinished.push(StartedTool {
                        id,
                        name,
                        args,
                        effect,
                    });
                }
            }
            Record::ToolDone { turn, id, .. } => {
                if let Some((current, interrupted)) = open_turn.as_mut()
                    && *current == turn
                {
                    interrupted.unfinished.retain(|t| t.id != id);
                }
            }
            Record::End { turn, .. } => {
                if open_turn
                    .as_ref()
                    .is_some_and(|(current, _)| *current == turn)
                {
                    open_turn = None;
                }
            }
            Record::Queue { items } => recovery.queued = items,
            Record::Receipt { request } => recovery.receipts.push(request),
        }
    }
    recovery.interrupted = open_turn.map(|(_, interrupted)| interrupted);
    recovery
}

/// The journal of a running turn. Holds the chat's journal locked until [`Self::end`] (or it is
/// dropped, as when its process dies).
pub struct TurnJournal {
    file: File,
    path: PathBuf,
    turn: String,
    receipts: Vec<String>,
    /// Calls recorded as started; agents report a call many times as it progresses.
    started: std::collections::HashSet<String>,
}

impl std::fmt::Debug for TurnJournal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TurnJournal")
            .field("path", &self.path)
            .finish()
    }
}

impl TurnJournal {
    /// Record that a turn started for `prompt` (`request`: the client's id for it). `None` when
    /// the journal cannot be written or another process holds it.
    pub fn start(session_file: &str, prompt: &str, request: Option<&str>) -> Option<Self> {
        let path = path_for(session_file);
        let mut file = open(&path)
            .map_err(|e| log::warn!("run journal {}: {e}", path.display()))
            .ok()?;
        file.try_lock().ok()?;
        // What an earlier turn left (receipts, queue) is carried; an unended turn here was
        // already reported when the chat was loaded.
        let earlier = summarize(read_records(&mut file));
        let turn = format!("{}-{}", now(), rand::random::<u32>());
        let mut receipts = earlier.receipts;
        if let Some(request) = request {
            receipts.push(request.to_string());
        }
        let mut journal = Self {
            file,
            path,
            turn: turn.clone(),
            receipts,
            started: Default::default(),
        };
        compact(&mut journal.file, &journal.path, &journal.receipts, &[]);
        journal.append(
            &Record::Start {
                turn,
                at: now(),
                prompt: cut(prompt, 200),
                request: request.map(str::to_owned),
            },
            true,
        );
        Some(journal)
    }

    fn append(&mut self, record: &Record, sync: bool) {
        let written = self
            .file
            .seek(SeekFrom::End(0))
            .and_then(|_| self.file.write_all(line(record).as_bytes()))
            .and_then(|()| if sync { self.file.sync_data() } else { Ok(()) });
        if let Err(e) = written {
            log::warn!("run journal {}: {e}", self.path.display());
        }
    }

    /// A tool call started. Calls that may change things are flushed to disk first thing.
    pub fn tool(&mut self, id: &str, name: &str, args: &str, effect: Effect) {
        if !self.started.insert(id.to_string()) {
            return;
        }
        let record = Record::Tool {
            turn: self.turn.clone(),
            id: id.to_string(),
            name: name.to_string(),
            args: cut(args, MAX_ARGS),
            effect,
        };
        self.append(&record, effect.may_have_changed_things());
    }

    /// A tool call reported its result.
    pub fn tool_done(&mut self, id: &str, error: bool) {
        if !self.started.remove(id) {
            return;
        }
        let record = Record::ToolDone {
            turn: self.turn.clone(),
            id: id.to_string(),
            error,
        };
        self.append(&record, false);
    }

    /// Messages now queued behind the turn.
    pub fn queue(&mut self, items: &[String]) {
        self.append(
            &Record::Queue {
                items: items.to_vec(),
            },
            false,
        );
    }

    /// The turn ended; keep only what outlives it.
    pub fn end(self, queued: &[String]) {
        compact_and_close(self.file, &self.path, &self.receipts, queued);
    }
}

/// What the journal of `session_file` says an earlier process left, unless a live process
/// holds it. Clears the interrupted turn from it (it is reported once), keeping the receipts
/// and the queue.
pub fn recover(session_file: &str) -> Option<Recovery> {
    let path = path_for(session_file);
    if !path.exists() {
        return None;
    }
    let mut file = open(&path).ok()?;
    // Held: a turn of a live process (this one or another oxi) is running.
    file.try_lock().ok()?;
    let recovery = summarize(read_records(&mut file));
    compact_and_close(file, &path, &recovery.receipts, &recovery.queued);
    Some(recovery)
}

/// Remember `queued` for the chat while no turn runs (after a failed turn, during a work
/// tree's setup, …). Skipped while a live process holds the journal.
pub fn save_queue(session_file: &str, queued: &[String]) {
    let path = path_for(session_file);
    if queued.is_empty() && !path.exists() {
        return;
    }
    let Ok(mut file) = open(&path) else {
        return;
    };
    if file.try_lock().is_err() {
        return;
    }
    let receipts = summarize(read_records(&mut file)).receipts;
    compact_and_close(file, &path, &receipts, queued);
}

/// Whether request `request` was already accepted for the chat.
pub fn has_receipt(session_file: &str, request: &str) -> bool {
    let path = path_for(session_file);
    let Ok(file) = File::open(&path) else {
        return false;
    };
    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str::<Record>(&l).ok())
        .any(|r| match r {
            Record::Receipt { request: r }
            | Record::Start {
                request: Some(r), ..
            } => r == request,
            _ => false,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> (PathBuf, String) {
        let dir = crate::fsutil::unique_temp_path(&std::env::temp_dir(), "oxi-run-journal", "d");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("chat.jsonl").to_string_lossy().into_owned();
        (dir, file)
    }

    #[test]
    fn a_turn_whose_process_went_away_is_reported_once_with_its_unfinished_tools() {
        let (dir, file) = session();
        let mut journal = TurnJournal::start(&file, "Fix the build", Some("req-1")).unwrap();
        journal.tool("t1", "read", "{\"path\":\"a.rs\"}", Effect::Read);
        journal.tool_done("t1", false);
        journal.tool("t2", "bash", "{\"command\":\"cargo test\"}", Effect::Shell);
        journal.queue(&["then commit".into()]);
        assert_eq!(recover(&file), None, "a live turn is not interrupted");
        // The process dies: the lock goes with it, the end is never written.
        drop(journal);

        let recovery = recover(&file).unwrap();
        let interrupted = recovery.interrupted.unwrap();
        assert_eq!(interrupted.prompt, "Fix the build");
        assert_eq!(interrupted.unfinished.len(), 1);
        assert_eq!(interrupted.unfinished[0].name, "bash");
        assert!(interrupted.unfinished[0].effect.may_have_changed_things());
        assert_eq!(recovery.queued, ["then commit"]);
        assert!(has_receipt(&file, "req-1"));

        let again = recover(&file).unwrap();
        assert_eq!(again.interrupted, None, "reported once");
        assert_eq!(again.queued, ["then commit"], "the queue stays until sent");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_ended_turn_leaves_only_receipts_and_a_clean_journal_goes_away() {
        let (dir, file) = session();
        let journal = TurnJournal::start(&file, "Hi", None).unwrap();
        journal.end(&[]);
        assert!(!path_for(&file).exists(), "nothing worth keeping");

        let mut journal = TurnJournal::start(&file, "Hi again", Some("req-2")).unwrap();
        journal.tool("t", "edit", "{}", Effect::Write);
        journal.end(&[]);
        let recovery = recover(&file).unwrap();
        assert_eq!(recovery.interrupted, None);
        assert_eq!(recovery.receipts, ["req-2"]);

        save_queue(&file, &["later".into()]);
        assert_eq!(recover(&file).unwrap().queued, ["later"]);
        save_queue(&file, &[]);
        assert_eq!(recover(&file).unwrap().queued, Vec::<String>::new());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn tools_are_classified_by_what_they_may_change() {
        assert_eq!(Effect::of("read", None), Effect::Read);
        assert_eq!(Effect::of("write", None), Effect::Write);
        assert_eq!(Effect::of("bash", None), Effect::Shell);
        assert_eq!(Effect::of("Bash", Some("execute")), Effect::Shell);
        assert_eq!(Effect::of("Read", Some("read")), Effect::Read);
        assert_eq!(Effect::of("mcp__x", None), Effect::External);
    }
}

/// One line per unfinished call: `bash cargo test (may have changed things)`.
fn unfinished_lines(interrupted: &Interrupted) -> Vec<String> {
    interrupted
        .unfinished
        .iter()
        .map(|tool| {
            let args = serde_json::from_str::<serde_json::Value>(&tool.args)
                .ok()
                .and_then(|v| {
                    ["command", "path", "file_path", "pattern", "url"]
                        .iter()
                        .find_map(|k| v[k].as_str().map(str::to_owned))
                })
                .unwrap_or_else(|| tool.args.clone());
            let note = if tool.effect.may_have_changed_things() {
                "outcome unknown, may have changed things"
            } else {
                "only read, safe to repeat"
            };
            format!("{} {} ({note})", tool.name, cut(args.trim(), 120))
        })
        .collect()
}

/// What happened, for the transcript and the chat's notice.
pub fn describe(interrupted: &Interrupted) -> String {
    let lines = unfinished_lines(interrupted);
    if lines.is_empty() {
        return "oxi closed before this response finished; no action was running.".into();
    }
    format!(
        "oxi closed before this response finished. Started without a result: {}.",
        lines.join("; ")
    )
}

/// The message "Continue" sends: carry on, after checking what the interrupted calls did.
pub fn continue_prompt(interrupted: &Interrupted) -> String {
    let lines = unfinished_lines(interrupted);
    if lines.is_empty() {
        return "Your previous response was interrupted because oxi closed. Continue where you \
                left off."
            .into();
    }
    format!(
        "Your previous response was interrupted because oxi closed. These actions had started \
         and their outcome is unknown:\n{}\n\nCheck the current state first (files, processes, \
         git status) instead of repeating them blindly, then continue the task.",
        lines
            .iter()
            .map(|l| format!("- {l}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

/// Show an interrupted turn in its transcript: its unfinished calls become interrupted tool
/// blocks of the reply, followed by a note. Returns whether the transcript changed.
pub fn mark_interrupted(
    messages: &mut Vec<crate::model::ChatMessage>,
    interrupted: &Interrupted,
) -> bool {
    use crate::model::{AssistantBlock, ChatMessage, MsgRole, ToolMetadata, ToolStatus};
    if messages.is_empty() {
        return false;
    }
    // A reply that had produced nothing yet was not saved: give the note one.
    if messages.last().is_some_and(|m| m.role == MsgRole::User) {
        let mut reply = ChatMessage::streaming_assistant(None);
        reply.started_at = None;
        messages.push(reply);
    }
    let Some(reply) = messages.last_mut() else {
        return false;
    };
    for tool in &interrupted.unfinished {
        let existing = reply.blocks.iter_mut().find_map(|block| match block {
            AssistantBlock::Tool {
                tool_call_id,
                metadata,
                is_error,
                ..
            } if *tool_call_id == tool.id => Some((metadata, is_error)),
            _ => None,
        });
        match existing {
            Some((metadata, is_error)) => {
                if let Some(meta) = metadata {
                    meta.status = ToolStatus::Interrupted;
                }
                *is_error = None;
            }
            None => reply.blocks.push(AssistantBlock::Tool {
                tool_call_id: tool.id.clone(),
                name: tool.name.clone(),
                args_summary: Some(tool.args.clone()),
                output: String::new(),
                diff: None,
                is_error: None,
                full_output_path: None,
                output_truncated: false,
                metadata: Some(Box::new(ToolMetadata {
                    title: tool.name.clone(),
                    name: Some(tool.name.clone()),
                    // The agent-protocol kind, so the call reads as what it may have done.
                    kind: match tool.effect {
                        Effect::Read => "read",
                        Effect::Write => "edit",
                        Effect::Shell => "execute",
                        Effect::External => "other",
                    }
                    .into(),
                    status: ToolStatus::Interrupted,
                    ..Default::default()
                })),
            }),
        }
    }
    reply.append_answer(&format!("\n[Interrupted] {}\n", describe(interrupted)));
    reply.finish_streaming();
    true
}

#[cfg(test)]
mod transcript_tests {
    use super::*;
    use crate::model::{AssistantBlock, ChatMessage, ToolStatus};

    #[test]
    fn an_interrupted_reply_lists_its_unfinished_calls_and_how_to_continue() {
        let interrupted = Interrupted {
            started: 0,
            prompt: "Fix it".into(),
            unfinished: vec![
                StartedTool {
                    id: "t1".into(),
                    name: "bash".into(),
                    args: "{\"command\":\"cargo test\"}".into(),
                    effect: Effect::Shell,
                },
                StartedTool {
                    id: "t2".into(),
                    name: "read".into(),
                    args: "{\"path\":\"a.rs\"}".into(),
                    effect: Effect::Read,
                },
            ],
        };
        let mut reply = ChatMessage::streaming_assistant(None);
        reply.start_tool("bash", Some("t1"), None);
        reply.finish_streaming();
        let mut messages = vec![ChatMessage::user("Fix it", &[]), reply];
        assert!(mark_interrupted(&mut messages, &interrupted));
        let blocks = &messages[1].blocks;
        let tools: Vec<_> = blocks
            .iter()
            .filter_map(|b| match b {
                AssistantBlock::Tool {
                    tool_call_id,
                    metadata,
                    ..
                } => Some((tool_call_id.as_str(), metadata.as_ref().map(|m| m.status))),
                _ => None,
            })
            .collect();
        assert_eq!(tools.len(), 2, "the started call is not duplicated");
        assert_eq!(tools[1], ("t2", Some(ToolStatus::Interrupted)));
        let text = crate::model::last_answer(&messages[1..]).unwrap_or_default();
        assert!(
            text.contains("bash cargo test (outcome unknown, may have changed things)"),
            "{text}"
        );
        assert!(
            text.contains("read a.rs (only read, safe to repeat)"),
            "{text}"
        );
        let prompt = continue_prompt(&interrupted);
        assert!(prompt.contains("Check the current state first"), "{prompt}");
    }
}
