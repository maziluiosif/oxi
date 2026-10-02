//! Pure helpers for explorer filtering, file presentation, and editor search.

use std::path::Path;

use eframe::egui;

use crate::theme::*;

const ALWAYS_SKIPPED_DIRS: &[&str] = &[".git"];

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

/// Byte offsets in `path` of the characters [`fuzzy_path_score`] matched, for highlighting.
/// A contiguous hit inside the file name wins over the greedy subsequence, mirroring the score.
pub(super) fn fuzzy_match_positions(path: &str, query: &str) -> Vec<usize> {
    if query.is_empty() {
        return Vec::new();
    }
    let path = path.to_ascii_lowercase();
    let query = query.to_ascii_lowercase();
    let filename_start = path.rfind('/').map_or(0, |index| index + 1);
    if let Some(index) = path[filename_start..].find(&query) {
        let start = filename_start + index;
        return path[start..start + query.len()]
            .char_indices()
            .map(|(i, _)| start + i)
            .collect();
    }
    let mut positions = Vec::new();
    let mut search_from = 0usize;
    for wanted in query.chars() {
        let Some(relative) = path[search_from..].find(wanted) else {
            return Vec::new();
        };
        positions.push(search_from + relative);
        search_from += relative + wanted.len_utf8();
    }
    positions
}

/// Search flags of the editor Find panel (Sublime's regex / case / whole-word toggles).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct FindOptions {
    pub regex: bool,
    pub case_sensitive: bool,
    pub whole_word: bool,
}

/// All matches of one query in one document revision.
#[derive(Default)]
pub(crate) struct FindResults {
    pub ranges: Vec<std::ops::Range<usize>>,
    /// Set when the query is not a valid regular expression.
    pub error: Option<String>,
    /// Compiled pattern, kept for `$1`-style replacement expansion in regex mode.
    pattern: Option<regex::Regex>,
    regex_mode: bool,
}

/// Highlighting every hit of a one-letter query in a huge file is useless and slow.
const MAX_FIND_MATCHES: usize = 50_000;

/// Find every non-empty, non-overlapping match of `query`. Literal queries are escaped and run
/// through the same engine, so case folding is Unicode-aware in every mode; `^`/`$` match at
/// line boundaries (CRLF included), like Sublime.
pub(crate) fn find_matches(content: &str, query: &str, options: FindOptions) -> FindResults {
    if query.is_empty() {
        return FindResults::default();
    }
    let source = if options.regex {
        std::borrow::Cow::Borrowed(query)
    } else {
        std::borrow::Cow::Owned(regex::escape(query))
    };
    let pattern = match regex::RegexBuilder::new(&source)
        .case_insensitive(!options.case_sensitive)
        .multi_line(true)
        .crlf(true)
        .build()
    {
        Ok(pattern) => pattern,
        Err(_) => {
            return FindResults {
                error: Some("Invalid pattern".to_owned()),
                ..Default::default()
            };
        }
    };
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let ranges = pattern
        .find_iter(content)
        .filter(|found| !found.is_empty())
        .filter(|found| {
            !options.whole_word
                || (!content[..found.start()]
                    .chars()
                    .next_back()
                    .is_some_and(is_word)
                    && !content[found.end()..].chars().next().is_some_and(is_word))
        })
        .map(|found| found.range())
        .take(MAX_FIND_MATCHES)
        .collect();
    FindResults {
        ranges,
        error: None,
        pattern: Some(pattern),
        regex_mode: options.regex,
    }
}

impl FindResults {
    /// Text that replaces the match at `range`: `$1`/`${name}` groups are expanded in regex
    /// mode, the replacement is inserted verbatim otherwise.
    pub(crate) fn replacement_for(
        &self,
        content: &str,
        range: &std::ops::Range<usize>,
        replacement: &str,
    ) -> String {
        if let (true, Some(pattern)) = (self.regex_mode, &self.pattern)
            && let Some(captures) = pattern.captures_at(content, range.start)
            && captures.get(0).is_some_and(|found| found.range() == *range)
        {
            let mut expanded = String::new();
            captures.expand(replacement, &mut expanded);
            return expanded;
        }
        replacement.to_owned()
    }
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

/// Hover text for a file: its path relative to the workspace (what the explorer and git panel
/// show), falling back to a `~`-abbreviated absolute path for files outside it.
pub(super) fn display_path(root: &Path, path: &Path) -> String {
    if let Ok(relative) = path.strip_prefix(root) {
        return relative.display().to_string();
    }
    // Documents may hold the canonical path (`/private/var/...` on macOS) of a symlinked root.
    if let Ok(canonical_root) = root.canonicalize()
        && let Ok(relative) = path.strip_prefix(&canonical_root)
    {
        return relative.display().to_string();
    }
    if let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from)
        && let Ok(relative) = path.strip_prefix(&home)
    {
        return format!("~/{}", relative.display());
    }
    path.display().to_string()
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

pub(crate) fn file_icon(path: &Path) -> (&'static str, egui::Color32) {
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

/// The workspace `.gitignore`, parsed once. Globs are compiled up front: matching used to
/// compile every pattern again for every path and every path suffix, which made walking a large
/// workspace (Cmd/Ctrl+P, the explorer, symbol indexing) take seconds.
#[derive(Default)]
pub(crate) struct GitIgnore {
    rules: Vec<IgnoreRule>,
}

struct IgnoreRule {
    pattern: String,
    glob: Option<glob::Pattern>,
}

impl GitIgnore {
    pub(super) fn load(root: &Path) -> Self {
        Self::parse(&std::fs::read_to_string(root.join(".gitignore")).unwrap_or_default())
    }

    pub(super) fn parse(text: &str) -> Self {
        let rules = text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#') && !line.starts_with('!'))
            .map(|line| {
                let pattern = line
                    .trim_start_matches('/')
                    .trim_end_matches('/')
                    .to_owned();
                IgnoreRule {
                    glob: glob::Pattern::new(&pattern).ok(),
                    pattern,
                }
            })
            .collect();
        Self { rules }
    }
}

pub(super) fn should_ignore(root: &Path, path: &Path, directory: bool, ignore: &GitIgnore) -> bool {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    (directory && ALWAYS_SKIPPED_DIRS.contains(&name))
        || is_gitignored(root, path, directory, ignore)
}

pub(super) fn is_gitignored(root: &Path, path: &Path, directory: bool, ignore: &GitIgnore) -> bool {
    if ignore.rules.is_empty() {
        return false;
    }
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
    // `a/b/c`, `b/c`, `c`: a pattern may match the path from any directory down.
    let suffixes = || {
        std::iter::once(relative.as_str()).chain(
            relative
                .match_indices('/')
                .map(|(index, _)| &relative[index + 1..]),
        )
    };
    ignore.rules.iter().any(|rule| {
        let pattern = &rule.pattern;
        let direct = if pattern.contains('*') {
            rule.glob
                .as_ref()
                .is_some_and(|glob| glob.matches(&relative) || glob.matches(name))
        } else {
            relative == *pattern
                || relative
                    .strip_prefix(pattern.as_str())
                    .is_some_and(|rest| rest.starts_with('/'))
                || name == pattern
        };
        direct
            || rule
                .glob
                .as_ref()
                .is_some_and(|glob| suffixes().any(|suffix| glob.matches(suffix)))
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
        let ranges = |content: &str, query: &str, case_sensitive: bool| {
            let options = FindOptions {
                case_sensitive,
                ..Default::default()
            };
            find_matches(content, query, options).ranges
        };
        assert_eq!(ranges("one two one", "one", true), vec![0..3, 8..11]);
        assert_eq!(ranges("One ONE", "one", false), vec![0..3, 4..7]);
        assert!(ranges("One ONE", "one", true).is_empty());
        assert!(ranges("anything", "", false).is_empty());
        // Literal mode treats regex syntax as plain text.
        assert_eq!(ranges("a.b axb", "a.b", true), vec![0..3]);
    }

    #[test]
    fn search_whole_word_and_regex_modes() {
        let whole_word = FindOptions {
            whole_word: true,
            ..Default::default()
        };
        assert_eq!(
            find_matches("foo foobar _foo foo", "foo", whole_word).ranges,
            vec![0..3, 16..19]
        );
        let regex = FindOptions {
            regex: true,
            case_sensitive: true,
            ..Default::default()
        };
        let results = find_matches("let a = 1;\r\nlet bc = 22;", r"^let (\w+)", regex);
        assert_eq!(results.ranges, vec![0..5, 12..18]);
        let content = "let a = 1;\r\nlet bc = 22;";
        assert_eq!(
            results.replacement_for(content, &results.ranges[1], "const $1"),
            "const bc"
        );
        // Empty matches (`x*`) are skipped rather than highlighted everywhere.
        assert_eq!(find_matches("axxb", "x*", regex).ranges, vec![1..3]);
        let invalid = find_matches("text", "(", regex);
        assert!(invalid.ranges.is_empty() && invalid.error.is_some());
    }

    #[test]
    fn gitignore_patterns_hide_matching_paths_but_not_gitignore_itself() {
        let root = Path::new("/workspace");
        let patterns = GitIgnore::parse("target\n*.log\n/build/*.js\n# comment\n");
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
        assert!(should_ignore(
            root,
            Path::new("/workspace/crates/core/target"),
            true,
            &patterns
        ));
        assert!(!should_ignore(
            root,
            Path::new("/workspace/src/targets.rs"),
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
