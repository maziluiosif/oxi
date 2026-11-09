//! Shared chrome widgets (sidebar fields, empty states, pill tabs, cards, …).

use eframe::egui::{
    self, Align, Color32, CornerRadius, FontId, Frame, Label, Layout, Margin, Response, RichText,
    Sense, Stroke, TextEdit, Ui,
};

use crate::theme::*;

/// A keyboard focus ring shared by hand-painted controls. Draw inside their allocation so
/// tightly clipped toolbar and list rows keep the entire ring visible.
pub fn keyboard_focus_ring(ui: &Ui, response: &Response, radius: CornerRadius) {
    if response.has_focus() && ui.is_enabled() {
        ui.painter().rect_stroke(
            response.rect.shrink(1.0),
            radius,
            Stroke::new(1.0, c_accent()),
            egui::StrokeKind::Inside,
        );
    }
}

mod copy_modal;
pub use copy_modal::*;
mod settings_kit;
pub use settings_kit::*;

pub fn sidebar_text_field(ui: &mut Ui, text: &mut String, hint: &str) -> Response {
    let id = ui.id().with(("sidebar_text_field", hint));
    let focused = ui.ctx().memory(|m| m.has_focus(id));
    let frame = Frame::new()
        .fill(c_bg_input())
        .stroke(Stroke::new(
            1.0,
            if focused {
                c_composer_focus_border()
            } else {
                c_border_subtle()
            },
        ))
        .corner_radius(RADIUS_CHIP)
        .inner_margin(Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(icon_glyph_rich(ICON_SEARCH, FS_TINY, c_text_faint()));
                ui.add(
                    TextEdit::singleline(text)
                        .id(id)
                        .frame(egui::Frame::NONE)
                        .margin(Margin::symmetric(1, 0))
                        .font(FontId::proportional(FS_SMALL))
                        .desired_width(ui.available_width())
                        .hint_text(hint),
                )
            })
            .inner
        });
    finish_text_field(ui, frame, RADIUS_CHIP)
}

fn finish_text_field(ui: &Ui, frame: egui::InnerResponse<Response>, radius: u8) -> Response {
    let focused = frame.inner.has_focus();
    if ui.is_enabled() && (focused || frame.response.contains_pointer()) {
        ui.painter().rect_stroke(
            frame.response.rect,
            CornerRadius::same(radius),
            Stroke::new(
                1.0,
                if focused {
                    c_composer_focus_border()
                } else {
                    c_border()
                },
            ),
            egui::StrokeKind::Inside,
        );
    }
    frame.inner
}

/// Framed single-line settings input — matches the sidebar field language so settings
/// don't fall back to bare egui `TextEdit` chrome.
pub fn settings_text_field(ui: &mut Ui, text: &mut String, hint: &str) -> Response {
    settings_text_field_opts(ui, text, hint, false, f32::INFINITY)
}

/// Like [`settings_text_field`], but with an explicit width (e.g. short numeric fields).
pub fn settings_text_field_width(
    ui: &mut Ui,
    text: &mut String,
    hint: &str,
    width: f32,
) -> Response {
    settings_text_field_opts(ui, text, hint, false, width)
}

/// Password variant of [`settings_text_field`].
pub fn settings_password_field(ui: &mut Ui, text: &mut String, hint: &str) -> Response {
    settings_text_field_opts(ui, text, hint, true, f32::INFINITY)
}

/// Password variant of [`settings_text_field_width`].
pub fn settings_password_field_width(
    ui: &mut Ui,
    text: &mut String,
    hint: &str,
    width: f32,
) -> Response {
    settings_text_field_opts(ui, text, hint, true, width)
}

fn settings_text_field_opts(
    ui: &mut Ui,
    text: &mut String,
    hint: &str,
    password: bool,
    width: f32,
) -> Response {
    let frame = Frame::new()
        .fill(c_bg_input())
        .stroke(Stroke::new(1.0, c_border_subtle()))
        .corner_radius(RADIUS_BUTTON)
        .inner_margin(Margin::symmetric(8, 4))
        .show(ui, |ui| {
            let mut edit = TextEdit::singleline(text)
                .frame(egui::Frame::NONE)
                .margin(Margin::symmetric(1, 0))
                .font(FontId::proportional(FS_SMALL))
                .desired_width(width)
                .hint_text(hint);
            if password {
                edit = edit.password(true);
            }
            ui.add(edit)
        });
    finish_text_field(ui, frame, RADIUS_BUTTON)
}

/// Framed multiline settings editor (system prompts, etc.).
pub fn settings_text_area(ui: &mut Ui, text: &mut String, hint: &str, rows: usize) -> Response {
    let frame = Frame::new()
        .fill(c_bg_input())
        .stroke(Stroke::new(1.0, c_border_subtle()))
        .corner_radius(RADIUS_BUTTON)
        .inner_margin(Margin::symmetric(8, 6))
        .show(ui, |ui| {
            ui.add(
                TextEdit::multiline(text)
                    .frame(egui::Frame::NONE)
                    .desired_width(f32::INFINITY)
                    .desired_rows(rows)
                    .hint_text(hint),
            )
        });
    finish_text_field(ui, frame, RADIUS_BUTTON)
}

/// Primary section title inside a panel.
pub fn settings_section_title(ui: &mut Ui, title: &str, subtitle: Option<&str>) {
    ui.label(
        RichText::new(title)
            .size(FS_H2)
            .color(c_text_strong())
            .strong(),
    );
    if let Some(sub) = subtitle {
        ui.add_space(3.0);
        ui.label(RichText::new(sub).size(FS_SMALL).color(c_text_muted()));
    }
    ui.add_space(18.0);
}

/// Card header: caption + optional one-line helper. Returns nothing; just paints.
pub fn settings_card_header(ui: &mut Ui, title: &str, help: Option<&str>) {
    let flash = ui.painter().add(egui::Shape::Noop);
    let mut rect = ui
        .label(
            RichText::new(title)
                .size(FS_BODY)
                .color(c_text_strong())
                .strong(),
        )
        .rect;
    if let Some(help) = help {
        ui.add_space(1.0);
        rect = rect.union(
            ui.label(RichText::new(help).size(FS_TINY).color(c_text_muted()))
                .rect,
        );
    }
    settings_reveal_flash(ui, title, rect, flash);
    ui.add_space(6.0);
}

/// A Settings search hit waiting to be shown: the page scrolls to the card (or caption) with
/// this title and tints it briefly so the eye lands on it.
#[derive(Clone)]
struct SettingsReveal {
    title: String,
    at: f64,
    scrolled: bool,
}

fn settings_reveal_id() -> egui::Id {
    egui::Id::new("oxi_settings_reveal")
}

/// Title of the settings card a search hit is about to reveal, if any (lets a folded group
/// open itself first).
pub fn settings_reveal_pending(ctx: &egui::Context) -> Option<String> {
    ctx.data(|d| d.get_temp::<SettingsReveal>(settings_reveal_id()))
        .map(|reveal| reveal.title)
}

/// Scroll to the settings card or caption titled `title` on the next frame and flash it.
pub fn request_settings_reveal(ctx: &egui::Context, title: &str) {
    let at = ctx.input(|i| i.time);
    ctx.data_mut(|d| {
        d.insert_temp(
            settings_reveal_id(),
            SettingsReveal {
                title: title.to_owned(),
                at,
                scrolled: false,
            },
        )
    });
}

/// Paint the reveal tint into the `slot` reserved before the header's text was laid out, so it
/// sits behind the text.
pub fn settings_reveal_flash(ui: &Ui, title: &str, rect: egui::Rect, slot: egui::layers::ShapeIdx) {
    const FLASH_S: f64 = 1.6;
    let Some(mut reveal) = ui
        .ctx()
        .data(|d| d.get_temp::<SettingsReveal>(settings_reveal_id()))
    else {
        return;
    };
    if !reveal.title.eq_ignore_ascii_case(title) {
        return;
    }
    let elapsed = ui.input(|i| i.time) - reveal.at;
    if elapsed > FLASH_S {
        ui.ctx()
            .data_mut(|d| d.remove::<SettingsReveal>(settings_reveal_id()));
        return;
    }
    let rect = rect.expand2(egui::vec2(8.0, 5.0));
    if !reveal.scrolled {
        reveal.scrolled = true;
        ui.scroll_to_rect(
            egui::Rect::from_min_max(rect.min - egui::vec2(0.0, 48.0), rect.max),
            Some(Align::TOP),
        );
        ui.ctx()
            .data_mut(|d| d.insert_temp(settings_reveal_id(), reveal));
    }
    // Hold, then fade out over the last half.
    let fade = (1.0 - ((elapsed / FLASH_S - 0.5) * 2.0).clamp(0.0, 1.0)) as f32;
    let accent = c_accent();
    ui.painter().set(
        slot,
        egui::Shape::rect_filled(
            rect,
            CornerRadius::same(RADIUS_ROW),
            Color32::from_rgba_unmultiplied(
                accent.r(),
                accent.g(),
                accent.b(),
                (48.0 * fade) as u8,
            ),
        ),
    );
    ui.ctx().request_repaint();
}

/// Width of the [`toggle`] track; with [`TOGGLE_GAP`], the indent that lines help text up
/// with a toggle's label.
pub const TOGGLE_W: f32 = 28.0;
pub const TOGGLE_GAP: f32 = 8.0;

/// On/off switch with a label, a drop-in for `ui.checkbox` in Settings: egui's checkbox box
/// nearly vanished on the card background, so a checked option read as plain text.
pub fn toggle(ui: &mut Ui, on: &mut bool, label: impl Into<egui::WidgetText>) -> Response {
    const TRACK_H: f32 = 16.0;
    let text: egui::WidgetText = label.into();
    let label_text = text.text().to_owned();
    let galley = (!label_text.is_empty()).then(|| {
        text.into_galley(
            ui,
            Some(egui::TextWrapMode::Wrap),
            (ui.available_width() - TOGGLE_W - TOGGLE_GAP).max(40.0),
            egui::TextStyle::Body,
        )
    });
    let row_h = galley
        .as_ref()
        .map_or(TRACK_H, |g| g.size().y / g.rows.len().max(1) as f32);
    // Taller than the text so a stack of toggles without help lines doesn't crowd.
    let line_h = row_h.max(24.0);
    let size = egui::vec2(
        TOGGLE_W + galley.as_ref().map_or(0.0, |g| TOGGLE_GAP + g.size().x),
        galley
            .as_ref()
            .map_or(0.0, |g| g.size().y + line_h - row_h)
            .max(line_h),
    );
    let (rect, mut response) = ui.allocate_exact_size(size, Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::Checkbox,
            ui.is_enabled(),
            *on,
            &label_text,
        )
    });
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let hovered = ui.is_enabled() && (response.hovered() || response.has_focus());
    let t = ui.ctx().animate_bool_with_time(response.id, *on, 0.12);
    let track = egui::Rect::from_min_size(
        egui::pos2(rect.left(), rect.top() + (line_h - TRACK_H) * 0.5),
        egui::vec2(TOGGLE_W, TRACK_H),
    );
    let off_fill = if hovered {
        c_border()
    } else {
        c_bg_elevated_2()
    };
    let fill = lerp_color(off_fill, c_accent(), t);
    let radius = CornerRadius::same(255);
    ui.painter().rect_filled(track, radius, fill);
    if t < 1.0 {
        ui.painter().rect_stroke(
            track,
            radius,
            Stroke::new(1.0, lerp_color(c_border(), c_accent(), t)),
            egui::StrokeKind::Inside,
        );
    }
    let knob_r = TRACK_H * 0.5 - 3.0;
    let knob_x = egui::lerp(
        (track.left() + 3.0 + knob_r)..=(track.right() - 3.0 - knob_r),
        t,
    );
    ui.painter().circle_filled(
        egui::pos2(knob_x, track.center().y),
        knob_r,
        lerp_color(c_text_muted(), Color32::from_gray(248), t),
    );
    if response.has_focus() {
        ui.painter().rect_stroke(
            track.expand(2.0),
            radius,
            Stroke::new(1.0, c_accent()),
            egui::StrokeKind::Outside,
        );
    }
    if let Some(galley) = galley {
        ui.painter().galley(
            egui::pos2(
                track.right() + TOGGLE_GAP,
                rect.top() + (line_h - row_h) * 0.5,
            ),
            galley,
            c_text(),
        );
    }
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

/// Side of the box or dot [`paint_choice_mark`] draws.
pub const CHOICE_MARK: f32 = 14.0;

/// A thin tab-close cross, `arm` px from the center to each tip. The icon font's xmark is a
/// heavy solid glyph that looks oversized next to a tab label; this stays light at any DPI.
pub fn paint_close_mark(painter: &egui::Painter, center: egui::Pos2, arm: f32, color: Color32) {
    let stroke = Stroke::new(1.25, color);
    painter.line_segment(
        [
            center + egui::vec2(-arm, -arm),
            center + egui::vec2(arm, arm),
        ],
        stroke,
    );
    painter.line_segment(
        [
            center + egui::vec2(-arm, arm),
            center + egui::vec2(arm, -arm),
        ],
        stroke,
    );
}

/// Paint an icon-font glyph centered at `center`. The close icon is drawn as the thin
/// [`paint_close_mark`] cross instead, so every "×" in the app has the same light weight.
pub fn paint_icon_glyph(
    painter: &egui::Painter,
    center: egui::Pos2,
    icon: &str,
    glyph_size: f32,
    color: Color32,
) {
    if icon == ICON_CLOSE {
        paint_close_mark(painter, center, (glyph_size * 0.3).max(2.5), color);
    } else {
        painter.text(
            center,
            egui::Align2::CENTER_CENTER,
            icon,
            FontId::new(glyph_size, icon_font()),
            color,
        );
    }
}

/// A check box (`multi`) or radio dot, drawn by hand in `mark`: egui's own take the faint
/// widget fill and border, and nearly vanish on cards and tinted backgrounds.
pub fn paint_choice_mark(
    painter: &egui::Painter,
    mark: egui::Rect,
    multi: bool,
    picked: bool,
    hovered: bool,
) {
    let outline = Stroke::new(
        1.5,
        if picked {
            c_accent()
        } else if hovered {
            c_text()
        } else {
            c_text_muted()
        },
    );
    if multi {
        let radius = CornerRadius::same(3);
        if picked {
            painter.rect_filled(mark, radius, c_accent());
            let c = mark.center();
            let tick = Stroke::new(1.6, c_on_accent());
            painter.line_segment([c + egui::vec2(-3.5, 0.0), c + egui::vec2(-1.2, 2.6)], tick);
            painter.line_segment([c + egui::vec2(-1.2, 2.6), c + egui::vec2(3.6, -2.8)], tick);
        } else {
            painter.rect_stroke(mark, radius, outline, egui::StrokeKind::Inside);
        }
    } else {
        painter.circle_stroke(mark.center(), mark.width() * 0.5 - 0.75, outline);
        if picked {
            painter.circle_filled(mark.center(), mark.width() * 0.25, c_accent());
        }
    }
}

/// Check box with a label, a drop-in for `ui.checkbox` where a switch would read wrong (an
/// option of an action about to happen, not a setting).
pub fn checkbox(ui: &mut Ui, on: &mut bool, label: impl Into<egui::WidgetText>) -> Response {
    const GAP: f32 = 8.0;
    let text: egui::WidgetText = label.into();
    let label_text = text.text().to_owned();
    let galley = text.into_galley(
        ui,
        Some(egui::TextWrapMode::Wrap),
        (ui.available_width() - CHOICE_MARK - GAP).max(40.0),
        egui::TextStyle::Body,
    );
    let row_h = galley.size().y / galley.rows.len().max(1) as f32;
    let line_h = row_h.max(CHOICE_MARK + 6.0);
    let size = egui::vec2(
        CHOICE_MARK + GAP + galley.size().x,
        galley.size().y + line_h - row_h,
    );
    let (rect, mut response) = ui.allocate_exact_size(size, Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::Checkbox,
            ui.is_enabled(),
            *on,
            &label_text,
        )
    });
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let hovered = ui.is_enabled() && (response.hovered() || response.has_focus());
    let mark = egui::Rect::from_center_size(
        egui::pos2(rect.left() + CHOICE_MARK * 0.5, rect.top() + line_h * 0.5),
        egui::vec2(CHOICE_MARK, CHOICE_MARK),
    );
    paint_choice_mark(ui.painter(), mark, true, *on, hovered);
    if response.has_focus() {
        ui.painter().rect_stroke(
            mark.expand(2.0),
            CornerRadius::same(5),
            Stroke::new(1.0, c_accent()),
            egui::StrokeKind::Outside,
        );
    }
    ui.painter().galley(
        egui::pos2(mark.right() + GAP, rect.top() + (line_h - row_h) * 0.5),
        galley,
        c_text(),
    );
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_unmultiplied(
        mix(a.r(), b.r()),
        mix(a.g(), b.g()),
        mix(a.b(), b.b()),
        mix(a.a(), b.a()),
    )
}

/// A group of related settings. Spans the full content width so a page's groups line up,
/// whatever each one contains.
pub fn card_frame() -> Card {
    Card
}

pub struct Card;

impl Card {
    pub fn show<R>(
        self,
        ui: &mut Ui,
        add_contents: impl FnOnce(&mut Ui) -> R,
    ) -> egui::InnerResponse<R> {
        // Flat, like the chat transcript: a section is its title and hairline-separated rows
        // on the page background, not a raised box.
        let response = Frame::new().show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            add_contents(ui)
        });
        ui.add_space(SECTION_GAP);
        response
    }
}

/// Single-line label → value field row used on settings panels.
pub fn field_label(ui: &mut Ui, text: &str) {
    ui.add_space(10.0);
    ui.label(RichText::new(text).size(FS_TINY).color(c_text_muted()));
    ui.add_space(3.0);
}

/// Like [`field_label`], but without the leading gap (for the first field in a card).
pub fn field_label_first(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).size(FS_TINY).color(c_text_muted()));
    ui.add_space(3.0);
}

/// Muted helper line under a control (kept short so cards stay scannable).
pub fn field_hint(ui: &mut Ui, text: &str) {
    ui.add_space(3.0);
    ui.label(RichText::new(text).size(FS_TINY).color(c_text_faint()));
}

/// Horizontal rule that matches the subtle border color.
pub fn hairline(ui: &mut Ui) {
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, 1.0), Sense::hover());
    ui.painter().hline(
        rect.x_range(),
        rect.center().y,
        Stroke::new(1.0, c_border_subtle()),
    );
}

/// Inline alert banner (`kind` chooses error / warning) — replaces the duplicated hard-coded
/// red/orange `Frame`s in the transcript and git panel.
pub fn alert_banner(ui: &mut Ui, text: &str, error: bool) {
    let (bg, stroke, fg) = if error {
        (
            crate::theme::c_error_bg(),
            Stroke::new(1.0, crate::theme::c_error_stroke()),
            crate::theme::c_error_fg(),
        )
    } else {
        (
            crate::theme::c_warning_bg(),
            Stroke::new(1.0, crate::theme::c_warning_stroke()),
            crate::theme::c_warning_fg(),
        )
    };
    Frame::new()
        .fill(bg)
        .stroke(stroke)
        .corner_radius(CornerRadius::same(RADIUS_BUTTON))
        .inner_margin(Margin::symmetric(8, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(text).size(FS_TINY).color(fg).monospace());
        });
}

/// How long a transient notice stays up before dismissing itself (paused while hovered).
const NOTICE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);

/// Transient error notice with a close button that also auto-dismisses after
/// [`NOTICE_TIMEOUT`]. The countdown restarts whenever the text changes or the notice reappears
/// after not being drawn, and is held while the pointer is over it, so it can be read at leisure. Returns true when the caller should
/// clear the underlying error.
pub fn dismissible_notice(ui: &mut Ui, id_salt: &str, text: &str) -> bool {
    let id = ui.id().with(("dismissible_notice", id_salt));
    let text_hash = egui::util::hash(text);
    let now = ui.input(|i| i.time);
    let pass = ui.ctx().cumulative_pass_nr();
    // (text hash, shown since, last pass drawn)
    let shown_since = ui.data_mut(|d| {
        let entry = d.get_temp_mut_or_insert_with(id, || (text_hash, now, pass));
        if entry.0 != text_hash || entry.2 + 1 < pass {
            *entry = (text_hash, now, pass);
        }
        entry.2 = pass;
        entry.1
    });

    let mut dismissed = false;
    let response = Frame::new()
        .fill(c_error_bg())
        .stroke(Stroke::new(1.0, c_error_stroke()))
        .corner_radius(CornerRadius::same(RADIUS_BUTTON))
        .inner_margin(Margin {
            left: 8,
            right: 4,
            top: 4,
            bottom: 4,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                    if icon_button_plain(ui, ICON_CLOSE, 18.0, false)
                        .on_hover_text("Dismiss")
                        .clicked()
                    {
                        dismissed = true;
                    }
                    ui.with_layout(Layout::top_down(Align::Min), |ui| {
                        ui.add_space(2.0);
                        ui.add(
                            Label::new(RichText::new(text).size(FS_TINY).color(c_error_fg()))
                                .wrap(),
                        );
                    });
                });
            });
        })
        .response;

    if response.contains_pointer() {
        ui.data_mut(|d| d.insert_temp(id, (text_hash, now, pass)));
    } else {
        let remaining = NOTICE_TIMEOUT.as_secs_f64() - (now - shown_since);
        if remaining <= 0.0 {
            dismissed = true;
        } else {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs_f64(remaining));
        }
    }
    if dismissed {
        ui.data_mut(|d| d.remove::<(u64, f64, u64)>(id));
    }
    dismissed
}

/// Render `icon` (Nerd-Font glyph) as a leading span followed by `label` text, each laid out with
/// its own font family (icon family + proportional), sharing `color`/`size`. Returned as a layout
/// job so it can be passed straight to [`egui::Button::new`] / [`egui::Label`].
pub fn icon_label_job(icon: &str, label: &str, size: f32, color: Color32) -> egui::WidgetText {
    use egui::WidgetText;
    use egui::text::{LayoutJob, TextFormat};
    if icon.is_empty() {
        return WidgetText::from(RichText::new(label).size(size).color(color));
    }
    let mut job = LayoutJob::default();
    job.wrap.max_width = f32::INFINITY;
    // Center the icon span vertically: the Nerd-Font glyph box has a deeper descent than the
    // proportional text font, so baseline alignment (the `TextFormat::simple` default) leaves
    // the icon visually riding below the label.
    let icon_fmt = TextFormat {
        font_id: FontId::new(size, icon_font()),
        color,
        valign: Align::Center,
        ..Default::default()
    };
    let label_fmt = TextFormat::simple(FontId::proportional(size), color);
    job.append(icon, 0.0, icon_fmt);
    job.append(" ", 0.0, label_fmt.clone());
    job.append(label, 0.0, label_fmt);
    WidgetText::LayoutJob(std::sync::Arc::new(job))
}

/// A shortcut such as `⇧⌘P` laid out for display. The text fonts have no ⌘⇧⌃⌥, so the fallback
/// font drew them small and low; they come from the icon font instead, centered on the keys.
pub fn shortcut_job(shortcut: &str, size: f32, color: Color32) -> egui::text::LayoutJob {
    use egui::text::{LayoutJob, TextFormat};
    let mut job = LayoutJob::default();
    job.wrap.max_width = f32::INFINITY;
    let text = TextFormat::simple(FontId::proportional(size), color);
    let glyph = TextFormat {
        font_id: FontId::new(size, icon_font()),
        color,
        valign: Align::Center,
        ..Default::default()
    };
    for ch in shortcut.chars() {
        let icon = match ch {
            '⌘' => "\u{f0633}",
            '⌃' => "\u{f0634}",
            '⌥' => "\u{f0635}",
            '⇧' => "\u{f0636}",
            _ => {
                let mut buf = [0u8; 4];
                job.append(ch.encode_utf8(&mut buf), 0.0, text.clone());
                continue;
            }
        };
        job.append(icon, 0.0, glyph.clone());
    }
    job
}

/// Render `icon` with the dedicated icon font family (no trailing label) at the given size/color.
pub fn icon_glyph_rich(icon: &str, size: f32, color: Color32) -> RichText {
    RichText::new(icon)
        .size(size)
        .color(color)
        .font(FontId::new(size, icon_font()))
}

fn icon_accessible_label(icon: &str) -> &str {
    match icon {
        ICON_CLOSE => "Close",
        ICON_PLUS => "Add",
        ICON_FILE => "File",
        ICON_PROMPTS => "Scratchpad",
        ICON_SETTINGS => "Settings",
        ICON_MENU => "Chats",
        ICON_EXPLORER => "Files",
        ICON_TERMINAL => "Terminal",
        ICON_GIT => "Git panel",
        ICON_REFRESH => "Refresh",
        ICON_SEND => "Send message",
        ICON_STOP => "Stop",
        ICON_ATTACH => "Attach image",
        ICON_MIC => "Dictation",
        ICON_EXTERNAL => "Open",
        ICON_TRASH => "Delete",
        ICON_FOLDER_PLUS => "Add workspace",
        ICON_COPY => "Copy",
        ICON_CHEVRON_LEFT => "Back",
        ICON_CHEVRON_RIGHT => "Expand",
        ICON_ANGLE_UP => "Collapse",
        ICON_ANGLE_DOWN => "Expand",
        ICON_SEARCH => "Search",
        ICON_DOWNLOAD => "Download",
        ICON_UPLOAD => "Upload",
        ICON_PLAY => "Start",
        _ => icon,
    }
}

/// Visual spec for [`icon_button_core`]: rest/hover fills and strokes plus the resting glyph ink.
/// The hover glyph ink is always the accent — every clickable icon in the app shares the sidebar
/// "Add workspace" hover language.
pub struct IconButtonLook {
    pub fill: Color32,
    pub hover_fill: Color32,
    pub stroke: Color32,
    pub hover_stroke: Color32,
    pub rounding: CornerRadius,
    pub glyph: Color32,
}

/// Two-pass icon button: allocate + sense first, then paint by hover state — an `egui::Button`
/// bakes its galley color at construction, so it can never recolor the glyph on hover. Painting
/// the glyph with `Align2::CENTER_CENTER` also sidesteps icon-font baseline drift.
pub fn icon_button_core(
    ui: &mut Ui,
    icon: &str,
    size: egui::Vec2,
    glyph_size: f32,
    active: bool,
    look: &IconButtonLook,
) -> Response {
    icon_button_core_with_hover(ui, icon, size, glyph_size, active, true, look)
}

/// Variant of [`icon_button_core`] that can keep its resting visuals while hovered. The button
/// remains clickable and keeps the pointer cursor, which is useful for strongly colored active
/// states where replacing the active fill or glyph would make the state appear to switch off.
#[allow(clippy::too_many_arguments)]
pub fn icon_button_core_with_hover(
    ui: &mut Ui,
    icon: &str,
    size: egui::Vec2,
    glyph_size: f32,
    active: bool,
    show_hover_visuals: bool,
    look: &IconButtonLook,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    response.widget_info(|| {
        if matches!(
            icon,
            ICON_MENU | ICON_EXPLORER | ICON_TERMINAL | ICON_GIT | ICON_MIC
        ) {
            egui::WidgetInfo::selected(
                egui::WidgetType::Button,
                ui.is_enabled(),
                active,
                icon_accessible_label(icon),
            )
        } else {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Button,
                ui.is_enabled(),
                icon_accessible_label(icon),
            )
        }
    });
    let hovered =
        ui.is_enabled() && show_hover_visuals && (response.hovered() || response.has_focus());
    let fill = if hovered { look.hover_fill } else { look.fill };
    let stroke = if hovered {
        look.hover_stroke
    } else {
        look.stroke
    };
    if fill != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, look.rounding, fill);
    }
    if stroke != Color32::TRANSPARENT {
        ui.painter().rect_stroke(
            rect,
            look.rounding,
            Stroke::new(1.0, stroke),
            egui::StrokeKind::Middle,
        );
    }
    paint_icon_glyph(
        ui.painter(),
        rect.center(),
        icon,
        glyph_size,
        if active || hovered {
            c_accent()
        } else {
            look.glyph
        },
    );
    keyboard_focus_ring(ui, &response, look.rounding);
    if ui.is_enabled() && response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

/// Compact square icon button for toolbars (refresh, hide, close, etc.) — uses the shared
/// elevated fill, subtle border, and 8px rounding so every toolbar chip in the app matches.
/// `active` keeps the icon on the accent color ("on" state for toggle buttons); hover swaps
/// the icon to the accent as well.
pub fn icon_button(ui: &mut Ui, icon: &str, height: f32, active: bool) -> Response {
    icon_button_framed(ui, icon, egui::vec2(height, height), active)
}

/// [`icon_button`] with an arbitrary width × height — for the wider header chips.
pub fn icon_button_framed(ui: &mut Ui, icon: &str, size: egui::Vec2, active: bool) -> Response {
    icon_button_core(
        ui,
        icon,
        size,
        FS_SMALL,
        active,
        &IconButtonLook {
            fill: if active {
                c_row_active()
            } else {
                c_bg_elevated()
            },
            hover_fill: c_row_hover(),
            stroke: c_border_subtle(),
            hover_stroke: c_border(),
            rounding: CornerRadius::same(RADIUS_CHIP),
            glyph: c_text_muted(),
        },
    )
}

/// Borderless, transparent square icon button for tight chrome strips (sidebar hide,
/// top-row toggles). Hover uses `c_row_hover` plus the accent glyph; no frame.
pub fn icon_button_plain(ui: &mut Ui, icon: &str, height: f32, active: bool) -> Response {
    icon_button_core(
        ui,
        icon,
        egui::vec2(height, height),
        FS_SMALL,
        active,
        &IconButtonLook {
            fill: if active {
                c_row_active()
            } else {
                Color32::TRANSPARENT
            },
            hover_fill: c_row_hover(),
            stroke: Color32::TRANSPARENT,
            hover_stroke: Color32::TRANSPARENT,
            rounding: CornerRadius::same(RADIUS_CHIP),
            glyph: c_sidebar_section(),
        },
    )
}

/// Tiny frameless icon button for list rows (18px square on 22px rows). Hand-allocated
/// because a plain `Button` inherits the global `button_padding`/`interact_size` and
/// inflates well past the row; the hover fill uses `c_row_active` so it still reads on
/// top of an already-hovered row.
pub fn icon_button_inline(ui: &mut Ui, icon: &str, glyph_size: f32, color: Color32) -> Response {
    const SIDE: f32 = 18.0;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(SIDE, SIDE), Sense::click());
    resp.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            ui.is_enabled(),
            icon_accessible_label(icon),
        )
    });
    let hovered = ui.is_enabled() && (resp.hovered() || resp.has_focus());
    keyboard_focus_ring(ui, &resp, CornerRadius::same(RADIUS_ROW));
    if hovered {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(4), c_row_active());
    }
    paint_icon_glyph(
        ui.painter(),
        rect.center(),
        icon,
        glyph_size,
        if hovered { c_accent() } else { color },
    );
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// Small pill-style status badge: `running` / `done` / `failed`. Uses the shared semantic badge
/// palette from [`crate::theme`] so every badge across the app shares one set of colors.
/// Pill-style tab used for provider selection or sub-tabs. Returns `true` when clicked.
pub fn pill_tab(ui: &mut Ui, label: &str, selected: bool) -> bool {
    let text_size = FS_SMALL;
    // PLACEHOLDER so the paint-time color (selected/hover state) actually applies — a galley
    // laid out with a concrete color ignores the fallback passed to `painter().galley()`.
    let galley = ui.painter().layout_no_wrap(
        label.to_string(),
        FontId::proportional(text_size),
        Color32::PLACEHOLDER,
    );
    let pad = egui::vec2(14.0, 6.0);
    let size = egui::vec2(
        galley.rect.width() + pad.x * 2.0,
        galley.rect.height() + pad.y * 2.0,
    );
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let hovered = response.hovered();
    let (fill, stroke, text_color) = if selected {
        (
            c_pill_selected_bg(),
            Stroke::new(1.0, c_pill_selected_border()),
            c_text_strong(),
        )
    } else if hovered {
        (c_row_hover(), Stroke::new(1.0, c_border_subtle()), c_text())
    } else {
        (
            Color32::TRANSPARENT,
            Stroke::new(1.0, c_border_subtle()),
            c_text_muted(),
        )
    };
    let r = CornerRadius::same(255);
    ui.painter().rect_filled(rect, r, fill);
    ui.painter()
        .rect_stroke(rect, r, stroke, egui::StrokeKind::Middle);
    let text_pos = egui::pos2(
        rect.left() + pad.x,
        rect.center().y - galley.rect.height() * 0.5,
    );
    ui.painter().galley(text_pos, galley, text_color);
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response.clicked()
}

/// Flat panel tab strip (VS Code / Zed style): labels with an optional faint count, the
/// selected one in strong ink over a 2 px accent underline, a hairline under the whole
/// strip. Tabs share any spare width so the strip spans the panel. Returns the clicked index.
pub fn underline_tabs(ui: &mut Ui, tabs: &[(&str, usize, bool)]) -> Option<usize> {
    let label_font = FontId::proportional(FS_SMALL);
    let count_font = FontId::proportional(FS_TINY);
    let pad_x = 6.0;
    let gap = 4.0;
    let height = 26.0;
    let galleys: Vec<_> = tabs
        .iter()
        .map(|(label, count, _)| {
            let label = ui.painter().layout_no_wrap(
                label.to_string(),
                label_font.clone(),
                Color32::PLACEHOLDER,
            );
            let count = (*count > 0).then(|| {
                ui.painter().layout_no_wrap(
                    count.to_string(),
                    count_font.clone(),
                    Color32::PLACEHOLDER,
                )
            });
            (label, count)
        })
        .collect();
    let natural: Vec<f32> = galleys
        .iter()
        .map(|(label, count)| {
            label.rect.width() + count.as_ref().map_or(0.0, |c| gap + c.rect.width()) + pad_x * 2.0
        })
        .collect();
    let avail = ui.available_width();
    let extra = ((avail - natural.iter().sum::<f32>()) / tabs.len().max(1) as f32).max(0.0);

    let (strip, _) = ui.allocate_exact_size(egui::vec2(avail, height), Sense::hover());
    let painter = ui.painter_at(strip);
    painter.hline(
        strip.x_range(),
        strip.bottom() - 0.5,
        Stroke::new(1.0, c_border_subtle()),
    );
    let mut clicked = None;
    let mut x = strip.left();
    for (i, ((label, count), w)) in galleys.into_iter().zip(&natural).enumerate() {
        let selected = tabs[i].2;
        let rect =
            egui::Rect::from_min_size(egui::pos2(x, strip.top()), egui::vec2(w + extra, height));
        x = rect.right();
        let response = ui
            .interact(rect, ui.id().with(("underline_tab", i)), Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, tabs[i].0)
        });
        let hovered = response.hovered();
        let ink = if selected {
            c_text_strong()
        } else if hovered {
            c_text()
        } else {
            c_text_muted()
        };
        let content_w = w - pad_x * 2.0;
        let mut tx = rect.center().x - content_w / 2.0;
        let cy = rect.center().y - 1.0;
        let label_w = label.rect.width();
        painter.galley(egui::pos2(tx, cy - label.rect.height() / 2.0), label, ink);
        tx += label_w + gap;
        if let Some(count) = count {
            let count_ink = if selected {
                c_text_muted()
            } else {
                c_text_faint()
            };
            painter.galley(
                egui::pos2(tx, cy - count.rect.height() / 2.0),
                count,
                count_ink,
            );
        }
        if selected || hovered {
            let line = egui::Rect::from_min_max(
                egui::pos2(rect.left() + 2.0, rect.bottom() - 2.0),
                egui::pos2(rect.right() - 2.0, rect.bottom()),
            );
            let color = if selected { c_accent() } else { c_border() };
            painter.rect_filled(line, CornerRadius::ZERO, color);
        }
        keyboard_focus_ring(ui, &response, CornerRadius::same(RADIUS_ROW));
        if response.clicked() {
            clicked = Some(i);
        }
    }
    clicked
}

/// Primary filled button with accent color.
pub fn primary_button_widget(label: &str) -> egui::Button<'_> {
    let rich = RichText::new(label).size(FS_SMALL).color(c_on_accent());
    egui::Button::new(rich)
        .fill(c_accent())
        .stroke(Stroke::NONE)
        .corner_radius(RADIUS_BUTTON)
        .min_size(egui::vec2(64.0, 28.0))
}
pub fn primary_button(ui: &mut Ui, label: &str) -> Response {
    ui.add(primary_button_widget(label))
        .on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Primary filled button with a leading Nerd-Font icon glyph.
pub fn primary_button_icon_widget<'a>(icon: &'a str, label: &'a str) -> egui::Button<'a> {
    let text = icon_label_job(icon, label, FS_SMALL, c_on_accent());
    egui::Button::new(text)
        .fill(c_accent())
        .stroke(Stroke::NONE)
        .corner_radius(RADIUS_BUTTON)
        .min_size(egui::vec2(64.0, 28.0))
}

/// Neutral secondary button — used for Sign out, Delete, etc. (with `danger` color swap).
pub fn ghost_button_widget(label: &str, danger: bool) -> egui::Button<'_> {
    let color = if danger {
        crate::theme::c_danger()
    } else {
        c_text()
    };
    egui::Button::new(RichText::new(label).size(FS_SMALL).color(color))
        .fill(c_bg_elevated_2())
        .stroke(Stroke::new(1.0, c_border_subtle()))
        .corner_radius(RADIUS_BUTTON)
        .min_size(egui::vec2(64.0, 28.0))
}
pub fn ghost_button(ui: &mut Ui, label: &str, danger: bool) -> Response {
    ui.add(ghost_button_widget(label, danger))
        .on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Two-pass icon + label button (painted, not an `egui::Button`) so the icon can recolor to the
/// accent on hover and both spans render vertically centered (no icon-font baseline drift).
/// `hover_glyph` lets destructive buttons keep the danger ink instead of the accent. When
/// `enabled` is false the button paints faint and only senses hover.
#[allow(clippy::too_many_arguments)]
fn icon_text_button_core(
    ui: &mut Ui,
    icon: &str,
    label: &str,
    text_size: f32,
    min_size: egui::Vec2,
    look: &IconButtonLook,
    label_color: Color32,
    hover_glyph: Color32,
    enabled: bool,
) -> Response {
    let pad_x = 8.0;
    let gap = 5.0;
    let icon_font_id = FontId::new(text_size, icon_font());
    let label_font_id = FontId::proportional(text_size);
    let icon_w = if icon.is_empty() {
        0.0
    } else {
        ui.painter()
            .layout_no_wrap(icon.to_string(), icon_font_id.clone(), look.glyph)
            .rect
            .width()
            + gap
    };
    let label_galley =
        ui.painter()
            .layout_no_wrap(label.to_string(), label_font_id.clone(), label_color);
    let size = egui::vec2(
        (pad_x * 2.0 + icon_w + label_galley.rect.width()).max(min_size.x),
        (label_galley.rect.height() + 8.0).max(min_size.y),
    );
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled() && enabled, label)
    });
    let hovered = enabled && ui.is_enabled() && (response.hovered() || response.has_focus());
    let fill = if hovered { look.hover_fill } else { look.fill };
    let stroke = if hovered {
        look.hover_stroke
    } else {
        look.stroke
    };
    if fill != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, look.rounding, fill);
    }
    if stroke != Color32::TRANSPARENT {
        ui.painter().rect_stroke(
            rect,
            look.rounding,
            Stroke::new(1.0, stroke),
            egui::StrokeKind::Middle,
        );
    }
    let (glyph_ink, label_ink) = if !enabled {
        (c_text_faint(), c_text_faint())
    } else if hovered {
        (hover_glyph, label_color)
    } else {
        (look.glyph, label_color)
    };
    let mut x = rect.left() + pad_x;
    if !icon.is_empty() {
        let painted = ui.painter().text(
            egui::pos2(x, rect.center().y),
            egui::Align2::LEFT_CENTER,
            icon,
            icon_font_id,
            glyph_ink,
        );
        x = painted.right() + gap;
    }
    ui.painter().text(
        egui::pos2(x, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        label_font_id,
        label_ink,
    );
    if enabled {
        keyboard_focus_ring(ui, &response, look.rounding);
    }
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

/// Ghost (neutral) secondary button with a leading Nerd-Font icon glyph. Hover recolors the
/// icon to the accent (danger buttons keep the danger ink).
pub fn ghost_button_icon(ui: &mut Ui, icon: &str, label: &str, danger: bool) -> Response {
    ghost_button_icon_enabled(ui, icon, label, danger, true)
}
pub fn ghost_button_icon_enabled(
    ui: &mut Ui,
    icon: &str,
    label: &str,
    danger: bool,
    enabled: bool,
) -> Response {
    let danger_ink = crate::theme::c_danger();
    let (label_ink, glyph_ink, hover_glyph) = if danger {
        (danger_ink, danger_ink, danger_ink)
    } else {
        (c_text(), c_text_muted(), c_accent())
    };
    icon_text_button_core(
        ui,
        icon,
        label,
        FS_SMALL,
        egui::vec2(0.0, 26.0),
        &IconButtonLook {
            fill: c_bg_elevated_2(),
            hover_fill: c_row_hover(),
            stroke: c_border_subtle(),
            hover_stroke: c_border(),
            rounding: CornerRadius::same(RADIUS_BUTTON),
            glyph: glyph_ink,
        },
        label_ink,
        hover_glyph,
        enabled,
    )
}

/// Compact chip-style button for tight toolbar rows (git Pull/Push/Fetch, etc.) — shares the
/// ghost-button border palette but renders at `FS_TINY` on `c_bg_elevated` with button rounding,
/// so the small action chips share one language across the app. Leading icon glyph variant.
pub fn mini_button_icon_enabled(ui: &mut Ui, icon: &str, label: &str, enabled: bool) -> Response {
    icon_text_button_core(
        ui,
        icon,
        label,
        FS_TINY,
        egui::vec2(0.0, 24.0),
        &IconButtonLook {
            fill: c_bg_elevated(),
            hover_fill: c_row_hover(),
            stroke: c_border_subtle(),
            hover_stroke: c_border(),
            rounding: CornerRadius::same(RADIUS_BUTTON),
            glyph: c_text_muted(),
        },
        c_text_muted(),
        c_accent(),
        enabled,
    )
}

/// Frameless icon + label button (transparent at rest, `c_row_hover` fill + accent icon on
/// hover) — for quiet inline actions like "Back to chat" or fold headers. `min_size` lets a
/// caller stretch it into a full-width row.
pub fn flat_button_icon(
    ui: &mut Ui,
    icon: &str,
    label: &str,
    text_size: f32,
    min_size: egui::Vec2,
    color: Color32,
) -> Response {
    icon_text_button_core(
        ui,
        icon,
        label,
        text_size,
        min_size,
        &IconButtonLook {
            fill: Color32::TRANSPARENT,
            hover_fill: c_row_hover(),
            stroke: Color32::TRANSPARENT,
            hover_stroke: Color32::TRANSPARENT,
            rounding: CornerRadius::same(RADIUS_BUTTON),
            glyph: color,
        },
        color,
        c_accent(),
        true,
    )
}

/// Settings-page sidebar nav row, drawn like a chat row in the chat sidebar: accent tint and
/// rail when selected, a hover tint otherwise, muted label until selected.
pub fn settings_nav_row(ui: &mut Ui, icon: &str, label: &str, selected: bool) -> Response {
    let row_w = ui.available_width();
    let h = 30.0;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(row_w, h), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::SelectableLabel,
            ui.is_enabled(),
            selected,
            label,
        )
    });
    let hovered = response.hovered() || response.has_focus();
    let fill = if selected {
        c_row_active()
    } else if hovered {
        c_row_hover()
    } else {
        Color32::TRANSPARENT
    };
    ui.painter()
        .rect_filled(rect, CornerRadius::same(RADIUS_ROW), fill);
    keyboard_focus_ring(ui, &response, CornerRadius::same(RADIUS_ROW));
    if selected {
        let rail = egui::Rect::from_min_max(
            rect.left_top() + egui::vec2(0.0, 8.0),
            rect.left_bottom() + egui::vec2(2.0, -8.0),
        );
        ui.painter().rect_filled(rail, 1.0, c_accent());
    }
    let text_color = if selected { c_text() } else { c_text_muted() };
    ui.painter().text(
        egui::pos2(rect.left() + 18.0, rect.center().y),
        egui::Align2::CENTER_CENTER,
        icon,
        FontId::new(FS_SMALL, icon_font()),
        if selected { c_accent() } else { c_text_faint() },
    );
    let mut job = egui::text::LayoutJob::simple_singleline(
        label.to_owned(),
        FontId::proportional(FS_SMALL),
        text_color,
    );
    job.wrap = egui::text::TextWrapping::truncate_at_width((rect.width() - 40.0).max(0.0));
    let galley = ui.painter().layout_job(job);
    ui.painter().galley(
        egui::pos2(rect.left() + 34.0, rect.center().y - galley.size().y * 0.5),
        galley,
        text_color,
    );
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

/// Caption over a group of rows in a sidebar, like the chat list's date headers.
pub fn sidebar_caption(ui: &mut Ui, text: &str) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 20.0), Sense::hover());
    ui.painter().text(
        egui::pos2(rect.left() + 8.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        text,
        FontId::proportional(FS_TINY),
        c_text_faint(),
    );
}
