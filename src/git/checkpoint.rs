//! Work tree snapshots around an agent turn, for agents that edit files on their own (ACP).
//!
//! A snapshot is a git tree of the work tree as `git add -A` would stage it (tracked and
//! untracked files, ignored ones left out), written to the object database from an in-memory
//! copy of the index. Nothing visible changes: no commit, ref, stash or index update. Comparing
//! the trees taken before and after a turn gives exactly what the turn changed, and the before
//! tree can put those paths back.

use std::path::{Path, PathBuf};

use git2::{Delta, DiffOptions, IndexAddOption, Oid, Repository};

use super::{CompareFile, err, open_repo, repo_root};

/// Prefix of a Compare "base" naming a snapshot tree rather than a branch.
pub const TURN_BASE_PREFIX: &str = "turn:";

/// How a Compare base reads in titles: a turn snapshot is "agent turn", a branch its name.
pub fn base_label(base: &str) -> &str {
    if base.starts_with(TURN_BASE_PREFIX) {
        "agent turn"
    } else {
        base
    }
}

/// A snapshot: the repository work tree it belongs to and the tree id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub repo: PathBuf,
    pub tree: String,
}

/// Snapshot the work tree of the repository containing `cwd`.
pub fn snapshot(cwd: &Path) -> Result<Snapshot, String> {
    let repo = open_repo(&cwd.to_string_lossy())?;
    let root = repo_root(&repo)?.to_path_buf();
    let tree = snapshot_tree(&repo)?;
    Ok(Snapshot {
        repo: root,
        tree: tree.to_string(),
    })
}

fn snapshot_tree(repo: &Repository) -> Result<Oid, String> {
    // A fresh handle's index is private to it; it is never written back to `.git/index`.
    let mut index = repo.index().map_err(err)?;
    index
        .add_all(["*"], IndexAddOption::DEFAULT, None)
        .map_err(err)?;
    index.update_all(["*"], None).map_err(err)?;
    index.write_tree_to(repo).map_err(err)
}

/// Files that differ between two snapshot trees, with line counts.
pub fn changed_files(repo: &Path, before: &str, after: &str) -> Result<Vec<CompareFile>, String> {
    let repo = open_repo(&repo.to_string_lossy())?;
    let diff = tree_diff(&repo, before, after)?;
    let mut files = Vec::new();
    for (i, delta) in diff.deltas().enumerate() {
        let path = delta_path(&delta);
        let status = match delta.status() {
            Delta::Added | Delta::Untracked => 'A',
            Delta::Deleted => 'D',
            _ => 'M',
        };
        let (added, deleted) = git2::Patch::from_diff(&diff, i)
            .ok()
            .flatten()
            .and_then(|p| p.line_stats().ok())
            .map(|(_, a, d)| (a, d))
            .unwrap_or((0, 0));
        files.push(CompareFile {
            path,
            old_path: None,
            status,
            added,
            deleted,
        });
    }
    Ok(files)
}

/// Put back the `before` state of every path the turn changed (`before` → `after`), or only
/// `only` when given. Refused when one of those paths no longer matches `after`, so later edits
/// (yours or a later turn's) are never overwritten.
pub fn restore(repo: &Path, before: &str, after: &str, only: Option<&str>) -> Result<(), String> {
    let repo = open_repo(&repo.to_string_lossy())?;
    let root = repo_root(&repo)?.to_path_buf();
    let changed: Vec<String> = tree_diff(&repo, before, after)?
        .deltas()
        .map(|d| delta_path(&d))
        .filter(|p| only.is_none_or(|o| o == p))
        .collect();
    if changed.is_empty() {
        return Ok(());
    }
    let now = snapshot_tree(&repo)?;
    let since: Vec<String> = tree_diff(&repo, after, &now.to_string())?
        .deltas()
        .map(|d| delta_path(&d))
        .collect();
    if let Some(path) = changed.iter().find(|p| since.contains(p)) {
        return Err(format!(
            "{path} changed after the response; restore was cancelled to protect your edits."
        ));
    }
    let before_tree = find_tree(&repo, before)?;
    for path in &changed {
        let target = root.join(path);
        match before_tree.get_path(Path::new(path)) {
            Ok(entry) => {
                let blob = repo.find_blob(entry.id()).map_err(err)?;
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| format!("{path}: {e}"))?;
                }
                // Replace the path itself, as a checkout would: writing through a symlink the
                // turn created would clobber whatever it points at.
                if entry.filemode() == FILEMODE_LINK {
                    restore_symlink(&target, blob.content()).map_err(|e| format!("{path}: {e}"))?;
                } else {
                    crate::fsutil::write_atomic(&target, blob.content())
                        .map_err(|e| format!("{path}: {e}"))?;
                    set_executable(&target, entry.filemode() == 0o100755);
                }
            }
            Err(_) => {
                match std::fs::remove_file(&target) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(format!("{path}: {e}")),
                }
                remove_empty_parents(&target, &root);
            }
        }
    }
    Ok(())
}

pub(super) fn find_tree<'r>(repo: &'r Repository, id: &str) -> Result<git2::Tree<'r>, String> {
    let oid = Oid::from_str(id).map_err(err)?;
    repo.find_tree(oid)
        .map_err(|_| "This snapshot is no longer in the repository".to_string())
}

fn tree_diff<'r>(
    repo: &'r Repository,
    before: &str,
    after: &str,
) -> Result<git2::Diff<'r>, String> {
    let old = find_tree(repo, before)?;
    let new = find_tree(repo, after)?;
    let mut opts = DiffOptions::new();
    opts.context_lines(0);
    repo.diff_tree_to_tree(Some(&old), Some(&new), Some(&mut opts))
        .map_err(err)
}

fn delta_path(delta: &git2::DiffDelta<'_>) -> String {
    delta
        .new_file()
        .path()
        .or_else(|| delta.old_file().path())
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default()
}

fn remove_empty_parents(path: &Path, root: &Path) {
    let mut dir = path.parent();
    while let Some(d) = dir {
        if d == root || !d.starts_with(root) || std::fs::remove_dir(d).is_err() {
            break;
        }
        dir = d.parent();
    }
}

#[cfg(unix)]
fn set_executable(path: &Path, executable: bool) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path) {
        let mut perms = meta.permissions();
        let mode = perms.mode();
        let mode = if executable {
            mode | 0o111
        } else {
            mode & !0o111
        };
        perms.set_mode(mode);
        let _ = std::fs::set_permissions(path, perms);
    }
}

#[cfg(not(unix))]
fn set_executable(_path: &Path, _executable: bool) {}

/// Git's file mode for a symbolic link; the blob holds the link target.
const FILEMODE_LINK: i32 = 0o120000;

#[cfg(unix)]
fn restore_symlink(path: &Path, target: &[u8]) -> Result<(), String> {
    use std::os::unix::ffi::OsStrExt;
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => {
            return Err("a directory is in the way of the symlink".to_string());
        }
        Ok(_) => std::fs::remove_file(path).map_err(|e| e.to_string())?,
        Err(_) => {}
    }
    std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(target), path).map_err(|e| e.to_string())
}

/// Without reliable symlink support, store the link target as a plain file, like git does with
/// `core.symlinks = false`.
#[cfg(not(unix))]
fn restore_symlink(path: &Path, target: &[u8]) -> Result<(), String> {
    crate::fsutil::write_atomic(path, target)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Removes the test repository when dropped.
    struct TempRepo(PathBuf);

    impl Drop for TempRepo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn repo_with_file() -> (TempRepo, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "oxi-checkpoint-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let dir = TempRepo(root.clone());
        let repo = Repository::init(&root).unwrap();
        std::fs::write(root.join("a.txt"), "one\n").unwrap();
        std::fs::write(root.join(".gitignore"), "target/\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_all(["*"], IndexAddOption::DEFAULT, None).unwrap();
        index.write().unwrap();
        (dir, root)
    }

    #[test]
    fn snapshot_restore_round_trip_covers_edits_new_and_deleted_files() {
        let (_dir, root) = repo_with_file();
        std::fs::write(root.join("keep.txt"), "untracked\n").unwrap();
        let before = snapshot(&root).unwrap();
        let index_before = std::fs::read(root.join(".git/index")).unwrap();

        std::fs::write(root.join("a.txt"), "two\n").unwrap();
        std::fs::remove_file(root.join("keep.txt")).unwrap();
        std::fs::create_dir_all(root.join("new/dir")).unwrap();
        std::fs::write(root.join("new/dir/b.txt"), "b\n").unwrap();
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::write(root.join("target/ignored"), "x").unwrap();
        let after = snapshot(&root).unwrap();

        assert_eq!(
            std::fs::read(root.join(".git/index")).unwrap(),
            index_before,
            "snapshots must not touch the real index"
        );
        let mut files = changed_files(&root, &before.tree, &after.tree).unwrap();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let summary: Vec<(String, char)> =
            files.iter().map(|f| (f.path.clone(), f.status)).collect();
        assert_eq!(
            summary,
            [
                ("a.txt".to_string(), 'M'),
                ("keep.txt".to_string(), 'D'),
                ("new/dir/b.txt".to_string(), 'A')
            ]
        );

        restore(&root, &before.tree, &after.tree, None).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("a.txt")).unwrap(),
            "one\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("keep.txt")).unwrap(),
            "untracked\n"
        );
        assert!(!root.join("new").exists());
        assert!(root.join("target/ignored").exists());
    }

    #[test]
    fn restore_refuses_when_a_turn_path_changed_later() {
        let (_dir, root) = repo_with_file();
        let before = snapshot(&root).unwrap();
        std::fs::write(root.join("a.txt"), "agent\n").unwrap();
        let after = snapshot(&root).unwrap();
        std::fs::write(root.join("a.txt"), "user\n").unwrap();
        let error = restore(&root, &before.tree, &after.tree, None).unwrap_err();
        assert!(error.contains("a.txt"), "{error}");
        assert_eq!(
            std::fs::read_to_string(root.join("a.txt")).unwrap(),
            "user\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn restore_resets_a_retargeted_symlink_without_writing_through_it() {
        let (_dir, root) = repo_with_file();
        std::fs::write(root.join("b.txt"), "bee\n").unwrap();
        std::os::unix::fs::symlink("a.txt", root.join("link")).unwrap();
        let before = snapshot(&root).unwrap();
        std::fs::remove_file(root.join("link")).unwrap();
        std::os::unix::fs::symlink("b.txt", root.join("link")).unwrap();
        let after = snapshot(&root).unwrap();

        restore(&root, &before.tree, &after.tree, None).unwrap();
        assert_eq!(
            std::fs::read_link(root.join("link")).unwrap(),
            PathBuf::from("a.txt")
        );
        assert_eq!(
            std::fs::read_to_string(root.join("b.txt")).unwrap(),
            "bee\n",
            "the link's new target must not be overwritten"
        );
    }

    #[test]
    fn restore_one_path_leaves_the_others() {
        let (_dir, root) = repo_with_file();
        let before = snapshot(&root).unwrap();
        std::fs::write(root.join("a.txt"), "agent\n").unwrap();
        std::fs::write(root.join("c.txt"), "c\n").unwrap();
        let after = snapshot(&root).unwrap();
        restore(&root, &before.tree, &after.tree, Some("a.txt")).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("a.txt")).unwrap(),
            "one\n"
        );
        assert!(root.join("c.txt").exists());
    }
}
