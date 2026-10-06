//! Single change blocks applied from the diff view: revert a block in the work tree, or
//! stage / unstage it in the index, like VS Code's per-hunk actions.
//!
//! The diff view already holds both sides of the block, so an edit is just "these lines at
//! this position become those lines". The target's current lines are checked against what the
//! view showed first: a file edited since the diff was computed is refused, never patched in
//! the wrong place.

use std::path::Path;

use git2::{IndexEntry, IndexTime, Repository};

use super::{err, open_repo, repo_root};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockTarget {
    /// The file on disk (revert a working-tree or since-base change).
    WorkTree,
    /// The file's staged content (stage or unstage a block).
    Index,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockEdit {
    pub path: String,
    pub target: BlockTarget,
    /// 1-based line in the target where `expected` starts (or where to insert, if empty).
    pub start: usize,
    /// The target's lines as the diff showed them, without line endings.
    pub expected: Vec<String>,
    /// The lines that replace them, without line endings.
    pub replacement: Vec<String>,
}

pub(super) fn apply_block(cwd: &str, edit: &BlockEdit) -> Result<(), String> {
    let repo = open_repo(cwd)?;
    match edit.target {
        BlockTarget::WorkTree => {
            let path = repo_root(&repo)?.join(&edit.path);
            let content = std::fs::read(&path).map_err(|e| format!("{}: {e}", edit.path))?;
            let content = String::from_utf8(content)
                .map_err(|_| format!("{} is not UTF-8 text", edit.path))?;
            let updated = splice_lines(&content, edit)?;
            std::fs::write(&path, updated).map_err(|e| format!("{}: {e}", edit.path))
        }
        BlockTarget::Index => write_index_block(&repo, edit),
    }
}

fn write_index_block(repo: &Repository, edit: &BlockEdit) -> Result<(), String> {
    let mut index = repo.index().map_err(err)?;
    let existing = index.get_path(Path::new(&edit.path), 0);
    let content = match &existing {
        Some(entry) => {
            let blob = repo.find_blob(entry.id).map_err(err)?;
            String::from_utf8(blob.content().to_vec())
                .map_err(|_| format!("{} is not UTF-8 text", edit.path))?
        }
        None => String::new(),
    };
    let updated = splice_lines(&content, edit)?;
    let entry = existing.unwrap_or_else(|| new_entry(repo, &edit.path));
    index
        .add_frombuffer(&entry, updated.as_bytes())
        .map_err(err)?;
    index.write().map_err(err)
}

/// An index entry for a file that isn't staged yet, with the work tree file's mode.
fn new_entry(repo: &Repository, path: &str) -> IndexEntry {
    #[cfg(unix)]
    let executable = {
        use std::os::unix::fs::PermissionsExt;
        repo_root(repo)
            .ok()
            .and_then(|root| std::fs::metadata(root.join(path)).ok())
            .is_some_and(|meta| meta.permissions().mode() & 0o111 != 0)
    };
    #[cfg(not(unix))]
    let executable = {
        let _ = repo;
        false
    };
    IndexEntry {
        ctime: IndexTime::new(0, 0),
        mtime: IndexTime::new(0, 0),
        dev: 0,
        ino: 0,
        mode: if executable { 0o100755 } else { 0o100644 },
        uid: 0,
        gid: 0,
        file_size: 0,
        id: git2::Oid::ZERO_SHA1,
        flags: 0,
        flags_extended: 0,
        path: path.as_bytes().to_vec(),
    }
}

/// Replace `edit.expected` at `edit.start` in `content` by `edit.replacement`, keeping the
/// file's line endings.
pub fn splice_lines(content: &str, edit: &BlockEdit) -> Result<String, String> {
    let lines: Vec<&str> = content.split_inclusive('\n').collect();
    let start = edit.start.saturating_sub(1);
    let end = start + edit.expected.len();
    let stale = || format!("{} changed since the diff was shown; try again", edit.path);
    if end > lines.len() || (edit.expected.is_empty() && start > lines.len()) {
        return Err(stale());
    }
    let matches = lines[start..end]
        .iter()
        .zip(&edit.expected)
        .all(|(line, expected)| line.trim_end_matches(['\r', '\n']) == expected);
    if !matches {
        return Err(stale());
    }
    let ending = if content.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let mut out = String::with_capacity(content.len());
    for line in &lines[..start] {
        out.push_str(line);
    }
    if !edit.replacement.is_empty() && !out.is_empty() && !out.ends_with('\n') {
        // Appending after a last line that had no newline.
        out.push_str(ending);
    }
    // The block ends the file without a final newline only where the replaced lines did.
    let keeps_open_end = end == lines.len()
        && lines.last().is_some_and(|line| !line.ends_with('\n'))
        && !edit.expected.is_empty();
    for (i, line) in edit.replacement.iter().enumerate() {
        out.push_str(line);
        let last = i + 1 == edit.replacement.len();
        if !(last && keeps_open_end) {
            out.push_str(ending);
        }
    }
    if edit.replacement.is_empty() && keeps_open_end && out.ends_with('\n') {
        // Removing the unterminated last line: the line before becomes the last one.
        out.truncate(out.trim_end_matches(['\r', '\n']).len());
    }
    for line in &lines[end..] {
        out.push_str(line);
    }
    Ok(out)
}

/// One change between a base text and the editor buffer (1-based starts; where lines would
/// be inserted when a side is empty).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextHunk {
    pub old_start: usize,
    pub old_lines: Vec<String>,
    pub new_start: usize,
    pub new_lines: Vec<String>,
}

/// The file's content the editor gutter compares against: the merge base with
/// `compare_base` (Compare tab), otherwise `HEAD`. Empty for a file the base lacks.
pub fn base_text(cwd: &str, relative: &str, compare_base: Option<&str>) -> Result<String, String> {
    let repo = open_repo(cwd)?;
    let head = repo
        .head()
        .and_then(|head| head.peel_to_commit())
        .map_err(err)?;
    let commit = match compare_base {
        Some(base) => {
            let base = repo
                .revparse_single(base)
                .and_then(|object| object.peel_to_commit())
                .map_err(err)?;
            let merge_base = repo.merge_base(head.id(), base.id()).map_err(err)?;
            repo.find_commit(merge_base).map_err(err)?
        }
        None => head,
    };
    let tree = commit.tree().map_err(err)?;
    let Ok(entry) = tree.get_path(Path::new(relative)) else {
        return Ok(String::new());
    };
    let blob = repo.find_blob(entry.id()).map_err(err)?;
    String::from_utf8(blob.content().to_vec()).map_err(|_| format!("{relative} is not UTF-8 text"))
}

/// Line changes from `old` to `new`, without context. Line endings don't count: a base blob
/// stored with LF against a CRLF checkout (Git for Windows' `autocrlf`) is not a change on
/// every line.
pub fn text_hunks(old: &str, new: &str) -> Vec<TextHunk> {
    let old = old.replace("\r\n", "\n");
    let new = new.replace("\r\n", "\n");
    let mut opts = git2::DiffOptions::new();
    opts.context_lines(0);
    let Ok(patch) =
        git2::Patch::from_buffers(old.as_bytes(), None, new.as_bytes(), None, Some(&mut opts))
    else {
        return Vec::new();
    };
    let mut hunks = Vec::new();
    for index in 0..patch.num_hunks() {
        let Ok((hunk, count)) = patch.hunk(index) else {
            continue;
        };
        let mut out = TextHunk {
            // A zero-length side's start names the line *before* the change.
            old_start: hunk.old_start() as usize + usize::from(hunk.old_lines() == 0),
            old_lines: Vec::new(),
            new_start: hunk.new_start() as usize + usize::from(hunk.new_lines() == 0),
            new_lines: Vec::new(),
        };
        for line in 0..count {
            let Ok(line) = patch.line_in_hunk(index, line) else {
                continue;
            };
            let text = String::from_utf8_lossy(line.content())
                .trim_end_matches(['\r', '\n'])
                .to_owned();
            match line.origin() {
                '-' => out.old_lines.push(text),
                '+' => out.new_lines.push(text),
                _ => {}
            }
        }
        hunks.push(out);
    }
    hunks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(start: usize, expected: &[&str], replacement: &[&str]) -> BlockEdit {
        BlockEdit {
            path: "f".into(),
            target: BlockTarget::WorkTree,
            start,
            expected: expected.iter().map(|s| s.to_string()).collect(),
            replacement: replacement.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn text_hunks_ignore_line_endings() {
        let hunks = text_hunks("a\nb\nc\n", "a\r\nB\r\nc\r\n");
        assert_eq!(
            hunks,
            vec![TextHunk {
                old_start: 2,
                old_lines: vec!["b".into()],
                new_start: 2,
                new_lines: vec!["B".into()],
            }]
        );
        assert!(text_hunks("a\nb\n", "a\r\nb\r\n").is_empty());
    }

    #[test]
    fn splices_replace_insert_and_delete_keeping_line_endings() {
        assert_eq!(
            splice_lines("a\nB\nB2\nc\n", &edit(2, &["B", "B2"], &["b"])).unwrap(),
            "a\nb\nc\n"
        );
        assert_eq!(
            splice_lines("a\r\nc\r\n", &edit(2, &[], &["b"])).unwrap(),
            "a\r\nb\r\nc\r\n"
        );
        assert_eq!(
            splice_lines("a\nb\nc", &edit(3, &["c"], &[])).unwrap(),
            "a\nb"
        );
        assert_eq!(
            splice_lines("a\nb", &edit(3, &[], &["c"])).unwrap(),
            "a\nb\nc\n"
        );
        assert_eq!(
            splice_lines("a\nx", &edit(2, &["x"], &["y"])).unwrap(),
            "a\ny"
        );
    }

    #[test]
    fn text_hunks_place_insertions_and_deletions() {
        let hunks = text_hunks("a\nb\nc\nd\n", "a\nB\nc\nnew\nd\n");
        assert_eq!(hunks.len(), 2);
        assert_eq!((hunks[0].old_start, hunks[0].new_start), (2, 2));
        assert_eq!(
            (hunks[0].old_lines.clone(), hunks[0].new_lines.clone()),
            (vec!["b".to_string()], vec!["B".to_string()])
        );
        assert_eq!((hunks[1].old_start, hunks[1].new_start), (4, 4));
        assert!(hunks[1].old_lines.is_empty());
        let deleted = text_hunks("a\nb\nc\n", "a\nc\n");
        assert_eq!((deleted[0].old_start, deleted[0].new_start), (2, 2));
        assert!(deleted[0].new_lines.is_empty());
    }

    #[test]
    fn refuses_a_target_that_no_longer_matches() {
        assert!(splice_lines("a\nchanged\n", &edit(2, &["b"], &["c"])).is_err());
        assert!(splice_lines("a\n", &edit(5, &["b"], &[])).is_err());
    }
}
