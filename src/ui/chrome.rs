//! Shared chrome widgets (sidebar fields, empty states, pill tabs, cards, …).

use eframe::egui::{
    self, Align, Color32, FontId, Frame, Label, Layout, Margin, Response, RichText, Rounding,
    Sense, Stroke, TextEdit, Ui,
};

use crate::theme::{
    C_ACCENT, C_BG_ELEVATED, C_BG_ELEVATED_2, C_BG_INPUT, C_BORDER, C_BORDER_SUBTLE, C_ROW_ACTIVE,
    C_ROW_HOVER, C_TEXT, C_TEXT_FAINT, C_TEXT_MUTED, FS_BODY, FS_SMALL, FS_TINY,
};

pub fn sidebar_text_field(ui: &mut Ui, text: &mut String, hint: &str) {
    Frame::none()
        .fill(C_BG_INPUT)
        .stroke(Stroke::new(1.0, C_BORDER_SUBTLE))
        .rounding(7.0)
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
            .color(C_TEXT_FAINT)
            .strong(),
    );
    ui.add_space(2.0);
}

/// Primary section title inside a panel.
pub fn settings_section_title(ui: &mut Ui, title: &str, subtitle: Option<&str>) {
    ui.label(RichText::new(title).size(17.0).color(C_TEXT).strong());
    if let Some(sub) = subtitle {
        ui.add_space(2.0);
        ui.label(RichText::new(sub).size(FS_SMALL).color(C_TEXT_MUTED));
    }
    ui.add_space(10.0);
}

/// A card frame used to group related settings. Matches the elevated background + subtle border.
pub fn card_frame() -> Frame {
    Frame::none()
        .fill(C_BG_ELEVATED)
        .stroke(Stroke::new(1.0, C_BORDER_SUBTLE))
        .rounding(10.0)
        .inner_margin(Margin::symmetric(14.0, 12.0))
}

/// Slightly elevated card variant — used for nested sub-cards inside a panel.
pub fn nested_card_frame() -> Frame {
    Frame::none()
        .fill(C_BG_ELEVATED_2)
        .stroke(Stroke::new(1.0, C_BORDER_SUBTLE))
        .rounding(8.0)
        .inner_margin(Margin::symmetric(10.0, 8.0))
}

/// Single-line label → value field row used on settings panels.
pub fn field_label(ui: &mut Ui, text: &str) {
    ui.add_space(6.0);
    ui.label(RichText::new(text).size(FS_TINY).color(C_TEXT_MUTED));
    ui.add_space(2.0);
}

/// Horizontal rule that matches the subtle border color.
pub fn hairline(ui: &mut Ui) {
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, 1.0), Sense::hover());
    ui.painter().hline(
        rect.x_range(),
        rect.center().y,
        Stroke::new(1.0, C_BORDER_SUBTLE),
    );
}

/// Pill-style tab used for provider selection or sub-tabs. Returns `true` when clicked.
pub fn pill_tab(ui: &mut Ui, label: &str, selected: bool) -> bool {
    let text_size = FS_SMALL;
    let galley = ui.painter().layout_no_wrap(
        label.to_string(),
        FontId::proportional(text_size),
        C_TEXT,
    );
    let pad = egui::vec2(14.0, 6.0);
    let size = egui::vec2(galley.rect.width() + pad.x * 2.0, galley.rect.height() + pad.y * 2.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let hovered = response.hovered();
    let (fill, stroke, text_color) = if selected {
        (C_ROW_ACTIVE, Stroke::new(1.0, C_BORDER), C_TEXT)
    } else if hovered {
        (C_ROW_HOVER, Stroke::new(1.0, C_BORDER_SUBTLE), C_TEXT)
    } else {
        (Color32::TRANSPARENT, Stroke::new(1.0, C_BORDER_SUBTLE), C_TEXT_MUTED)
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
pub fn primary_button(ui: &mut Ui, label: &str) -> Response {
    let rich = RichText::new(label).size(FS_SMALL).color(Color32::WHITE);
    let btn = egui::Button::new(rich)
        .fill(C_ACCENT)
        .stroke(Stroke::NONE)
        .rounding(7.0)
        .min_size(egui::vec2(0.0, 26.0));
    ui.add(btn)
}

/// Neutral secondary button — used for Sign out, Delete, etc. (with `danger` color swap).
pub fn ghost_button(ui: &mut Ui, label: &str, danger: bool) -> Response {
    let color = if danger {
        crate::theme::C_DANGER
    } else {
        C_TEXT
    };
    let btn = egui::Button::new(RichText::new(label).size(FS_SMALL).color(color))
        .fill(C_BG_ELEVATED_2)
        .stroke(Stroke::new(1.0, C_BORDER_SUBTLE))
        .rounding(7.0)
        .min_size(egui::vec2(0.0, 26.0));
    ui.add(btn)
}

/// Settings-page sidebar nav row (icon + label, rounded pill row).
pub fn settings_nav_row(ui: &mut Ui, icon: &str, label: &str, selected: bool) -> Response {
    let row_w = ui.available_width();
    let h = 30.0;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(row_w, h), Sense::click());
    let hovered = response.hovered();
    let fill = if selected {
        C_ROW_ACTIVE
    } else if hovered {
        C_ROW_HOVER
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, Rounding::same(7.0), fill);
    if selected {
        ui.painter().rect_stroke(
            rect,
            Rounding::same(7.0),
            Stroke::new(1.0, C_BORDER_SUBTLE),
        );
    }
    let text_color = if selected { C_TEXT } else { C_TEXT_MUTED };
    ui.allocate_new_ui(
        egui::UiBuilder::new().max_rect(rect.shrink2(egui::vec2(10.0, 4.0))),
        |ui| {
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.label(
                    RichText::new(icon)
                        .size(FS_BODY)
                        .color(if selected { C_ACCENT } else { C_TEXT_FAINT }),
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

pub fn render_empty_state(ui: &mut Ui) {
    ui.add_space(32.0);
    ui.set_max_width(520.0);

    ui.vertical(|ui| {
        ui.label(
            RichText::new("oxi")
                .size(24.0)
                .color(crate::theme::C_TEXT)
                .strong(),
        );
        ui.add_space(6.0);
        ui.label(
            RichText::new("An AI coding agent running locally in your workspace.")
                .size(FS_BODY)
                .color(crate::theme::C_TEXT_MUTED),
        );
        ui.add_space(18.0);

        let hints = [
            ("⚙", "Configure provider & model in Settings"),
            ("📁", "Use \"Add workspace\" to set your project root"),
            (
                "🔧",
                "Tools (read, write, bash, …) run inside the workspace",
            ),
            (
                "⌨",
                "Enter to send · Shift+Enter for newline · ↑/↓ for history",
            ),
            ("🖼", "Attach images with + or paste with Ctrl/Cmd+V"),
        ];
        for (icon, tip) in hints {
            ui.horizontal(|ui| {
                ui.add_space(2.0);
                ui.label(
                    RichText::new(icon)
                        .size(FS_SMALL)
                        .color(crate::theme::C_TEXT_MUTED),
                );
                ui.add_space(8.0);
                ui.label(RichText::new(tip).size(FS_SMALL).color(C_TEXT_MUTED));
            });
            ui.add_space(5.0);
        }
    });
}
