use eframe::egui::{self, Frame, LayerId};

use crate::model::MsgRole;
use crate::theme::C_BG_MAIN;

use super::OxiApp;

impl eframe::App for OxiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.layer_painter(LayerId::background())
            .rect_filled(ctx.screen_rect(), 0.0, C_BG_MAIN);

        self.consume_dropped_files(ctx);
        self.drain_agent(ctx);
        self.drain_oauth(ctx);
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

        // Composer lives in the chat column (see `render_main_area`) so the sidebar can span the
        // full window height and stays aligned with the centered transcript column.
        egui::CentralPanel::default()
            .frame(Frame::none().fill(C_BG_MAIN))
            .show(ctx, |ui| {
                if self.conv.settings_open {
                    self.render_settings_page(ui);
                } else {
                    self.render_main_area(ui);
                }
            });
    }
}
