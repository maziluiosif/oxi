//! Incremental Tree-sitter parsing and query-based highlighting for the workspace editor.

use std::borrow::Cow;

use eframe::egui::{self, FontId};
use tree_sitter::{InputEdit, Parser, Point, Query, QueryCursor, StreamingIterator, Tree};

use super::{SyntaxPalette, active_palette};

/// Per-document parse state. The syntax tree is edited and reused on every buffer change.
pub struct EditorSyntaxState {
    language: String,
    content: String,
    content_revision: Option<u64>,
    tree: Tree,
    job: egui::text::LayoutJob,
    palette: SyntaxPalette,
    parser: Parser,
    query: Query,
}

#[cfg(test)]
pub fn highlight_editor_code(
    state: &mut Option<EditorSyntaxState>,
    content: &str,
    language: &str,
    font_id: FontId,
) -> Option<egui::text::LayoutJob> {
    highlight_editor_code_with_revision(state, content, language, font_id, None)
}

pub fn highlight_editor_code_with_revision(
    state: &mut Option<EditorSyntaxState>,
    content: &str,
    language: &str,
    font_id: FontId,
    content_revision: Option<u64>,
) -> Option<egui::text::LayoutJob> {
    let (ts_language, query_source) = language_config(language)?;
    let palette = active_palette().syntax;
    if let Some(current) = state.as_ref()
        && current.language == language
        && current.palette == palette
        && content_revision.map_or_else(
            || current.content == content,
            |revision| current.content_revision == Some(revision),
        )
    {
        return Some(current.job.clone());
    }

    if let Some(current) = state.as_mut()
        && current.language == language
        && current.palette == palette
    {
        let edit = input_edit(&current.content, content);
        current.tree.edit(&edit);
        let tree = current.parser.parse(content, Some(&current.tree))?;
        let job = layout_job(content, &tree, &current.query, palette, font_id);
        current.content = content.to_owned();
        current.content_revision = content_revision;
        current.tree = tree;
        current.job = job.clone();
        return Some(job);
    }

    let mut parser = Parser::new();
    parser.set_language(&ts_language).ok()?;
    let query = Query::new(&ts_language, &query_source).ok()?;
    let tree = parser.parse(content, None)?;
    let job = layout_job(content, &tree, &query, palette, font_id);
    *state = Some(EditorSyntaxState {
        language: language.to_owned(),
        content: content.to_owned(),
        content_revision,
        tree,
        job: job.clone(),
        palette,
        parser,
        query,
    });
    Some(job)
}

fn language_config(language: &str) -> Option<(tree_sitter::Language, Cow<'static, str>)> {
    Some(match language {
        "rs" => (
            tree_sitter_rust::LANGUAGE.into(),
            Cow::Borrowed(tree_sitter_rust::HIGHLIGHTS_QUERY),
        ),
        "py" => (
            tree_sitter_python::LANGUAGE.into(),
            Cow::Borrowed(tree_sitter_python::HIGHLIGHTS_QUERY),
        ),
        "js" => (
            tree_sitter_javascript::LANGUAGE.into(),
            Cow::Borrowed(tree_sitter_javascript::HIGHLIGHT_QUERY),
        ),
        "jsx" => (
            tree_sitter_javascript::LANGUAGE.into(),
            Cow::Owned(format!(
                "{}\n{}",
                tree_sitter_javascript::HIGHLIGHT_QUERY,
                tree_sitter_javascript::JSX_HIGHLIGHT_QUERY
            )),
        ),
        "ts" => (
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Cow::Borrowed(tree_sitter_typescript::HIGHLIGHTS_QUERY),
        ),
        "tsx" => (
            tree_sitter_typescript::LANGUAGE_TSX.into(),
            Cow::Borrowed(tree_sitter_typescript::HIGHLIGHTS_QUERY),
        ),
        "json" => (
            tree_sitter_json::LANGUAGE.into(),
            Cow::Borrowed(tree_sitter_json::HIGHLIGHTS_QUERY),
        ),
        "toml" => (
            tree_sitter_toml_ng::LANGUAGE.into(),
            Cow::Borrowed(tree_sitter_toml_ng::HIGHLIGHTS_QUERY),
        ),
        "yaml" => (
            tree_sitter_yaml::LANGUAGE.into(),
            Cow::Borrowed(tree_sitter_yaml::HIGHLIGHTS_QUERY),
        ),
        "html" => (
            tree_sitter_html::LANGUAGE.into(),
            Cow::Borrowed(tree_sitter_html::HIGHLIGHTS_QUERY),
        ),
        "css" => (
            tree_sitter_css::LANGUAGE.into(),
            Cow::Borrowed(tree_sitter_css::HIGHLIGHTS_QUERY),
        ),
        "sh" => (
            tree_sitter_bash::LANGUAGE.into(),
            Cow::Borrowed(tree_sitter_bash::HIGHLIGHT_QUERY),
        ),
        "c" => (
            tree_sitter_c::LANGUAGE.into(),
            Cow::Borrowed(tree_sitter_c::HIGHLIGHT_QUERY),
        ),
        "cpp" => (
            tree_sitter_cpp::LANGUAGE.into(),
            Cow::Borrowed(tree_sitter_cpp::HIGHLIGHT_QUERY),
        ),
        "go" => (
            tree_sitter_go::LANGUAGE.into(),
            Cow::Borrowed(tree_sitter_go::HIGHLIGHTS_QUERY),
        ),
        "java" => (
            tree_sitter_java::LANGUAGE.into(),
            Cow::Borrowed(tree_sitter_java::HIGHLIGHTS_QUERY),
        ),
        _ => return None,
    })
}
fn layout_job(
    content: &str,
    tree: &Tree,
    query: &Query,
    palette: SyntaxPalette,
    font_id: FontId,
) -> egui::text::LayoutJob {
    let names = query.capture_names();
    let mut colors = vec![palette.foreground; content.len()];
    let mut priorities = vec![0usize; content.len()];
    let mut cursor = QueryCursor::new();
    let mut captures = cursor.captures(query, tree.root_node(), content.as_bytes());
    while let Some((matched, capture_index)) = captures.next() {
        let capture = matched.captures[*capture_index];
        let name = names[capture.index as usize];
        let color = capture_color(name, palette);
        let range = capture.node.byte_range();
        if range.end <= colors.len() {
            // Prefer the more specific (shorter) capture when query patterns overlap. This keeps
            // nested strings/escapes, fields, and function names from being flattened by a later
            // broad parent capture.
            let priority = content.len().saturating_sub(range.len());
            for index in range {
                if priority >= priorities[index] {
                    priorities[index] = priority;
                    colors[index] = color;
                }
            }
        }
    }

    let mut job = egui::text::LayoutJob {
        text: content.to_owned(),
        ..Default::default()
    };
    let mut start = 0;
    while start < content.len() {
        let color = colors[start];
        let mut end = start + 1;
        while end < content.len() && colors[end] == color {
            end += 1;
        }
        while end < content.len() && !content.is_char_boundary(end) {
            end += 1;
        }
        job.sections.push(egui::text::LayoutSection {
            leading_space: 0.0,
            byte_range: egui::text::ByteIndex(start)..egui::text::ByteIndex(end),
            format: egui::text::TextFormat {
                font_id: font_id.clone(),
                color,
                italics: name_is_comment_color(color, palette),
                ..Default::default()
            },
        });
        start = end;
    }
    job
}

fn capture_color(name: &str, p: SyntaxPalette) -> egui::Color32 {
    if name.contains("comment") {
        p.comment
    } else if name.contains("string") {
        p.string
    } else if name.contains("escape") || name.contains("regex") {
        p.regexp
    } else if name.contains("function") || name.contains("method") || name.contains("constructor") {
        p.function
    } else if name.contains("type") || name.contains("class") || name.contains("namespace") {
        p.type_name
    } else if name.contains("keyword")
        || name.contains("conditional")
        || name.contains("repeat")
        || name.contains("exception")
    {
        p.keyword
    } else if name.contains("number") || name.contains("float") {
        p.number
    } else if name.contains("constant") || name.contains("boolean") {
        p.constant
    } else if name.contains("property") || name.contains("field") || name.contains("attribute") {
        p.attribute
    } else if name.contains("tag") {
        p.tag
    } else if name.contains("operator") || name.contains("punctuation") {
        p.operator
    } else if name.contains("variable") || name.contains("parameter") {
        p.variable
    } else {
        p.foreground
    }
}

fn name_is_comment_color(color: egui::Color32, palette: SyntaxPalette) -> bool {
    color == palette.comment
}

fn input_edit(old: &str, new: &str) -> InputEdit {
    let mut start = 0;
    for (a, b) in old.chars().zip(new.chars()) {
        if a != b {
            break;
        }
        start += a.len_utf8();
    }
    let mut suffix = 0;
    for (a, b) in old[start..].chars().rev().zip(new[start..].chars().rev()) {
        if a != b {
            break;
        }
        let width = a.len_utf8();
        if suffix + width > old.len() - start || suffix + width > new.len() - start {
            break;
        }
        suffix += width;
    }
    let old_end = old.len() - suffix;
    let new_end = new.len() - suffix;
    InputEdit {
        start_byte: start,
        old_end_byte: old_end,
        new_end_byte: new_end,
        start_position: point_at(old, start),
        old_end_position: point_at(old, old_end),
        new_end_position: point_at(new, new_end),
    }
}

fn point_at(text: &str, byte: usize) -> Point {
    let byte = floor_char_boundary(text, byte);
    let prefix = &text[..byte];
    let row = prefix.bytes().filter(|byte| *byte == b'\n').count();
    let column = prefix
        .rsplit_once('\n')
        .map_or(prefix.len(), |(_, tail)| tail.len());
    Point::new(row, column)
}

fn floor_char_boundary(text: &str, mut byte: usize) -> usize {
    byte = byte.min(text.len());
    while byte > 0 && !text.is_char_boundary(byte) {
        byte -= 1;
    }
    byte
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incrementally_reparses_unicode_without_invalid_ranges() {
        let mut state = None;
        let font = FontId::monospace(12.0);
        highlight_editor_code(
            &mut state,
            "fn main() { println!(\"↑\"); }",
            "rs",
            font.clone(),
        )
        .unwrap();
        let job =
            highlight_editor_code(&mut state, "fn main() { println!(\"↑ ok\"); }", "rs", font)
                .unwrap();
        assert!(job.sections.iter().all(|section| {
            job.text.is_char_boundary(section.byte_range.start.0)
                && job.text.is_char_boundary(section.byte_range.end.0)
        }));
    }

    #[test]
    fn rust_query_produces_multiple_syntax_colors() {
        let mut state = None;
        let job = highlight_editor_code(
            &mut state,
            "fn main() { let answer = 42; }",
            "rs",
            FontId::monospace(12.0),
        )
        .unwrap();
        let mut colors = job
            .sections
            .iter()
            .map(|section| section.format.color)
            .collect::<Vec<_>>();
        colors.sort_by_key(|color| color.to_array());
        colors.dedup();
        assert!(colors.len() >= 3);
    }
}
