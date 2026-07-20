//! Pure helpers for explorer filtering, file presentation, and editor search.

use std::path::Path;

use eframe::egui;

use crate::theme::*;

use super::ALWAYS_SKIPPED_DIRS;

/// Sublime-style fuzzy score. Filename hits, consecutive characters, word/path boundaries and
/// earlier matches rank higher; long gaps and deep paths rank lower.
pub(super) fn fuzzy_path_score(path: &str, query: &str) -> Option<i64> {
    if query.is_empty() {
        let depth = path.bytes().filter(|byte| *byte == b'/').count() as i64;
        return Some(-depth * 20 - path.len() as i64);
    }
    let path = path.to_ascii_lowercase();
    let query = query.to_ascii_lowercase();
    let filename_start = path.rfind('/').map_or(0, |index| index + 1);
    let mut score = 0i64;
    let mut search_from = 0usize;
    let mut previous = None;
    for wanted in query.chars() {
        let relative = path[search_from..].find(wanted)?;
        let index = search_from + relative;
        let boundary =
            index == 0 || matches!(path.as_bytes()[index - 1], b'/' | b'_' | b'-' | b'.' | b' ');
        score += if index >= filename_start { 90 } else { 35 };
        if boundary {
            score += 85;
        }
        if previous.is_some_and(|previous| previous + 1 == index) {
            score += 120;
        }
        score -= relative as i64 * 4;
        previous = Some(index);
        search_from = index + wanted.len_utf8();
    }
    if let Some(index) = path[filename_start..].find(&query) {
        score += 900 - index as i64 * 10;
    } else if let Some(index) = path.find(&query) {
        score += 350 - index as i64 * 3;
    }
    score -= path.len() as i64;
    score -= path.bytes().filter(|byte| *byte == b'/').count() as i64 * 12;
    Some(score)
}

pub(crate) fn find_match_ranges(
    content: &str,
    query: &str,
    case_sensitive: bool,
) -> Vec<std::ops::Range<usize>> {
    if query.is_empty() {
        return Vec::new();
    }
    if case_sensitive {
        return content
            .match_indices(query)
            .map(|(start, matched)| start..start + matched.len())
            .collect();
    }

    // Unicode lowercasing can expand a character and change byte lengths. Keep a source span for
    // every folded character so matches can be mapped back to the original string.
    let mut folded_content = String::new();
    let mut source_spans = Vec::new();
    for (start, character) in content.char_indices() {
        let end = start + character.len_utf8();
        for folded in character.to_lowercase() {
            folded_content.push(folded);
            source_spans.push(start..end);
        }
    }
    let folded_query = query.to_lowercase();
    folded_content
        .match_indices(&folded_query)
        .filter_map(|(start, matched)| {
            let start_char = folded_content[..start].chars().count();
            let end_char = start_char + matched.chars().count();
            Some(source_spans.get(start_char)?.start..source_spans.get(end_char - 1)?.end)
        })
        .collect()
}

pub(super) fn apply_search_highlights(
    job: &mut egui::text::LayoutJob,
    matches: &[std::ops::Range<usize>],
    active: Option<usize>,
) {
    if matches.is_empty() {
        return;
    }
    let passive = crate::theme::blend_color(c_bg_main(), c_warning_fg(), 0.38);
    let active_color = crate::theme::blend_color(c_bg_main(), c_accent(), 0.72);
    let mut sections = Vec::with_capacity(job.sections.len() + matches.len() * 2);
    for section in &job.sections {
        let section_start = section.byte_range.start.0;
        let section_end = section.byte_range.end.0;
        let mut cursor = section_start;
        for (match_index, range) in matches.iter().enumerate() {
            let start = range.start.max(section_start);
            let end = range.end.min(section_end);
            if start >= end {
                continue;
            }
            if cursor < start {
                let mut untouched = section.clone();
                untouched.byte_range = egui::text::ByteIndex(cursor)..egui::text::ByteIndex(start);
                sections.push(untouched);
            }
            let mut highlighted = section.clone();
            highlighted.byte_range = egui::text::ByteIndex(start)..egui::text::ByteIndex(end);
            highlighted.format.background = if active == Some(match_index) {
                active_color
            } else {
                passive
            };
            sections.push(highlighted);
            cursor = end;
        }
        if cursor < section_end {
            let mut tail = section.clone();
            tail.byte_range = egui::text::ByteIndex(cursor)..egui::text::ByteIndex(section_end);
            sections.push(tail);
        }
    }
    job.sections = sections;
}

pub(super) fn apply_definition_underline(
    job: &mut egui::text::LayoutJob,
    range: &std::ops::Range<usize>,
) {
    let mut sections = Vec::with_capacity(job.sections.len() + 2);
    for section in &job.sections {
        let section_start = section.byte_range.start.0;
        let section_end = section.byte_range.end.0;
        let start = range.start.max(section_start);
        let end = range.end.min(section_end);
        if start >= end {
            sections.push(section.clone());
            continue;
        }
        if section_start < start {
            let mut before = section.clone();
            before.byte_range = egui::text::ByteIndex(section_start)..egui::text::ByteIndex(start);
            sections.push(before);
        }
        let mut underlined = section.clone();
        underlined.byte_range = egui::text::ByteIndex(start)..egui::text::ByteIndex(end);
        underlined.format.underline = egui::Stroke::new(1.0, c_accent());
        sections.push(underlined);
        if end < section_end {
            let mut after = section.clone();
            after.byte_range = egui::text::ByteIndex(end)..egui::text::ByteIndex(section_end);
            sections.push(after);
        }
    }
    job.sections = sections;
}

pub(super) fn language_for_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "rs" => "rs",
        "py" => "py",
        "js" => "js",
        "jsx" => "jsx",
        "ts" => "ts",
        "tsx" => "tsx",
        "json" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "html" => "html",
        "css" | "scss" => "css",
        "md" => "md",
        "sh" | "bash" | "zsh" => "sh",
        "c" | "h" => "c",
        "cpp" | "cc" | "hpp" => "cpp",
        "go" => "go",
        "java" => "java",
        _ => "txt",
    }
}

pub(super) fn file_icon(path: &Path) -> (&'static str, egui::Color32) {
    let color = match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "rs" | "js" | "jsx" => crate::theme::blend_color(c_text_muted(), c_warning_fg(), 0.72),
        "ts" | "tsx" | "md" | "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" => {
            crate::theme::blend_color(c_text_muted(), c_accent(), 0.72)
        }
        "json" | "toml" | "yaml" | "yml" => {
            crate::theme::blend_color(c_text_muted(), c_warning_fg(), 0.72)
        }
        "html" | "css" | "scss" => crate::theme::blend_color(c_text_muted(), c_danger(), 0.68),
        _ => c_text_muted(),
    };
    (ICON_FILE, color)
}

pub(super) fn load_gitignore_patterns(root: &Path) -> Vec<String> {
    std::fs::read_to_string(root.join(".gitignore"))
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#') && !line.starts_with('!'))
        .map(|line| {
            line.trim_start_matches('/')
                .trim_end_matches('/')
                .to_owned()
        })
        .collect()
}

pub(super) fn should_ignore(
    root: &Path,
    path: &Path,
    directory: bool,
    patterns: &[String],
) -> bool {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    (directory && ALWAYS_SKIPPED_DIRS.contains(&name))
        || is_gitignored(root, path, directory, patterns)
}

pub(super) fn is_gitignored(
    root: &Path,
    path: &Path,
    directory: bool,
    patterns: &[String],
) -> bool {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if name == ".gitignore" {
        return false;
    }
    if directory && ALWAYS_SKIPPED_DIRS.contains(&name) {
        return false;
    }
    let relative = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    patterns.iter().any(|pattern| {
        let direct = if pattern.contains('*') {
            glob::Pattern::new(pattern)
                .is_ok_and(|glob| glob.matches(&relative) || glob.matches(name))
        } else {
            relative == *pattern || relative.starts_with(&format!("{pattern}/")) || name == pattern
        };
        if direct {
            return true;
        }
        relative.split('/').enumerate().any(|(index, _)| {
            let suffix = relative
                .split('/')
                .skip(index)
                .collect::<Vec<_>>()
                .join("/");
            glob::Pattern::new(pattern).is_ok_and(|glob| glob.matches(&suffix))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_is_selected_from_extension() {
        assert_eq!(language_for_path(Path::new("src/main.rs")), "rs");
        assert_eq!(language_for_path(Path::new("web/app.tsx")), "tsx");
        assert_eq!(language_for_path(Path::new("README")), "txt");
    }

    #[test]
    fn fuzzy_search_ranks_filename_and_consecutive_matches_higher() {
        let direct =
            fuzzy_path_score("src/file_explorer.rs", "file").expect("the direct path should match");
        let scattered = fuzzy_path_score("src/features/image_loader.rs", "file")
            .expect("the scattered path should match");
        assert!(direct > scattered);
        assert!(fuzzy_path_score("src/file_explorer.rs", "fexp").is_some());
        assert!(fuzzy_path_score("src/file_explorer.rs", "xyz").is_none());
    }

    #[test]
    fn search_ranges_use_non_overlapping_matches() {
        assert_eq!(
            find_match_ranges("one two one", "one", true),
            vec![0..3, 8..11]
        );
        assert_eq!(find_match_ranges("One ONE", "one", false), vec![0..3, 4..7]);
        assert!(find_match_ranges("One ONE", "one", true).is_empty());
        assert!(find_match_ranges("anything", "", false).is_empty());
    }

    #[test]
    fn gitignore_patterns_hide_matching_paths_but_not_gitignore_itself() {
        let root = Path::new("/workspace");
        let patterns = vec!["target".into(), "*.log".into(), "build/*.js".into()];
        assert!(should_ignore(
            root,
            Path::new("/workspace/target"),
            true,
            &patterns
        ));
        assert!(should_ignore(
            root,
            Path::new("/workspace/debug.log"),
            false,
            &patterns
        ));
        assert!(should_ignore(
            root,
            Path::new("/workspace/build/app.js"),
            false,
            &patterns
        ));
        assert!(!should_ignore(
            root,
            Path::new("/workspace/.gitignore"),
            false,
            &patterns
        ));
    }
}
