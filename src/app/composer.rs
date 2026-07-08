use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use eframe::egui::{
    self, Button, Color32, ComboBox, CornerRadius, Frame, Id, Image, Margin, Order, Pos2, RichText,
    Sense, Stroke, TextEdit, TextureHandle, Ui, text::CCursor, text::CCursorRange,
};

use crate::agent::context_char_budget_from_tokens;
use crate::model::{AssistantBlock, ChatMessage, MsgRole};
use crate::theme::*;

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

impl OxiApp {
    pub(crate) fn render_composer(&mut self, ui: &mut Ui, column_center_w: f32) {
        let pad = ((column_center_w - CHAT_COLUMN_MAX.min(column_center_w)) * 0.5).max(0.0);
        let can_send = !self.conv.input.trim().is_empty() || !self.conv.pending_images.is_empty();

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
                let composer_w = CHAT_COLUMN_MAX.min(column_center_w);
                ui.set_width(composer_w);
                let composer_card = Frame::new()
                    .fill(c_bg_elevated())
                    .stroke(Stroke::new(1.0, card_border))
                    .corner_radius(crate::theme::RADIUS_PANEL)
                    .inner_margin(Margin::same(COMPOSER_FRAME_MARGIN as i8))
                    .show(ui, |ui| {
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
        self.conv.composer_measured_full_h = row.response.rect.height();
    }

    /// `[+]  [model ▾]                                  [↑]`
    fn render_controls_row(&mut self, ui: &mut Ui, can_send: bool, composer_focused: bool) {
        ui.spacing_mut().item_spacing.x = 6.0;

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

        // ── Left: mic (dictation) button, only when configured in Settings ──
        if self.conv.settings.dictation.enabled {
            self.render_mic_button(ui);
        }

        // ── Left: minimal model selector (plain text + chevron) ────────────
        self.render_model_selector(ui);

        // ── Right: round send / stop button ────────────────────────────────
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let active_session_streaming = self.active_waiting_response();
            let (fill, fg, enabled, icon, hover) = if active_session_streaming {
                (
                    c_accent(),
                    crate::theme::c_on_accent(),
                    true,
                    ICON_STOP,
                    "Stop generation",
                )
            } else if can_send {
                (
                    c_accent(),
                    crate::theme::c_on_accent(),
                    true,
                    ICON_SEND,
                    "Send message",
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

            self.render_context_indicator(ui);
            ui.add_space(8.0);

            // Quiet keyboard hint, faded in only while the input has focus.
            let hint_t = ui.ctx().animate_bool_with_time(
                Id::new("composer_hint_anim"),
                composer_focused,
                0.15,
            );
            if hint_t > 0.0 {
                ui.add_space(8.0);
                ui.label(
                    RichText::new("Shift+Enter for newline")
                        .size(FS_TINY)
                        .color(c_text_faint().gamma_multiply(hint_t)),
                );
            }
        });
    }

    /// Round mic button: idle → click starts recording (lazy-loads the whisper model on
    /// first use if needed); recording → click stops and transcribes into `conv.input`.
    ///
    /// While `transcribing` (mic just turned off, waiting on model load + inference — can
    /// take a few seconds on first use) the button shows three pulsing dots instead of the
    /// mic glyph so it's clear the app is still working, not stuck.
    fn render_mic_button(&mut self, ui: &mut Ui) {
        let recording = self.conv.voice_ui.recording;
        let transcribing = self.conv.voice_ui.transcribing;
        let rounding = CornerRadius::same((MIC_DIAM * 0.5) as u8);

        if transcribing {
            let (rect, response) = ui.allocate_exact_size(
                egui::vec2(MIC_DIAM, MIC_DIAM),
                Sense::hover(),
            );
            ui.painter()
                .rect_filled(rect, rounding, c_bg_elevated_2());
            ui.painter().rect_stroke(
                rect,
                rounding,
                Stroke::new(1.0, c_border_subtle()),
                egui::StrokeKind::Middle,
            );
            let time = ui.input(|i| i.time);
            crate::theme::paint_three_dots(ui.painter(), rect.center(), time, c_text_faint(), 1.6);
            response.on_hover_text("Transcribing…");
            return;
        }

        let (fill, stroke, glyph, hover) = if recording {
            (c_danger(), c_danger(), c_on_accent(), "Stop recording")
        } else {
            (c_bg_input(), c_border_subtle(), c_text_muted(), "Dictate")
        };
        let mic = crate::ui::chrome::icon_button_core(
            ui,
            ICON_MIC,
            egui::vec2(MIC_DIAM, MIC_DIAM),
            14.0,
            false,
            &crate::ui::chrome::IconButtonLook {
                fill,
                hover_fill: c_row_hover(),
                stroke,
                hover_stroke: c_border(),
                rounding,
                glyph,
            },
        )
        .on_hover_text(hover);
        if mic.clicked() {
            self.toggle_dictation();
        }
    }

    /// Path of the downloaded model selected in Settings → Voice, if any.
    fn active_voice_model_path(&self) -> Option<std::path::PathBuf> {
        let id = self.conv.settings.dictation.model_id.as_ref()?;
        self.conv
            .voice_ui
            .downloaded
            .iter()
            .find(|m| &m.id == id)
            .map(|m| std::path::PathBuf::from(&m.path))
    }

    fn toggle_dictation(&mut self) {
        let Some(model_path) = self.active_voice_model_path() else {
            self.conv.settings_open = true;
            self.conv.settings_tab = super::state::SettingsTab::Voice;
            return;
        };
        if self.conv.voice_ui.recording {
            self.conv.voice_ui.recording = false;
            self.conv.voice_ui.transcribing = true;
            self.conv.voice_ui.error = None;
            let keep_loaded = self.conv.settings.dictation.keep_loaded;
            let language = self.conv.settings.dictation.language.clone();
            self.voice.stop_and_transcribe(model_path, keep_loaded, language);
        } else {
            self.conv.voice_ui.error = None;
            self.conv.voice_ui.recording = true;
            self.voice.start_recording();
        }
    }

    /// Drain results from the background voice engine (see [`crate::voice_engine`]),
    /// called once per frame from the main update loop.
    pub(crate) fn drain_voice(&mut self, ctx: &egui::Context) {
        use crate::voice_engine::VoiceMsg;
        loop {
            match self.conv.voice_rx.try_recv() {
                Ok(VoiceMsg::RecordingStarted(Ok(()))) => {
                    ctx.request_repaint();
                }
                Ok(VoiceMsg::RecordingStarted(Err(e))) => {
                    self.conv.voice_ui.recording = false;
                    self.conv.voice_ui.error = Some(e);
                    ctx.request_repaint();
                }
                Ok(VoiceMsg::ModelLoading) => {
                    ctx.request_repaint();
                }
                Ok(VoiceMsg::TranscriptionDone(result)) => {
                    self.conv.voice_ui.transcribing = false;
                    match result {
                        Ok(text) if !text.trim().is_empty() => {
                            let text = text.trim();
                            if !self.conv.input.is_empty()
                                && !self.conv.input.ends_with(' ')
                                && !self.conv.input.ends_with('\n')
                            {
                                self.conv.input.push(' ');
                            }
                            self.conv.input.push_str(text);
                            self.conv.focus_chat_input_next_frame = true;
                        }
                        Ok(_) => {}
                        Err(e) => self.conv.voice_ui.error = Some(e),
                    }
                    ctx.request_repaint();
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            }
        }
    }

    fn render_context_indicator(&self, ui: &mut Ui) {
        let cfg = self.conv.settings.active_config();
        let max_tokens = cfg.effective_context_window(self.conv.settings.context_window_default);
        let key = self.active_session_key();
        let used_chars = self.estimated_active_context_chars();
        let cpt = self.calibrated_chars_per_token(key) as f64;
        let used_tokens = ((used_chars as f64) / cpt).ceil().max(0.0) as usize;
        let pct = if max_tokens == 0 {
            0.0
        } else {
            used_tokens as f32 / max_tokens as f32
        }
        .clamp(0.0, 1.0);

        let size = egui::vec2(26.0, 26.0);
        let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
        let center = rect.center();
        let radius = 8.0;
        ui.painter()
            .circle_stroke(center, radius, Stroke::new(3.0, c_border_subtle()));
        paint_arc(
            ui,
            center,
            radius,
            -std::f32::consts::FRAC_PI_2,
            std::f32::consts::TAU * pct,
            Stroke::new(3.0, context_indicator_color(pct)),
        );
        ui.painter().circle_filled(center, 3.0, c_bg_elevated_2());

        let hover = format!(
            "Context {} / {} ({:.0}%)",
            format_context_tokens(used_tokens as u64),
            format_context_tokens(max_tokens as u64),
            pct * 100.0
        );
        // Show the context tooltip immediately; egui's default hover tooltip delay feels
        // sluggish for this tiny status indicator in the composer.
        if resp.hovered() {
            resp.show_tooltip_text(hover);
        }
    }

    fn estimated_active_context_chars(&self) -> usize {
        let key = self.active_session_key();
        let current_input = self.conv.input.len()
            + self
                .conv
                .pending_images
                .iter()
                .map(|(_, data)| data.len() * 4 / 3)
                .sum::<usize>();
        let budget_chars = context_char_budget_from_tokens(
            self.conv
                .settings
                .active_config()
                .effective_context_window(self.conv.settings.context_window_default),
            self.calibrated_chars_per_token(key),
        );
        (self.estimated_session_context_chars(key) + current_input).min(budget_chars)
    }

    /// The calibrated chars-per-token ratio for a session (measured from the last provider
    /// `Usage` event), or the conservative default before any turn has reported usage.
    pub(crate) fn calibrated_chars_per_token(&self, key: SessionKey) -> f32 {
        self.session_by_key(key)
            .chars_per_token
            .unwrap_or(crate::agent::DEFAULT_CHARS_PER_TOKEN)
    }

    /// Estimated size (chars) of what a run for `key` sends: system prompt + tool definitions
    /// + all persisted messages. Excludes unsent composer input.
    pub(crate) fn estimated_session_context_chars(&self, key: SessionKey) -> usize {
        let root = self.conv.workspaces[key.workspace_idx].root_path.as_str();
        let system_chars =
            crate::agent::prompt::build_system_prompt(&self.conv.settings, root).len();
        let messages_chars = self
            .session_by_key(key)
            .messages
            .iter()
            .map(estimate_message_chars)
            .sum::<usize>();
        let tools_chars = crate::agent::tools::tool_definitions_json(
            &self.conv.settings.tools_enabled,
            self.conv.settings.bash_timeout_cap_secs,
        )
        .iter()
        .map(|v| v.to_string().len())
        .sum::<usize>();
        system_chars + messages_chars + tools_chars
    }

    /// Estimated tokens currently in a session's context, using the calibrated ratio.
    pub(crate) fn estimated_session_context_tokens(&self, key: SessionKey) -> usize {
        let cpt = self.calibrated_chars_per_token(key).max(0.1);
        ((self.estimated_session_context_chars(key) as f32) / cpt).ceil() as usize
    }

    /// Two borderless dropdowns styled as quiet text with a chevron: provider (only
    /// providers the user has actually configured), then model within that provider's
    /// config.
    fn render_model_selector(&mut self, ui: &mut Ui) {
        let oauth = crate::oauth::load_oauth_store();
        let configured = self.conv.settings.configured_provider_kinds(&oauth);
        let active_provider = self.conv.settings.active_provider;

        ui.scope(|ui| {
            quiet_combo_style(ui);

            let label = active_provider.label().to_string();
            let resp = ComboBox::from_id_salt("provider_combo")
                .selected_text(RichText::new(label).size(FS_SMALL).color(c_text_muted()))
                .icon(quiet_combo_icon)
                .width(150.0)
                .height(300.0) // matching height
                .show_ui(ui, |ui| {
                    for kind in &configured {
                        let selected = active_provider == *kind;
                        if ui.selectable_label(selected, kind.label()).clicked() && !selected {
                            self.conv.settings.active_provider = *kind;
                            // Refresh the model list for the newly active provider so the
                            // model dropdown offers the full catalog; the config keeps
                            // whatever model id it last had selected in the meantime.
                            self.spawn_model_fetch(ui.ctx(), *kind);
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
        let fetched = self
            .conv
            .fetched_models
            .get(&kind)
            .map(|f| f.models.clone())
            .unwrap_or_default();
        let items: Vec<String> = if !fetched.is_empty() {
            fetched
        } else if !current.is_empty() {
            vec![current.clone()]
        } else {
            Vec::new()
        };

        ui.scope(|ui| {
            quiet_combo_style(ui);

            let label = if current.is_empty() {
                "(custom)".to_string()
            } else {
                current.clone()
            };
            let resp = ComboBox::from_id_salt("active_model_combo")
                .selected_text(RichText::new(label).size(FS_SMALL).color(c_text_muted()))
                .icon(quiet_combo_icon)
                .width(150.0)
                .height(300.0) // Set explicit high height for the dropdown popup
                .show_ui(ui, |ui| {
                    for m in &items {
                        if ui.selectable_label(m == &current, m.clone()).clicked() {
                            self.conv.settings.provider_mut(kind).model_id = m.clone();
                        }
                    }
                });
            resp.response
                .on_hover_cursor(egui::CursorIcon::PointingHand);
        });
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
                                rounding: CornerRadius::same(8),
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

fn paint_arc(ui: &Ui, center: Pos2, radius: f32, start: f32, sweep: f32, stroke: Stroke) {
    if sweep <= 0.0 {
        return;
    }
    let segments = ((sweep.abs() / std::f32::consts::TAU) * 48.0)
        .ceil()
        .max(6.0) as usize;
    let mut points = Vec::with_capacity(segments + 1);
    for i in 0..=segments {
        let t = start + sweep * (i as f32 / segments as f32);
        points.push(Pos2::new(
            center.x + radius * t.cos(),
            center.y + radius * t.sin(),
        ));
    }
    ui.painter().add(egui::Shape::line(points, stroke));
}

fn context_indicator_color(pct: f32) -> Color32 {
    if pct >= 0.9 {
        c_danger()
    } else if pct >= 0.75 {
        c_warning_fg()
    } else {
        c_accent()
    }
}

fn format_context_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}m", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

fn estimate_message_chars(m: &ChatMessage) -> usize {
    match m.role {
        MsgRole::User => {
            m.text.len()
                + m.attachments
                    .iter()
                    .map(|a| match a {
                        crate::model::UserAttachment::Image { data, .. } => data.len() * 4 / 3,
                    })
                    .sum::<usize>()
        }
        MsgRole::Assistant => m
            .blocks
            .iter()
            .map(|b| match b {
                AssistantBlock::Thinking(t) | AssistantBlock::Answer(t) => t.len(),
                AssistantBlock::Tool {
                    name,
                    output,
                    args_summary,
                    ..
                } => {
                    name.len()
                        + output.len().min(8_000)
                        + args_summary.as_deref().unwrap_or("").len()
                }
            })
            .sum(),
    }
}
