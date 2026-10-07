//! Right-click editing menu and Linux middle-click paste for the chat input.
//!
//! egui's `TextEdit` handles keyboard shortcuts but has no context menu, and it does not
//! read the X11/Wayland primary selection. Both operate on `conv.composer.input` directly using the
//! TextEdit's char-based cursor state.

use super::*;

impl OxiApp {
    pub(super) fn attach_large_paste(&mut self, text: &str) -> bool {
        if !is_large_paste(text) {
            return false;
        }
        let mut index = 1;
        let name = loop {
            let candidate = format!("pasted-{index}.txt");
            if !self.conv.composer.pending_texts.iter().any(|a| matches!(a, crate::model::UserAttachment::Text { name, .. } if name == &candidate)) { break candidate; }
            index += 1;
        };
        self.conv
            .composer
            .pending_texts
            .push(crate::model::UserAttachment::Text {
                name,
                text: text.to_owned(),
            });
        true
    }

    pub(super) fn intercept_large_pastes(&mut self, ui: &Ui, input_id: Id) {
        if !ui.ctx().memory(|m| m.has_focus(input_id)) {
            return;
        }
        let pastes = ui.input_mut(|input| {
            let mut pastes = Vec::new();
            input.events.retain(|event| {
                if let egui::Event::Paste(text) = event
                    && is_large_paste(text)
                {
                    pastes.push(text.clone());
                    return false;
                }
                true
            });
            pastes
        });
        for text in pastes {
            self.attach_large_paste(&text);
        }
    }

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
            if self.attach_large_paste(&text) {
                return;
            }
            let at = galley.cursor_from_pos(pos - galley_pos).index.0;
            let caret = replace_char_range(&mut self.conv.composer.input, at..at, &text);
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
        let has_text = !self.conv.composer.input.is_empty();

        response.context_menu(|ui| {
            ui.set_min_width(140.0);
            if ui
                .add_enabled(selection.is_some(), egui::Button::new("Cut"))
                .clicked()
                && let Some(range) = selection.clone()
            {
                ui.ctx()
                    .copy_text(char_slice(&self.conv.composer.input, range.clone()).to_string());
                let caret = replace_char_range(&mut self.conv.composer.input, range, "");
                set_char_range(ui.ctx(), input_id, caret..caret);
                ui.close();
            }
            if ui
                .add_enabled(selection.is_some(), egui::Button::new("Copy"))
                .clicked()
                && let Some(range) = selection.clone()
            {
                ui.ctx()
                    .copy_text(char_slice(&self.conv.composer.input, range).to_string());
                ui.close();
            }
            if ui.button("Paste").clicked() {
                // An image on the clipboard becomes an attachment, same as Cmd/Ctrl+V.
                if !self.paste_clipboard_image()
                    && let Some(text) = read_clipboard_text()
                {
                    if self.attach_large_paste(&text) {
                        ui.close();
                        return;
                    }
                    let at = caret.unwrap_or_else(|| self.conv.composer.input.chars().count());
                    let range = selection.clone().unwrap_or(at..at);
                    let caret = replace_char_range(&mut self.conv.composer.input, range, &text);
                    set_char_range(ui.ctx(), input_id, caret..caret);
                }
                ui.close();
            }
            ui.separator();
            if ui
                .add_enabled(has_text, egui::Button::new("Select all"))
                .clicked()
            {
                set_char_range(
                    ui.ctx(),
                    input_id,
                    0..self.conv.composer.input.chars().count(),
                );
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

/// Only paste events become attachments; normal typing is never converted.
fn is_large_paste(text: &str) -> bool {
    text.chars().take(2_001).count() > 2_000 || text.lines().take(21).count() > 20
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_paste_threshold_counts_characters_and_lines() {
        assert!(!is_large_paste(&"ă".repeat(2_000)));
        assert!(is_large_paste(&"ă".repeat(2_001)));
        assert!(!is_large_paste(&"line\n".repeat(20)));
        assert!(is_large_paste(&"line\n".repeat(21)));
        assert!(!is_large_paste("small paste"));
    }

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
