//! Messages queued while a chat is answering: shown above the composer, sent one per finished
//! response, or sent right away by interrupting the current one ("Send now").

use eframe::egui::{self, RichText, Ui};

use super::compaction::QueuedSend;
use super::{OxiApp, SessionKey};
use crate::theme::{FS_SMALL, FS_TINY, ICON_SEND, c_text_muted, icon_font};

impl OxiApp {
    /// Move the composer's text and attachments out, leaving it empty.
    pub(crate) fn take_composer_payload(&mut self) -> QueuedSend {
        QueuedSend {
            text: std::mem::take(&mut self.conv.input).trim().to_string(),
            images: std::mem::take(&mut self.conv.pending_images),
            texts: std::mem::take(&mut self.conv.pending_texts),
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
        self.conv.input = queued.text;
        self.conv.pending_images = queued.images;
        self.conv.pending_texts = queued.texts;
        self.send_message_for(key, skip_autocompact);
        let leftover = self.take_composer_payload();
        self.conv.input = draft.text;
        self.conv.pending_images = draft.images;
        self.conv.pending_texts = draft.texts;
        if !leftover.is_empty() {
            self.run_state_mut(key).queued.push_front(leftover);
        }
    }

    /// Like [`Self::take_composer_payload`] but keeps the draft text byte-for-byte.
    fn take_composer_payload_raw(&mut self) -> QueuedSend {
        QueuedSend {
            text: std::mem::take(&mut self.conv.input),
            images: std::mem::take(&mut self.conv.pending_images),
            texts: std::mem::take(&mut self.conv.pending_texts),
        }
    }

    /// After a response completes, send the next queued message for that chat.
    pub(crate) fn send_next_queued(&mut self, key: SessionKey) {
        if self.run_state(key).is_some_and(|r| r.waiting_response) {
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

    /// Send queued message `index` now, stopping the response in progress if there is one.
    pub(crate) fn steer_queued(&mut self, key: SessionKey, index: usize) {
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

    /// Queued messages for the active chat, each with Send now / Edit / Remove.
    pub(super) fn render_queue_panel(&mut self, ui: &mut Ui) {
        let key = self.active_session_key();
        let Some(run) = self.run_state(key) else {
            return;
        };
        if run.queued.is_empty() {
            return;
        }
        let running = run.waiting_response;
        let rows: Vec<String> = run.queued.iter().map(QueuedSend::preview).collect();
        let mut action = None;
        for (i, preview) in rows.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 5.0;
                ui.label(
                    RichText::new(ICON_SEND)
                        .font(egui::FontId::new(FS_TINY, icon_font()))
                        .color(c_text_muted()),
                );
                ui.label(RichText::new("Queued").size(FS_SMALL).color(c_text_muted()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .small_button("×")
                        .on_hover_text("Remove from queue")
                        .clicked()
                    {
                        action = Some(QueueAction::Remove(i));
                    }
                    if ui
                        .small_button("Edit")
                        .on_hover_text("Move back into the composer")
                        .clicked()
                    {
                        action = Some(QueueAction::Edit(i));
                    }
                    let (label, hover) = if running {
                        (
                            "Send now",
                            "Stop the current response and send this instead",
                        )
                    } else {
                        ("Send", "Send this message")
                    };
                    if ui.small_button(label).on_hover_text(hover).clicked() {
                        action = Some(QueueAction::Send(i));
                    }
                    ui.add(
                        egui::Label::new(RichText::new(preview).size(FS_SMALL))
                            .truncate()
                            .selectable(false),
                    )
                    .on_hover_text(preview);
                });
            });
        }
        ui.add_space(4.0);
        match action {
            Some(QueueAction::Send(i)) => self.steer_queued(key, i),
            Some(QueueAction::Remove(i)) => {
                self.run_state_mut(key).queued.remove(i);
            }
            Some(QueueAction::Edit(i)) => {
                if let Some(item) = self.run_state_mut(key).queued.remove(i) {
                    let draft = std::mem::take(&mut self.conv.input);
                    self.conv.input = if draft.trim().is_empty() {
                        item.text
                    } else {
                        format!("{}\n\n{}", item.text, draft.trim())
                    };
                    self.conv.pending_images.extend(item.images);
                    self.conv.pending_texts.extend(item.texts);
                    self.conv.focus_chat_input_next_frame = true;
                }
            }
            None => {}
        }
    }
}

enum QueueAction {
    Send(usize),
    Edit(usize),
    Remove(usize),
}

impl QueuedSend {
    pub(crate) fn is_empty(&self) -> bool {
        self.text.trim().is_empty() && self.images.is_empty() && self.texts.is_empty()
    }

    /// One-line label for the queue row.
    fn preview(&self) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

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
