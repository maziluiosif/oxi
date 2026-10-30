//! Plain-text transcripts that carry a chat across ACP agents: the whole conversation for an
//! agent that had to start a blank session, or just the turns it missed (another agent spoke)
//! for one that resumed its own session. Both are trimmed to fit; every turn carries its number
//! (`[#n]`), and the untrimmed turns go to [`acp_chat_archive`], which the agent reads back with
//! oxi's `chat_read` tool.

use std::path::{Path, PathBuf};

use crate::model::{AssistantBlock, ChatMessage, MsgRole, ToolMetadata};
use crate::settings::LlmProviderKind;

/// Upper bound on the replayed transcript; turns after the first are dropped oldest first. The
/// agent can read what was dropped through `chat_read`, so this stays well inside any window.
pub(super) const ACP_HISTORY_MAX_CHARS: usize = 120_000;
/// The chat's first message (usually the task) is kept over the budget up to this size.
const FIRST_TURN_MAX_CHARS: usize = 20_000;
/// Tool output and diffs kept per call in the `chat_read` archive.
const ARCHIVE_TOOL_CHARS: usize = 8_000;
/// Tool output kept per call in the latest assistant turns, and for failed calls anywhere.
const TOOL_OUTPUT_RECENT_CHARS: usize = 2_000;
/// Tool output kept per call in older turns.
const TOOL_OUTPUT_OLDER_CHARS: usize = 300;
/// How many trailing assistant turns count as recent.
const RECENT_ASSISTANT_TURNS: usize = 3;

/// Transcript of earlier turns for an agent that had to start a blank session (it could not
/// resume its own), so it still knows the conversation. Keeps user text, compaction summaries,
/// answers, tool calls with their (trimmed) results, and the files the conversation changed;
/// drops thinking.
pub(super) fn acp_history_transcript(chat: &[ChatMessage]) -> String {
    transcript(chat, 0, Detail::History)
}

/// Every turn of `chat` with its number, tool output only lightly trimmed: what `chat_read`
/// serves when a transcript left something out.
pub(super) fn acp_chat_archive(chat: &[ChatMessage]) -> Vec<(usize, String)> {
    turns(chat, 0, Detail::Archive)
        .into_iter()
        .map(|t| (t.n, t.text))
        .collect()
}

/// Turns `provider` has not seen: everything after its own last answer. Empty when it answered
/// last or never answered in this chat (a resumed session from before agents were recorded
/// already holds the conversation). The other agent's answers already sum up what it read, so
/// read-only tools keep only their call and other tools a short tail.
pub(super) fn acp_missed_transcript(chat: &[ChatMessage], provider: LlmProviderKind) -> String {
    // A reply another agent finished after `provider` ran out of quota mid-turn: `provider` saw
    // the request but not the rest of the reply.
    let ran_out = format!("{}:", provider.label());
    let Some(start) = chat
        .iter()
        .rposition(|m| {
            m.role == MsgRole::Assistant
                && m.route.as_deref().is_some_and(|r| {
                    r.provider == provider
                        || r.interrupted_providers.contains(&provider)
                        || r.failover_from
                            .as_deref()
                            .is_some_and(|f| f.starts_with(&ran_out))
                })
        })
        .map(|i| match chat[i].route.as_deref() {
            Some(r) if r.provider == provider => i + 1,
            _ => i,
        })
    else {
        return String::new();
    };
    let missed = &chat[start..];
    // Only the user's own follow-ups (e.g. a failed turn) are not worth a recap.
    if !missed.iter().any(|m| m.role == MsgRole::Assistant) {
        return String::new();
    }
    transcript(missed, start, Detail::Recap)
}

/// Append where the other agents in this chat keep their own notes (Claude Code's auto-memory,
/// Codex's memories), so the agent can look something up when the transcript lacks it. Only
/// paths that exist, and nothing when there is no transcript to hand over.
pub(super) fn with_agent_notes(
    transcript: String,
    chat: &[ChatMessage],
    provider: LlmProviderKind,
    cwd: &Path,
) -> String {
    if transcript.is_empty() {
        return transcript;
    }
    let mut others: Vec<LlmProviderKind> = Vec::new();
    for kind in chat
        .iter()
        .filter_map(|m| m.route.as_deref().map(|r| r.provider))
    {
        if kind != provider && !others.contains(&kind) {
            others.push(kind);
        }
    }
    let notes: Vec<String> = others
        .into_iter()
        .filter_map(|kind| {
            let path = notes_index(kind, cwd).filter(|p| p.is_file())?;
            Some(format!(
                "{}: {}",
                kind.label().trim_end_matches(" (ACP)"),
                path.display()
            ))
        })
        .collect();
    if notes.is_empty() {
        return transcript;
    }
    format!(
        "{transcript}\n\n[Notes the other agents keep across sessions: {}. Each is an index with details in the files beside it; grep or read only the part you need when the conversation lacks context. They can be out of date, so check them against the code.]",
        notes.join("; ")
    )
}

/// The memory index an agent keeps for `cwd`, if it has a known place.
fn notes_index(kind: LlmProviderKind, cwd: &Path) -> Option<PathBuf> {
    let home_or = |var: &str, dir: &str| {
        std::env::var_os(var)
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|h| h.join(dir)))
    };
    match kind {
        LlmProviderKind::ClaudeCodeAcp => Some(
            home_or("CLAUDE_CONFIG_DIR", ".claude")?
                .join("projects")
                .join(claude_project_slug(cwd))
                .join("memory")
                .join("MEMORY.md"),
        ),
        LlmProviderKind::CodexAcp => Some(
            home_or("CODEX_HOME", ".codex")?
                .join("memories")
                .join("memory_summary.md"),
        ),
        _ => None,
    }
}

/// Claude Code's per-project directory name: the path with every other character as `-`.
fn claude_project_slug(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Tools that only look things up; their output is not worth repeating in a recap.
fn read_only(name: &str, metadata: Option<&ToolMetadata>) -> bool {
    // Agents report MCP tools as `other`, so oxi's own lookups go by name.
    if matches!(name, "chat_read" | "scratchpad_read") {
        return true;
    }
    match metadata {
        Some(meta) => matches!(meta.kind.as_str(), "read" | "search" | "fetch" | "think"),
        None => crate::agent::approval::allowed_in_plan_mode(name),
    }
}

/// How much of each turn a transcript keeps.
#[derive(Clone, Copy, PartialEq)]
enum Detail {
    /// The whole conversation, for an agent that starts a blank session.
    History,
    /// The turns another agent answered, for one that resumes its own session.
    Recap,
    /// Everything, for `chat_read`.
    Archive,
}

/// One rendered message and its number in the chat (1-based), shared with `chat_read`.
struct Turn {
    n: usize,
    text: String,
}

/// `chat` rendered message by message; `offset` is the index of `chat[0]` in the whole chat.
fn turns(chat: &[ChatMessage], offset: usize, detail: Detail) -> Vec<Turn> {
    let assistant_turns = chat.iter().filter(|m| m.role == MsgRole::Assistant).count();
    let mut seen_assistant = 0;
    let mut turns: Vec<Turn> = Vec::new();
    for (i, m) in chat.iter().enumerate() {
        let text = match m.role {
            MsgRole::User if m.is_summary => {
                format!("[Summary of earlier conversation]\n{}", m.text.trim())
            }
            MsgRole::User => {
                let mut t = format!("User: {}", m.user_prompt_text().trim());
                let images = m
                    .attachments
                    .iter()
                    .filter(|a| matches!(a, crate::model::UserAttachment::Image { .. }))
                    .count();
                if images > 0 {
                    t.push_str(&format!("\n[{images} image(s) attached]"));
                }
                t
            }
            MsgRole::Assistant => {
                seen_assistant += 1;
                let recent = assistant_turns - seen_assistant < RECENT_ASSISTANT_TURNS;
                let parts: Vec<String> = m
                    .blocks
                    .iter()
                    .filter_map(|b| block_text(b, recent, detail))
                    .collect();
                if parts.is_empty() {
                    continue;
                }
                let who = m
                    .route
                    .as_deref()
                    .map(|r| format!("Assistant ({} · {})", r.provider.label(), r.model))
                    .unwrap_or_else(|| "Assistant".to_string());
                format!("{who}: {}", parts.join("\n"))
            }
        };
        let n = offset + i + 1;
        turns.push(Turn {
            n,
            text: format!("[#{n}] {text}"),
        });
    }
    turns
}

/// The turns that fit [`ACP_HISTORY_MAX_CHARS`]: the first one (usually the task) and then the
/// newest, with a note naming the turns left out so the agent can `chat_read` them.
fn transcript(chat: &[ChatMessage], offset: usize, detail: Detail) -> String {
    let turns = turns(chat, offset, detail);
    let files = changed_files(chat);
    let header = if files.is_empty() {
        String::new()
    } else {
        format!("[Files changed in this conversation: {}]", files.join(", "))
    };
    let mut budget = ACP_HISTORY_MAX_CHARS.saturating_sub(header.len());
    let mut first = None;
    if let Some(t) = turns.first()
        && chat.first().is_some_and(|m| m.role == MsgRole::User)
    {
        let text = head(&t.text, FIRST_TURN_MAX_CHARS);
        budget = budget.saturating_sub(text.len() + 2);
        first = Some(text);
    }
    let floor = usize::from(first.is_some());
    let mut start = turns.len();
    let mut total = 0;
    while start > floor && total + turns[start - 1].text.len() <= budget {
        start -= 1;
        total += turns[start].text.len() + 2;
    }
    let mut parts: Vec<String> = Vec::new();
    if !header.is_empty() && !turns.is_empty() {
        parts.push(header);
    }
    parts.extend(first);
    if start > floor {
        let (from, to) = (turns[floor].n, turns[start - 1].n);
        let count = start - floor;
        parts.push(format!(
            "[{count} turn(s), #{from}–#{to}, left out for length. Read them with oxi's chat_read tool if you need them.]"
        ));
    }
    parts.extend(turns[start..].iter().map(|t| t.text.clone()));
    parts.join("\n\n")
}

fn block_text(block: &AssistantBlock, recent: bool, detail: Detail) -> Option<String> {
    match block {
        AssistantBlock::Answer(a) if !a.trim().is_empty() => Some(a.trim().to_string()),
        // Codex's own approval reviewer, reported as a tool call: nothing the agent did.
        AssistantBlock::Tool { tool_call_id, .. }
            if tool_call_id.starts_with(crate::agent::acp::GUARDIAN_REVIEW_ID_PREFIX) =>
        {
            None
        }
        AssistantBlock::Tool {
            name,
            args_summary,
            output,
            diff,
            is_error,
            metadata,
            ..
        } => {
            let failed = *is_error == Some(true);
            let mut t = match args_summary {
                Some(args) => format!("[tool {name}: {args}]"),
                None => format!("[tool {name}]"),
            };
            if failed {
                t.push_str(" failed");
            }
            let limit = match detail {
                // Earlier turns read back: archiving them would nest the chat in itself.
                Detail::Archive if name == "chat_read" => 0,
                Detail::Archive => ARCHIVE_TOOL_CHARS,
                _ if failed => TOOL_OUTPUT_RECENT_CHARS,
                Detail::Recap if read_only(name, metadata.as_deref()) => 0,
                Detail::History if recent => TOOL_OUTPUT_RECENT_CHARS,
                _ => TOOL_OUTPUT_OLDER_CHARS,
            };
            let output = if limit == 0 { "" } else { output.trim() };
            if !output.is_empty() {
                t.push('\n');
                t.push_str(&tail(output, limit));
            }
            let diff_limit = match detail {
                Detail::Archive => Some(ARCHIVE_TOOL_CHARS),
                _ if recent => Some(TOOL_OUTPUT_RECENT_CHARS),
                _ => None,
            };
            if let Some(limit) = diff_limit
                && let Some(diff) = diff.as_deref().map(str::trim).filter(|d| !d.is_empty())
            {
                t.push('\n');
                t.push_str(&head(diff, limit));
            }
            Some(t)
        }
        _ => None,
    }
}

/// The end of `s` (test summaries and errors come last), at most `max` bytes.
fn tail(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut cut = s.len() - max;
    while !s.is_char_boundary(cut) {
        cut += 1;
    }
    format!("[…]{}", &s[cut..])
}

fn head(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut cut = max;
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}[…]", &s[..cut])
}

/// Paths written by edit-like tools, in first-touched order.
fn changed_files(chat: &[ChatMessage]) -> Vec<String> {
    let mut files: Vec<String> = Vec::new();
    let mut add = |path: &str| {
        if !path.is_empty() && !files.iter().any(|f| f == path) {
            files.push(path.to_string());
        }
    };
    for b in chat.iter().flat_map(|m| &m.blocks) {
        let AssistantBlock::Tool {
            name,
            args_summary,
            metadata,
            is_error,
            ..
        } = b
        else {
            continue;
        };
        if *is_error == Some(true) {
            continue;
        }
        match metadata {
            Some(meta) if matches!(meta.kind.as_str(), "edit" | "delete" | "move") => {
                for location in &meta.locations {
                    add(&location.path);
                }
            }
            Some(_) => {}
            None if matches!(name.as_str(), "edit" | "write") => {
                if let Some(path) = args_summary
                    .as_deref()
                    .and_then(|a| serde_json::from_str::<serde_json::Value>(a).ok())
                    .and_then(|v| v.get("path").and_then(|p| p.as_str()).map(str::to_owned))
                {
                    add(&path);
                }
            }
            None => {}
        }
    }
    files
}

#[cfg(test)]
#[path = "handoff/tests.rs"]
mod tests;
