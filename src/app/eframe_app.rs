use eframe::egui::{self, Frame, Key, LayerId, Modifiers};

use crate::model::MsgRole;
use crate::theme::*;

use super::OxiApp;

impl eframe::App for OxiApp {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        if self.conv.settings_open || !ctx.memory(|m| m.has_focus(egui::Id::new("composer_input")))
        {
            return;
        }

        // A bitmap-only Windows clipboard may produce Ctrl+V but no `Paste(String)`. Inspect raw
        // input before egui consumes it, and suppress normal paste only if an image was attached.
        let paste_requested = raw_input.events.iter().any(|event| match event {
            egui::Event::Paste(_) => true,
            egui::Event::Key {
                key: Key::V,
                pressed: true,
                modifiers,
                ..
            } => modifiers.command,
            _ => false,
        });
        // Try the bitmap clipboard globally. This also covers a freshly opened app where the
        // composer requests focus during this frame but wasn't focused in the previous frame.
        if paste_requested && self.paste_clipboard_image() {
            self.clipboard_image_paste_key_down = true;
            raw_input.events.retain(|event| {
                !matches!(event, egui::Event::Paste(_))
                    && !matches!(
                        event,
                        egui::Event::Key {
                            key: Key::V,
                            pressed: true,
                            modifiers,
                            ..
                        } if modifiers.command
                    )
            });
        }
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_windows_clipboard_image_paste(ctx);
        self.handle_global_shortcuts(ctx);
        self.consume_dropped_files(ctx);
        self.drain_agent(ctx);
        self.drain_models(ctx);
        self.drain_local_models(ctx);
        self.drain_voice(ctx);
        self.drain_voice_models(ctx);
        self.drain_ssh_test(ctx);
        self.pin_observed_host_keys();
        self.drain_oauth(ctx);
        self.ensure_update_checked(ctx, false);
        self.drain_update_check(ctx);
        self.bind_git_ctx(ctx);
        self.ensure_active_models_fetched(ctx);
        self.drain_git(ctx);
        self.drain_commit_gen(ctx);
        self.drain_compaction(ctx);
        let any_assistant_streaming = self.conv.workspaces.iter().any(|w| {
            w.sessions.iter().any(|s| {
                s.messages
                    .last()
                    .is_some_and(|m| m.role == MsgRole::Assistant && m.streaming)
            })
        });
        if self.any_waiting_response() || any_assistant_streaming || self.conv.voice_ui.transcribing
        {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        crate::theme::set_chat_column_max_width(ui.ctx(), self.conv.settings.chat_column_max_width);
        ui.ctx().layer_painter(LayerId::background()).rect_filled(
            ui.ctx().content_rect(),
            0,
            c_bg_main(),
        );

        // Persistent status bar (sidebar/git/terminal/settings toggles + branch) — claims the very
        // bottom strip of the window, below the terminal panel. It stays visible on Settings too;
        // clicking any non-settings toggle leaves Settings and returns to the normal chat layout.
        self.render_status_bar(ui);

        // Bottom terminal panel (added before the CentralPanel so it claims the bottom strip and
        // the chat area fills what's left). Hidden while the settings page is open.
        if self.conv.terminal_open && !self.conv.settings_open {
            self.render_terminal_panel(ui);
        }

        // Composer lives in the chat column (see `render_main_area`) so the sidebar can span the
        // full window height and stays aligned with the centered transcript column.
        egui::CentralPanel::default()
            .frame(Frame::new().fill(c_bg_main()))
            .show(ui, |ui| {
                if self.conv.settings_open {
                    self.render_settings_page(ui);
                } else {
                    self.render_main_area(ui);
                }
            });

        self.render_file_picker(ui.ctx());

        // Shared destructive-action confirmation modal, on top of everything.
        self.render_confirm_prompt(ui.ctx());
    }
}

impl OxiApp {
    /// egui-winit consumes Ctrl+V while asking arboard for text. A Windows clipboard containing
    /// only a screenshot therefore produces neither `Event::Paste` nor a usable key event.
    /// Poll the physical chord before rendering and attach the bitmap once per key press.
    #[cfg(windows)]
    fn poll_windows_clipboard_image_paste(&mut self, ctx: &egui::Context) {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL};

        if !ctx.memory(|m| m.has_focus(egui::Id::new("composer_input"))) {
            self.clipboard_image_paste_key_down = false;
            return;
        }

        // SAFETY: GetAsyncKeyState has no pointer arguments and is safe for GUI-thread polling.
        let down =
            unsafe { GetAsyncKeyState(VK_CONTROL as i32) < 0 && GetAsyncKeyState('V' as i32) < 0 };
        if down && !self.clipboard_image_paste_key_down {
            // Clipboard access can transiently fail when another Windows process still owns the
            // global clipboard lock. Keep retrying on subsequent frames while Ctrl+V is held,
            // and latch only after an image was actually consumed.
            self.clipboard_image_paste_key_down = self.paste_clipboard_image();
            if !self.clipboard_image_paste_key_down {
                ctx.request_repaint_after(std::time::Duration::from_millis(16));
            }
        } else if !down {
            self.clipboard_image_paste_key_down = false;
        }
    }

    #[cfg(not(windows))]
    fn poll_windows_clipboard_image_paste(&mut self, _ctx: &egui::Context) {}

    /// Global shortcuts that work outside the composer TextEdit.
    /// Cmd/Ctrl+N new chat, Cmd/Ctrl+` terminal, Cmd/Ctrl+B chats sidebar,
    /// Cmd/Ctrl+E workspace explorer,
    /// Cmd/Ctrl+Shift+B git changes panel, Cmd/Ctrl+P opens any workspace file,
    /// Cmd/Ctrl+S saves, Cmd/Ctrl+F finds and F12 navigates
    /// to a Rust definition in an open editor, Cmd/Ctrl+. stops a run.
    fn handle_global_shortcuts(&mut self, ctx: &egui::Context) {
        let cmd = Modifiers::COMMAND;
        let cmd_shift = Modifiers::COMMAND.plus(Modifiers::SHIFT);
        let (
            new_chat,
            toggle_term,
            toggle_sidebar,
            toggle_explorer,
            toggle_git,
            open_file,
            save_file,
            find_file,
            find_replace,
            goto_definition,
            stop,
            escape,
        ) = ctx.input(|i| {
            (
                i.modifiers.matches_exact(cmd) && i.key_pressed(Key::N),
                i.modifiers.matches_exact(cmd) && i.key_pressed(Key::Backtick),
                i.modifiers.matches_exact(cmd) && i.key_pressed(Key::B),
                i.modifiers.matches_exact(cmd) && i.key_pressed(Key::E),
                i.modifiers.matches_exact(cmd_shift) && i.key_pressed(Key::B),
                i.modifiers.matches_exact(cmd) && i.key_pressed(Key::P),
                i.modifiers.matches_exact(cmd) && i.key_pressed(Key::S),
                i.modifiers.matches_exact(cmd) && i.key_pressed(Key::F),
                i.modifiers.matches_exact(cmd) && i.key_pressed(Key::H),
                i.modifiers.is_none() && i.key_pressed(Key::F12),
                i.modifiers.matches_exact(cmd) && i.key_pressed(Key::Period),
                i.key_pressed(Key::Escape),
            )
        });

        if new_chat && !self.conv.settings_open {
            self.new_chat();
        }
        if toggle_term {
            self.request_settings_exit(super::state::SettingsExitAction::ToggleTerminal);
        }
        if toggle_sidebar {
            self.request_settings_exit(super::state::SettingsExitAction::ToggleSidebar);
        }
        if toggle_explorer {
            self.request_settings_exit(super::state::SettingsExitAction::ToggleExplorer);
        }
        if toggle_git {
            self.request_settings_exit(super::state::SettingsExitAction::ToggleGitChanges);
        }
        if open_file && !self.conv.settings_open && !self.conv.editor.file_picker_open {
            self.open_file_picker();
        }
        if save_file && !self.conv.settings_open && self.conv.editor.active_document().is_some() {
            self.save_editor_file();
        }
        if (find_file || find_replace)
            && !self.conv.settings_open
            && self.conv.editor.active_document().is_some()
        {
            let find_next = find_file
                && self.conv.editor.find_open
                && !self.conv.editor.find_query.is_empty();
            if find_next {
                let match_count = self
                    .conv
                    .editor
                    .active_document()
                    .map(|document| {
                        super::file_explorer::find_match_ranges(
                            &document.content,
                            &self.conv.editor.find_query,
                            self.conv.editor.find_case_sensitive,
                        )
                        .len()
                    })
                    .unwrap_or(0);
                if match_count > 0 {
                    self.conv.editor.find_active_match = if self.conv.editor.find_has_navigated {
                        (self.conv.editor.find_active_match + 1) % match_count
                    } else {
                        0
                    };
                    self.conv.editor.find_has_navigated = true;
                    self.conv.editor.find_select_pending = false;
                    self.conv.editor.find_reveal_pending = true;
                    // Cmd/Ctrl+F is Find Next and must keep keyboard focus in the actual
                    // Find widget after the editor scroll/caret update runs this frame.
                    self.conv.editor.find_focus_editor_pending = false;
                    self.conv.editor.focus_find_next_frame = true;
                }
            } else {
                self.conv.editor.find_open = true;
                self.conv.editor.find_replace_open = find_replace;
                // Opening Find only focuses its field; it must not move the document.
                self.conv.editor.find_select_pending = false;
                self.conv.editor.find_reveal_pending = false;
                self.conv.editor.find_has_navigated = false;
                self.conv.editor.focus_find_next_frame = true;
                ctx.memory_mut(|memory| {
                    memory.request_focus(egui::Id::new("workspace_editor_find"));
                });
            }
        }
        if goto_definition
            && !self.conv.settings_open
            && self.conv.editor.active_document().is_some_and(|document| {
                document.path.extension().and_then(|ext| ext.to_str()) == Some("rs")
            })
        {
            self.conv.editor.goto_definition_requested = true;
        }
        if stop && self.any_waiting_response() {
            self.send_abort();
        }
        // Escape: leave settings, or hand focus back to the composer. Skip when the
        // terminal panel is showing so Escape stays available to the PTY (the terminal
        // is hidden while Settings is open, so Escape works there regardless). Escape
        // for the shared confirm modal is handled by the modal itself.
        if escape && self.conv.editor.file_picker_open {
            self.cancel_file_picker();
        } else if escape && self.conv.editor.find_open {
            self.conv.editor.find_open = false;
            // Preserve/apply the current result before returning focus to the editor.
            self.conv.editor.find_select_pending = self.conv.editor.find_has_navigated;
            self.conv.editor.find_focus_editor_pending = true;
        } else if escape
            && (!self.conv.terminal_open || self.conv.settings_open)
            && !self.confirm_prompt_open()
        {
            if self.conv.settings_open {
                if self.conv.settings_exit_prompt.is_some() {
                    // Modal already up: Escape means "Stay".
                    self.conv.settings_exit_prompt = None;
                } else if self.settings_dirty() {
                    self.request_settings_exit(super::state::SettingsExitAction::BackToChat);
                } else {
                    self.close_settings_page();
                }
            } else {
                self.conv.focus_chat_input_next_frame = true;
            }
        }
    }
}
