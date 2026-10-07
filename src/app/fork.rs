//! Right-click a transcript message → "Fork chat from here": a new chat with the history up to
//! that point, so another direction can be tried without losing this one. Forking at one of
//! your messages puts it back in the composer of the new chat to edit and resend.

use eframe::egui::{self, RichText};

use super::OxiApp;
use crate::model::{MsgRole, UserAttachment};
use crate::theme::*;

/// The open message menu: which transcript unit and where it was opened.
#[derive(Clone)]
pub(crate) struct MessageMenu {
    pub workspace_idx: usize,
    pub session_idx: usize,
    /// The unit's messages are `start..end`.
    pub start: usize,
    pub end: usize,
    pub pos: egui::Pos2,
    pub opened_pass: u64,
}

impl OxiApp {
    /// Open the message menu when the transcript unit `start..end` (drawn in `rect`) is
    /// right-clicked.
    pub(crate) fn detect_message_menu(
        &mut self,
        ui: &egui::Ui,
        rect: egui::Rect,
        (wi, si): (usize, usize),
        (start, end): (usize, usize),
    ) {
        let Some(pos) = ui.input(|i| {
            i.pointer
                .secondary_clicked()
                .then(|| i.pointer.interact_pos())
                .flatten()
        }) else {
            return;
        };
        if rect.contains(pos) && ui.clip_rect().contains(pos) {
            self.conv.transcript.message_menu = Some(MessageMenu {
                workspace_idx: wi,
                session_idx: si,
                start,
                end,
                pos,
                opened_pass: ui.ctx().cumulative_pass_nr(),
            });
        }
    }

    pub(crate) fn render_message_menu(&mut self, ctx: &egui::Context) {
        let Some(menu) = self.conv.transcript.message_menu.clone() else {
            return;
        };
        let key = self.active_session_key();
        if (menu.workspace_idx, menu.session_idx) != (key.workspace_idx, key.session_idx)
            || menu.end > self.active_session().messages.len()
        {
            self.conv.transcript.message_menu = None;
            return;
        }
        let is_user = self.active_session().messages[menu.start].role == MsgRole::User;
        let busy = self.active_waiting_response();
        let mut fork = false;
        let mut copy = false;
        let area = egui::Area::new(egui::Id::new("transcript_message_menu"))
            .order(egui::Order::Foreground)
            .fixed_pos(menu.pos)
            .show(ctx, |ui| {
                egui::Frame::menu(ui.style()).show(ui, |ui| {
                    ui.set_min_width(170.0);
                    let label = if is_user {
                        "Edit in a fork"
                    } else {
                        "Fork chat from here"
                    };
                    let hover = if is_user {
                        "New chat with the history before this message, and this message in the composer"
                    } else {
                        "New chat with the history up to this response"
                    };
                    fork = ui
                        .add_enabled(!busy, egui::Button::new(RichText::new(label).size(FS_SMALL)))
                        .on_hover_text(hover)
                        .on_disabled_hover_text("Wait for the response to finish")
                        .clicked();
                    copy = ui
                        .button(RichText::new("Copy message").size(FS_SMALL))
                        .clicked();
                });
            });
        let just_opened = ctx.cumulative_pass_nr() == menu.opened_pass;
        let escape = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        if fork || copy || escape || (!just_opened && area.response.clicked_elsewhere()) {
            self.conv.transcript.message_menu = None;
        }
        if copy {
            let text = message_text(&self.active_session().messages[menu.start..menu.end]);
            ctx.copy_text(text);
        }
        if fork {
            self.fork_active_chat(menu.start, menu.end, is_user);
        }
    }

    /// Fork the active chat: keep `messages[..end]` (or `[..start]` with that user message put
    /// in the composer when `edit_user` is set).
    fn fork_active_chat(&mut self, start: usize, end: usize, edit_user: bool) {
        let source = self.active_session();
        let keep = if edit_user { start } else { end };
        let mut messages = source.messages[..keep].to_vec();
        // Reverting the original turns from the fork would touch the same files twice.
        for message in &mut messages {
            message.changes = None;
        }
        let prefill = edit_user.then(|| source.messages[start].clone());
        let config = source.config.clone();
        let title = format!("{} (fork)", source.title.trim());

        self.new_chat();
        let key = self.active_session_key();
        {
            let session = self.session_mut_by_key(key);
            session.messages = messages;
            session.messages_loaded = true;
            session.title = title;
            if config.is_some() {
                session.config = config;
            }
        }
        if !self.session_by_key(key).messages.is_empty() {
            let root = self.conv.workspaces[key.workspace_idx].root_path.clone();
            if let Err(e) =
                crate::session_store::save_session_messages(&root, self.session_mut_by_key(key))
            {
                self.run_state_mut(key).stream_error = Some(format!("Save session: {e}"));
            }
            self.persist_active_session_selection();
        }
        if let Some(user) = prefill {
            self.conv.composer.input =
                crate::app::mentions::strip_mention_context(&user.text).to_string();
            for attachment in user.attachments {
                match attachment {
                    UserAttachment::Image { mime, data } => {
                        self.conv.composer.pending_images.push((mime, data));
                    }
                    text @ UserAttachment::Text { .. } => {
                        self.conv.composer.pending_texts.push(text)
                    }
                }
            }
        }
        self.conv.transcript.scroll_to_bottom_once = true;
        self.conv.composer.focus_next_frame = true;
    }
}

/// Plain text of a transcript unit, for "Copy message".
fn message_text(messages: &[crate::model::ChatMessage]) -> String {
    let mut parts = Vec::new();
    for message in messages {
        match message.role {
            MsgRole::User => {
                parts.push(crate::app::mentions::strip_mention_context(&message.text).to_string());
            }
            MsgRole::Assistant => {
                for block in &message.blocks {
                    if let crate::model::AssistantBlock::Answer(text) = block
                        && !text.trim().is_empty()
                    {
                        parts.push(text.trim().to_string());
                    }
                }
            }
        }
    }
    parts.join("\n\n")
}
