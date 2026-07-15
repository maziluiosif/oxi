use eframe::egui::{self, Frame, Key, LayerId, Modifiers};

use crate::model::MsgRole;
use crate::theme::*;

use super::OxiApp;

impl eframe::App for OxiApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
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

        // Shared destructive-action confirmation modal, on top of everything.
        self.render_confirm_prompt(ui.ctx());
    }
}

impl OxiApp {
    /// Global shortcuts that work outside the composer TextEdit.
    /// Cmd/Ctrl+N new chat, Cmd/Ctrl+` terminal, Cmd/Ctrl+B sidebar,
    /// Cmd/Ctrl+. stop run, Escape focuses composer (or closes settings).
    fn handle_global_shortcuts(&mut self, ctx: &egui::Context) {
        let cmd = Modifiers::COMMAND;
        let (new_chat, toggle_term, toggle_sidebar, stop, escape) = ctx.input(|i| {
            (
                i.modifiers.matches_exact(cmd) && i.key_pressed(Key::N),
                i.modifiers.matches_exact(cmd) && i.key_pressed(Key::Backtick),
                i.modifiers.matches_exact(cmd) && i.key_pressed(Key::B),
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
        if stop && self.any_waiting_response() {
            self.send_abort();
        }
        // Escape: leave settings, or hand focus back to the composer. Skip when the
        // terminal panel is showing so Escape stays available to the PTY (the terminal
        // is hidden while Settings is open, so Escape works there regardless). Escape
        // for the shared confirm modal is handled by the modal itself.
        if escape
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
