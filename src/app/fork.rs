//! Right-click a transcript message → "Fork chat from here": a new chat with the history up to
//! that point, so another direction can be tried without losing this one. Forking at one of
//! your messages puts it back in the composer of the new chat to edit and resend. A text
//! selection survives the right-click, and the menu offers "Copy selection" for it.

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
        let Some(pos) = self.conv.transcript.secondary_click.or_else(|| {
            ui.input(|i| {
                i.pointer
                    .secondary_clicked()
                    .then(|| i.pointer.interact_pos())
                    .flatten()
            })
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

    /// Keep a right-click over a selected transcript away from egui. Its labels treat any press
    /// as the start of a new selection, so the part from the pointer down vanished until the
    /// held selection was put back a frame later. The click is handed to `detect_message_menu`
    /// on release instead.
    pub(crate) fn route_transcript_right_click(
        &mut self,
        ctx: &egui::Context,
        raw_input: &mut egui::RawInput,
    ) {
        let transcript = &mut self.conv.transcript;
        transcript.secondary_click = None;
        let viewport = transcript.viewport;
        let mut swallowing = transcript.swallowing_secondary;
        let mut click = None;
        raw_input.events.retain(|event| {
            let egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Secondary,
                pressed,
                ..
            } = *event
            else {
                return true;
            };
            if pressed {
                swallowing = viewport.is_some_and(|(layer, rect)| {
                    rect.contains(pos) && ctx.layer_id_at(pos) == Some(layer)
                }) && ctx
                    .plugin::<egui::text_selection::LabelSelectionState>()
                    .lock()
                    .has_selection();
                !swallowing
            } else if swallowing {
                swallowing = false;
                click = Some(pos);
                false
            } else {
                true
            }
        });
        transcript.swallowing_secondary = swallowing;
        transcript.secondary_click = click;
    }

    /// Keep the transcript's text selection through a right-click and the message menu that
    /// follows (egui collapses a label selection on any press). Call before the transcript's
    /// labels are drawn; returns true when this frame injected a Copy event for "Copy selection",
    /// which the caller removes once the labels have seen it.
    pub(crate) fn hold_selection_for_message_menu(&mut self, ctx: &egui::Context) -> bool {
        use egui::PointerButton::{Primary, Secondary};
        let plugin = ctx.plugin::<egui::text_selection::LabelSelectionState>();
        let transcript = &mut self.conv.transcript;
        if std::mem::take(&mut transcript.copy_held_selection) {
            let Some(held) = transcript.held_selection.take() else {
                return false;
            };
            *plugin.lock() = held;
            ctx.input_mut(|i| i.events.push(egui::Event::Copy));
            return true;
        }
        let swallowed = transcript.swallowing_secondary || transcript.secondary_click.is_some();
        let (secondary, primary) = ctx.input(|i| {
            let active = |b| i.pointer.button_down(b) || i.pointer.button_released(b);
            (active(Secondary) || swallowed, active(Primary))
        });
        match &transcript.held_selection {
            None => {
                let pressed = swallowed || ctx.input(|i| i.pointer.button_pressed(Secondary));
                if pressed && plugin.lock().has_selection() {
                    transcript.held_selection = Some(plugin.lock().clone());
                }
            }
            // A left click (on the menu or elsewhere) may change the selection as usual.
            Some(held) if (secondary || transcript.message_menu.is_some()) && !primary => {
                *plugin.lock() = held.clone();
            }
            Some(_) if !secondary && transcript.message_menu.is_none() => {
                transcript.held_selection = None;
            }
            Some(_) => {}
        }
        false
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
        let rewindable = is_user && !self.active_session().messages[menu.start].is_summary;
        let busy = self.active_waiting_response();
        let mut fork = false;
        let mut rewind = false;
        let mut copy = false;
        let mut copy_selection = false;
        let has_selection = self.conv.transcript.held_selection.is_some();
        let area = egui::Area::new(egui::Id::new("transcript_message_menu"))
            .order(egui::Order::Foreground)
            .fixed_pos(menu.pos)
            .show(ctx, |ui| {
                egui::Frame::menu(ui.style()).show(ui, |ui| {
                    ui.set_min_width(170.0);
                    if has_selection {
                        copy_selection = ui
                            .button(RichText::new("Copy selection").size(FS_SMALL))
                            .clicked();
                        ui.separator();
                    }
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
                    if rewindable {
                        rewind = ui
                            .add_enabled(
                                !busy,
                                egui::Button::new(RichText::new("Rewind to here\u{2026}").size(FS_SMALL)),
                            )
                            .on_hover_text(
                                "Go back to before this message in this chat, optionally \
                                 restoring the files the later responses changed",
                            )
                            .on_disabled_hover_text("Wait for the response to finish")
                            .clicked();
                    }
                    copy = ui
                        .button(RichText::new("Copy message").size(FS_SMALL))
                        .clicked();
                });
            });
        let just_opened = ctx.cumulative_pass_nr() == menu.opened_pass;
        let escape = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        if fork
            || rewind
            || copy
            || copy_selection
            || escape
            || (!just_opened && area.response.clicked_elsewhere())
        {
            self.conv.transcript.message_menu = None;
            if copy_selection {
                self.conv.transcript.copy_held_selection = true;
                ctx.request_repaint();
            } else {
                self.conv.transcript.held_selection = None;
            }
        }
        if copy {
            let text = message_text(&self.active_session().messages[menu.start..menu.end]);
            ctx.copy_text(text);
        }
        if fork {
            self.fork_active_chat(menu.start, menu.end, is_user);
        }
        if rewind {
            self.request_confirm(super::state::ConfirmAction::Rewind {
                wi: menu.workspace_idx,
                si: menu.session_idx,
                index: menu.start,
                restore_files: false,
            });
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
        // The copied transcript fixes the context at this point. A native session copied
        // later could already contain further turns from the original chat.

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ChatMessage;

    #[test]
    fn a_fork_keeps_its_transcript_without_binding_to_the_mutable_parent_session() {
        crate::secrets::use_mock_store();
        let dir = std::env::temp_dir().join(format!("oxi-fork-snapshot-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        let root = dir.to_string_lossy().into_owned();
        let mut app = OxiApp::new();
        let mut session = OxiApp::blank_session("Original");
        session
            .messages
            .push(ChatMessage::user("First request", &[]));
        let mut reply = ChatMessage::streaming_assistant(None);
        reply.append_answer("First answer");
        reply.finish_streaming();
        session.messages.push(reply);
        crate::session_store::save_session_messages(&root, &mut session).unwrap();
        app.conv.workspaces = vec![crate::app::Workspace {
            root_path: root,
            sessions: vec![session],
            active: 0,
            sidebar_folded: false,
            pinned: vec![],
            folded_groups: vec![],
            worktree: None,
        }];
        app.conv.active_workspace = 0;
        app.conv.no_workspace = false;
        app.flow.sessions.clear();
        let source = app.acp_session_key(app.active_session_key());
        crate::agent::acp::bind_session(&source, "test-acp", &dir, "mutable-parent");

        app.fork_active_chat(1, 2, false);
        let fork = app.acp_session_key(app.active_session_key());
        app.conv.workspaces[0].sessions[1]
            .messages
            .push(ChatMessage::user("Later request in the original", &[]));
        assert_eq!(app.active_session().messages.len(), 2);
        assert_eq!(app.active_session().messages[0].text, "First request");
        let bindings: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(crate::app_dirs::data_dir().join("acp/sessions.json"))
                .unwrap(),
        )
        .unwrap();
        assert!(bindings.get(&source).is_some());
        assert!(
            bindings.get(&fork).is_none(),
            "the fork must start from its saved transcript"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
