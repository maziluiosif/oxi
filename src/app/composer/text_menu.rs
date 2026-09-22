//! Right-click editing menu and Linux middle-click paste for the chat input.
//!
//! egui's `TextEdit` handles keyboard shortcuts but has no context menu, and it does not
//! read the X11/Wayland primary selection. Both operate on `conv.input` directly using the
//! TextEdit's char-based cursor state.

use super::*;

impl OxiApp {
    pub(super) fn composer_text_menu(
        &mut self,
        ui: &Ui,
        input_id: Id,
        response: &egui::Response,
        #[cfg_attr(not(target_os = "linux"), allow(unused_variables))] galley: &egui::Galley,
        #[cfg_attr(not(target_os = "linux"), allow(unused_variables))] galley_pos: egui::Pos2,
    ) {
        #[cfg(target_os = "linux")]
        if response.clicked_by(egui::PointerButton::Middle)
            && let Some(pos) = response.interact_pointer_pos()
            && let Some(text) = read_primary_selection()
        {
            let at = galley.cursor_from_pos(pos - galley_pos).index.0;
            let caret = replace_char_range(&mut self.conv.input, at..at, &text);
            set_char_range(ui.ctx(), input_id, caret..caret);
        }

        let cursor = TextEdit::load_state(ui.ctx(), input_id).and_then(|s| s.cursor.char_range());
        let selection = cursor
            .map(|r| {
                let r = r.as_sorted_char_range();
                r.start.0..r.end.0
            })
            .filter(|r| !r.is_empty());
        let caret = cursor.map(|r| r.primary.index.0);
        let has_text = !self.conv.input.is_empty();

        response.context_menu(|ui| {
            ui.set_min_width(140.0);
            if ui
                .add_enabled(selection.is_some(), egui::Button::new("Cut"))
                .clicked()
                && let Some(range) = selection.clone()
            {
                ui.ctx()
                    .copy_text(char_slice(&self.conv.input, range.clone()).to_string());
                let caret = replace_char_range(&mut self.conv.input, range, "");
                set_char_range(ui.ctx(), input_id, caret..caret);
                ui.close();
            }
            if ui
                .add_enabled(selection.is_some(), egui::Button::new("Copy"))
                .clicked()
                && let Some(range) = selection.clone()
            {
                ui.ctx()
                    .copy_text(char_slice(&self.conv.input, range).to_string());
                ui.close();
            }
            if ui.button("Paste").clicked() {
                // An image on the clipboard becomes an attachment, same as Cmd/Ctrl+V.
                if !self.paste_clipboard_image()
                    && let Some(text) = read_clipboard_text()
                {
                    let at = caret.unwrap_or_else(|| self.conv.input.chars().count());
                    let range = selection.clone().unwrap_or(at..at);
                    let caret = replace_char_range(&mut self.conv.input, range, &text);
                    set_char_range(ui.ctx(), input_id, caret..caret);
                }
                ui.close();
            }
            ui.separator();
            if ui
                .add_enabled(has_text, egui::Button::new("Select all"))
                .clicked()
            {
                set_char_range(ui.ctx(), input_id, 0..self.conv.input.chars().count());
                ui.close();
            }
        });
    }
}

/// Select `range` (char indices; empty = caret) and give the input focus back so typing
/// continues where the edit left off.
fn set_char_range(ctx: &egui::Context, input_id: Id, range: std::ops::Range<usize>) {
    let mut state = TextEdit::load_state(ctx, input_id).unwrap_or_default();
    state.cursor.set_char_range(Some(CCursorRange::two(
        CCursor::new(range.start),
        CCursor::new(range.end),
    )));
    state.store(ctx, input_id);
    ctx.memory_mut(|m| m.request_focus(input_id));
}

fn char_to_byte(text: &str, char_idx: usize) -> usize {
    text.char_indices()
        .nth(char_idx)
        .map_or(text.len(), |(byte, _)| byte)
}

fn char_slice(text: &str, range: std::ops::Range<usize>) -> &str {
    &text[char_to_byte(text, range.start)..char_to_byte(text, range.end)]
}

/// Replace a char range with `insert`, returning the char index just after the insertion.
fn replace_char_range(text: &mut String, range: std::ops::Range<usize>, insert: &str) -> usize {
    let start = char_to_byte(text, range.start);
    let end = char_to_byte(text, range.end);
    text.replace_range(start..end, insert);
    range.start + insert.chars().count()
}

fn read_clipboard_text() -> Option<String> {
    arboard::Clipboard::new()
        .ok()?
        .get_text()
        .ok()
        .filter(|t| !t.is_empty())
}

#[cfg(target_os = "linux")]
fn read_primary_selection() -> Option<String> {
    use arboard::{GetExtLinux, LinuxClipboardKind};
    arboard::Clipboard::new()
        .ok()?
        .get()
        .clipboard(LinuxClipboardKind::Primary)
        .text()
        .ok()
        .filter(|t| !t.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replace_char_range_handles_multibyte_text() {
        let mut text = "héllo wörld".to_string();
        let caret = replace_char_range(&mut text, 6..11, "oxi");
        assert_eq!(text, "héllo oxi");
        assert_eq!(caret, 9);
        assert_eq!(char_slice(&text, 1..5), "éllo");
    }

    #[test]
    fn insert_at_end_and_empty_range() {
        let mut text = "ab".to_string();
        let caret = replace_char_range(&mut text, 2..2, "ç");
        assert_eq!(text, "abç");
        assert_eq!(caret, 3);
    }
}
