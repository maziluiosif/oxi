use eframe::egui::{self, Frame, LayerId};

use crate::model::MsgRole;
use crate::theme::*;

use super::OxiApp;

impl eframe::App for OxiApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.consume_dropped_files(ctx);
        self.drain_agent(ctx);
        self.drain_models(ctx);
        self.drain_ssh_test(ctx);
        self.pin_observed_host_keys();
        self.drain_oauth(ctx);
        self.ensure_update_checked(ctx, false);
        self.drain_update_check(ctx);
        self.bind_git_ctx(ctx);
        self.ensure_active_models_fetched(ctx);
        self.drain_git(ctx);
        self.drain_commit_gen(ctx);
        let any_assistant_streaming = self.conv.workspaces.iter().any(|w| {
            w.sessions.iter().any(|s| {
                s.messages
                    .last()
                    .is_some_and(|m| m.role == MsgRole::Assistant && m.streaming)
            })
        });
        if self.any_waiting_response() || any_assistant_streaming {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        ui.ctx().layer_painter(LayerId::background()).rect_filled(
            ui.ctx().content_rect(),
            0,
            c_bg_main(),
        );

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
    }
}
