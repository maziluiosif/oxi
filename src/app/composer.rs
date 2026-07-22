#[path = "composer/voice_context.rs"]
mod voice_context;

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use eframe::egui::{
    self, Button, Color32, ComboBox, CornerRadius, Frame, Id, Image, Margin, Order, RichText,
    Sense, Stroke, TextEdit, TextureHandle, Ui, text::CCursor, text::CCursorRange,
};

use crate::agent::context_char_budget_from_tokens;
use crate::theme::*;

use super::composer_helpers::{
    context_indicator_color, estimate_message_chars, format_context_tokens, paint_arc,
    truncate_label,
};
use super::{OxiApp, SessionKey};

/// Diameter of the round send button.
const SEND_DIAM: f32 = 30.0;
/// Diameter of the round attach (`+`) button.
const ATTACH_DIAM: f32 = 28.0;
/// Diameter of the round mic (dictation) button.
const MIC_DIAM: f32 = 28.0;
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

/// Quiet pill styling shared by the composer combos (provider + model): transparent at
/// rest, soft fill + hairline on hover, fully rounded.
fn quiet_combo_style(ui: &mut Ui) {
    let widgets = &mut ui.visuals_mut().widgets;
    widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
    widgets.inactive.bg_fill = Color32::TRANSPARENT;
    widgets.inactive.bg_stroke = Stroke::NONE;
    widgets.inactive.corner_radius = CornerRadius::same(255);
    widgets.hovered.weak_bg_fill = c_row_hover();
    widgets.hovered.bg_stroke = Stroke::new(1.0, c_border_subtle());
    widgets.hovered.corner_radius = CornerRadius::same(255);
    widgets.active.weak_bg_fill = c_row_hover();
    widgets.active.bg_stroke = Stroke::NONE;
    widgets.active.corner_radius = CornerRadius::same(255);
    widgets.open.weak_bg_fill = c_row_hover();
    widgets.open.bg_stroke = Stroke::NONE;
    widgets.open.corner_radius = CornerRadius::same(255);
}

/// Nerd Font chevron for the quiet combos — the default painted triangle is nearly
/// invisible on these dark surfaces.
fn quiet_combo_icon(
    ui: &Ui,
    rect: egui::Rect,
    visuals: &egui::style::WidgetVisuals,
    _is_open: bool,
) {
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        ICON_ANGLE_DOWN,
        egui::FontId::new(10.0, icon_font()),
        visuals.fg_stroke.color,
    );
}

/// How long a composer notice stays visible.
const COMPOSER_NOTICE_SECS: f32 = 5.0;

impl OxiApp {
    /// Raise a short-lived inline notice under the composer (blocked send, rejected
    /// attachment, …). Replaces any previous notice.
    pub(crate) fn notify_composer(&mut self, msg: impl Into<String>) {
        self.conv.composer_notice = Some((msg.into(), std::time::Instant::now()));
    }

    /// Small warning line inside the composer card; auto-expires.
    fn render_composer_notice(&mut self, ui: &mut Ui) {
        let Some((msg, raised_at)) = self.conv.composer_notice.clone() else {
            return;
        };
        if raised_at.elapsed().as_secs_f32() > COMPOSER_NOTICE_SECS {
            self.conv.composer_notice = None;
            return;
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 5.0;
            ui.label(
                RichText::new(ICON_INFO)
                    .font(egui::FontId::new(FS_TINY, icon_font()))
                    .color(c_warning_fg()),
            );
            ui.label(RichText::new(msg).size(FS_TINY).color(c_warning_fg()));
        });
        ui.add_space(COMPOSER_GAP);
        // Keep frames coming so the notice disappears on time.
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(250));
    }

    pub(crate) fn render_composer(&mut self, ui: &mut Ui, column_center_w: f32) {
        let chat_column_max = crate::theme::chat_column_max_width(ui.ctx());
        let pad = ((column_center_w - chat_column_max.min(column_center_w)) * 0.5).max(0.0);
        let can_send = !self.conv.input.trim().is_empty() || !self.conv.pending_images.is_empty();
        let had_draft_content = !self.conv.input.is_empty() || !self.conv.pending_images.is_empty();

        // Focus state persists in egui memory across frames, so reading it here (before
        // the TextEdit runs) is exact, not one frame late.
        let input_id = Id::new("composer_input");
        let composer_focused = ui.ctx().memory(|m| m.has_focus(input_id));
        let focus_t =
            ui.ctx()
                .animate_bool_with_time(Id::new("composer_focus_anim"), composer_focused, 0.12);
        let card_border = blend_color(c_border(), c_composer_focus_border(), focus_t);

        // Top-align the row so a parent `bottom_up` layout cannot vertically stretch/center the
        // block and shift the field off-screen.
        let row = ui.horizontal_top(|ui| {
            if pad > 0.0 {
                ui.add_space(pad);
            }
            ui.vertical(|ui| {
                let composer_w = chat_column_max.min(column_center_w);
                ui.set_width(composer_w);
                let composer_card = Frame::new()
                    .fill(c_bg_elevated())
                    .stroke(Stroke::new(1.0, card_border))
                    .corner_radius(crate::theme::RADIUS_PANEL)
                    .inner_margin(Margin::same(COMPOSER_FRAME_MARGIN as i8))
                    .show(ui, |ui| {
                        // === Transient notice (blocked send, rejected attachment, …) ===
                        self.render_composer_notice(ui);
                        if self.conv.editing_last_prompt.is_some() {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new("Editing previous prompt")
                                        .size(FS_SMALL)
                                        .color(c_warning_fg()),
                                );
                                if ui.small_button("Cancel").clicked() {
                                    self.cancel_edit_last_prompt();
                                }
                            });
                            ui.add_space(COMPOSER_GAP);
                        }

                        // === Attachment thumbnails (above the text, like Cursor) ===
                        if !self.conv.pending_images.is_empty() {
                            self.render_attachment_thumbnails(ui);
                            ui.add_space(COMPOSER_GAP);
                        }

                        // === Text area ===
                        // desired_rows(1) keeps it compact; it grows naturally
                        // as the user types (both newlines and soft-wrap).
                        let mut te_output = TextEdit::multiline(&mut self.conv.input)
                            .id(input_id)
                            .hint_text(
                                RichText::new("Message oxi…")
                                    .size(FS_BODY)
                                    .color(c_text_faint()),
                            )
                            .desired_width(f32::INFINITY)
                            .desired_rows(1)
                            .frame(egui::Frame::NONE)
                            .show(ui);
                        if self.conv.focus_chat_input_next_frame {
                            // Navigation should put the caret at the end of any existing draft,
                            // not at egui's default/start position.
                            let end = CCursor::new(self.conv.input.chars().count());
                            te_output
                                .state
                                .cursor
                                .set_char_range(Some(CCursorRange::one(end)));
                            te_output.state.store(ui.ctx(), input_id);
                            te_output.response.request_focus();
                            self.conv.focus_chat_input_next_frame = false;
                        }

                        let galley_h = te_output.galley.rect.height();
                        self.conv.composer_measured_text_h = galley_h;

                        // Enter → send, Shift+Enter → newline; ↑/↓ → input history.
                        // Suppressed while the confirm modal is up: Enter there means
                        // "confirm", not "send".
                        let enter_pressed = ui.input(|i| i.key_pressed(egui::Key::Enter));
                        let shift_held = ui.input(|i| i.modifiers.shift);
                        if te_output.response.has_focus()
                            && enter_pressed
                            && !shift_held
                            && !self.confirm_prompt_open()
                        {
                            while self.conv.input.ends_with('\n') {
                                self.conv.input.pop();
                            }
                            let can_send_now = !self.conv.input.trim().is_empty()
                                || !self.conv.pending_images.is_empty();
                            if can_send_now {
                                self.send_message();
                            }
                        }
                        if te_output.response.has_focus() {
                            self.handle_composer_history_keys(ui, &te_output.response);
                        }

                        ui.add_space(COMPOSER_GAP);

                        // === Controls row ===
                        ui.horizontal(|ui| {
                            self.render_controls_row(ui, can_send, composer_focused);
                        });
                    });

                // Clicking anywhere inside the composer card should focus the text input, not just
                // the TextEdit's own line-height rect. This makes the lower controls/empty area of
                // the orange-focused border behave like one large chat input surface. We request
                // focus without consuming the click, so buttons/combos inside the card keep their
                // normal behavior.
                let clicked_inside_card = ui.ctx().input(|i| {
                    i.pointer.primary_clicked()
                        && i.pointer
                            .interact_pos()
                            .is_some_and(|pos| composer_card.response.rect.contains(pos))
                });
                if clicked_inside_card {
                    ui.ctx().memory_mut(|m| m.request_focus(input_id));
                }
            });
            if pad > 0.0 {
                ui.add_space(pad);
            }
        });
        let measured_h = row.response.rect.height();
        let draft_cleared =
            had_draft_content && self.conv.input.is_empty() && self.conv.pending_images.is_empty();
        if draft_cleared {
            // Sending clears the model after TextEdit has already laid out the old text in this
            // pass. Reset to the compact anchor before the second pass instead of carrying that
            // stale, tall measurement into it.
            self.conv.composer_measured_full_h = 0.0;
            ui.ctx().request_discard("composer draft cleared");
        } else if (measured_h - self.conv.composer_measured_full_h).abs() > 0.5 {
            self.conv.composer_measured_full_h = measured_h;
            // The floating rect was positioned earlier in this pass using the old height.
            // Re-run layout before painting instead of exposing one incorrectly anchored frame.
            ui.ctx().request_discard("composer height changed");
        }
    }

    /// `[+]  [model ▾]                                  [↑]`
    fn render_controls_row(&mut self, ui: &mut Ui, can_send: bool, composer_focused: bool) {
        ui.spacing_mut().item_spacing.x = 6.0;
        let narrow = ui.available_width() < 520.0;
        let compact = ui.available_width() < 410.0;

        // ── Left: round attach button ──────────────────────────────────────
        let attach = crate::ui::chrome::icon_button_core(
            ui,
            ICON_ATTACH,
            egui::vec2(ATTACH_DIAM, ATTACH_DIAM),
            15.0,
            false,
            &crate::ui::chrome::IconButtonLook {
                fill: c_bg_input(),
                hover_fill: c_row_hover(),
                stroke: c_border_subtle(),
                hover_stroke: c_border(),
                rounding: CornerRadius::same((ATTACH_DIAM * 0.5) as u8),
                glyph: c_text_muted(),
            },
        )
        .on_hover_text("Attach image");
        if attach.clicked() {
            self.pick_image_attachment();
        }

        // ── Left: provider + model (compact widths when the chat column is squeezed) ──
        self.render_model_selector(ui, narrow, compact);

        // ── Right: round send / stop button ────────────────────────────────
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let active_session_streaming = self.active_waiting_response();
            let (fill, fg, enabled, icon, hover) = if active_session_streaming {
                (
                    c_accent(),
                    crate::theme::c_on_accent(),
                    true,
                    ICON_STOP,
                    "Stop generation (Cmd/Ctrl+.)",
                )
            } else if can_send {
                (
                    c_accent(),
                    crate::theme::c_on_accent(),
                    true,
                    ICON_SEND,
                    if self.conv.editing_last_prompt.is_some() {
                        "Restore changes and send"
                    } else {
                        "Send message"
                    },
                )
            } else {
                (
                    c_bg_elevated_2(),
                    c_text_muted(),
                    false,
                    ICON_SEND,
                    "Type a message or attach an image",
                )
            };
            let clicked = ui
                .add_enabled(
                    enabled,
                    Button::new(crate::ui::chrome::icon_glyph_rich(icon, 15.0, fg))
                        .min_size(egui::vec2(SEND_DIAM, SEND_DIAM))
                        .fill(fill)
                        .stroke(Stroke::NONE)
                        .corner_radius(SEND_DIAM * 0.5),
                )
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text(hover)
                .clicked();
            if clicked {
                if active_session_streaming {
                    self.send_abort();
                } else if can_send {
                    self.send_message();
                }
            }

            // ── Mic (dictation) button, only when configured in Settings ──
            if self.conv.settings.dictation.enabled {
                self.render_mic_button(ui);
            }

            self.render_context_indicator(ui);

            // Keep the primary keyboard action discoverable while the composer has focus.
            // On smaller windows the compact version retains the information without pushing
            // controls out of the row.
            if !narrow {
                ui.add_space(8.0);
                let hint_t = ui.ctx().animate_bool_with_time(
                    Id::new("composer_hint_anim"),
                    composer_focused,
                    0.15,
                );
                if hint_t > 0.0 {
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new("Enter to send · Shift+Enter for newline")
                            .size(FS_TINY)
                            .color(c_text_faint().gamma_multiply(hint_t)),
                    );
                }
            } else if composer_focused && !compact {
                ui.label(
                    RichText::new("Enter sends")
                        .size(FS_TINY)
                        .color(c_text_faint()),
                );
            }
        });
    }

    /// Two borderless dropdowns styled as quiet text with a chevron: provider (only
    /// providers the user has actually configured), then model within that provider's
    /// config. `narrow` and `compact` shrink the controls before they can crowd out
    /// the attachment/send actions on small desktop windows.
    fn render_model_selector(&mut self, ui: &mut Ui, narrow: bool, compact: bool) {
        let oauth = crate::oauth::load_oauth_store();
        let configured = self.conv.settings.configured_provider_kinds(&oauth);
        let active_provider = self.conv.settings.active_provider;
        let combo_w = if compact {
            76.0
        } else if narrow {
            110.0
        } else {
            150.0
        };

        ui.scope(|ui| {
            quiet_combo_style(ui);

            let provider_label = active_provider.label();
            let label = if compact {
                truncate_label(provider_label, 9)
            } else {
                provider_label.to_string()
            };
            let resp = ComboBox::from_id_salt("provider_combo")
                .selected_text(RichText::new(label).size(FS_SMALL).color(c_text_muted()))
                .icon(quiet_combo_icon)
                .width(combo_w)
                .height(300.0)
                .show_ui(ui, |ui| {
                    for kind in &configured {
                        let selected = active_provider == *kind;
                        if ui.selectable_label(selected, kind.label()).clicked() && !selected {
                            self.conv.settings.active_provider = *kind;
                            self.save_settings_quietly();
                            // Remote/local HF choices come from its downloaded-model list;
                            // `/v1/models` only reports the one model currently loaded.
                            if !matches!(
                                kind,
                                crate::settings::LlmProviderKind::LocalHf
                                    | crate::settings::LlmProviderKind::RemoteHf
                            ) {
                                self.spawn_model_fetch(ui.ctx(), *kind);
                            } else {
                                self.refresh_local_hf_model_choices();
                            }
                        }
                    }
                });
            resp.response
                .on_hover_cursor(egui::CursorIcon::PointingHand);
        });

        // Second dropdown: model within the active provider, populated from the fetched
        // model list (falling back to just the current model id so it's never empty).
        let kind = self.conv.settings.active_provider;
        let current = self.conv.settings.active_config().model_id.clone();
        // Local HF's runtime endpoint only exposes the model currently loaded. Its
        // composer dropdown must instead use every downloaded model, otherwise refreshing
        // `/v1/models` makes all switch targets except the old running model disappear.
        let fetched = if matches!(
            kind,
            crate::settings::LlmProviderKind::LocalHf | crate::settings::LlmProviderKind::RemoteHf
        ) {
            let downloaded = if kind == crate::settings::LlmProviderKind::RemoteHf {
                &self.conv.local_models.remote_downloaded
            } else {
                &self.conv.local_models.downloaded
            };
            downloaded.iter().map(|m| m.id.clone()).collect()
        } else {
            self.conv
                .fetched_models
                .get(&kind)
                .map(|f| f.models.clone())
                .unwrap_or_default()
        };
        let items: Vec<String> = if !fetched.is_empty() {
            fetched
        } else if !current.is_empty() {
            vec![current.clone()]
        } else {
            Vec::new()
        };

        let mut selected_model: Option<String> = None;
        ui.scope(|ui| {
            quiet_combo_style(ui);

            let label = if current.is_empty() {
                "(custom)".to_string()
            } else if narrow {
                truncate_label(&current, if compact { 9 } else { 18 })
            } else {
                current.clone()
            };
            let resp = ComboBox::from_id_salt("active_model_combo")
                .selected_text(RichText::new(label).size(FS_SMALL).color(c_text_muted()))
                .icon(quiet_combo_icon)
                .width(combo_w)
                .height(300.0)
                .show_ui(ui, |ui| {
                    for m in &items {
                        if ui.selectable_label(m == &current, m.clone()).clicked() && m != &current
                        {
                            selected_model = Some(m.clone());
                        }
                    }
                });
            resp.response
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text(&current);
        });

        if let Some(model_id) = selected_model {
            if matches!(
                kind,
                crate::settings::LlmProviderKind::LocalHf
                    | crate::settings::LlmProviderKind::RemoteHf
            ) {
                // HF selection is a runtime operation, not merely a config edit. Keep the
                // currently active id until llama-server confirms that the replacement is
                // healthy. Otherwise a failed remote switch leaves the failed id selected
                // and the user cannot retry it from this combo without switching away first.
                self.start_selected_local_hf_model(ui.ctx(), &model_id);
            } else {
                self.conv.settings.provider_mut(kind).model_id = model_id;
                self.save_settings_quietly();
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
                let frame = Frame::new()
                    .fill(c_bg_input())
                    .stroke(Stroke::new(1.0, c_border()))
                    .corner_radius(CornerRadius::same(crate::theme::RADIUS_CHIP))
                    .inner_margin(Margin::same(0))
                    .show(ui, |ui| {
                        if let Some(tex) = tex {
                            let mut sz = tex.size_vec2();
                            if sz.y > 0.0 {
                                sz *= THUMB_H / sz.y;
                            }
                            if sz.x > THUMB_MAX_W {
                                sz *= THUMB_MAX_W / sz.x;
                            }
                            ui.add(
                                Image::new((tex.id(), sz))
                                    .corner_radius(CornerRadius::same(crate::theme::RADIUS_CHIP)),
                            );
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
                        if crate::ui::chrome::icon_button_core(
                            ui,
                            ICON_CLOSE,
                            egui::vec2(15.0, 15.0),
                            12.0,
                            false,
                            &crate::ui::chrome::IconButtonLook {
                                fill: c_bg_main(),
                                hover_fill: c_bg_main(),
                                stroke: c_border(),
                                hover_stroke: c_border(),
                                rounding: CornerRadius::same(RADIUS_CHIP),
                                glyph: c_text(),
                            },
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
