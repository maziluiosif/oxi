//! Shared chrome widgets (sidebar fields, empty states).

use eframe::egui::{FontId, Frame, Margin, RichText, Stroke, TextEdit, Ui};

use crate::theme::{C_BG_INPUT, C_BORDER, C_TEXT_MUTED, FS_BODY, FS_SMALL};

pub fn sidebar_text_field(ui: &mut Ui, text: &mut String, hint: &str) {
    Frame::none()
        .fill(C_BG_INPUT)
        .stroke(Stroke::new(1.0, C_BORDER))
        .rounding(7.0)
        .inner_margin(Margin::symmetric(8.0, 3.0))
        .show(ui, |ui| {
            ui.add(
                TextEdit::singleline(text)
                    .frame(false)
                    .margin(Margin::symmetric(1.0, 0.0))
                    .font(FontId::proportional(FS_SMALL))
                    .desired_width(ui.available_width())
                    .hint_text(hint),
            );
        });
    ui.add_space(3.0);
}

pub fn render_empty_state(ui: &mut Ui) {
    ui.add_space(24.0);
    ui.set_max_width(480.0);
    ui.label(
        RichText::new("Send a message. Set provider and model in the chip; replies stream here.")
            .size(FS_BODY)
            .color(C_TEXT_MUTED),
    );
}
