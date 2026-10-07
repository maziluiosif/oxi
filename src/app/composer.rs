#[path = "composer/plan_tasks.rs"]
mod plan_tasks;
#[path = "composer/selectors.rs"]
mod selectors;
#[path = "composer/slash_menu.rs"]
mod slash_menu;
#[path = "composer/text_menu.rs"]
mod text_menu;
#[path = "composer/voice_context.rs"]
mod voice_context;

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use eframe::egui::{
    self, Button, Color32, CornerRadius, Frame, Id, Image, Margin, Order, RichText, Sense, Stroke,
    TextEdit, TextureHandle, Ui, text::CCursor, text::CCursorRange,
};

use crate::theme::*;

use super::composer_helpers::{
    context_indicator_color, estimate_message_chars, format_context_tokens, format_tokens_per_sec,
    paint_arc,
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

/// Short provider names for the composer combo — full settings labels like
/// "OpenAI Compatible" / "Claude Code (ACP)" make the bar jump around.
fn composer_provider_label(kind: crate::settings::LlmProviderKind) -> &'static str {
    use crate::settings::LlmProviderKind::*;
    match kind {
        OpenAi => "OpenAI",
        OpenRouter => "OpenRouter",
        AzureOpenAi => "Azure",
        CustomAnthropic => "Anthropic",
        GptCodex => "GPT Codex",
        OpenCodeGo => "OpenCode",
        LmStudio => "LM Studio",
        LlamaCpp => "llama.cpp",
        Ollama => "Ollama",
        LocalHf => "Local HF",
        RemoteHf => "Remote HF",
        ClaudeCodeAcp => "Claude",
        CursorAcp => "Cursor",
        CodexAcp => "Codex",
        Router => "Router",
    }
}

fn composer_provider_groups(
    configured: &[crate::settings::LlmProviderKind],
) -> Vec<(&'static str, Vec<crate::settings::LlmProviderKind>)> {
    super::settings_ui::PROVIDER_GROUPS
        .iter()
        .filter_map(|(label, providers)| {
            let providers: Vec<_> = providers
                .iter()
                .copied()
                .filter(|kind| configured.contains(kind))
                .collect();
            (!providers.is_empty()).then_some((*label, providers))
        })
        .collect()
}

fn composer_text_fits(ui: &Ui, text: &str, extra_gap: f32) -> bool {
    let galley = ui.painter().layout_no_wrap(
        text.to_owned(),
        egui::FontId::proportional(FS_TINY),
        c_text_faint(),
    );
    galley.size().x + ui.spacing().item_spacing.x + extra_gap <= ui.available_width()
}

/// Cached size of the fixed request overhead (system prompt + tool definitions).
pub(crate) struct ContextOverhead {
    /// Hash of the workspace root and the settings that shape the overhead.
    key: u64,
    at: std::time::Instant,
    chars: usize,
}

/// How long a composer notice stays visible.
const COMPOSER_NOTICE_SECS: f32 = 5.0;

fn composer_text_edit(
    ui: &mut Ui,
    input: &mut String,
    input_id: Id,
    lock_focus: bool,
    plan_mode: bool,
) -> egui::text_edit::TextEditOutput {
    // Offset this pass paints with: what the scroll area stored at the end of the last pass.
    let painted_offset_id = input_id.with("painted_scroll_offset");
    let painted_offset = ui
        .ctx()
        .data(|d| d.get_temp::<f32>(painted_offset_id))
        .unwrap_or(0.0);
    let scroll = egui::ScrollArea::vertical()
        .id_salt("composer_text_scroll")
        .animated(false)
        .min_scrolled_height(160.0)
        .max_height(160.0)
        .show(ui, |ui| {
            let selection_shape = ui.painter().add(egui::Shape::Noop);
            let output = ui
                .scope(|ui| {
                    ui.visuals_mut().selection.bg_fill = Color32::TRANSPARENT;
                    ui.visuals_mut().selection.stroke.color = c_text();
                    TextEdit::multiline(input)
                        .id(input_id)
                        .lock_focus(lock_focus)
                        .hint_text(
                            RichText::new(if plan_mode {
                                "Describe what to plan…"
                            } else {
                                "Message oxi…"
                            })
                            .size(FS_BODY)
                            .color(c_text_faint()),
                        )
                        .desired_width(f32::INFINITY)
                        .desired_rows(1)
                        .frame(egui::Frame::NONE)
                        .show(ui)
                })
                .inner;
            if let Some(range) = output.cursor_range
                && !range.is_empty()
            {
                let rects = crate::ui::text_selection::galley_selection_rects(
                    &output.galley,
                    output.galley_pos,
                    range,
                );
                ui.painter().set(
                    selection_shape,
                    crate::ui::text_selection::selection_shape(&rects, editor_selection_fill()),
                );
            }
            output
        });
    // ScrollArea resolves TextEdit's caret target after painting its contents. Re-layout
    // with the final offset before presenting a frame, so typing never shows stale scroll.
    // Compare offsets, not rects: the content origin is pixel-rounded, so at fractional DPI
    // (125% on Windows) rect positions never match the offset exactly and every pass was
    // discarded — the input flickered while typing.
    let offset = scroll.state.offset.y;
    ui.ctx()
        .data_mut(|d| d.insert_temp(painted_offset_id, offset));
    if (offset - painted_offset).abs() > 0.5 / ui.pixels_per_point() {
        ui.ctx().request_discard("composer input scroll changed");
    }
    scroll.inner
}

impl OxiApp {
    pub(super) fn text_attachment_scope(&self) -> Id {
        let wi = self.conv.active_workspace;
        let workspace = &self.conv.workspaces[wi];
        Id::new((
            "text_attachment_draft",
            &workspace.root_path,
            wi,
            workspace.active,
            &workspace.sessions[workspace.active].session_file,
        ))
    }

    fn render_text_attachments(&mut self, ui: &mut Ui) {
        if self.conv.pending_texts.is_empty() {
            return;
        }
        let scope = self.text_attachment_scope();
        let mut remove = None;
        ui.horizontal_wrapped(|ui| {
            for (index, attachment) in self.conv.pending_texts.iter().enumerate() {
                if let crate::model::UserAttachment::Text { name, text } = attachment {
                    if ui
                        .button(format!(
                            "{ICON_FILE} {name} · {} lines",
                            text.lines().count()
                        ))
                        .clicked()
                    {
                        crate::ui::text_attachment::open_draft(ui.ctx(), name, text, scope);
                    }
                    if ui
                        .small_button("×")
                        .on_hover_text("Remove text attachment")
                        .clicked()
                    {
                        remove = Some(index);
                    }
                }
            }
        });
        if let Some(index) = remove {
            self.conv.pending_texts.remove(index);
        }
    }

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
        let input_id = Id::new("composer_input");
        self.intercept_large_pastes(ui, input_id);
        let can_send = !self.conv.input.trim().is_empty()
            || !self.conv.pending_images.is_empty()
            || !self.conv.pending_texts.is_empty();
        let had_draft_content = !self.conv.input.is_empty()
            || !self.conv.pending_images.is_empty()
            || !self.conv.pending_texts.is_empty();

        // Focus state persists in egui memory across frames, so reading it here (before
        // the TextEdit runs) is exact, not one frame late.
        let composer_focused = ui.ctx().memory(|m| m.has_focus(input_id));
        let focus_t =
            ui.ctx()
                .animate_bool_with_time(Id::new("composer_focus_anim"), composer_focused, 0.12);
        let plan_mode = self.plan_mode_on();
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
                        // === Agent checklist and plan hand-off ===
                        self.render_task_panel(ui);
                        self.render_plan_ready_bar(ui);
                        self.render_queue_panel(ui);
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

                        self.render_text_attachments(ui);

                        // === Text area ===
                        // The `/` menu takes its keys first so they don't reach the TextEdit.
                        let slash_menu_open = self.slash_menu_keys(ui, input_id, composer_focused);
                        // desired_rows(1) keeps it compact; it grows naturally
                        // as the user types (both newlines and soft-wrap).
                        let mut te_output = composer_text_edit(
                            ui,
                            &mut self.conv.input,
                            input_id,
                            slash_menu_open,
                            plan_mode,
                        );
                        self.composer_text_menu(
                            ui,
                            input_id,
                            &te_output.response,
                            &te_output.galley,
                            te_output.galley_pos,
                        );
                        self.render_slash_menu(
                            ui,
                            input_id,
                            te_output.response.rect,
                            te_output.response.has_focus(),
                        );
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

                        let galley_h = te_output.galley.rect.height().min(160.0);
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
                                || !self.conv.pending_images.is_empty()
                                || !self.conv.pending_texts.is_empty();
                            if can_send_now {
                                self.send_message();
                            }
                        }
                        if te_output.response.has_focus() {
                            self.handle_composer_history_keys(ui, &te_output.response);
                        }

                        ui.add_space(COMPOSER_GAP);

                        // === Controls row ===
                        self.render_controls_row(ui, can_send, composer_focused);
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
        let draft_cleared = had_draft_content
            && self.conv.input.is_empty()
            && self.conv.pending_images.is_empty()
            && self.conv.pending_texts.is_empty();
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

    /// Move selectors above the action row when the column cannot fit both groups.
    fn render_controls_row(&mut self, ui: &mut Ui, can_send: bool, composer_focused: bool) {
        ui.spacing_mut().item_spacing.x = 6.0;
        let width = ui.available_width();
        let narrow = width < 520.0;
        let compact = width < 410.0;
        let stacked = width < self.composer_single_row_width(ui.ctx(), narrow, compact);
        if stacked {
            // Attach leads the selector row so the second row is only mode + send controls;
            // left on the action row it sat alone under the selectors, detached from both.
            ui.horizontal_wrapped(|ui| {
                self.render_attach_button(ui);
                self.render_measured_selectors(ui, narrow, compact);
            });
            ui.add_space(COMPOSER_GAP);
        }
        ui.horizontal(|ui| {
            self.render_action_controls(ui, can_send, composer_focused, stacked, narrow, compact);
        });
    }

    /// Round paper-clip button that opens the image picker.
    fn render_attach_button(&mut self, ui: &mut Ui) {
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
    }

    fn render_action_controls(
        &mut self,
        ui: &mut Ui,
        can_send: bool,
        composer_focused: bool,
        stacked: bool,
        narrow: bool,
        compact: bool,
    ) {
        if !stacked {
            self.render_attach_button(ui);
        }

        // ── Left: provider + model (compact widths when the chat column is squeezed) ──
        if !stacked {
            self.render_measured_selectors(ui, narrow, compact);
        }
        self.render_plan_toggle(ui, compact);

        // ── Right: round send / stop button ────────────────────────────────
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let active_session_streaming = self.active_waiting_response();
            // While streaming, a typed message is queued instead of replacing Stop.
            let queue_instead = active_session_streaming && can_send;
            let (fill, fg, enabled, icon, hover) = if queue_instead {
                (
                    c_accent(),
                    crate::theme::c_on_accent(),
                    true,
                    ICON_SEND,
                    "Queue message — sent when the current response finishes",
                )
            } else if active_session_streaming {
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
                if queue_instead {
                    self.send_message();
                } else if active_session_streaming {
                    self.send_abort();
                } else if can_send {
                    self.send_message();
                }
            }

            // ── Mic (dictation) button, only when configured in Settings ──
            if self.conv.settings.dictation.enabled {
                self.render_mic_button(ui);
            }

            if ui.available_width() >= 32.0 {
                self.render_context_indicator(ui);
            }
            self.render_tokens_per_sec(ui);

            let show_hint = composer_focused && self.active_session().messages.is_empty();
            let hint_t =
                ui.ctx()
                    .animate_bool_with_time(Id::new("composer_hint_anim"), show_hint, 0.15);
            if hint_t > 0.0 {
                for hint in ["Enter to send · Shift+Enter for newline", "Enter sends"] {
                    if composer_text_fits(ui, hint, 8.0) {
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new(hint)
                                .size(FS_TINY)
                                .color(c_text_faint().gamma_multiply(hint_t)),
                        );
                        break;
                    }
                }
            }
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
                let size = match &tex {
                    Some(tex) => {
                        let mut sz = tex.size_vec2();
                        if sz.y > 0.0 {
                            sz *= THUMB_H / sz.y;
                        }
                        if sz.x > THUMB_MAX_W {
                            sz *= THUMB_MAX_W / sz.x;
                        }
                        sz
                    }
                    None => egui::vec2(THUMB_H * 1.6, THUMB_H),
                };
                // Allocated at its final size so the row wraps: a `Frame` is placed before its
                // size is known, so many images ran off the right and widened the composer.
                let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
                let radius = CornerRadius::same(crate::theme::RADIUS_CHIP);
                ui.painter().rect_filled(rect, radius, c_bg_input());
                if let Some(tex) = tex {
                    Image::new((tex.id(), size))
                        .corner_radius(radius)
                        .paint_at(ui, rect);
                } else {
                    let short = mime.strip_prefix("image/").unwrap_or(mime.as_str());
                    ui.painter().text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        short,
                        egui::FontId::proportional(FS_TINY),
                        c_text_muted(),
                    );
                }
                ui.painter().rect_stroke(
                    rect,
                    radius,
                    Stroke::new(1.0, c_border()),
                    egui::StrokeKind::Inside,
                );

                // Corner remove (×) overlay positioned over the top-right of the thumbnail.
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

#[cfg(test)]
mod layout_tests {
    use super::*;

    #[test]
    fn typing_keeps_short_input_visible_in_bottom_anchored_composer() {
        let ctx = egui::Context::default();
        crate::theme::apply_theme(&ctx, "dark");
        let id = Id::new("composer_input");
        let mut input = String::new();
        let mut height: f32 = 80.0;
        let mut observed = Vec::new();
        ctx.memory_mut(|m| m.request_focus(id));
        let mut expected = String::new();
        for insert in [
            "", "a", "b", "c", "\n", "d", "e", "\n", "f", "g", "h", "\n", "i", "j", "\n", "k", "l",
            "\n", "m", "n", "\n", "o", "p",
        ] {
            expected.push_str(insert);
            observed.clear();
            let _ = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(600.0, 500.0),
                    )),
                    events: if insert.is_empty() {
                        vec![]
                    } else if insert == "\n" {
                        vec![egui::Event::Key {
                            key: egui::Key::Enter,
                            physical_key: None,
                            pressed: true,
                            repeat: false,
                            modifiers: egui::Modifiers::SHIFT,
                        }]
                    } else {
                        vec![egui::Event::Text(insert.into())]
                    },
                    ..Default::default()
                },
                |ui| {
                    let bottom = ui.max_rect().bottom();
                    let rect = egui::Rect::from_min_size(
                        egui::pos2(10.0, bottom - height.max(80.0)),
                        egui::vec2(560.0, height.max(80.0)),
                    );
                    let row = ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                        Frame::new()
                            .inner_margin(Margin::same(COMPOSER_FRAME_MARGIN as i8))
                            .show(ui, |ui| {
                                let top = ui.cursor().top();
                                let output = composer_text_edit(ui, &mut input, id, false, false);
                                observed.push((
                                    output.galley_pos.y - top,
                                    output.galley.size().y,
                                    output.response.rect,
                                ));
                                ui.add_space(COMPOSER_GAP);
                                ui.horizontal(|ui| {
                                    let _ = ui.button("Send");
                                });
                            });
                    });
                    let measured = row.response.rect.height();
                    if (measured - height).abs() > 0.5 {
                        height = measured;
                        ctx.request_discard("composer height changed");
                    }
                },
            );
            assert_eq!(input, expected);
            let (offset, text_height, rect) = observed.last().unwrap();
            assert!(*text_height <= 160.0);
            assert!(
                offset.abs() < 0.5,
                "visible input moved while typing {input:?}: offset={offset}, rect={rect:?}, passes={observed:?}"
            );
        }
    }

    #[test]
    fn idle_input_settles_in_one_pass_at_fractional_scale() {
        // At 125% the scroll area's pixel-rounded content origin never equals its offset; the
        // stale-scroll check compared rects and discarded every pass, flickering the input.
        for scale in [1.0, 1.25, 1.5, 1.75] {
            let ctx = egui::Context::default();
            crate::theme::apply_theme(&ctx, "dark");
            ctx.set_pixels_per_point(scale);
            let id = Id::new("composer_input");
            let mut input = "a draft that is long enough to wrap in the composer. ".repeat(12);
            let mut passes = Vec::new();
            for _ in 0..6 {
                let output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::pos2(0.0, 0.3),
                            egui::vec2(600.0, 500.0),
                        )),
                        ..Default::default()
                    },
                    |ui| {
                        ctx.memory_mut(|m| m.request_focus(id));
                        ui.add_space(37.3);
                        composer_text_edit(ui, &mut input, id, false, false);
                    },
                );
                passes.push(output.platform_output.num_completed_passes);
            }
            assert_eq!(
                passes[3..],
                [1, 1, 1],
                "scale {scale}: passes per frame {passes:?}"
            );
        }
    }

    #[test]
    fn shortening_scrolled_input_does_not_paint_text_above_its_viewport() {
        let ctx = egui::Context::default();
        crate::theme::apply_theme(&ctx, "dark");
        let id = Id::new("composer_input");
        let mut input = "long draft\n".repeat(30);
        let mut frame = |replace: Option<&str>| {
            if let Some(text) = replace {
                input = text.into();
            }
            let mut offset = 0.0;
            let _ = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(600.0, 500.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    ctx.memory_mut(|m| m.request_focus(id));
                    let mut state =
                        egui::text_edit::TextEditState::load(&ctx, id).unwrap_or_default();
                    state
                        .cursor
                        .set_char_range(Some(CCursorRange::one(CCursor::new(
                            input.chars().count(),
                        ))));
                    state.store(&ctx, id);
                    let top = ui.cursor().top();
                    let output = composer_text_edit(ui, &mut input, id, false, false);
                    offset = output.galley_pos.y - top;
                },
            );
            offset
        };
        for _ in 0..3 {
            frame(None);
        }
        let offset = frame(Some("a"));
        assert!(
            offset.abs() < 0.5,
            "short text was painted outside viewport: offset={offset}"
        );
        assert!(frame(Some("ab")).abs() < 0.5);
    }

    #[test]
    fn typing_at_scroll_limit_keeps_caret_visible_in_the_same_frame() {
        let ctx = egui::Context::default();
        crate::theme::apply_theme(&ctx, "dark");
        let id = Id::new("composer_input");
        let mut input = "long draft\n".repeat(30);
        let mut frame = |events: Vec<egui::Event>| {
            let mut caret = egui::Rect::NOTHING;
            let full = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(600.0, 500.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| {
                    ctx.memory_mut(|m| m.request_focus(id));
                    let mut state =
                        egui::text_edit::TextEditState::load(&ctx, id).unwrap_or_default();
                    state
                        .cursor
                        .set_char_range(Some(CCursorRange::one(CCursor::new(
                            input.chars().count(),
                        ))));
                    state.store(&ctx, id);
                    let output = composer_text_edit(ui, &mut input, id, false, false);
                    let range = output.cursor_range.unwrap();
                    caret = output
                        .galley
                        .pos_from_cursor(range.primary)
                        .translate(output.galley_pos.to_vec2());
                },
            );
            let clip = full
                .shapes
                .iter()
                .find_map(|s| match &s.shape {
                    egui::Shape::Text(t) if t.galley.job.text == input => Some(s.clip_rect),
                    _ => None,
                })
                .unwrap();
            (caret, clip)
        };
        for _ in 0..3 {
            frame(vec![]);
        }
        for _ in 0..3 {
            let (caret, clip) = frame(vec![egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::SHIFT,
            }]);
            assert!(
                caret.bottom() <= clip.bottom() + 1.0,
                "caret clipped while typing: {caret:?}, viewport={clip:?}"
            );
        }
    }

    #[test]
    #[ignore = "UI review artifact; run with --ignored"]
    fn render_composer_typing_review() {
        for (width, scale) in [(640.0, 1.0), (320.0, 1.25), (420.0, 1.5)] {
            let id = Id::new("composer_input");
            let mut harness = egui_kittest::Harness::builder()
                .with_size(egui::vec2(width, 400.0))
                .with_pixels_per_point(scale)
                .wgpu()
                .build_ui_state(
                    |ui, state: &mut (String, f32)| {
                        crate::theme::apply_theme(ui.ctx(), "dark");
                        ui.ctx().memory_mut(|m| m.request_focus(id));
                        let mut text_state =
                            egui::text_edit::TextEditState::load(ui.ctx(), id).unwrap_or_default();
                        text_state
                            .cursor
                            .set_char_range(Some(CCursorRange::one(CCursor::new(
                                state.0.chars().count(),
                            ))));
                        text_state.store(ui.ctx(), id);
                        let available = ui.max_rect();
                        let rect = egui::Rect::from_min_max(
                            egui::pos2(available.left(), available.bottom() - state.1.max(80.0)),
                            available.right_bottom(),
                        );
                        let row = ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                            Frame::new()
                                .fill(c_bg_elevated())
                                .inner_margin(Margin::same(COMPOSER_FRAME_MARGIN as i8))
                                .show(ui, |ui| {
                                    composer_text_edit(ui, &mut state.0, id, false, false);
                                    ui.add_space(COMPOSER_GAP);
                                    let _ = ui.button("Send");
                                });
                        });
                        let height = row.response.rect.height();
                        if (height - state.1).abs() > 0.5 {
                            state.1 = height;
                            ui.ctx().request_discard("composer height changed");
                        }
                    },
                    (
                        "A long editable draft with wrapping text.\n".repeat(25),
                        80.0,
                    ),
                );
            harness.run_steps(3);
            harness.event(egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::SHIFT,
            });
            harness.step();
            harness
                .render()
                .unwrap()
                .save(format!("/tmp/oxi-composer-long-{width}.png"))
                .unwrap();
            harness.state_mut().0 = "Text remains visible while typing.".into();
            harness.event(egui::Event::Text(" ă".into()));
            harness.step();
            harness
                .render()
                .unwrap()
                .save(format!("/tmp/oxi-composer-short-{width}.png"))
                .unwrap();
        }
    }

    #[test]
    fn provider_groups_include_every_provider_once() {
        use crate::settings::LlmProviderKind;

        let groups = composer_provider_groups(&LlmProviderKind::ALL);
        assert_eq!(groups.len(), 4);
        let mut grouped: Vec<_> = groups.into_iter().flat_map(|(_, kinds)| kinds).collect();
        let mut expected = LlmProviderKind::ALL.to_vec();
        grouped.sort();
        expected.sort();
        assert_eq!(grouped, expected);
    }

    #[test]
    fn provider_groups_filter_unconfigured_providers_and_empty_categories() {
        use crate::settings::LlmProviderKind::*;

        assert_eq!(
            composer_provider_groups(&[CodexAcp, CursorAcp, Ollama]),
            vec![
                ("Local / self-hosted", vec![Ollama]),
                ("External agents (ACP)", vec![CursorAcp, CodexAcp]),
            ]
        );
        assert!(composer_provider_groups(&[]).is_empty());
    }

    #[test]
    fn optional_hint_never_expands_the_action_row() {
        for width in [40.0, 100.0, 180.0, 300.0, 520.0, 660.0, 800.0] {
            let mut harness = egui_kittest::Harness::builder()
                .with_size(egui::vec2(width + 20.0, 100.0))
                .build_ui_state(
                    |ui, fits: &mut bool| {
                        ui.set_width(width);
                        let row = ui.horizontal(|ui| {
                            let (left, _) =
                                ui.allocate_exact_size(egui::vec2(28.0, 28.0), Sense::hover());
                            let right = ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let (send, _) = ui.allocate_exact_size(
                                        egui::vec2(30.0, 30.0),
                                        Sense::hover(),
                                    );
                                    for hint in
                                        ["Enter to send · Shift+Enter for newline", "Enter sends"]
                                    {
                                        if composer_text_fits(ui, hint, 8.0) {
                                            ui.add_space(8.0);
                                            let response =
                                                ui.label(RichText::new(hint).size(FS_TINY));
                                            assert!(response.rect.right() <= send.left());
                                            assert!(response.rect.left() >= left.right());
                                            *fits = true;
                                            break;
                                        }
                                    }
                                },
                            );
                            assert!(right.response.rect.right() <= ui.max_rect().right() + 1.0);
                        });
                        assert!(row.response.rect.width() <= width + 1.0);
                    },
                    false,
                );
            harness.run_steps(3);
            assert_eq!(*harness.state(), width >= 180.0);
        }
    }
}
