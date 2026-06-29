use eframe::egui::{self, Frame, LayerId};

use crate::model::MsgRole;
use crate::theme::*;

use super::OxiApp;

impl eframe::App for OxiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.layer_painter(LayerId::background())
            .rect_filled(ctx.screen_rect(), 0.0, c_bg_main());

        self.consume_dropped_files(ctx);
        self.drain_agent(ctx);
        self.drain_models(ctx);
        self.drain_oauth(ctx);
        self.bind_git_ctx(ctx);
        self.drain_git(ctx);
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

        // Bottom terminal panel (added before the CentralPanel so it claims the bottom strip and
        // the chat area fills what's left). Hidden while the settings page is open.
        if self.conv.terminal_open && !self.conv.settings_open {
            self.render_terminal_panel(ctx);
        }

        // Composer lives in the chat column (see `render_main_area`) so the sidebar can span the
        // full window height and stays aligned with the centered transcript column.
        egui::CentralPanel::default()
            .frame(Frame::none().fill(c_bg_main()))
            .show(ctx, |ui| {
                if self.conv.settings_open {
                    self.render_settings_page(ui);
                } else {
                    self.render_main_area(ui);
                }
            });
    }
}
