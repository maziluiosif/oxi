//! Messages written while a chat is answering: steered into the running turn (when its agent
//! takes messages mid-turn and the setting asks for it), or queued above the composer and sent
//! one per finished response, or sent right away by stopping the current one ("Stop & send").

use eframe::egui::{self, RichText, Ui};

use super::compaction::QueuedSend;
use super::{OxiApp, SessionKey};
use crate::theme::{FS_SMALL, FS_TINY, ICON_SEND, c_accent, c_text, c_text_muted, icon_font};

impl OxiApp {
    /// Move the composer's text and attachments out, leaving it empty.
    pub(crate) fn take_composer_payload(&mut self) -> QueuedSend {
        QueuedSend {
            text: std::mem::take(&mut self.conv.composer.input)
                .trim()
                .to_string(),
            images: std::mem::take(&mut self.conv.composer.pending_images),
            texts: std::mem::take(&mut self.conv.composer.pending_texts),
        }
    }

    /// Send a deferred message into `key`'s chat without disturbing the active composer draft.
    /// Whatever could not be sent (e.g. the chat is mid-compaction) goes back to the queue front.
    pub(crate) fn send_queued(
        &mut self,
        key: SessionKey,
        queued: QueuedSend,
        skip_autocompact: bool,
    ) {
        let draft = self.take_composer_payload_raw();
        self.conv.composer.input = queued.text;
        self.conv.composer.pending_images = queued.images;
        self.conv.composer.pending_texts = queued.texts;
        self.send_message_for(key, skip_autocompact);
        let leftover = self.take_composer_payload();
        self.conv.composer.input = draft.text;
        self.conv.composer.pending_images = draft.images;
        self.conv.composer.pending_texts = draft.texts;
        if !leftover.is_empty() {
            self.run_state_mut(key).queued.push_front(leftover);
        }
    }

    /// Like [`Self::take_composer_payload`] but keeps the draft text byte-for-byte.
    fn take_composer_payload_raw(&mut self) -> QueuedSend {
        QueuedSend {
            text: std::mem::take(&mut self.conv.composer.input),
            images: std::mem::take(&mut self.conv.composer.pending_images),
            texts: std::mem::take(&mut self.conv.composer.pending_texts),
        }
    }

    /// After a response completes, send the next queued message for that chat.
    pub(crate) fn send_next_queued(&mut self, key: SessionKey) {
        if self.run_state(key).is_some_and(|r| r.waiting_response)
            || self.setup_holding_agent(key.workspace_idx).is_some()
        {
            return;
        }
        let Some(next) = self
            .flow
            .sessions
            .get_mut(&key)
            .and_then(|r| r.queued.pop_front())
        else {
            return;
        };
        self.send_queued(key, next, false);
    }

    /// Whether queued message `index` can be handed to `key`'s running turn: oxi's own agent is
    /// answering, and the message is plain text.
    pub(crate) fn can_steer(&self, key: SessionKey, index: usize) -> bool {
        self.run_state(key).is_some_and(|run| {
            run.waiting_response
                && run.steer.is_some()
                && run
                    .queued
                    .get(index)
                    .is_some_and(|q| q.images.is_empty() && q.texts.is_empty())
        })
    }

    /// Hand queued message `index` to the running turn: the agent reads it at its next step,
    /// without stopping (see [`crate::agent::steer`]).
    pub(crate) fn steer_into_turn(&mut self, key: SessionKey, index: usize) {
        if !self.can_steer(key, index) {
            return;
        }
        let cwd = std::path::PathBuf::from(&self.conv.workspaces[key.workspace_idx].root_path);
        let run = self.run_state_mut(key);
        let (Some(inbox), Some(item)) = (run.steer.clone(), run.queued.remove(index)) else {
            return;
        };
        run.steer_pending.push(item.text.clone());
        inbox.push(super::mentions::expand_at_mentions(&item.text, &cwd));
    }

    /// Take a message handed to the running turn back into the queue, if the agent has not
    /// read it yet.
    fn unsteer(&mut self, key: SessionKey, index: usize) {
        let run = self.run_state_mut(key);
        let Some(inbox) = run.steer.clone() else {
            return;
        };
        if inbox.remove(index).is_some() && index < run.steer_pending.len() {
            let text = run.steer_pending.remove(index);
            run.queued.push_front(QueuedSend {
                text,
                images: Vec::new(),
                texts: Vec::new(),
            });
        }
    }

    /// The running turn read a message sent into it: show it as a prompt, and the rest of the
    /// turn as the reply to it.
    pub(crate) fn show_steered_message(&mut self, key: SessionKey, text: &str) {
        let route = self.last_assistant_mut(key).and_then(|m| {
            m.finish_streaming();
            m.route.clone()
        });
        let session = self.session_mut_by_key(key);
        session
            .messages
            .push(crate::model::ChatMessage::user(text, &[]));
        session
            .messages
            .push(crate::model::ChatMessage::streaming_assistant(route));
        let run = self.run_state_mut(key);
        run.steered = true;
        if !run.steer_pending.is_empty() {
            run.steer_pending.remove(0);
        }
        if key == self.active_session_key() {
            self.conv.transcript.scroll_to_bottom_once = true;
        }
    }

    /// The oldest message sent into the running turn did not reach it: back to the queue front.
    pub(crate) fn return_steered_message(&mut self, key: SessionKey) {
        let run = self.run_state_mut(key);
        if run.steer_pending.is_empty() {
            return;
        }
        let text = run.steer_pending.remove(0);
        run.queued.push_front(QueuedSend {
            text,
            images: Vec::new(),
            texts: Vec::new(),
        });
        self.persist_queue(key);
        self.notify_composer("The agent did not take the message mid-response; it is queued.");
    }

    /// Composer shortcuts for the queue, taken before the text field sees the keys:
    /// Cmd/Ctrl+Shift+Enter steers the first queued message into the running turn,
    /// Alt+Up moves the last queued message back into the composer for editing.
    pub(super) fn handle_queue_keys(&mut self, ui: &mut Ui) {
        let key = self.active_session_key();
        let Some(run) = self.run_state(key) else {
            return;
        };
        let count = run.queued.len();
        if count == 0 {
            return;
        }
        let steer = ui.input_mut(|i| {
            i.consume_key(
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                egui::Key::Enter,
            )
        });
        if steer {
            if self.can_steer(key, 0) {
                self.steer_into_turn(key, 0);
                self.persist_queue(key);
            } else {
                self.notify_composer("This response can't take messages; it stays queued.");
            }
            return;
        }
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::ALT, egui::Key::ArrowUp)) {
            self.edit_queued(key, count - 1);
            self.persist_queue(key);
        }
    }

    /// Move queued message `index` back into the composer, ahead of the current draft.
    fn edit_queued(&mut self, key: SessionKey, index: usize) {
        if let Some(item) = self.run_state_mut(key).queued.remove(index) {
            let draft = std::mem::take(&mut self.conv.composer.input);
            self.conv.composer.input = if draft.trim().is_empty() {
                item.text
            } else {
                format!("{}\n\n{}", item.text, draft.trim())
            };
            self.conv.composer.pending_images.extend(item.images);
            self.conv.composer.pending_texts.extend(item.texts);
            self.conv.composer.focus_next_frame = true;
        }
    }

    /// Send queued message `index` now, stopping the response in progress if there is one.
    pub(crate) fn steer_queued(&mut self, key: SessionKey, index: usize) {
        if self.setup_holding_agent(key.workspace_idx).is_some() {
            self.notify_composer("The agent starts once the work tree setup finishes.");
            return;
        }
        let Some(item) = self
            .flow
            .sessions
            .get_mut(&key)
            .and_then(|r| r.queued.remove(index))
        else {
            return;
        };
        if self.run_state(key).is_some_and(|r| r.waiting_response) {
            self.stop_agent_run(key);
        }
        self.send_queued(key, item, false);
    }

    /// Messages handed to the running turn, then the queued ones. Each row reads left to right:
    /// its state, its text, then its main action (Steer while the turn takes messages, Send when
    /// the chat is idle); the rest (Stop & send, Edit, Remove) sits in its "…" menu.
    pub(super) fn render_queue_panel(&mut self, ui: &mut Ui) {
        let key = self.active_session_key();
        let Some(run) = self.run_state(key) else {
            return;
        };
        if run.queued.is_empty() && run.steer_pending.is_empty() {
            return;
        }
        let running = run.waiting_response;
        let steering: Vec<String> = run.steer_pending.iter().map(|t| one_line(t)).collect();
        let rows: Vec<(String, bool)> = (0..run.queued.len())
            .map(|i| (run.queued[i].preview(), self.can_steer(key, i)))
            .collect();
        let mut action = None;
        for (i, preview) in steering.iter().enumerate() {
            queue_row(
                ui,
                ("Steering", c_accent()),
                "Sent into the response in progress; the agent reads it at its next step",
                preview,
                |ui| {
                    if ui
                        .small_button("Undo")
                        .on_hover_text("Take it back to the queue (until the agent reads it)")
                        .clicked()
                    {
                        action = Some(QueueAction::Unsteer(i));
                    }
                },
            );
        }
        let last = rows.len().saturating_sub(1);
        for (i, (preview, steerable)) in rows.iter().enumerate() {
            let hover = if running {
                "Sent when the response finishes"
            } else {
                "Waiting to be sent"
            };
            queue_row(ui, ("Queued", c_text_muted()), hover, preview, |ui| {
                ui.menu_button(RichText::new("…").size(FS_SMALL), |ui| {
                    if running
                        && ui
                            .button("Stop & send")
                            .on_hover_text("Stop the current response and send this instead")
                            .clicked()
                    {
                        action = Some(QueueAction::Send(i));
                    }
                    let edit = if i == last { "Edit  (Alt+↑)" } else { "Edit" };
                    if ui
                        .button(edit)
                        .on_hover_text("Move back into the composer")
                        .clicked()
                    {
                        action = Some(QueueAction::Edit(i));
                    }
                    if ui.button("Remove").clicked() {
                        action = Some(QueueAction::Remove(i));
                    }
                })
                .response
                .on_hover_text("More");
                if !running {
                    if ui
                        .small_button("Send")
                        .on_hover_text("Send this message")
                        .clicked()
                    {
                        action = Some(QueueAction::Send(i));
                    }
                } else if *steerable {
                    let hint = if i == 0 {
                        "  (Cmd/Ctrl+Shift+Enter)"
                    } else {
                        ""
                    };
                    if ui
                        .small_button(RichText::new("Steer").color(c_accent()))
                        .on_hover_text(format!(
                            "Add this to the response in progress: the agent reads it at its \
                                 next step, without stopping{hint}"
                        ))
                        .clicked()
                    {
                        action = Some(QueueAction::Steer(i));
                    }
                }
            });
        }
        ui.add_space(4.0);
        let changed = action.is_some();
        match action {
            Some(QueueAction::Send(i)) => self.steer_queued(key, i),
            Some(QueueAction::Steer(i)) => self.steer_into_turn(key, i),
            Some(QueueAction::Unsteer(i)) => self.unsteer(key, i),
            Some(QueueAction::Remove(i)) => {
                self.run_state_mut(key).queued.remove(i);
            }
            Some(QueueAction::Edit(i)) => self.edit_queued(key, i),
            None => {}
        }
        if changed {
            self.persist_queue(key);
        }
    }

    /// A turn of `key`'s chat ended, however it ended: messages sent into it that it did not
    /// read go back to the queue, and its journal keeps only what is still waiting to be sent.
    pub(crate) fn close_turn_bookkeeping(&mut self, key: SessionKey) {
        let Some(run) = self.flow.sessions.get_mut(&key) else {
            return;
        };
        run.steer_offered = None;
        if let Some(inbox) = run.steer.take() {
            inbox.take();
        }
        for text in std::mem::take(&mut run.steer_pending).into_iter().rev() {
            run.queued.push_front(QueuedSend {
                text,
                images: Vec::new(),
                texts: Vec::new(),
            });
        }
        if let Some(journal) = run.journal.take() {
            let queued: Vec<String> = run.queued.iter().map(|q| q.text.clone()).collect();
            journal.end(&queued);
        }
    }

    /// Keep `key`'s queued messages on disk with the chat, so a restart brings them back
    /// (they were never sent; sending them later repeats nothing).
    pub(crate) fn persist_queue(&mut self, key: SessionKey) {
        if self.is_remote_key(key) || self.conn.no_session {
            return;
        }
        let Some(file) = self.session_by_key(key).session_file.clone() else {
            return;
        };
        let run = self.run_state_mut(key);
        let queued: Vec<String> = run
            .queued
            .iter()
            .map(|q| q.text.clone())
            .filter(|t| !t.trim().is_empty())
            .collect();
        match run.journal.as_mut() {
            Some(journal) => journal.queue(&queued),
            None => crate::session_store::run_journal::save_queue(&file, &queued),
        }
    }

    /// Show what an earlier oxi process left behind in `key`'s chat: a turn it was running
    /// (marked in the transcript, with its unfinished calls) and the messages it had queued
    /// (back in the queue, not sent). Nothing is resent by itself.
    pub(crate) fn recover_interrupted_run(&mut self, key: SessionKey) {
        if self.is_remote_key(key) || self.conn.no_session {
            return;
        }
        if self.run_state(key).is_some_and(|r| r.waiting_response) {
            return;
        }
        let Some(file) = self.session_by_key(key).session_file.clone() else {
            return;
        };
        let Some(recovery) = crate::session_store::run_journal::recover(&file) else {
            return;
        };
        if let Some(interrupted) = recovery.interrupted {
            let root = self.conv.workspaces[key.workspace_idx].root_path.clone();
            let session = self.session_mut_by_key(key);
            if crate::session_store::run_journal::mark_interrupted(
                &mut session.messages,
                &interrupted,
            ) && let Err(e) = crate::session_store::save_session_messages(&root, session)
            {
                log::warn!("saving the interrupted turn: {e}");
            }
            self.run_state_mut(key).interrupted = Some(interrupted);
        }
        let run = self.run_state_mut(key);
        if run.queued.is_empty() {
            run.queued = recovery
                .queued
                .into_iter()
                .map(|text| QueuedSend {
                    text,
                    images: Vec::new(),
                    texts: Vec::new(),
                })
                .collect();
        }
    }

    /// The notice about a turn an earlier process left unfinished, with a way to continue it.
    pub(super) fn render_interrupted_notice(&mut self, ui: &mut Ui) {
        let key = self.active_session_key();
        let Some(interrupted) = self.run_state(key).and_then(|r| r.interrupted.clone()) else {
            return;
        };
        let mut action = None;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.label(
                RichText::new(crate::theme::ICON_WARNING)
                    .font(egui::FontId::new(FS_TINY, icon_font()))
                    .color(crate::theme::c_warning_fg()),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .small_button("Dismiss")
                    .on_hover_text("Nothing is resent")
                    .clicked()
                {
                    action = Some(false);
                }
                if ui
                    .small_button(RichText::new("Continue").color(c_accent()))
                    .on_hover_text(
                        "Ask the agent to check what the interrupted actions did, then carry on",
                    )
                    .clicked()
                {
                    action = Some(true);
                }
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.add(
                        egui::Label::new(
                            RichText::new(crate::session_store::run_journal::summary(&interrupted))
                                .size(FS_SMALL)
                                .color(crate::theme::c_warning_fg()),
                        )
                        .truncate()
                        .selectable(false),
                    )
                    .on_hover_text(crate::session_store::run_journal::describe(&interrupted));
                });
            });
        });
        ui.add_space(4.0);
        match action {
            Some(true) => {
                self.run_state_mut(key).interrupted = None;
                let draft = std::mem::replace(
                    &mut self.conv.composer.input,
                    crate::session_store::run_journal::continue_prompt(&interrupted),
                );
                self.send_message_for(key, false);
                if self.conv.composer.input.is_empty() {
                    self.conv.composer.input = draft;
                }
            }
            Some(false) => self.run_state_mut(key).interrupted = None,
            None => {}
        }
    }
}

/// First non-blank line of a message, for a one-line row.
fn one_line(text: &str) -> String {
    text.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string()
}

enum QueueAction {
    Send(usize),
    Steer(usize),
    Unsteer(usize),
    Edit(usize),
    Remove(usize),
}

impl QueuedSend {
    pub(crate) fn is_empty(&self) -> bool {
        self.text.trim().is_empty() && self.images.is_empty() && self.texts.is_empty()
    }

    /// One-line label for the queue row.
    pub(crate) fn preview(&self) -> String {
        let line = self
            .text
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("");
        let attachments = self.images.len() + self.texts.len();
        match (line.trim(), attachments) {
            ("", n) => format!("{n} attachment(s)"),
            (l, 0) => l.to_string(),
            (l, n) => format!("{l} (+{n} attachment(s))"),
        }
    }
}

/// One row of the queue panel: its state, its text (left-aligned, cut to fit) and its actions
/// on the right, added right to left by `actions`.
fn queue_row(
    ui: &mut Ui,
    (state, color): (&str, egui::Color32),
    state_hover: &str,
    preview: &str,
    actions: impl FnOnce(&mut Ui),
) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(
            RichText::new(ICON_SEND)
                .font(egui::FontId::new(FS_TINY, icon_font()))
                .color(color),
        );
        ui.label(RichText::new(state).size(FS_SMALL).color(color))
            .on_hover_text(state_hover);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            actions(ui);
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.add(
                    egui::Label::new(RichText::new(preview).size(FS_SMALL).color(c_text()))
                        .truncate()
                        .selectable(false),
                )
                .on_hover_text(preview);
            });
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_turn_an_earlier_process_left_running_is_shown_and_its_queue_restored() {
        use crate::session_store::run_journal::{Effect, TurnJournal};
        crate::secrets::use_mock_store();
        let root = crate::fsutil::unique_temp_path(&std::env::temp_dir(), "oxi-recover", "d");
        std::fs::create_dir_all(&root).unwrap();
        let mut app = OxiApp::new();
        let key = app.active_session_key();
        app.conv.workspaces[key.workspace_idx].root_path = root.to_string_lossy().into_owned();
        app.active_session_mut().messages = vec![crate::model::ChatMessage::user("Fix it", &[])];
        crate::session_store::save_session_messages(
            &root.to_string_lossy(),
            app.active_session_mut(),
        )
        .unwrap();
        let file = app.active_session().session_file.clone().unwrap();
        // An earlier process ran the turn and died mid-command.
        let mut journal = TurnJournal::start(&file, "Fix it", None).unwrap();
        journal.tool("t1", "bash", "{\"command\":\"cargo test\"}", Effect::Shell);
        journal.queue(&["then commit".into()]);
        drop(journal);

        app.recover_interrupted_run(key);
        let run = app.run_state(key).unwrap();
        assert!(run.interrupted.is_some());
        assert!(!run.waiting_response, "nothing was resent");
        assert_eq!(run.queued.len(), 1);
        assert_eq!(run.queued[0].text, "then commit");
        let note = crate::model::last_answer(&app.active_session().messages).unwrap_or_default();
        assert!(note.contains("bash cargo test"), "{note}");
        assert!(
            app.last_turn_rollback(key).is_err(),
            "Edit & retry cannot undo what the interrupted command did"
        );
        // Saved with the note, and not reported again.
        let saved = crate::session_store::load_session(&file).unwrap();
        assert_eq!(saved.messages.len(), 2);
        app.run_state_mut(key).interrupted = None;
        app.recover_interrupted_run(key);
        assert!(app.run_state(key).unwrap().interrupted.is_none());

        // A turn the user stopped is not reported as interrupted later.
        app.run_state_mut(key).journal = TurnJournal::start(&file, "Again", None);
        app.run_state_mut(key).begin_waiting_response();
        app.stop_agent_run(key);
        let recovery = crate::session_store::run_journal::recover(&file);
        assert!(
            recovery.as_ref().is_none_or(|r| r.interrupted.is_none()),
            "{recovery:?}"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn preview_uses_first_non_blank_line_and_counts_attachments() {
        let q = QueuedSend {
            text: "\n  fix the build\nthen run tests".into(),
            images: vec![("image/png".into(), vec![1])],
            texts: vec![],
        };
        assert_eq!(q.preview(), "fix the build (+1 attachment(s))");
        assert!(!q.is_empty());
        let empty = QueuedSend {
            text: "  ".into(),
            images: vec![],
            texts: vec![],
        };
        assert!(empty.is_empty());
    }
}
