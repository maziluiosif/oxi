//! Where a clicked link goes. Web links (`http`, `https`, `mailto`) open in the browser as
//! before. A link to a file in the workspace, as agents like to write them (`src/main.rs`,
//! `src/main.rs#L42`, `src/main.rs:42:7`, `file:///…/src/main.rs`), opens in the editor at that
//! line. Anything else (custom URL schemes, files outside the workspace) is dropped instead of
//! being handed to the operating system: chat text can come from a prompt-injected web page.

use std::path::{Path, PathBuf};

use eframe::egui;

use super::OxiApp;

impl OxiApp {
    /// Take this frame's link clicks away from eframe's default browser handling where they
    /// should go elsewhere. Call after everything that can emit a link has been drawn.
    pub(crate) fn route_opened_links(&mut self, ctx: &egui::Context) {
        let mut files = Vec::new();
        ctx.output_mut(|output| {
            output.commands.retain(|command| match command {
                egui::OutputCommand::OpenUrl(open) if !is_web_link(&open.url) => {
                    files.push(open.url.clone());
                    false
                }
                _ => true,
            });
        });
        let root = PathBuf::from(&self.active_workspace().root_path);
        for link in files {
            match workspace_file_link(&link, &root) {
                Some((path, line)) => {
                    self.open_file_at_line(path, line);
                }
                None => self.notify_composer(format!(
                    "Not opened: {} is not a web link or a file in this workspace",
                    crate::logging::excerpt(&link)
                )),
            }
        }
    }
}

fn is_web_link(link: &str) -> bool {
    let lower = link.trim_start().to_ascii_lowercase();
    ["http://", "https://", "mailto:"]
        .iter()
        .any(|scheme| lower.starts_with(scheme))
}

/// The workspace file `link` points at, with its 1-based line when the link names one.
fn workspace_file_link(link: &str, root: &Path) -> Option<(PathBuf, Option<usize>)> {
    let link = link.trim();
    let (body, fragment) = match link.split_once('#') {
        Some((body, fragment)) => (body, Some(fragment)),
        None => (link, None),
    };
    let mut line = fragment.and_then(fragment_line);
    let body = match split_line_suffix(body) {
        (body, Some(suffix_line)) => {
            line = line.or(Some(suffix_line));
            body
        }
        (body, None) => body,
    };
    if body.is_empty() {
        return None;
    }
    let path = if body.to_ascii_lowercase().starts_with("file://") {
        url::Url::parse(body).ok()?.to_file_path().ok()?
    } else if is_windows_drive_path(body) {
        PathBuf::from(body)
    } else if has_url_scheme(body) {
        return None;
    } else {
        // Resolve like a browser would, which also decodes `%20` and friends.
        let base = url::Url::from_directory_path(root).ok()?;
        base.join(body).ok()?.to_file_path().ok()?
    };
    let root = std::fs::canonicalize(root).ok()?;
    let path = std::fs::canonicalize(path).ok()?;
    (path.starts_with(&root) && path.is_file()).then_some((path, line))
}

/// `L42` / `L42-L50` (GitHub style) or a bare `42`.
fn fragment_line(fragment: &str) -> Option<usize> {
    let digits: String = fragment
        .trim_start_matches(['L', 'l'])
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok().filter(|line| *line > 0)
}

/// Split a trailing `:line` or `:line:column` off a path.
fn split_line_suffix(body: &str) -> (&str, Option<usize>) {
    let mut parts = body.rsplitn(3, ':');
    let last = parts.next().unwrap_or_default();
    let middle = parts.next();
    let rest = parts.next();
    let is_number = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    match (middle, rest) {
        // `path:line:column`
        (Some(line), Some(path)) if is_number(line) && is_number(last) && !path.is_empty() => {
            (path, line.parse().ok())
        }
        // `path:line`
        (Some(_), _) if is_number(last) => {
            let path = &body[..body.len() - last.len() - 1];
            if path.is_empty() || is_windows_drive_path(body) && path.len() == 1 {
                (body, None)
            } else {
                (path, last.parse().ok())
            }
        }
        _ => (body, None),
    }
}

fn is_windows_drive_path(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
}

/// `scheme:` at the start, per RFC 3986 (`vscode://`, `javascript:`, `ms-settings:`).
fn has_url_scheme(text: &str) -> bool {
    let Some((scheme, _)) = text.split_once(':') else {
        return false;
    };
    let mut chars = scheme.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> PathBuf {
        let root = crate::fsutil::unique_temp_path(&std::env::temp_dir(), "oxi-links", "d");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(root.join("src/with space.rs"), "\n").unwrap();
        root
    }

    fn resolve(link: &str, root: &Path) -> Option<(String, Option<usize>)> {
        let canonical_root = std::fs::canonicalize(root).unwrap();
        workspace_file_link(link, root).map(|(path, line)| {
            (
                path.strip_prefix(&canonical_root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/"),
                line,
            )
        })
    }

    #[test]
    fn web_links_keep_going_to_the_browser() {
        assert!(is_web_link("https://example.com"));
        assert!(is_web_link("HTTP://example.com"));
        assert!(is_web_link("mailto:a@b.c"));
        assert!(!is_web_link("src/main.rs"));
        assert!(!is_web_link("file:///etc/passwd"));
        assert!(!is_web_link("javascript:alert(1)"));
    }

    #[test]
    fn workspace_links_resolve_with_lines() {
        let root = workspace();
        let main = Some(("src/main.rs".to_string(), None));
        assert_eq!(resolve("src/main.rs", &root), main);
        assert_eq!(resolve("./src/main.rs", &root), main);
        assert_eq!(
            resolve("src/main.rs#L42", &root),
            Some(("src/main.rs".into(), Some(42)))
        );
        assert_eq!(
            resolve("src/main.rs#L3-L9", &root),
            Some(("src/main.rs".into(), Some(3)))
        );
        assert_eq!(
            resolve("src/main.rs:12", &root),
            Some(("src/main.rs".into(), Some(12)))
        );
        assert_eq!(
            resolve("src/main.rs:12:5", &root),
            Some(("src/main.rs".into(), Some(12)))
        );
        assert_eq!(
            resolve("src/with%20space.rs", &root),
            Some(("src/with space.rs".into(), None))
        );
        let absolute = url::Url::from_file_path(root.join("src/main.rs")).unwrap();
        assert_eq!(resolve(absolute.as_str(), &root), main);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn other_schemes_and_outside_files_are_refused() {
        let root = workspace();
        assert_eq!(resolve("vscode://file/x", &root), None);
        assert_eq!(resolve("javascript:alert(1)", &root), None);
        assert_eq!(resolve("ms-settings:privacy", &root), None);
        assert_eq!(resolve("../outside.rs", &root), None);
        assert_eq!(resolve("src/missing.rs", &root), None);
        assert_eq!(resolve("src", &root), None);
        assert_eq!(resolve("", &root), None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn line_suffixes_split_only_on_numbers() {
        assert_eq!(split_line_suffix("a.rs:10"), ("a.rs", Some(10)));
        assert_eq!(split_line_suffix("a.rs:10:2"), ("a.rs", Some(10)));
        assert_eq!(split_line_suffix("a.rs"), ("a.rs", None));
        assert_eq!(split_line_suffix("a:b.rs"), ("a:b.rs", None));
        assert_eq!(split_line_suffix("C:\\x\\a.rs:7"), ("C:\\x\\a.rs", Some(7)));
        assert_eq!(fragment_line("L0"), None);
        assert_eq!(fragment_line("l15"), Some(15));
    }
}
