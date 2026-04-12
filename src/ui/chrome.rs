//! Shared chrome widgets (sidebar fields, empty states, etc.).

use eframe::egui::{FontId, Frame, Margin, RichText, Stroke, TextEdit, Ui};

use crate::theme::{C_BG_INPUT, C_BORDER, C_TEXT_MUTED, FS_BODY, FS_SMALL, FS_TINY};

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
                    .font(FontId::proportional(FS_TINY))
                    .desired_width(ui.available_width())
                    .hint_text(hint),
            );
        });
    ui.add_space(3.0);
}

pub fn render_empty_state(ui: &mut Ui) {
    ui.add_space(32.0);
    ui.set_max_width(520.0);

    ui.vertical(|ui| {
        ui.label(
            RichText::new("oxi")
                .size(22.0)
                .color(crate::theme::C_TEXT)
                .strong(),
        );
        ui.add_space(8.0);
        ui.label(
            RichText::new("An AI coding agent running locally in your workspace.")
                .size(FS_BODY)
                .color(crate::theme::C_TEXT_MUTED),
        );
        ui.add_space(16.0);

        let hints = [
            ("⚙", "Configure provider & model in Settings"),
            ("📁", "Use \"Add workspace\" to set your project root"),
            ("🔧", "Tools (read, write, bash, …) run inside the workspace"),
            ("⌨", "Enter to send · Shift+Enter for newline · ↑/↓ for history"),
            ("🖼", "Attach images with + or paste with Ctrl/Cmd+V"),
        ];
        for (icon, tip) in hints {
            ui.horizontal(|ui| {
                ui.add_space(2.0);
                ui.label(RichText::new(icon).size(FS_SMALL).color(crate::theme::C_TEXT_MUTED));
                ui.add_space(6.0);
                ui.label(RichText::new(tip).size(FS_SMALL).color(C_TEXT_MUTED));
            });
            ui.add_space(3.0);
        }
    });
}
