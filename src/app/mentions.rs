//! Expand `@path` mentions in composer text into inline file/folder context.

use std::fs;
use std::path::{Path, PathBuf};

use crate::agent::tools::resolve_under_cwd;

/// Max bytes of a single mentioned file to inject.
const MENTION_FILE_MAX_BYTES: usize = 32 * 1024;
/// Max files listed when mentioning a directory.
const MENTION_DIR_MAX_ENTRIES: usize = 80;

/// Expand `@path` tokens (paths relative to `cwd`) into the message text with
/// attached file/folder contents. Unresolved mentions are left as-is.
pub fn expand_at_mentions(text: &str, cwd: &Path) -> String {
    if !text.contains('@') {
        return text.to_string();
    }

    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let mut injected = Vec::new();

    while let Some(at) = rest.find('@') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let end = after
            .find(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | ')' | ']' | '"' | '\''))
            .unwrap_or(after.len());
        if end == 0 {
            out.push('@');
            rest = after;
            continue;
        }
        let token = &after[..end];
        // Skip email-like tokens (user@host) and bare @mentions without a path separator
        // when they look like handles — still allow `@src/foo.rs` and `@README.md`.
        if token.contains('@')
            || (!token.contains('/') && !token.contains('.') && !token.contains('\\'))
        {
            // Allow simple filenames like `@Cargo.toml` (has a dot) and `@src` (dir).
            // Bare words without `.` or `/` are treated as handles unless they resolve.
            if resolve_under_cwd(cwd, token).is_err() {
                out.push('@');
                out.push_str(token);
                rest = &after[end..];
                continue;
            }
        }

        match resolve_and_read(cwd, token) {
            Ok(block) => {
                out.push('@');
                out.push_str(token);
                injected.push(block);
            }
            Err(_) => {
                out.push('@');
                out.push_str(token);
            }
        }
        rest = &after[end..];
    }
    out.push_str(rest);

    if injected.is_empty() {
        return out;
    }

    out.push_str("\n\n---\nAttached context from @mentions:\n");
    for block in injected {
        out.push_str(&block);
        out.push('\n');
    }
    out
}

fn resolve_and_read(cwd: &Path, token: &str) -> Result<String, String> {
    let abs = resolve_under_cwd(cwd, token)?;
    let meta = fs::metadata(&abs).map_err(|e| e.to_string())?;
    if meta.is_dir() {
        return Ok(format_dir_listing(token, &abs));
    }
    if !meta.is_file() {
        return Err("not a file".into());
    }
    if meta.len() as usize > MENTION_FILE_MAX_BYTES {
        return Ok(format!(
            "### @{token}\n(file too large: {} bytes; use the read tool)\n",
            meta.len()
        ));
    }
    let contents = fs::read_to_string(&abs).map_err(|e| e.to_string())?;
    Ok(format!("### @{token}\n```\n{contents}\n```\n"))
}

fn format_dir_listing(token: &str, abs: &Path) -> String {
    let mut entries: Vec<PathBuf> = fs::read_dir(abs)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .collect();
    entries.sort();
    let total = entries.len();
    entries.truncate(MENTION_DIR_MAX_ENTRIES);
    let mut body = format!("### @{token}/ (directory, {total} entries)\n");
    for p in entries {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("?");
        let suffix = if p.is_dir() { "/" } else { "" };
        body.push_str(&format!("- {name}{suffix}\n"));
    }
    if total > MENTION_DIR_MAX_ENTRIES {
        body.push_str(&format!("… and {} more\n", total - MENTION_DIR_MAX_ENTRIES));
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaves_plain_text_alone() {
        assert_eq!(
            expand_at_mentions("hello world", Path::new(".")),
            "hello world"
        );
    }

    #[test]
    fn expands_existing_file() {
        let root = std::env::temp_dir().join(format!("oxi-mention-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("note.txt"), "hello mention").unwrap();
        let out = expand_at_mentions("see @note.txt please", &root);
        assert!(out.contains("hello mention"));
        assert!(out.contains("Attached context from @mentions"));
        let _ = fs::remove_dir_all(root);
    }
}
