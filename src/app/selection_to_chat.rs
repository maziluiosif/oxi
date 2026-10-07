//! "Add Selection to Chat" (Cmd/Ctrl+Shift+L): the editor's selection becomes a text attachment
//! named `path:start-end`, and the chat comes forward with the composer focused.

use std::path::Path;

use super::OxiApp;
use crate::model::UserAttachment;

impl OxiApp {
    pub(crate) fn add_editor_selection_to_chat(&mut self) {
        let Some(document) = self.conv.editor.active_document() else {
            return;
        };
        let Some((start, end)) = self
            .conv
            .editor
            .editor_selection_chars
            .filter(|(start, end)| start < end)
        else {
            self.notify_composer("Select some code in the editor first.");
            return;
        };
        let root = Path::new(&self.active_workspace().root_path);
        let path = document
            .path
            .strip_prefix(root)
            .unwrap_or(&document.path)
            .to_string_lossy()
            .replace('\\', "/");
        let attachment = selection_attachment(&document.content, start, end, &path);
        self.conv.composer.pending_texts.push(attachment);
        self.reveal_chat_view();
        self.conv.composer.focus_next_frame = true;
    }
}

/// The selected chars `start..end` of `content` as a fenced attachment named after their lines.
fn selection_attachment(content: &str, start: usize, end: usize, path: &str) -> UserAttachment {
    let byte = |chars: usize| {
        content
            .char_indices()
            .nth(chars)
            .map_or(content.len(), |(i, _)| i)
    };
    let (from, to) = (byte(start), byte(end));
    let snippet = &content[from..to];
    let first_line = content[..from].matches('\n').count() + 1;
    // A selection ending right after a newline does not reach into the next line.
    let last_line = first_line + snippet.trim_end_matches('\n').matches('\n').count();
    let lang = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let name = if first_line == last_line {
        format!("{path}:{first_line}")
    } else {
        format!("{path}:{first_line}-{last_line}")
    };
    UserAttachment::Text {
        name,
        text: format!("```{lang}\n{}\n```", snippet.trim_end_matches('\n')),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attachment_names_the_selected_lines() {
        let content = "fn a() {}\nfn b() {\n    1\n}\nfn c() {}\n";
        let start = content.find("fn b").unwrap();
        let end = content.find("fn c").unwrap();
        let UserAttachment::Text { name, text } =
            selection_attachment(content, start, end, "src/x.rs")
        else {
            panic!("text attachment expected");
        };
        assert_eq!(name, "src/x.rs:2-4");
        assert_eq!(text, "```rs\nfn b() {\n    1\n}\n```");
    }

    #[test]
    fn single_line_and_multibyte_selection() {
        let content = "ăâ\nîș ț\n";
        let start = content.chars().position(|c| c == 'î').unwrap();
        let UserAttachment::Text { name, text } =
            selection_attachment(content, start, start + 2, "notes.md")
        else {
            panic!("text attachment expected");
        };
        assert_eq!(name, "notes.md:2");
        assert_eq!(text, "```md\nîș\n```");
    }
}
