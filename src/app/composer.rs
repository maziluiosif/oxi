use eframe::egui::{self, Button, Color32, Frame, Margin, RichText, Stroke, TextEdit, Ui};

use crate::theme::*;

use super::OxiApp;

const COMPOSER_ACTION: f32 = 30.0;
const COMPOSER_CONTROL_H: f32 = 30.0;
const COMPOSER_FRAME_MARGIN: f32 = 10.0;
const COMPOSER_GAP: f32 = 6.0;

impl OxiApp {
    pub(crate) fn render_composer(&mut self, ui: &mut Ui, column_center_w: f32) {
        let pad = ((column_center_w - CHAT_COLUMN_MAX.min(column_center_w)) * 0.5).max(0.0);
        let can_send = !self.conv.input.trim().is_empty() || !self.conv.pending_images.is_empty();

        // Top-align the row so a parent `bottom_up` layout cannot vertically stretch/center the
        // block and shift the field off-screen.
        let row = ui.horizontal_top(|ui| {
            if pad > 0.0 {
                ui.add_space(pad);
            }
            ui.vertical(|ui| {
                let composer_w = CHAT_COLUMN_MAX.min(column_center_w);
                ui.set_width(composer_w);
                Frame::none()
                    .fill(C_BG_ELEVATED)
                    .stroke(Stroke::new(1.0, C_BORDER))
                    .rounding(12.0)
                    .inner_margin(Margin::same(COMPOSER_FRAME_MARGIN))
                    .show(ui, |ui| {
                        // === Text area ===
                        // desired_rows(1) keeps it compact; it grows naturally
                        // as the user types (both newlines and soft-wrap).
                        // We cap the height so it doesn't eat the screen.
                        let te_output = TextEdit::multiline(&mut self.conv.input)
                            .desired_width(f32::INFINITY)
                            .desired_rows(1)
                            .hint_text("Ask oxi…")
                            .frame(false)
                            .show(ui);

                        // If the text area grew beyond the max, we need to scroll
                        // internally. For now we just let it grow and rely on the
                        // conversation scroll area shrinking above.
                        let galley_h = te_output.galley.rect.height();
                        self.conv.composer_measured_text_h = galley_h;

                        // Enter → send, Shift+Enter → newline
                        let enter_pressed = ui.input(|i| i.key_pressed(egui::Key::Enter));
                        let shift_held = ui.input(|i| i.modifiers.shift);
                        if te_output.response.has_focus() && enter_pressed && !shift_held {
                            while self.conv.input.ends_with('\n') {
                                self.conv.input.pop();
                            }
                            let can_send_now = !self.conv.input.trim().is_empty()
                                || !self.conv.pending_images.is_empty();
                            if can_send_now {
                                self.send_message();
                            }
                        }

                        ui.add_space(COMPOSER_GAP);

                        // === Controls row ===
                        ui.horizontal(|ui| {
                            self.render_controls_row(ui, can_send);
                        });
                        });
            });
            if pad > 0.0 {
                ui.add_space(pad);
            }
        });
        self.conv.composer_measured_full_h = row.response.rect.height();
    }

    /// `[+ attach] [chips…]  …  [model ▾] [▶ send]`
    fn render_controls_row(&mut self, ui: &mut Ui, can_send: bool) {
        if ui.button("+").on_hover_text("Attach image").clicked() {
            self.pick_image_attachment();
        }
        if !self.conv.pending_images.is_empty() {
            ui.add_space(4.0);
            self.render_attachment_chips_inline(ui);
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let active_session_streaming = self.active_waiting_response();
            let (fill, fg, enabled, icon, hover) = if active_session_streaming {
                (C_ACCENT, Color32::WHITE, true, "■", "Stop generation")
            } else if can_send {
                (C_ACCENT, Color32::WHITE, true, "▶", "Send")
            } else {
                (
                    Color32::from_rgb(0x35, 0x37, 0x3d),
                    C_TEXT_MUTED,
                    false,
                    "▶",
                    "Message is empty",
                )
            };
            let clicked = ui
                .add_enabled(
                    enabled,
                    Button::new(RichText::new(icon).size(13.5).color(fg))
                        .min_size(egui::vec2(COMPOSER_ACTION - 2.0, COMPOSER_CONTROL_H - 1.0))
                        .fill(fill)
                        .stroke(Stroke::NONE)
                        .rounding(10.0),
                )
                .on_hover_text(hover)
                .clicked();
            if clicked {
                if active_session_streaming {
                    self.send_abort();
                } else if can_send {
                    self.send_message();
                }
            }

            egui::ComboBox::from_id_salt("profile_combo")
                .selected_text(
                    self.conv
                        .settings
                        .active_profile()
                        .map(|p| p.subtitle())
                        .unwrap_or_else(|| "No profile".to_string()),
                )
                .width(220.0)
                .show_ui(ui, |ui| {
                    let current_id = self.conv.settings.active_profile_id.clone();
                    let items: Vec<(String, String)> = self
                        .conv
                        .settings
                        .profiles
                        .iter()
                        .map(|p| (p.id.clone(), p.subtitle()))
                        .collect();
                    for (id, label) in items {
                        if ui.selectable_label(current_id == id, label).clicked() {
                            self.conv.settings.set_active_profile(&id);
                        }
                    }
                });
        });
    }

    pub(crate) fn render_attachment_chips_inline(&mut self, ui: &mut Ui) {
        let mut remove_idx: Option<usize> = None;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for (i, (mime, _)) in self.conv.pending_images.iter().enumerate() {
                let short = mime.strip_prefix("image/").unwrap_or(mime.as_str());
                ui.horizontal(|ui| {
                    Frame::none()
                        .fill(C_BG_MAIN)
                        .stroke(Stroke::new(1.0, C_BORDER))
                        .rounding(4.0)
                        .inner_margin(Margin::symmetric(4.0, 1.0))
                        .show(ui, |ui| {
                            ui.label(RichText::new(short).size(FS_TINY).color(C_ACCENT));
                            if ui
                                .add(
                                    Button::new(
                                        RichText::new("×").size(11.0).color(C_TEXT_MUTED),
                                    )
                                    .frame(false)
                                    .fill(Color32::TRANSPARENT)
                                    .min_size(egui::vec2(14.0, 14.0)),
                                )
                                .on_hover_text("Remove image")
                                .clicked()
                            {
                                remove_idx = Some(i);
                            }
                        });
                });
            }
        });
        if let Some(i) = remove_idx {
            self.remove_pending_image_at(i);
        }
    }
}
