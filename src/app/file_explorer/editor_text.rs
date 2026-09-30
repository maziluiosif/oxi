//! The editor's [`egui::TextBuffer`]: a `String` with fast char <-> byte index conversion.
//!
//! egui's `TextBuffer for String` turns every char index into a byte index by walking
//! `char_indices()` from the start, twice per keystroke; with the caret deep in a large file that
//! walk dominated typing. Source code is mostly ASCII, where both indices coincide, so an ASCII
//! prefix (checked with SIMD) answers at once; otherwise UTF-8 lead bytes are counted, which the
//! compiler vectorizes.

use std::ops::Range;

use eframe::egui::{
    self,
    text::{ByteIndex, CharIndex},
};

pub(crate) struct EditorText<'a>(pub &'a mut String);

/// Whether `byte` starts a UTF-8 character (is not a continuation byte).
fn starts_char(byte: &u8) -> bool {
    (byte & 0xC0) != 0x80
}

pub(crate) fn byte_index(text: &str, char_index: usize) -> usize {
    let bytes = text.as_bytes();
    if char_index >= bytes.len() {
        // At most one byte per char: only an all-ASCII text can have this many chars.
        return if char_index == bytes.len() && bytes.is_ascii() {
            char_index
        } else {
            char_count_to_byte(bytes, char_index)
        };
    }
    if bytes[..char_index].is_ascii() {
        return char_index;
    }
    char_count_to_byte(bytes, char_index)
}

fn char_count_to_byte(bytes: &[u8], char_index: usize) -> usize {
    // Count lead bytes in chunks (vectorized), then finish inside the chunk that holds the char.
    const CHUNK: usize = 4096;
    let mut chars = 0;
    let mut start = 0;
    for chunk in bytes.chunks(CHUNK) {
        let in_chunk = chunk.iter().filter(|b| starts_char(b)).count();
        if chars + in_chunk > char_index {
            for (offset, byte) in chunk.iter().enumerate() {
                if starts_char(byte) {
                    if chars == char_index {
                        return start + offset;
                    }
                    chars += 1;
                }
            }
        }
        chars += in_chunk;
        start += chunk.len();
    }
    bytes.len()
}

pub(crate) fn char_index(text: &str, byte_index: usize) -> usize {
    let prefix = &text.as_bytes()[..byte_index.min(text.len())];
    if prefix.is_ascii() {
        prefix.len()
    } else {
        prefix.iter().filter(|b| starts_char(b)).count()
    }
}

impl egui::TextBuffer for EditorText<'_> {
    fn is_mutable(&self) -> bool {
        true
    }

    fn as_str(&self) -> &str {
        self.0.as_str()
    }

    fn insert_text(&mut self, text: &str, char_index: CharIndex) -> usize {
        let at = byte_index(self.0, char_index.0);
        self.0.insert_str(at, text);
        text.chars().count()
    }

    fn delete_char_range(&mut self, char_range: Range<CharIndex>) {
        assert!(
            char_range.start <= char_range.end,
            "start must be <= end, but got {char_range:?}"
        );
        let start = byte_index(self.0, char_range.start.0);
        let end = start + byte_index(&self.0[start..], char_range.end.0 - char_range.start.0);
        self.0.drain(start..end);
    }

    fn char_range(&self, char_range: Range<CharIndex>) -> &str {
        let start = byte_index(self.0, char_range.start.0);
        let end = start
            + byte_index(
                &self.0[start..],
                char_range.end.0.saturating_sub(char_range.start.0),
            );
        &self.0[start..end]
    }

    fn byte_index_from_char_index(&self, char_index: CharIndex) -> ByteIndex {
        ByteIndex(byte_index(self.0, char_index.0))
    }

    fn char_index_from_byte_index(&self, byte_index: ByteIndex) -> CharIndex {
        CharIndex(char_index(self.0, byte_index.0))
    }

    fn clear(&mut self) {
        self.0.clear();
    }

    fn replace_with(&mut self, text: &str) {
        text.clone_into(self.0);
    }

    fn take(&mut self) -> String {
        std::mem::take(self.0)
    }

    fn type_id(&self) -> std::any::TypeId {
        std::any::TypeId::of::<EditorText<'static>>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::TextBuffer as _;

    #[test]
    fn indices_match_egui_for_ascii_and_unicode() {
        let long_unicode = "ă↑😀b".repeat(3000);
        for text in ["", "abc", "ă↑😀b", "ab\ncd", long_unicode.as_str()] {
            let chars = text.chars().count();
            for char_index in 0..=chars + 2 {
                assert_eq!(
                    ByteIndex(byte_index(text, char_index)),
                    egui::text_selection::text_cursor_state::byte_index_from_char_index(
                        text,
                        CharIndex(char_index)
                    ),
                    "{text:?} @ {char_index}"
                );
            }
            for (byte, _) in text.char_indices().chain([(text.len(), ' ')]) {
                assert_eq!(
                    CharIndex(char_index(text, byte)),
                    egui::text_selection::text_cursor_state::char_index_from_byte_index(
                        text,
                        ByteIndex(byte)
                    )
                );
            }
        }
    }

    #[test]
    fn edits_match_string() {
        let mut ours = "fn ↑ main() {}".to_owned();
        let mut theirs = ours.clone();
        EditorText(&mut ours).insert_text("😀x", CharIndex(5));
        theirs.insert_text("😀x", CharIndex(5));
        assert_eq!(ours, theirs);
        EditorText(&mut ours).delete_char_range(CharIndex(3)..CharIndex(7));
        theirs.delete_char_range(CharIndex(3)..CharIndex(7));
        assert_eq!(ours, theirs);
        assert_eq!(
            EditorText(&mut ours).char_range(CharIndex(1)..CharIndex(4)),
            theirs.char_range(CharIndex(1)..CharIndex(4))
        );
    }
}
