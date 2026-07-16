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
    let language = normalized_language(language);
    let key = HighlightKey {
        content: content.to_owned(),
        language: language.to_owned(),
        font_id: font_id.clone(),
        palette,
    };
    let cache = HIGHLIGHT_CACHE.get_or_init(|| Mutex::new(Vec::new()));
    if let Ok(mut entries) = cache.lock()
        && let Some(index) = entries.iter().position(|(cached, _)| cached == &key)
    {
        let entry = entries.remove(index);
        let job = entry.1.clone();
        entries.push(entry);
        return job;
    }

    let syntax_set = SYNTAX_SET.get_or_init(SyntaxSet::load_defaults_newlines);
    let job = if let Some(syntax) = syntax_set
        .find_syntax_by_extension(language)
        .or_else(|| syntax_set.find_syntax_by_token(language))
        .or_else(|| syntax_set.find_syntax_by_name(language))
    {
        highlight_uncached(content, syntax, syntax_set, palette, font_id)
    } else {
        egui::text::LayoutJob::simple(content.into(), font_id, palette.foreground, f32::INFINITY)
    };

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
    fn unknown_language_uses_theme_foreground() {
        let job = highlight_code("hello", "definitely-unknown", FontId::monospace(12.0));
        assert_eq!(
            job.sections[0].format.color,
            active_palette().syntax.foreground
        );
    }
}
