//! Incremental Tree-sitter parsing and query-based highlighting for the workspace editor.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use eframe::egui::{self, FontId};
use tree_sitter::{InputEdit, Parser, Point, Query, QueryCursor, StreamingIterator, Tree};

use super::{SyntaxPalette, active_palette};

/// Per-document parse state. The syntax tree is edited and reused on every buffer change.
pub struct EditorSyntaxState {
    language: String,
    content: String,
    content_revision: Option<u64>,
    tree: Tree,
    /// The last highlighted byte range and its job, reused while nothing changed.
    job: Option<(std::ops::Range<usize>, egui::text::LayoutJob)>,
    palette: SyntaxPalette,
    parser: Parser,
    query: Arc<Query>,
}

/// Compiled highlight queries, one per language. Compiling Rust's query alone takes ~20 ms,
/// and it never changes, so every document of a language shares one.
fn highlight_query(language: &str) -> Option<Arc<Query>> {
    static QUERIES: OnceLock<Mutex<HashMap<String, Arc<Query>>>> = OnceLock::new();
    let queries = QUERIES.get_or_init(Default::default);
    if let Some(query) = queries.lock().ok()?.get(language) {
        return Some(Arc::clone(query));
    }
    // Compile outside the lock; a rare duplicate compile beats blocking other languages.
    let (ts_language, query_source) = language_config(language)?;
    let query = Arc::new(Query::new(&ts_language, &query_source).ok()?);
    queries
        .lock()
        .ok()?
        .entry(language.to_owned())
        .or_insert(query)
        .clone()
        .into()
}

/// Compile the queries for common languages and load the fallback syntax set off the UI
/// thread, so opening the first file of each kind does not stall a frame.
pub fn prewarm_editor_highlighting() {
    std::thread::spawn(|| {
        for language in ["rs", "toml", "json", "ts", "js", "py"] {
            let _ = highlight_query(language);
        }
        super::syntax::prewarm_syntax_set();
    });
}

#[cfg(test)]
pub fn highlight_editor_code(
    state: &mut Option<EditorSyntaxState>,
    content: &str,
    language: &str,
    font_id: FontId,
) -> Option<egui::text::LayoutJob> {
    highlight_editor_code_with_revision(state, content, language, font_id, None, None)
}

/// Syntax-colored layout of `content[byte_range]` (the whole text when `None`). The returned
/// job holds only that slice of text, with section ranges relative to its start.
///
/// The parse tree is kept per document and edited incrementally; only the requested range is
/// queried and colored, so a keystroke in a large file costs about one screen of work instead
/// of re-coloring the whole document.
pub fn highlight_editor_code_with_revision(
    state: &mut Option<EditorSyntaxState>,
    content: &str,
    language: &str,
    font_id: FontId,
    content_revision: Option<u64>,
    byte_range: Option<std::ops::Range<usize>>,
) -> Option<egui::text::LayoutJob> {
    let (ts_language, _) = language_config(language)?;
    let palette = active_palette().syntax;
    let range = byte_range.unwrap_or(0..content.len());
    let range = range.start.min(content.len())..range.end.min(content.len());
    let range = floor_char_boundary(content, range.start)..floor_char_boundary(content, range.end);

    let same_language = state
        .as_ref()
        .is_some_and(|current| current.language == language);
    let up_to_date = state.as_ref().is_some_and(|current| {
        same_language
            && content_revision.map_or_else(
                || current.content == content,
                |revision| current.content_revision == Some(revision),
            )
    });

    if !up_to_date {
        if let Some(current) = state.as_mut().filter(|_| same_language) {
            let edit = input_edit(&current.content, content);
            current.tree.edit(&edit);
            let tree = current.parser.parse(content, Some(&current.tree))?;
            current.content = content.to_owned();
            current.content_revision = content_revision;
            current.tree = tree;
            current.job = None;
        } else {
            let mut parser = Parser::new();
            parser.set_language(&ts_language).ok()?;
            let query = highlight_query(language)?;
            let tree = parser.parse(content, None)?;
            *state = Some(EditorSyntaxState {
                language: language.to_owned(),
                content: content.to_owned(),
                content_revision,
                tree,
                job: None,
                palette,
                parser,
                query,
            });
        }
    }

    let current = state.as_mut()?;
    if current.palette == palette
        && let Some((cached_range, job)) = &current.job
        && *cached_range == range
    {
        return Some(job.clone());
    }
    let job = layout_job(
        content,
        range.clone(),
        &current.tree,
        &current.query,
        palette,
        font_id,
    );
    current.palette = palette;
    current.job = Some((range, job.clone()));
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
    range: std::ops::Range<usize>,
    tree: &Tree,
    query: &Query,
    palette: SyntaxPalette,
    font_id: FontId,
) -> egui::text::LayoutJob {
    let text = &content[range.clone()];
    let base = range.start;
    // Color per capture, resolved once instead of substring-matching every capture's name.
    let capture_colors: Vec<egui::Color32> = query
        .capture_names()
        .iter()
        .map(|name| capture_color(name, palette))
        .collect();
    let mut colors = vec![palette.foreground; text.len()];
    let mut priorities = vec![0u32; text.len()];
    let mut cursor = QueryCursor::new();
    cursor.set_byte_range(range.clone());
    let mut captures = cursor.captures(query, tree.root_node(), content.as_bytes());
    while let Some((matched, capture_index)) = captures.next() {
        let capture = matched.captures[*capture_index];
        let color = capture_colors[capture.index as usize];
        let node = capture.node.byte_range();
        // Prefer the more specific (shorter) capture when query patterns overlap. This keeps
        // nested strings/escapes, fields, and function names from being flattened by a later
        // broad parent capture.
        let priority = u32::MAX.saturating_sub(node.len().min(u32::MAX as usize) as u32);
        let start = node.start.max(range.start) - base;
        let end = node.end.min(range.end).saturating_sub(base);
        for index in start..end.max(start) {
            if priority >= priorities[index] {
                priorities[index] = priority;
                colors[index] = color;
            }
        }
    }

    let mut job = egui::text::LayoutJob {
        text: text.to_owned(),
        ..Default::default()
    };
    // Whitespace is invisible, so it joins whichever run it touches. Otherwise every space
    // between two tokens of the same color splits the run, and text layout cost grows with
    // the number of sections.
    let bytes = text.as_bytes();
    let mut start = 0;
    while start < text.len() {
        let color = bytes[start..]
            .iter()
            .position(|b| !b.is_ascii_whitespace())
            .map_or(palette.foreground, |offset| colors[start + offset]);
        let mut end = start + 1;
        while end < text.len() && (colors[end] == color || bytes[end].is_ascii_whitespace()) {
            end += 1;
        }
        while end < text.len() && !text.is_char_boundary(end) {
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
    // Common prefix and suffix by bytes (memcmp speed on large buffers), then pulled back to
    // char boundaries so the edit never splits a UTF-8 sequence.
    let (a, b) = (old.as_bytes(), new.as_bytes());
    let mut start = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    while start > 0 && (!old.is_char_boundary(start) || !new.is_char_boundary(start)) {
        start -= 1;
    }
    let max_suffix = (a.len() - start).min(b.len() - start);
    let mut suffix = a[a.len() - max_suffix..]
        .iter()
        .rev()
        .zip(b[b.len() - max_suffix..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    while suffix > 0
        && (!old.is_char_boundary(old.len() - suffix) || !new.is_char_boundary(new.len() - suffix))
    {
        suffix -= 1;
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
    let row = memchr::memchr_iter(b'\n', prefix.as_bytes()).count();
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
    fn range_highlight_matches_the_full_job() {
        let source = "fn one() { let a = \"x\"; }\nfn two() { let b = 2; }\nfn three() {}\n";
        let font = FontId::monospace(12.0);
        let mut full_state = None;
        let full = highlight_editor_code(&mut full_state, source, "rs", font.clone()).unwrap();
        let line_two = source.find("fn two").unwrap()..source.find("fn three").unwrap();
        let mut state = None;
        let part = highlight_editor_code_with_revision(
            &mut state,
            source,
            "rs",
            font,
            Some(1),
            Some(line_two.clone()),
        )
        .unwrap();
        assert_eq!(part.text, &source[line_two.clone()]);
        // Same color at every byte as the whole-document job.
        let color_at = |job: &egui::text::LayoutJob, byte: usize| {
            job.sections
                .iter()
                .find(|s| s.byte_range.start.0 <= byte && byte < s.byte_range.end.0)
                .map(|s| s.format.color)
        };
        for byte in 0..part.text.len() {
            if part.text.as_bytes()[byte].is_ascii_whitespace() {
                continue;
            }
            assert_eq!(
                color_at(&part, byte),
                color_at(&full, line_two.start + byte)
            );
        }
    }

    #[test]
    fn input_edit_finds_the_changed_span() {
        let edit = input_edit("let a = 1;\nlet b = 2;", "let a = 1;\nlet bb = 2;");
        assert_eq!(edit.start_byte, 16);
        assert_eq!(edit.old_end_byte, 16);
        assert_eq!(edit.new_end_byte, 17);
        assert_eq!(edit.start_position, Point::new(1, 5));
        let unicode = input_edit("a↑b", "a↓b");
        assert_eq!((unicode.start_byte, unicode.old_end_byte), (1, 4));
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
