use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use eframe::egui::{
    self, Button, Color32, ComboBox, Frame, Id, Image, Margin, Order, RichText, Rounding, Stroke,
    TextEdit, TextureHandle, Ui,
};

use crate::theme::*;

use super::OxiApp;

/// Diameter of the round send button.
const SEND_DIAM: f32 = 30.0;
/// Diameter of the round attach (`+`) button.
const ATTACH_DIAM: f32 = 28.0;
const COMPOSER_FRAME_MARGIN: f32 = 10.0;
const COMPOSER_GAP: f32 = 6.0;
/// Fixed height of an attachment thumbnail; width follows the image aspect ratio.
const THUMB_H: f32 = 52.0;
const THUMB_MAX_W: f32 = 132.0;

/// Decode + cache a small thumbnail texture for a pending image attachment.
fn composer_thumb_texture(ui: &Ui, data: &[u8]) -> Option<TextureHandle> {
    let mut hasher = DefaultHasher::new();
    data.hash(&mut hasher);
    let h = hasher.finish();
    let cache_id = Id::new(("composer_thumb_tex", h));
    if let Some(tex) = ui
        .ctx()
        .data_mut(|d| d.get_persisted::<TextureHandle>(cache_id))
    {
        return Some(tex);
    }
    let dyn_img = image::load_from_memory(data).ok()?;
    let rgba = dyn_img.thumbnail(160, 160).to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    let color_image = egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
    let tex = ui.ctx().load_texture(
        format!("composer_thumb_{h:016x}"),
        color_image,
        egui::TextureOptions::default(),
    );
    ui.ctx()
        .data_mut(|d| d.insert_persisted(cache_id, tex.clone()));
    Some(tex)
}

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
                    .fill(c_bg_elevated())
                    .stroke(Stroke::new(1.0, c_border()))
                    .rounding(14.0)
                    .inner_margin(Margin::same(COMPOSER_FRAME_MARGIN))
                    .show(ui, |ui| {
                        // === Attachment thumbnails (above the text, like Cursor) ===
                        if !self.conv.pending_images.is_empty() {
                            self.render_attachment_thumbnails(ui);
                            ui.add_space(COMPOSER_GAP);
                        }

                        // === Text area ===
                        // desired_rows(1) keeps it compact; it grows naturally
                        // as the user types (both newlines and soft-wrap).
                        let te_output = TextEdit::multiline(&mut self.conv.input)
                            .desired_width(f32::INFINITY)
                            .desired_rows(1)
                            .frame(false)
                            .show(ui);

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

    /// `[+]  [model ▾]                                  [↑]`
    fn render_controls_row(&mut self, ui: &mut Ui, can_send: bool) {
        ui.spacing_mut().item_spacing.x = 6.0;

        // ── Left: round attach button ──────────────────────────────────────
        let attach = ui
            .add(
                Button::new(RichText::new("+").size(16.0).color(c_text_muted()))
                    .min_size(egui::vec2(ATTACH_DIAM, ATTACH_DIAM))
                    .fill(c_bg_input())
                    .stroke(Stroke::new(1.0, c_border_subtle()))
                    .rounding(ATTACH_DIAM * 0.5),
            )
            .on_hover_text("Attach image");
        if attach.clicked() {
            self.pick_image_attachment();
        }

        // ── Left: minimal model selector (plain text + chevron) ────────────
        self.render_model_selector(ui);

        // ── Right: round send / stop button ────────────────────────────────
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let active_session_streaming = self.active_waiting_response();
            let no_profile = self.conv.settings.active_profile().is_none();
            let (fill, fg, enabled, icon, hover) = if active_session_streaming {
                (c_text(), c_bg_main(), true, "■", "Stop generation")
            } else if no_profile {
                (
                    c_bg_elevated_2(),
                    c_text_muted(),
                    false,
                    "↑",
                    "Configure an active provider profile in Settings",
                )
            } else if can_send {
                (c_text(), c_bg_main(), true, "↑", "Send message")
            } else {
                (
                    c_bg_elevated_2(),
                    c_text_muted(),
                    false,
                    "↑",
                    "Type a message or attach an image",
                )
            };
            let clicked = ui
                .add_enabled(
                    enabled,
                    Button::new(RichText::new(icon).size(15.0).color(fg))
                        .min_size(egui::vec2(SEND_DIAM, SEND_DIAM))
                        .fill(fill)
                        .stroke(Stroke::NONE)
                        .rounding(SEND_DIAM * 0.5),
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
        });
    }

    /// Borderless model dropdown styled as quiet text with a chevron.
    fn render_model_selector(&mut self, ui: &mut Ui) {
        let label = self
            .conv
            .settings
            .active_profile()
            .map(|p| p.subtitle())
            .unwrap_or_else(|| "No profile".to_string());

        ui.scope(|ui| {
            let widgets = &mut ui.visuals_mut().widgets;
            widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
            widgets.inactive.bg_fill = Color32::TRANSPARENT;
            widgets.inactive.bg_stroke = Stroke::NONE;
            widgets.hovered.weak_bg_fill = c_row_hover();
            widgets.hovered.bg_stroke = Stroke::NONE;
            widgets.active.weak_bg_fill = c_row_hover();
            widgets.active.bg_stroke = Stroke::NONE;
            widgets.open.weak_bg_fill = c_row_hover();
            widgets.open.bg_stroke = Stroke::NONE;

            ComboBox::from_id_salt("profile_combo")
                .selected_text(RichText::new(label).size(FS_SMALL).color(c_text_muted()))
                .width(190.0)
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

        // Second row: model picker for the active profile, populated from the fetched model list.
        if let Some(p) = self.conv.settings.active_profile() {
            let pid = p.id.clone();
            let fetched = self
                .conv
                .fetched_models
                .get(&pid)
                .map(|f| f.models.clone())
                .unwrap_or_default();
            let current = p.model_id.clone();
            if !fetched.is_empty() {
                ui.scope(|ui| {
                    let widgets = &mut ui.visuals_mut().widgets;
                    widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
                    widgets.inactive.bg_fill = Color32::TRANSPARENT;
                    widgets.inactive.bg_stroke = Stroke::NONE;
                    widgets.hovered.weak_bg_fill = c_row_hover();
                    widgets.hovered.bg_stroke = Stroke::NONE;
                    widgets.active.weak_bg_fill = c_row_hover();
                    widgets.active.bg_stroke = Stroke::NONE;
                    widgets.open.weak_bg_fill = c_row_hover();
                    widgets.open.bg_stroke = Stroke::NONE;

                    let label = if current.is_empty() {
                        "(custom)".to_string()
                    } else {
                        current.clone()
                    };
                    ComboBox::from_id_salt("active_model_combo")
                        .selected_text(RichText::new(label).size(FS_SMALL).color(c_text_muted()))
                        .width(190.0)
                        .show_ui(ui, |ui| {
                            for m in &fetched {
                                if ui.selectable_label(m == &current, m.clone()).clicked() {
                                    if let Some(p) = self
                                        .conv
                                        .settings
                                        .profiles
                                        .iter_mut()
                                        .find(|pp| pp.id == pid)
                                    {
                                        p.model_id = m.clone();
                                    }
                                }
                            }
                        });
                });
            }
        }
    }

    /// Image attachment thumbnails shown at the top of the composer, each with a
    /// corner remove button (Cursor-style).
    pub(crate) fn render_attachment_thumbnails(&mut self, ui: &mut Ui) {
        let mut remove_idx: Option<usize> = None;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
            for (i, (mime, data)) in self.conv.pending_images.iter().enumerate() {
                let tex = composer_thumb_texture(ui, data);
                let frame = Frame::none()
                    .fill(c_bg_input())
                    .stroke(Stroke::new(1.0, c_border()))
                    .rounding(Rounding::same(8.0))
                    .inner_margin(Margin::same(0.0))
                    .show(ui, |ui| {
                        if let Some(tex) = tex {
                            let mut sz = tex.size_vec2();
                            if sz.y > 0.0 {
                                sz *= THUMB_H / sz.y;
                            }
                            if sz.x > THUMB_MAX_W {
                                sz *= THUMB_MAX_W / sz.x;
                            }
                            ui.add(Image::new((tex.id(), sz)).rounding(Rounding::same(8.0)));
                        } else {
                            let short = mime.strip_prefix("image/").unwrap_or(mime.as_str());
                            ui.allocate_ui(egui::vec2(THUMB_H * 1.6, THUMB_H), |ui| {
                                ui.centered_and_justified(|ui| {
                                    ui.label(
                                        RichText::new(short).size(FS_TINY).color(c_text_muted()),
                                    );
                                });
                            });
                        }
                    });

                // Corner remove (×) overlay positioned over the top-right of the thumbnail.
                let rect = frame.response.rect;
                let x_pos = egui::pos2(rect.right() - 18.0, rect.top() + 4.0);
                egui::Area::new(Id::new(("composer_thumb_x", i)))
                    .order(Order::Foreground)
                    .fixed_pos(x_pos)
                    .show(ui.ctx(), |ui| {
                        if ui
                            .add(
                                Button::new(RichText::new("×").size(12.0).color(c_text()))
                                    .min_size(egui::vec2(15.0, 15.0))
                                    .fill(c_bg_main())
                                    .stroke(Stroke::new(1.0, c_border()))
                                    .rounding(7.5),
                            )
                            .on_hover_text("Remove image")
                            .clicked()
                        {
                            remove_idx = Some(i);
                        }
                    });
            }
        });
        if let Some(i) = remove_idx {
            self.remove_pending_image_at(i);
        }
    }
}
