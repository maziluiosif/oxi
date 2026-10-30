//! Syntax highlighting driven by the active Oxi theme.
//!
//! Syntect remains responsible for parsing language scopes; this module supplies the colors so
//! editor, minimap, and Markdown code all change together with the application theme.

use std::str::FromStr;
use std::sync::{Mutex, OnceLock};

use eframe::egui::{self, FontId};
use syntect::easy::HighlightLines;
use syntect::highlighting::{Color, FontStyle, ScopeSelectors, StyleModifier, Theme, ThemeItem};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

use super::{SyntaxPalette, active_palette};

const HIGHLIGHT_CACHE_LIMIT: usize = 32;

static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();

/// Load syntect's bundled grammars (~60 ms) ahead of the first Markdown or other
/// non-tree-sitter file.
pub(super) fn prewarm_syntax_set() {
    SYNTAX_SET.get_or_init(SyntaxSet::load_defaults_newlines);
}
static HIGHLIGHT_CACHE: OnceLock<Mutex<Vec<(HighlightKey, egui::text::LayoutJob)>>> =
    OnceLock::new();

#[derive(Clone, Hash, PartialEq)]
struct HighlightKey {
    content: String,
    language: String,
    font_id: FontId,
    palette: SyntaxPalette,
}

fn color(c: egui::Color32) -> Color {
    Color {
        r: c.r(),
        g: c.g(),
        b: c.b(),
        a: 255,
    }
}

fn item(scope: &str, foreground: egui::Color32, font_style: Option<FontStyle>) -> ThemeItem {
    ThemeItem {
        scope: ScopeSelectors::from_str(scope).expect("valid built-in syntax scope"),
        style: StyleModifier {
            foreground: Some(color(foreground)),
            font_style,
            ..Default::default()
        },
    }
}

fn syntax_theme(p: SyntaxPalette) -> Theme {
    // More specific selectors follow broad ones; Syntect combines matching rules by specificity.
    let scopes = vec![
        item("variable", p.variable, None),
        item("keyword", p.keyword, Some(FontStyle::BOLD)),
        item("storage", p.keyword, None),
        item("comment", p.comment, Some(FontStyle::ITALIC)),
        item("string", p.string, None),
        item("string.regexp", p.regexp, None),
        item("constant.numeric", p.number, None),
        item("constant", p.constant, None),
        item("entity.name.function, support.function", p.function, None),
        item(
            "entity.name.type, entity.name.class, support.type, support.class",
            p.type_name,
            None,
        ),
        item("keyword.operator", p.operator, None),
        item("entity.name.tag", p.tag, None),
        item("entity.other.attribute-name", p.attribute, None),
        item("variable.parameter", p.variable, Some(FontStyle::ITALIC)),
    ];
    Theme {
        name: Some("Oxi active theme".into()),
        settings: syntect::highlighting::ThemeSettings {
            foreground: Some(color(p.foreground)),
            ..Default::default()
        },
        scopes,
        ..Default::default()
    }
}

fn normalized_language(language: &str) -> &str {
    match language.trim().to_ascii_lowercase().as_str() {
        "js" | "jsx" => "js",
        "ts" | "tsx" => "ts",
        "py" => "py",
        "rb" => "rb",
        "rs" => "rs",
        "sh" | "bash" | "zsh" => "sh",
        "yml" => "yml",
        "md" | "markdown" => "md",
        _ => language,
    }
}

/// Highlight source using Syntect scopes and the currently active Oxi syntax palette.
/// Results are cached because editor layout requests this on every frame.
pub fn highlight_code(content: &str, language: &str, font_id: FontId) -> egui::text::LayoutJob {
    let palette = active_palette().syntax;
    let key = HighlightKey {
        content: content.to_owned(),
        language: normalized_language(language).to_owned(),
        font_id: font_id.clone(),
        palette,
    };
    if let Some(job) = cached_highlight(&key) {
        return job;
    }
    highlight_and_cache(key)
}

/// Like [`highlight_code`], but never blocks the UI: on a cache miss the text is
/// highlighted on a worker thread and `ctx` repaints when it is ready. Returns `None`
/// meanwhile, so callers can show plain text. Syntect's regex grammars (Markdown in
/// particular) can take 100 ms+ for a few hundred lines.
pub fn highlight_code_async(
    content: &str,
    language: &str,
    font_id: FontId,
    ctx: &egui::Context,
) -> Option<egui::text::LayoutJob> {
    static PENDING: OnceLock<Mutex<std::collections::HashSet<u64>>> = OnceLock::new();
    let key = HighlightKey {
        content: content.to_owned(),
        language: normalized_language(language).to_owned(),
        font_id,
        palette: active_palette().syntax,
    };
    if let Some(job) = cached_highlight(&key) {
        return Some(job);
    }
    let id = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        key.hash(&mut hasher);
        hasher.finish()
    };
    let pending = PENDING.get_or_init(Default::default);
    if pending.lock().is_ok_and(|mut set| set.insert(id)) {
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            highlight_and_cache(key);
            if let Ok(mut set) = pending.lock() {
                set.remove(&id);
            }
            ctx.request_repaint();
        });
    }
    None
}

/// The newest cached highlight of the same language, font and palette, recolored onto
/// `content` by keeping the colors of the unchanged prefix and suffix; the edited middle takes
/// the color just before it. While [`highlight_code_async`] works on a keystroke, this keeps
/// the editor from flashing plain text.
pub fn stale_highlight(
    content: &str,
    language: &str,
    font_id: &FontId,
) -> Option<egui::text::LayoutJob> {
    let language = normalized_language(language);
    let palette = active_palette().syntax;
    let cache = HIGHLIGHT_CACHE.get()?;
    let entries = cache.lock().ok()?;
    let (_, old) = entries.iter().rev().find(|(key, _)| {
        key.language == language && key.font_id == *font_id && key.palette == palette
    })?;
    if old.sections.is_empty() {
        return None;
    }
    Some(remap_job(old, content))
}

fn remap_job(old: &egui::text::LayoutJob, text: &str) -> egui::text::LayoutJob {
    use egui::text::ByteIndex;
    let before = old.text.as_bytes();
    let after = text.as_bytes();
    let mut prefix = before.iter().zip(after).take_while(|(a, b)| a == b).count();
    while !text.is_char_boundary(prefix) {
        prefix -= 1;
    }
    let max_suffix = before.len().min(after.len()) - prefix;
    let mut suffix = before
        .iter()
        .rev()
        .zip(after.iter().rev())
        .take(max_suffix)
        .take_while(|(a, b)| a == b)
        .count();
    while !text.is_char_boundary(after.len() - suffix) {
        suffix -= 1;
    }
    let old_tail = before.len() - suffix;
    let new_tail = after.len() - suffix;

    let mut job = egui::text::LayoutJob {
        text: text.to_owned(),
        sections: Vec::with_capacity(old.sections.len() + 1),
        wrap: old.wrap.clone(),
        ..Default::default()
    };
    let mut push = |range: std::ops::Range<usize>, format: &egui::text::TextFormat| {
        if !range.is_empty() {
            job.sections.push(egui::text::LayoutSection {
                leading_space: 0.0,
                byte_range: ByteIndex(range.start)..ByteIndex(range.end),
                format: format.clone(),
            });
        }
    };
    let mut middle_format = None;
    for section in &old.sections {
        let (start, end) = (section.byte_range.start.0, section.byte_range.end.0);
        if start < prefix {
            push(start..end.min(prefix), &section.format);
            middle_format = Some(&section.format);
        }
    }
    let middle_format = middle_format.or_else(|| old.sections.first().map(|s| &s.format));
    if let Some(format) = middle_format {
        push(prefix..new_tail, format);
    }
    for section in &old.sections {
        let (start, end) = (section.byte_range.start.0, section.byte_range.end.0);
        if end > old_tail {
            let start = start.max(old_tail);
            push(
                start - old_tail + new_tail..end - old_tail + new_tail,
                &section.format,
            );
        }
    }
    job
}

fn cached_highlight(key: &HighlightKey) -> Option<egui::text::LayoutJob> {
    let cache = HIGHLIGHT_CACHE.get_or_init(|| Mutex::new(Vec::new()));
    let mut entries = cache.lock().ok()?;
    let index = entries.iter().position(|(cached, _)| cached == key)?;
    // Move to the back: the cache evicts from the front.
    let entry = entries.remove(index);
    let job = entry.1.clone();
    entries.push(entry);
    Some(job)
}

fn highlight_and_cache(key: HighlightKey) -> egui::text::LayoutJob {
    let syntax_set = SYNTAX_SET.get_or_init(SyntaxSet::load_defaults_newlines);
    let language = key.language.as_str();
    let job = if let Some(syntax) = syntax_set
        .find_syntax_by_extension(language)
        .or_else(|| syntax_set.find_syntax_by_token(language))
        .or_else(|| syntax_set.find_syntax_by_name(language))
    {
        highlight_uncached(
            &key.content,
            syntax,
            syntax_set,
            key.palette,
            key.font_id.clone(),
        )
    } else {
        egui::text::LayoutJob::simple(
            key.content.clone(),
            key.font_id.clone(),
            key.palette.foreground,
            f32::INFINITY,
        )
    };

    let cache = HIGHLIGHT_CACHE.get_or_init(|| Mutex::new(Vec::new()));
    if let Ok(mut entries) = cache.lock() {
        if entries.len() >= HIGHLIGHT_CACHE_LIMIT {
            entries.remove(0);
        }
        entries.push((key, job.clone()));
    }
    job
}

fn highlight_uncached(
    content: &str,
    syntax: &syntect::parsing::SyntaxReference,
    syntax_set: &SyntaxSet,
    palette: SyntaxPalette,
    font_id: FontId,
) -> egui::text::LayoutJob {
    let theme = syntax_theme(palette);
    let mut highlighter = HighlightLines::new(syntax, &theme);
    let mut job = egui::text::LayoutJob {
        text: content.into(),
        ..Default::default()
    };
    let whole_start = content.as_ptr() as usize;

    for line in LinesWithEndings::from(content) {
        let Ok(regions) = highlighter.highlight_line(line, syntax_set) else {
            continue;
        };
        for (style, region) in regions {
            let start = region.as_ptr() as usize - whole_start;
            let foreground = style.foreground;
            let text_color = egui::Color32::from_rgb(foreground.r, foreground.g, foreground.b);
            job.sections.push(egui::text::LayoutSection {
                leading_space: 0.0,
                byte_range: egui::text::ByteIndex(start)
                    ..egui::text::ByteIndex(start + region.len()),
                format: egui::text::TextFormat {
                    font_id: font_id.clone(),
                    color: text_color,
                    italics: style.font_style.contains(FontStyle::ITALIC),
                    underline: if style.font_style.contains(FontStyle::UNDERLINE) {
                        egui::Stroke::new(1.0, text_color)
                    } else {
                        egui::Stroke::NONE
                    },
                    ..Default::default()
                },
            });
        }
    }
    job
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_highlighting_uses_multiple_semantic_colors() {
        let job = highlight_code(
            "fn main() { let value = \"hello\"; }",
            "rs",
            FontId::monospace(12.0),
        );
        let mut colors = job
            .sections
            .iter()
            .map(|s| s.format.color)
            .collect::<Vec<_>>();
        colors.sort_by_key(|c| c.to_array());
        colors.dedup();
        assert!(colors.len() >= 3);
    }

    #[test]
    fn remapped_job_keeps_colors_around_an_edit() {
        let old = highlight_code(
            "# Title\n\nsome *text* here\n",
            "md",
            FontId::monospace(12.0),
        );
        for text in [
            "# Title\n\nsome *text* hereș\n",
            "# Title\n\nsome *tex here\n",
            "x# Title\n\nsome *text* here\n",
            "",
            "# Title\n\nsome *text* here\n\nmore",
        ] {
            let job = remap_job(&old, text);
            assert_eq!(job.text, text);
            let mut at = 0;
            for section in &job.sections {
                assert_eq!(section.byte_range.start.0, at);
                at = section.byte_range.end.0;
                assert!(text.is_char_boundary(at));
            }
            assert_eq!(at, text.len());
        }
        let job = remap_job(&old, "# Title!\n\nsome *text* here\n");
        assert_eq!(job.sections[0].format.color, old.sections[0].format.color);
        let last = |job: &egui::text::LayoutJob| job.sections.last().unwrap().format.color;
        assert_eq!(last(&job), last(&old));
    }

    #[test]
    fn unknown_language_uses_theme_foreground() {
        let _guard = super::super::palette::PALETTE_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let job = highlight_code("hello", "definitely-unknown", FontId::monospace(12.0));
        assert_eq!(
            job.sections[0].format.color,
            active_palette().syntax.foreground
        );
    }
}
