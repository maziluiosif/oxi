//! Codex OAuth settings and browser sign-in flow.

use eframe::egui::{self, Align, Layout, RichText, Ui};

use crate::app::task_runner::spawn_async_task;
use crate::oauth::{OAuthUiMsg, clear_codex, load_oauth_store, save_oauth_store};
use crate::theme::*;
use crate::ui::chrome::card_frame;

use super::super::OxiApp;
use super::layout::{active_pill, inactive_pill};

impl OxiApp {
    // ── OAuth sections ────────────────────────────────────────────────────────

    pub(super) fn render_codex_oauth_section(&mut self, ui: &mut Ui) {
        let oauth = load_oauth_store();
        let signed_in = oauth.openai_codex.is_some();
        card_frame().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("ChatGPT / Codex OAuth")
                        .size(FS_BODY)
                        .color(c_text())
                        .strong(),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if signed_in {
                        active_pill(ui, "Signed in");
                    } else {
                        inactive_pill(ui, "Signed out");
                    }
                });
            });
            ui.add_space(2.0);
            ui.label(
                RichText::new("Browser + localhost:1455 callback")
                    .size(FS_TINY)
                    .color(c_text_faint()),
            );
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        !self.conv.oauth_busy,
                        crate::ui::chrome::primary_button_widget("Sign in with ChatGPT"),
                    )
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
                    self.spawn_codex_oauth(ui.ctx());
                }
                if self.conv.oauth_busy {
                    ui.add(egui::Spinner::new().size(13.0).color(c_text_muted()));
                    ui.label(
                        RichText::new("Waiting for the browser sign-in…")
                            .size(FS_TINY)
                            .color(c_text_muted()),
                    );
                }
                if ui
                    .add_enabled(
                        signed_in,
                        crate::ui::chrome::ghost_button_widget("Sign out", false),
                    )
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
                    let mut s = load_oauth_store();
                    clear_codex(&mut s);
                    self.conv.oauth_last_message = Some(match save_oauth_store(&s) {
                        Ok(()) => "Signed out Codex OAuth.".into(),
                        Err(e) => format!("Could not update the OS keychain: {e}"),
                    });
                }
            });
            if let Some(ref msg) = self.conv.oauth_last_message {
                ui.add_space(6.0);
                ui.label(RichText::new(msg).size(FS_TINY).color(c_text_muted()));
            }
        });
    }

    // ── OAuth spawn helpers ───────────────────────────────────────────────────

    fn spawn_codex_oauth(&mut self, ctx: &egui::Context) {
        if self.conv.oauth_busy {
            return;
        }
        self.conv.oauth_busy = true;
        self.conv.oauth_last_message = None;
        let (tx, rx) = std::sync::mpsc::channel();
        self.conn.oauth_rx = Some(rx);
        let ctx = ctx.clone();
        spawn_async_task(
            {
                let tx = tx.clone();
                let ctx = ctx.clone();
                move |err| {
                    let _ = tx.send(OAuthUiMsg::CodexDone(Err(err)));
                    ctx.request_repaint();
                }
            },
            move |rt| {
                let tx2 = tx.clone();
                let r = rt.block_on(crate::oauth::login_openai_codex(tx2));
                let _ = tx.send(OAuthUiMsg::CodexDone(r));
                ctx.request_repaint();
            },
        );
    }
}
