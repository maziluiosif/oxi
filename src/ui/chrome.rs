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
    WidgetText::LayoutJob(job)
}

/// Render `icon` with the dedicated icon font family (no trailing label) at the given size/color.
pub fn icon_glyph_rich(icon: &str, size: f32, color: Color32) -> RichText {
    RichText::new(icon)
        .size(size)
        .color(color)
        .font(FontId::new(size, icon_font()))
}

/// Visual spec for [`icon_button_core`]: rest/hover fills and strokes plus the resting glyph ink.
/// The hover glyph ink is always the accent — every clickable icon in the app shares the sidebar
/// "Add workspace" hover language.
pub struct IconButtonLook {
    pub fill: Color32,
    pub hover_fill: Color32,
    pub stroke: Color32,
    pub hover_stroke: Color32,
    pub rounding: Rounding,
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
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let hovered = response.hovered();
    let fill = if hovered { look.hover_fill } else { look.fill };
    let stroke = if hovered { look.hover_stroke } else { look.stroke };
    if fill != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, look.rounding, fill);
    }
    if stroke != Color32::TRANSPARENT {
        ui.painter()
            .rect_stroke(rect, look.rounding, Stroke::new(1.0, stroke));
    }
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        icon,
        FontId::new(glyph_size, icon_font()),
        if active || hovered {
            c_accent()
        } else {
            look.glyph
        },
    );
    if hovered {
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
            fill: c_bg_elevated(),
            hover_fill: c_row_hover(),
            stroke: c_border_subtle(),
            hover_stroke: c_border(),
            rounding: Rounding::same(RADIUS_CHIP),
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
            fill: Color32::TRANSPARENT,
            hover_fill: c_row_hover(),
            stroke: Color32::TRANSPARENT,
            hover_stroke: Color32::TRANSPARENT,
            rounding: Rounding::same(RADIUS_CHIP),
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
        if hovered { c_accent() } else { color },
    );
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
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
        .on_hover_cursor(egui::CursorIcon::PointingHand)
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
        .on_hover_cursor(egui::CursorIcon::PointingHand)
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
    let hovered = enabled && response.hovered();
    let fill = if hovered { look.hover_fill } else { look.fill };
    let stroke = if hovered { look.hover_stroke } else { look.stroke };
    if fill != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, look.rounding, fill);
    }
    if stroke != Color32::TRANSPARENT {
        ui.painter()
            .rect_stroke(rect, look.rounding, Stroke::new(1.0, stroke));
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
            rounding: Rounding::same(RADIUS_BUTTON),
            glyph: glyph_ink,
        },
        label_ink,
        hover_glyph,
        enabled,
    )
}

/// Compact chip-style button for tight toolbar rows (git Pull/Push/Fetch, etc.) — shares the
/// ghost-button border palette but renders at `FS_TINY` on `c_bg_elevated` with 6px rounding,
/// so the small action chips share one language across the app. Leading icon glyph variant.
pub fn mini_button_icon(ui: &mut Ui, icon: &str, label: &str) -> Response {
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
            rounding: Rounding::same(6.0),
            glyph: c_text_muted(),
        },
        c_text_muted(),
        c_accent(),
        true,
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
            rounding: Rounding::same(RADIUS_BUTTON),
            glyph: color,
        },
        color,
        c_accent(),
        true,
    )
}

/// Full-width elevated row button with icon + label — the sidebar "Add workspace" look as a
/// reusable widget (elevated fill → `c_row_hover`, subtle border → `c_border`, accent icon on
/// hover). Used for sidebar-style rows like the Settings footer.
pub fn row_button_icon(ui: &mut Ui, icon: &str, label: &str, min_size: egui::Vec2) -> Response {
    icon_text_button_core(
        ui,
        icon,
        label,
        FS_SMALL,
        min_size,
        &IconButtonLook {
            fill: c_bg_elevated(),
            hover_fill: c_row_hover(),
            stroke: c_border_subtle(),
            hover_stroke: c_border(),
            rounding: Rounding::same(RADIUS_CHIP),
            glyph: c_text_muted(),
        },
        c_text(),
        c_accent(),
        true,
    )
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
                        .color(if selected || hovered {
                            c_accent()
                        } else {
                            c_text_faint()
                        }),
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
