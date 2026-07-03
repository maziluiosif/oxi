//! Shared chrome widgets (sidebar fields, empty states, pill tabs, cards, …).

use eframe::egui::{
    self, Align, Color32, FontId, Frame, Label, Layout, Margin, Response, RichText, Rounding,
    Sense, Stroke, TextEdit, Ui,
};

use crate::theme::*;

pub fn sidebar_text_field(ui: &mut Ui, text: &mut String, hint: &str) {
    Frame::none()
        .fill(c_bg_input())
        .stroke(Stroke::new(1.0, c_border_subtle()))
        .rounding(RADIUS_BUTTON)
        .inner_margin(Margin::symmetric(8.0, 4.0))
        .show(ui, |ui| {
            ui.add(
                TextEdit::singleline(text)
                    .frame(false)
                    .margin(Margin::symmetric(1.0, 0.0))
                    .font(FontId::proportional(FS_TINY))
                    .desired_width(ui.available_width())
                    .hint_text(hint),
            );
        });
    ui.add_space(3.0);
}

/// Section caption used above groups of settings: small, uppercase, muted.
pub fn settings_caption(ui: &mut Ui, text: &str) {
    ui.label(
        RichText::new(text.to_uppercase())
            .size(FS_TINY)
            .color(c_text_faint())
            .strong(),
    );
    ui.add_space(2.0);
}

/// Primary section title inside a panel.
pub fn settings_section_title(ui: &mut Ui, title: &str, subtitle: Option<&str>) {
    ui.label(RichText::new(title).size(FS_H2).color(c_text()).strong());
    if let Some(sub) = subtitle {
        ui.add_space(2.0);
        ui.label(RichText::new(sub).size(FS_SMALL).color(c_text_muted()));
    }
    ui.add_space(10.0);
}

/// A card frame used to group related settings. Matches the elevated background + subtle border.
pub fn card_frame() -> Frame {
    Frame::none()
        .fill(c_bg_elevated())
        .stroke(Stroke::new(1.0, c_border_subtle()))
        .rounding(10.0)
        .inner_margin(Margin::symmetric(14.0, 12.0))
}

/// Slightly elevated card variant — used for nested sub-cards inside a panel.
pub fn nested_card_frame() -> Frame {
    Frame::none()
        .fill(c_bg_elevated_2())
        .stroke(Stroke::new(1.0, c_border_subtle()))
        .rounding(RADIUS_CHIP)
        .inner_margin(Margin::symmetric(10.0, 8.0))
}

/// Single-line label → value field row used on settings panels.
pub fn field_label(ui: &mut Ui, text: &str) {
    ui.add_space(10.0);
    ui.label(RichText::new(text).size(FS_TINY).color(c_text_muted()));
    ui.add_space(3.0);
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
    Frame::none()
        .fill(bg)
        .stroke(stroke)
        .rounding(Rounding::same(6.0))
        .inner_margin(Margin::symmetric(8.0, 6.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(text).size(FS_TINY).color(fg).monospace());
        });
}

/// Render `icon` (Nerd-Font glyph) as a leading span followed by `label` text, each laid out with
/// its own font family (icon family + proportional), sharing `color`/`size`. Returned as a layout
/// job so it can be passed straight to [`egui::Button::new`] / [`egui::Label`].
pub fn icon_label_job(icon: &str, label: &str, size: f32, color: Color32) -> egui::WidgetText {
    use egui::text::{LayoutJob, TextFormat};
    use egui::WidgetText;
    if icon.is_empty() {
        return WidgetText::from(RichText::new(label).size(size).color(color));
    }
    let mut job = LayoutJob::default();
    job.wrap.max_width = f32::INFINITY;
    let icon_fmt = TextFormat::simple(FontId::new(size, icon_font()), color);
    let label_fmt = TextFormat::simple(FontId::proportional(size), color);
    job.append(icon, 0.0, icon_fmt);
    job.append(" ", 0.0, label_fmt.clone());
    job.append(label, 0.0, label_fmt);
    WidgetText::LayoutJob(job)
}

/// Render `icon` with the dedicated icon font family (no trailing label) at the given size/color.
pub fn icon_glyph_rich(icon: &str, size: f32, color: Color32) -> RichText {
    RichText::new(icon)
        .size(size)
        .color(color)
        .font(FontId::new(size, icon_font()))
}

/// Compact square icon button for toolbars (refresh, hide, close, etc.) —uses the shared
/// elevated fill, subtle border, and 8px rounding so every toolbar chip in the app matches.
/// `active` swaps the icon to the accent color ("on" state for toggle buttons).
pub fn icon_button(ui: &mut Ui, icon: &str, height: f32, active: bool) -> Response {
    let color = if active { c_accent() } else { c_text_muted() };
    ui.add(
        egui::Button::new(icon_glyph_rich(icon, FS_SMALL, color))
            .fill(c_bg_elevated())
            .stroke(Stroke::new(1.0, c_border_subtle()))
            .rounding(RADIUS_CHIP)
            .min_size(egui::vec2(height, height)),
    )
}

/// Borderless, transparent square icon button for tight chrome strips (sidebar hide,
/// top-row toggles). Hover uses `c_row_hover` only; no frame.
pub fn icon_button_plain(ui: &mut Ui, icon: &str, height: f32, active: bool) -> Response {
    let color = if active {
        c_accent()
    } else {
        c_sidebar_section()
    };
    ui.add(
        egui::Button::new(icon_glyph_rich(icon, FS_SMALL, color))
            .frame(false)
            .fill(Color32::TRANSPARENT)
            .min_size(egui::vec2(height, height)),
    )
}

/// Tiny frameless icon button for list rows (18px square on 22px rows). Hand-allocated
/// because a plain `Button` inherits the global `button_padding`/`interact_size` and
/// inflates well past the row; the hover fill uses `c_row_active` so it still reads on
/// top of an already-hovered row.
pub fn icon_button_inline(ui: &mut Ui, icon: &str, glyph_size: f32, color: Color32) -> Response {
    const SIDE: f32 = 18.0;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(SIDE, SIDE), Sense::click());
    let hovered = resp.hovered();
    if hovered {
        ui.painter()
            .rect_filled(rect, Rounding::same(4.0), c_row_active());
    }
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        icon,
        FontId::new(glyph_size, icon_font()),
        if hovered { c_text() } else { color },
    );
    resp
}

/// Small pill-style status badge: `running` / `done` / `failed`. Uses the shared semantic badge
/// palette from [`crate::theme`] so every badge across the app shares one set of colors.
pub fn status_badge(ui: &mut Ui, text: &str, fg: Color32, bg: Color32, stroke: Color32) {
    Frame::none()
        .fill(bg)
        .stroke(Stroke::new(1.0, stroke))
        .rounding(Rounding::same(999.0))
        .inner_margin(Margin::symmetric(7.0, 2.0))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(FS_TINY).color(fg));
        });
}

/// Pre-colored variants of [`status_badge`] for the three standard tool states.
pub fn running_badge(ui: &mut Ui) {
    let (fg, bg, stroke) = crate::theme::badge_running_parts();
    status_badge(ui, "running", fg, bg, stroke)
}
pub fn done_badge(ui: &mut Ui) {
    let (fg, bg, stroke) = crate::theme::badge_done_parts();
    status_badge(ui, "done", fg, bg, stroke)
}
pub fn failed_badge(ui: &mut Ui) {
    let (fg, bg, stroke) = crate::theme::badge_failed_parts();
    status_badge(ui, "failed", fg, bg, stroke)
}

/// Pill-style tab used for provider selection or sub-tabs. Returns `true` when clicked.
pub fn pill_tab(ui: &mut Ui, label: &str, selected: bool) -> bool {
    let text_size = FS_SMALL;
    let galley =
        ui.painter()
            .layout_no_wrap(label.to_string(), FontId::proportional(text_size), c_text());
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
    let r = Rounding::same(999.0);
    ui.painter().rect_filled(rect, r, fill);
    ui.painter().rect_stroke(rect, r, stroke);
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

/// Primary filled button with accent color.
pub fn primary_button_widget(label: &str) -> egui::Button<'_> {
    let rich = RichText::new(label).size(FS_SMALL).color(c_on_accent());
    egui::Button::new(rich)
        .fill(c_accent())
        .stroke(Stroke::NONE)
        .rounding(RADIUS_BUTTON)
        .min_size(egui::vec2(0.0, 26.0))
}
pub fn primary_button(ui: &mut Ui, label: &str) -> Response {
    ui.add(primary_button_widget(label))
}

/// Primary filled button with a leading Nerd-Font icon glyph.
pub fn primary_button_icon_widget<'a>(icon: &'a str, label: &'a str) -> egui::Button<'a> {
    let text = icon_label_job(icon, label, FS_SMALL, c_on_accent());
    egui::Button::new(text)
        .fill(c_accent())
        .stroke(Stroke::NONE)
        .rounding(RADIUS_BUTTON)
        .min_size(egui::vec2(0.0, 26.0))
}
pub fn primary_button_icon(ui: &mut Ui, icon: &str, label: &str) -> Response {
    ui.add(primary_button_icon_widget(icon, label))
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
        .rounding(RADIUS_BUTTON)
        .min_size(egui::vec2(0.0, 26.0))
}
pub fn ghost_button(ui: &mut Ui, label: &str, danger: bool) -> Response {
    ui.add(ghost_button_widget(label, danger))
}

/// Ghost (neutral) secondary button with a leading Nerd-Font icon glyph.
pub fn ghost_button_icon_widget<'a>(
    icon: &'a str,
    label: &'a str,
    danger: bool,
) -> egui::Button<'a> {
    let color = if danger {
        crate::theme::c_danger()
    } else {
        c_text()
    };
    let text = icon_label_job(icon, label, FS_SMALL, color);
    let btn = egui::Button::new(text)
        .fill(c_bg_elevated_2())
        .stroke(Stroke::new(1.0, c_border_subtle()))
        .rounding(RADIUS_BUTTON)
        .min_size(egui::vec2(0.0, 26.0));
    btn
}
pub fn ghost_button_icon(ui: &mut Ui, icon: &str, label: &str, danger: bool) -> Response {
    ui.add(ghost_button_icon_widget(icon, label, danger))
}

/// Compact chip-style button for tight toolbar rows (git Pull/Push/Fetch, etc.) — shares the
/// ghost-button fill/border/rounding palette but renders at `FS_TINY` on `c_bg_elevated` with
/// 6px rounding, so the small action chips share one language across the app. Leading icon
/// glyph variant.
pub fn mini_button_icon_widget<'a>(icon: &'a str, label: &'a str) -> egui::Button<'a> {
    let text = icon_label_job(icon, label, FS_TINY, c_text_muted());
    egui::Button::new(text)
        .fill(c_bg_elevated())
        .stroke(Stroke::new(1.0, c_border_subtle()))
        .rounding(6.0)
}

/// Settings-page sidebar nav row (icon + label, rounded pill row).
pub fn settings_nav_row(ui: &mut Ui, icon: &str, label: &str, selected: bool) -> Response {
    let row_w = ui.available_width();
    let h = 30.0;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(row_w, h), Sense::click());
    let hovered = response.hovered();
    let fill = if selected {
        c_row_active()
    } else if hovered {
        c_row_hover()
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, Rounding::same(7.0), fill);
    if selected {
        ui.painter().rect_stroke(
            rect,
            Rounding::same(7.0),
            Stroke::new(1.0, c_border_subtle()),
        );
    }
    let text_color = if selected { c_text() } else { c_text_muted() };
    ui.allocate_new_ui(
        egui::UiBuilder::new().max_rect(rect.shrink2(egui::vec2(10.0, 4.0))),
        |ui| {
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.label(
                    RichText::new(icon)
                        .size(FS_BODY)
                        .font(FontId::new(FS_BODY, icon_font()))
                        .color(if selected { c_accent() } else { c_text_faint() }),
                );
                ui.add_space(8.0);
                ui.add(
                    Label::new(RichText::new(label).size(FS_SMALL).color(text_color))
                        .selectable(false),
                );
            });
        },
    );
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}
