//! Rewind a chat to before one of your messages, also after oxi restarted: the later turns are
//! dropped and the message goes back to the composer. The files those turns changed can be put
//! back too, from the snapshots saved with each turn (see [`crate::git::checkpoint`]), once a
//! check confirms nothing else changed them since.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex};

use eframe::egui;

use super::{OxiApp, SessionKey};
use crate::agent::tools::{ToolSideEffect, TurnUndoJournal, tool_side_effect};
use crate::git::checkpoint::RestorePlan;
use crate::model::{AssistantBlock, ChatMessage, MsgRole, UserAttachment};

/// How the files changed after a message can be put back.
#[derive(Clone, Default)]
pub(crate) struct RewindFiles {
    /// Repository of the snapshots (as [`OxiApp::turn_repo`] resolves it).
    pub repo: Option<PathBuf>,
    /// Snapshot pairs of the later turns that changed something, oldest first.
    pub turns: Vec<(String, String)>,
    /// Files those turns changed, as far as their saved lists tell.
    pub files: usize,
    /// Later responses that may have changed files with no snapshot to restore them from.
    pub unrestorable: usize,
    /// The latest response's in-memory journal (oxi's own file tools), when it covers the
    /// files instead of snapshots.
    pub journal: Option<Arc<Mutex<TurnUndoJournal>>>,
}

impl RewindFiles {
    /// Anything to put back.
    pub(crate) fn any(&self) -> bool {
        !self.turns.is_empty() || self.journal.is_some()
    }
}

/// The rewind behind the confirmation modal and its background conflict check.
pub(crate) struct RewindCheck {
    pub key: SessionKey,
    /// The user message the chat goes back to before.
    pub index: usize,
    pub files: RewindFiles,
    /// `None` while the check runs (and when there are no snapshots to check).
    pub plan: Option<Result<RestorePlan, String>>,
    rx: Option<Receiver<Result<RestorePlan, String>>>,
}

impl RewindCheck {
    /// Whether restoring the files can go ahead: the check found no file changed since.
    pub(crate) fn files_ready(&self) -> bool {
        if self.files.turns.is_empty() {
            return true;
        }
        matches!(&self.plan, Some(Ok(plan)) if plan.conflicts.is_empty())
    }
}

/// A rewind waiting for its files to be put back before the transcript is cut.
pub(crate) struct RewindJob {
    key: SessionKey,
    session_file: Option<String>,
    index: usize,
    rx: Receiver<Result<(), String>>,
}

/// Whether a reply block may have changed files: a built-in mutating tool, or an agent's
/// edit / delete / move / command call.
fn may_change_files(block: &AssistantBlock) -> bool {
    let AssistantBlock::Tool { name, metadata, .. } = block else {
        return false;
    };
    match metadata.as_deref() {
        Some(meta) => matches!(meta.kind.as_str(), "edit" | "delete" | "move" | "execute"),
        None => {
            name != "scratchpad"
                && matches!(
                    tool_side_effect(name),
                    ToolSideEffect::WorkspaceMutation | ToolSideEffect::Shell
                )
        }
    }
}

impl OxiApp {
    /// How the files changed by the turns after message `index` of `key`'s chat can be put back.
    pub(crate) fn rewind_files(&self, key: SessionKey, index: usize) -> RewindFiles {
        let messages = &self.session_by_key(key).messages;
        let mut out = RewindFiles::default();
        // oxi's own tools journal the latest turn in memory (no snapshots): usable when the
        // rewind only drops that turn.
        let last_user = messages
            .iter()
            .rposition(|m| m.role == MsgRole::User && !m.is_summary);
        let journal = self
            .run_state(key)
            .and_then(|run| run.undo_journal.clone())
            .filter(|_| last_user == Some(index))
            .filter(|j| {
                let j = j.lock().unwrap_or_else(|e| e.into_inner());
                j.unavailable_reason().is_none() && j.checkpoint_base().is_none()
            });
        for message in messages.iter().skip(index + 1) {
            if message.role != MsgRole::Assistant {
                continue;
            }
            match message.changes.as_deref() {
                Some(changes) => {
                    let repo = self.turn_repo(&changes.repo);
                    if out.repo.as_ref().is_some_and(|r| *r != repo) {
                        out.unrestorable += 1;
                        continue;
                    }
                    out.repo = Some(repo);
                    if changes.before != changes.after {
                        out.files += changes.files.as_ref().map_or(0, Vec::len);
                        out.turns
                            .push((changes.before.clone(), changes.after.clone()));
                    }
                }
                None if journal.is_some() => {}
                None => {
                    if message.blocks.iter().any(may_change_files) {
                        out.unrestorable += 1;
                    }
                }
            }
        }
        if out.turns.is_empty() {
            out.repo = None;
        }
        out.journal = journal;
        out
    }

    /// Start checking a rewind of `key`'s chat to before message `index`: which files would go
    /// back, and whether any changed since.
    pub(crate) fn start_rewind_check(
        &mut self,
        ctx: &egui::Context,
        key: SessionKey,
        index: usize,
    ) {
        let files = self.rewind_files(key, index);
        let rx = files.repo.clone().map(|repo| {
            let turns = files.turns.clone();
            let (tx, rx) = std::sync::mpsc::channel();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let _ = tx.send(crate::git::checkpoint::plan_restore(&repo, &turns));
                ctx.request_repaint();
            });
            rx
        });
        self.conv.rewind_check = Some(RewindCheck {
            key,
            index,
            files,
            plan: None,
            rx,
        });
    }

    /// Pick up a finished rewind check.
    pub(crate) fn poll_rewind_check(&mut self) {
        let Some(check) = self.conv.rewind_check.as_mut() else {
            return;
        };
        let Some(rx) = check.rx.as_ref() else {
            return;
        };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("The check stopped unexpectedly".into()),
        };
        check.plan = Some(result);
        check.rx = None;
    }

    /// Rewind `key`'s chat to before message `index`, putting back the files the later turns
    /// changed first when `restore_files`. The transcript is only cut once the files are back;
    /// a refused restore leaves both as they were.
    pub(crate) fn start_rewind(
        &mut self,
        ctx: &egui::Context,
        key: SessionKey,
        index: usize,
        restore_files: bool,
    ) {
        if self.chat_is_busy(key) {
            self.notify_composer("Wait for the response to finish before rewinding.");
            return;
        }
        let files = self.rewind_files(key, index);
        if !restore_files || !files.any() {
            self.apply_rewind(key, index);
            return;
        }
        let root = PathBuf::from(&self.conv.workspaces[key.workspace_idx].root_path);
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let mut result = Ok(());
            if let Some(repo) = &files.repo {
                result = crate::git::checkpoint::restore_turns(repo, &files.turns, None);
            }
            if result.is_ok()
                && let Some(journal) = &files.journal
            {
                result = journal
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .restore(&root);
            }
            let _ = tx.send(result);
            ctx.request_repaint();
        });
        self.conv.rewind_jobs.push(RewindJob {
            key,
            session_file: self.session_by_key(key).session_file.clone(),
            index,
            rx,
        });
        self.notify_composer("Restoring files…");
    }

    /// Finish rewinds whose files are back.
    pub(crate) fn drain_rewind_jobs(&mut self) {
        if self.conv.rewind_jobs.is_empty() {
            return;
        }
        let mut done = Vec::new();
        self.conv.rewind_jobs.retain(|job| match job.rx.try_recv() {
            Ok(result) => {
                done.push((job.key, job.session_file.clone(), job.index, result));
                false
            }
            Err(TryRecvError::Empty) => true,
            Err(TryRecvError::Disconnected) => {
                done.push((
                    job.key,
                    job.session_file.clone(),
                    job.index,
                    Err("The restore stopped unexpectedly".into()),
                ));
                false
            }
        });
        for (key, file, index, result) in done {
            self.conv.explorer.cache.invalidate();
            self.request(crate::git::GitOp::Refresh);
            if let Err(e) = result {
                self.notify_composer(format!("Rewind cancelled: {e}"));
                continue;
            }
            // The chat list may have moved while the files were restored.
            let still_there = self
                .conv
                .workspaces
                .get(key.workspace_idx)
                .is_some_and(|w| {
                    w.sessions
                        .get(key.session_idx)
                        .is_some_and(|s| s.session_file == file && s.messages.len() > index)
                });
            if still_there {
                self.apply_rewind(key, index);
            } else {
                self.notify_composer("Files restored; the chat changed meanwhile, so it was kept.");
            }
        }
    }

    /// Cut `key`'s chat before message `index` and put that message in the composer.
    fn apply_rewind(&mut self, key: SessionKey, index: usize) {
        let Some(user) = self.session_by_key(key).messages.get(index).cloned() else {
            return;
        };
        self.session_mut_by_key(key).messages.truncate(index);
        self.invalidate_wire_cache(key);
        self.reset_acp_session(key);
        {
            let run = self.run_state_mut(key);
            run.undo_journal = None;
            run.last_user_prompt = None;
            run.stream_error = None;
        }
        if self.is_remote_key(key) {
            self.remote_save_session(key);
        } else if !self.conn.no_session {
            let root = self.conv.workspaces[key.workspace_idx].root_path.clone();
            if let Err(e) =
                crate::session_store::save_session_messages(&root, self.session_mut_by_key(key))
            {
                self.run_state_mut(key).stream_error = Some(format!("Save session: {e}"));
            }
        }
        if key == self.active_session_key() {
            self.put_message_in_composer(&user);
        } else {
            // Another chat is showing: the message waits as that chat's draft.
            let session = self.session_mut_by_key(key);
            if session.input_text.trim().is_empty() {
                session.input_text =
                    crate::app::mentions::strip_mention_context(&user.text).to_string();
                for attachment in user.attachments {
                    match attachment {
                        UserAttachment::Image { mime, data } => {
                            session.pending_images.push((mime, data))
                        }
                        text @ UserAttachment::Text { .. } => session.pending_texts.push(text),
                    }
                }
            }
        }
        self.notify_composer("Rewound. The message is back in the composer.");
    }

    /// Load a sent message into the composer to edit and resend. A draft there moves to the
    /// input history (Up recalls it).
    fn put_message_in_composer(&mut self, user: &ChatMessage) {
        let draft = self.conv.composer.input.trim().to_string();
        if !draft.is_empty() {
            self.push_input_history(&draft);
        }
        self.conv.composer.input =
            crate::app::mentions::strip_mention_context(&user.text).to_string();
        self.conv.composer.pending_images.clear();
        self.conv.composer.pending_texts.clear();
        for attachment in &user.attachments {
            match attachment {
                UserAttachment::Image { mime, data } => {
                    self.conv
                        .composer
                        .pending_images
                        .push((mime.clone(), data.clone()));
                }
                text @ UserAttachment::Text { .. } => {
                    self.conv.composer.pending_texts.push(text.clone())
                }
            }
        }
        self.conv.composer.focus_next_frame = true;
        self.conv.transcript.scroll_to_bottom_once = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TurnChanges;

    fn reply_with(changes: Option<TurnChanges>, tool: Option<&str>) -> ChatMessage {
        let mut m = ChatMessage::streaming_assistant(None);
        if let Some(name) = tool {
            m.blocks.push(AssistantBlock::Tool {
                tool_call_id: "t".into(),
                name: name.into(),
                args_summary: None,
                output: String::new(),
                diff: None,
                is_error: Some(false),
                full_output_path: None,
                output_truncated: false,
                metadata: None,
            });
        }
        m.append_answer("done");
        m.finish_streaming();
        m.changes = changes.map(Box::new);
        m
    }

    #[test]
    fn a_rewind_after_a_restart_restores_saved_snapshots_and_cuts_the_chat() {
        crate::secrets::use_mock_store();
        let root = crate::fsutil::unique_temp_path(&std::env::temp_dir(), "oxi-rewind", "d");
        std::fs::create_dir_all(&root).unwrap();
        git2::Repository::init(&root).unwrap();
        std::fs::write(root.join("a.txt"), "original\n").unwrap();
        let t0 = crate::git::checkpoint::snapshot(&root).unwrap();
        std::fs::write(root.join("a.txt"), "first turn\n").unwrap();
        let t1 = crate::git::checkpoint::snapshot(&root).unwrap();
        std::fs::write(root.join("b.txt"), "second turn\n").unwrap();
        let t2 = crate::git::checkpoint::snapshot(&root).unwrap();
        let turn = |before: &str, after: &str| TurnChanges {
            repo: t0.repo.to_string_lossy().into_owned(),
            before: before.into(),
            after: after.into(),
            files: None,
        };

        // As loaded from disk: no run state, only the saved snapshot pairs.
        let mut app = OxiApp::new();
        app.conn.no_session = true;
        let key = app.active_session_key();
        app.active_session_mut().messages = vec![
            ChatMessage::user("first", &[]),
            reply_with(Some(turn(&t0.tree, &t1.tree)), None),
            ChatMessage::user("second", &[]),
            reply_with(Some(turn(&t1.tree, &t2.tree)), None),
            ChatMessage::user("third", &[]),
            // Read-only tools without a snapshot do not block anything.
            reply_with(Some(turn(&t2.tree, &t2.tree)), Some("read")),
        ];
        let files = app.rewind_files(key, 2);
        assert_eq!(files.turns.len(), 1, "an unchanged turn needs no restore");
        assert_eq!(files.unrestorable, 0);
        let files = app.rewind_files(key, 0);
        assert_eq!(files.turns.len(), 2);

        let ctx = egui::Context::default();
        app.start_rewind(&ctx, key, 0, true);
        for _ in 0..500 {
            app.drain_rewind_jobs();
            if app.conv.rewind_jobs.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(app.active_session().messages.is_empty());
        assert_eq!(app.conv.composer.input, "first");
        assert_eq!(
            std::fs::read_to_string(root.join("a.txt")).unwrap(),
            "original\n"
        );
        assert!(!root.join("b.txt").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn replies_that_changed_files_without_a_snapshot_are_counted() {
        crate::secrets::use_mock_store();
        let mut app = OxiApp::new();
        let key = app.active_session_key();
        app.active_session_mut().messages = vec![
            ChatMessage::user("edit it", &[]),
            reply_with(None, Some("edit")),
            ChatMessage::user("look", &[]),
            reply_with(None, Some("read")),
        ];
        let files = app.rewind_files(key, 0);
        assert_eq!(files.unrestorable, 1);
        assert!(!files.any());
    }
}
