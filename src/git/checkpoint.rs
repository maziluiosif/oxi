//! Work tree snapshots around an agent turn, for agents that edit files on their own (ACP).
//!
//! A snapshot is a git tree of the work tree as `git add -A` would stage it (tracked and
//! untracked files, ignored ones left out), written to the object database from an in-memory
//! copy of the index. Nothing visible changes: no commit, stash, index or branch update.
//! Comparing the trees taken before and after a turn gives exactly what the turn changed, and
//! the before tree can put those paths back.
//!
//! Each snapshot tree is kept by a ref under `refs/oxi/checkpoints/` so `git gc` does not
//! collect it while a chat can still show or restore that turn, also after oxi restarts. Refs
//! older than [`RETENTION`], and the oldest beyond [`MAX_REFS`] per repository, are pruned.

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

/// Namespace of the refs that keep snapshot trees reachable. Each is named
/// `<unix seconds>-<tree id>`, the time it was last taken.
pub const REF_PREFIX: &str = "refs/oxi/checkpoints/";
/// Snapshots not taken again for this long lose their ref, and `git gc` may collect them.
pub const RETENTION: std::time::Duration = std::time::Duration::from_secs(30 * 24 * 60 * 60);
/// Snapshot refs kept per repository at most; the oldest are pruned first.
pub const MAX_REFS: usize = 2000;
/// Pruning runs at most this often per repository and process.
const PRUNE_EVERY: std::time::Duration = std::time::Duration::from_secs(60 * 60);
/// A snapshot ref younger than this is not renamed when the same tree is taken again.
const REFRESH_AFTER: u64 = 24 * 60 * 60;

/// Snapshot the work tree of the repository containing `cwd`.
pub fn snapshot(cwd: &Path) -> Result<Snapshot, String> {
    let repo = open_repo(&cwd.to_string_lossy())?;
    let root = repo_root(&repo)?.to_path_buf();
    let tree = snapshot_tree(&repo)?;
    // Best effort: an unwritable ref only means the snapshot is not protected from gc.
    if let Err(e) = keep(&repo, tree) {
        log::warn!("checkpoint ref for {tree}: {e}");
    }
    prune_if_due(&repo, &root);
    Ok(Snapshot {
        repo: root,
        tree: tree.to_string(),
    })
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Snapshot refs of `repo`: (ref name, seconds it was taken, tree id).
fn checkpoint_refs(repo: &Repository) -> Result<Vec<(String, u64, String)>, String> {
    let mut refs = Vec::new();
    for reference in repo
        .references_glob(&format!("{REF_PREFIX}*"))
        .map_err(err)?
        .flatten()
    {
        let Ok(name) = reference.name() else {
            continue;
        };
        let Some((secs, tree)) = name
            .strip_prefix(REF_PREFIX)
            .and_then(|rest| rest.split_once('-'))
        else {
            continue;
        };
        if let Ok(secs) = secs.parse() {
            refs.push((name.to_owned(), secs, tree.to_owned()));
        }
    }
    Ok(refs)
}

/// Point a snapshot ref at `tree`, or refresh the time of the one that already does.
fn keep(repo: &Repository, tree: Oid) -> Result<(), String> {
    let now = now_secs();
    let tree_id = tree.to_string();
    let mut fresh = false;
    for reference in repo
        .references_glob(&format!("{REF_PREFIX}*-{tree_id}"))
        .map_err(err)?
        .flatten()
    {
        let Ok(name) = reference.name() else {
            continue;
        };
        let taken = name
            .strip_prefix(REF_PREFIX)
            .and_then(|rest| rest.split_once('-'))
            .and_then(|(secs, _)| secs.parse::<u64>().ok())
            .unwrap_or(0);
        if now.saturating_sub(taken) < REFRESH_AFTER && !fresh {
            fresh = true;
        } else {
            let mut reference = reference;
            let _ = reference.delete();
        }
    }
    if !fresh {
        repo.reference(
            &format!("{REF_PREFIX}{now}-{tree_id}"),
            tree,
            true,
            "oxi: keep turn snapshot",
        )
        .map_err(err)?;
    }
    Ok(())
}

/// Drop snapshot refs past [`RETENTION`] and beyond [`MAX_REFS`], at most once per
/// [`PRUNE_EVERY`] for each repository in this process.
fn prune_if_due(repo: &Repository, root: &Path) {
    static LAST: std::sync::Mutex<Option<std::collections::HashMap<PathBuf, std::time::Instant>>> =
        std::sync::Mutex::new(None);
    {
        let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
        let last = last.get_or_insert_with(Default::default);
        if last.get(root).is_some_and(|at| at.elapsed() < PRUNE_EVERY) {
            return;
        }
        last.insert(root.to_path_buf(), std::time::Instant::now());
    }
    if let Err(e) = prune(repo, now_secs(), RETENTION, MAX_REFS) {
        log::warn!("pruning checkpoint refs: {e}");
    }
}

/// Delete the snapshot refs older than `retention` (as of `now`) and all but the newest `max`.
/// Returns how many were deleted.
fn prune(
    repo: &Repository,
    now: u64,
    retention: std::time::Duration,
    max: usize,
) -> Result<usize, String> {
    let mut refs = checkpoint_refs(repo)?;
    refs.sort_by_key(|(_, secs, _)| std::cmp::Reverse(*secs));
    let mut deleted = 0;
    for (i, (name, secs, _)) in refs.iter().enumerate() {
        if i >= max || now.saturating_sub(*secs) > retention.as_secs() {
            if let Ok(mut reference) = repo.find_reference(name) {
                reference.delete().map_err(err)?;
            }
            deleted += 1;
        }
    }
    Ok(deleted)
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
    if let Some(result) = super::remote::call::<Result<Vec<CompareFile>, String>>(
        &repo.to_string_lossy(),
        "changed_files",
        serde_json::json!({ "before": before, "after": after }),
    ) {
        return result.and_then(|r| r);
    }
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

/// What the turn changed as a unified diff (3 lines of context), for another agent to review.
/// Files whose patch would push the text past `max_bytes` are listed by name only.
pub fn patch(repo: &Path, before: &str, after: &str, max_bytes: usize) -> Result<String, String> {
    if let Some(result) = super::remote::call::<Result<String, String>>(
        &repo.to_string_lossy(),
        "patch",
        serde_json::json!({ "before": before, "after": after, "max_bytes": max_bytes }),
    ) {
        return result.and_then(|r| r);
    }
    let repo = open_repo(&repo.to_string_lossy())?;
    let old = find_tree(&repo, before)?;
    let new = find_tree(&repo, after)?;
    let diff = repo
        .diff_tree_to_tree(Some(&old), Some(&new), None)
        .map_err(err)?;
    let mut out = String::new();
    let mut skipped = Vec::new();
    for i in 0..diff.deltas().len() {
        let Ok(Some(mut patch)) = git2::Patch::from_diff(&diff, i) else {
            continue;
        };
        let text = match patch.to_buf() {
            Ok(buf) => String::from_utf8_lossy(&buf).into_owned(),
            Err(_) => continue,
        };
        if out.len() + text.len() > max_bytes {
            skipped.push(delta_path(&patch.delta()));
            continue;
        }
        out.push_str(&text);
    }
    if !skipped.is_empty() {
        out.push_str("\n# Left out to keep this short (diff the snapshot trees to see them):\n");
        for path in skipped {
            out.push_str(&format!("#   {path}\n"));
        }
    }
    Ok(out)
}

/// Write the files of snapshot `tree` into `dest`, a plain folder without git metadata. The
/// repository's index and work tree are left alone.
pub fn checkout_to(repo: &Path, tree: &str, dest: &Path) -> Result<(), String> {
    let repo = open_repo(&repo.to_string_lossy())?;
    let tree = find_tree(&repo, tree)?;
    std::fs::create_dir_all(dest).map_err(err)?;
    let mut checkout = git2::build::CheckoutBuilder::new();
    checkout
        .target_dir(dest)
        .force()
        .recreate_missing(true)
        .update_index(false);
    repo.checkout_tree(tree.as_object(), Some(&mut checkout))
        .map_err(err)
}

/// Put back the `before` state of every path the turn changed (`before` → `after`), or only
/// `only` when given. Refused when one of those paths no longer matches `after`, so later edits
/// (yours or a later turn's) are never overwritten.
pub fn restore(repo: &Path, before: &str, after: &str, only: Option<&str>) -> Result<(), String> {
    if let Some(result) = super::remote::call::<Result<(), String>>(
        &repo.to_string_lossy(),
        "restore",
        serde_json::json!({ "before": before, "after": after, "only": only }),
    ) {
        return result.and_then(|r| r);
    }
    restore_turns(repo, &[(before.to_string(), after.to_string())], only)
}

/// What putting back several turns would do: see [`plan_restore`].
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RestorePlan {
    /// Paths that go back to how they were before the first turn that changed them.
    pub paths: Vec<String>,
    /// Paths changed since the last turn that touched them (by you or another program);
    /// restoring is refused while there are any.
    pub conflicts: Vec<String>,
}

/// Check what [`restore_turns`] would put back for `turns` (`(before, after)` snapshot pairs,
/// oldest first) and which of those paths changed since, without writing anything.
pub fn plan_restore(repo: &Path, turns: &[(String, String)]) -> Result<RestorePlan, String> {
    if let Some(result) = super::remote::call::<Result<RestorePlan, String>>(
        &repo.to_string_lossy(),
        "plan_restore",
        serde_json::json!({ "turns": turns }),
    ) {
        return result.and_then(|r| r);
    }
    let repo = open_repo(&repo.to_string_lossy())?;
    let (plan, _) = plan_in(&repo, turns, None)?;
    Ok(plan)
}

/// Where each path a run of turns changed goes back to: the index of the first turn that
/// changed it (its `before` tree is the target) and of the last (its `after` tree is what the
/// work tree must still hold).
type PathTurns = std::collections::BTreeMap<String, (usize, usize)>;

fn plan_in(
    repo: &Repository,
    turns: &[(String, String)],
    only: Option<&str>,
) -> Result<(RestorePlan, PathTurns), String> {
    let mut paths = PathTurns::new();
    for (i, (before, after)) in turns.iter().enumerate() {
        for delta in tree_diff(repo, before, after)?.deltas() {
            let path = delta_path(&delta);
            if only.is_some_and(|o| o != path) {
                continue;
            }
            paths.entry(path).and_modify(|e| e.1 = i).or_insert((i, i));
        }
    }
    let mut plan = RestorePlan {
        paths: paths.keys().cloned().collect(),
        conflicts: Vec::new(),
    };
    if paths.is_empty() {
        return Ok((plan, paths));
    }
    let now = repo.find_tree(snapshot_tree(repo)?).map_err(err)?;
    let afters = turns
        .iter()
        .map(|(_, after)| find_tree(repo, after))
        .collect::<Result<Vec<_>, _>>()?;
    let entry = |tree: &git2::Tree<'_>, path: &str| {
        tree.get_path(Path::new(path))
            .ok()
            .map(|e| (e.id(), e.filemode()))
    };
    for (path, (_, last)) in &paths {
        if entry(&afters[*last], path) != entry(&now, path) {
            plan.conflicts.push(path.clone());
        }
    }
    Ok((plan, paths))
}

/// Put back the state from before `turns` (`(before, after)` snapshot pairs, oldest first),
/// or only path `only`: every path they changed returns to its content before the first turn
/// that changed it. Refused, before anything is written, when one of those paths no longer
/// matches what the last turn that changed it left, so later edits are never overwritten.
pub fn restore_turns(
    repo: &Path,
    turns: &[(String, String)],
    only: Option<&str>,
) -> Result<(), String> {
    if let Some(result) = super::remote::call::<Result<(), String>>(
        &repo.to_string_lossy(),
        "restore_turns",
        serde_json::json!({ "turns": turns, "only": only }),
    ) {
        return result.and_then(|r| r);
    }
    let repo = open_repo(&repo.to_string_lossy())?;
    let root = repo_root(&repo)?.to_path_buf();
    let (plan, paths) = plan_in(&repo, turns, only)?;
    if let Some(path) = plan.conflicts.first() {
        return Err(if turns.len() == 1 {
            format!(
                "{path} changed after the response; restore was cancelled to protect your edits."
            )
        } else {
            format!(
                "{} changed after the responses ({path}{}); restore was cancelled to protect \
                 your edits.",
                if plan.conflicts.len() == 1 {
                    "1 file".to_string()
                } else {
                    format!("{} files", plan.conflicts.len())
                },
                if plan.conflicts.len() > 1 {
                    ", …"
                } else {
                    ""
                }
            )
        });
    }
    let befores = turns
        .iter()
        .map(|(before, _)| find_tree(&repo, before))
        .collect::<Result<Vec<_>, _>>()?;
    for (path, (first, _)) in &paths {
        let target = root.join(path);
        match befores[*first].get_path(Path::new(path)) {
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

    #[test]
    fn snapshots_are_kept_by_refs_until_pruned() {
        let (_dir, root) = repo_with_file();
        let first = snapshot(&root).unwrap();
        let again = snapshot(&root).unwrap();
        assert_eq!(first.tree, again.tree);
        let repo = Repository::open(&root).unwrap();
        let refs = checkpoint_refs(&repo).unwrap();
        assert_eq!(
            refs.len(),
            1,
            "the same tree taken twice keeps one ref: {refs:?}"
        );
        assert_eq!(refs[0].2, first.tree);
        let target = repo.find_reference(&refs[0].0).unwrap().target().unwrap();
        assert_eq!(target.to_string(), first.tree);

        std::fs::write(root.join("a.txt"), "changed\n").unwrap();
        let second = snapshot(&root).unwrap();
        assert_eq!(checkpoint_refs(&repo).unwrap().len(), 2);
        // Beyond the cap the oldest go; past the retention all of them.
        let old = format!("{REF_PREFIX}100-{}", first.tree);
        repo.reference(&old, Oid::from_str(&first.tree).unwrap(), true, "test")
            .unwrap();
        let now = now_secs();
        assert_eq!(prune(&repo, now, RETENTION, 2).unwrap(), 1);
        let kept: Vec<String> = checkpoint_refs(&repo)
            .unwrap()
            .into_iter()
            .map(|(name, _, _)| name)
            .collect();
        assert!(!kept.contains(&old), "{kept:?}");
        assert!(kept.iter().any(|name| name.ends_with(&second.tree)));
        assert_eq!(
            prune(&repo, now + RETENTION.as_secs() + 1, RETENTION, MAX_REFS).unwrap(),
            2
        );
        assert!(checkpoint_refs(&repo).unwrap().is_empty());
    }

    #[test]
    fn git_gc_keeps_a_snapshot_its_ref_protects() {
        let (_dir, root) = repo_with_file();
        std::fs::write(root.join("only-in-snapshot.txt"), "precious\n").unwrap();
        let kept = snapshot(&root).unwrap();
        std::fs::remove_file(root.join("only-in-snapshot.txt")).unwrap();
        let Ok(status) = std::process::Command::new("git")
            .args(["gc", "--prune=now", "--quiet"])
            .current_dir(&root)
            .status()
        else {
            return; // No git binary to run gc with.
        };
        assert!(status.success());
        let repo = Repository::open(&root).unwrap();
        let tree = find_tree(&repo, &kept.tree).unwrap();
        let entry = tree.get_path(Path::new("only-in-snapshot.txt")).unwrap();
        assert_eq!(repo.find_blob(entry.id()).unwrap().content(), b"precious\n");
    }

    #[test]
    fn restoring_several_turns_goes_back_to_before_the_first_after_a_conflict_check() {
        let (_dir, root) = repo_with_file();
        let t0 = snapshot(&root).unwrap().tree;
        std::fs::write(root.join("a.txt"), "turn one\n").unwrap();
        std::fs::write(root.join("b.txt"), "b\n").unwrap();
        let t1 = snapshot(&root).unwrap().tree;
        std::fs::write(root.join("a.txt"), "turn two\n").unwrap();
        std::fs::write(root.join("c.txt"), "c\n").unwrap();
        let t2 = snapshot(&root).unwrap().tree;
        let turns = vec![(t0.clone(), t1.clone()), (t1, t2)];

        let plan = plan_restore(&root, &turns).unwrap();
        assert_eq!(plan.paths, ["a.txt", "b.txt", "c.txt"]);
        assert!(plan.conflicts.is_empty());

        // An edit made after the last turn blocks everything, and nothing is written.
        std::fs::write(root.join("c.txt"), "yours\n").unwrap();
        assert_eq!(plan_restore(&root, &turns).unwrap().conflicts, ["c.txt"]);
        let error = restore_turns(&root, &turns, None).unwrap_err();
        assert!(error.contains("c.txt"), "{error}");
        assert_eq!(
            std::fs::read_to_string(root.join("a.txt")).unwrap(),
            "turn two\n"
        );

        std::fs::write(root.join("c.txt"), "c\n").unwrap();
        restore_turns(&root, &turns, None).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("a.txt")).unwrap(),
            "one\n"
        );
        assert!(!root.join("b.txt").exists());
        assert!(!root.join("c.txt").exists());
    }

    #[test]
    fn patch_is_a_unified_diff_and_lists_files_over_budget() {
        let (_dir, root) = repo_with_file();
        let before = snapshot(&root).unwrap();
        std::fs::write(root.join("a.txt"), "two\n").unwrap();
        std::fs::write(root.join("big.txt"), "x\n".repeat(500)).unwrap();
        let after = snapshot(&root).unwrap();
        let text = patch(&root, &before.tree, &after.tree, 200).unwrap();
        assert!(text.contains("--- a/a.txt"), "{text}");
        assert!(text.contains("-one\n+two"), "{text}");
        assert!(!text.contains("+x"), "{text}");
        assert!(text.contains("#   big.txt"), "{text}");
    }
}
