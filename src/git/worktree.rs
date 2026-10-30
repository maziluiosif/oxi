//! Linked work trees for agent chats: each one gets its own branch and folder, so an agent can
//! work there while the main checkout stays untouched, and the result is merged back later.

use std::path::{Path, PathBuf};

use git2::{
    BranchType, IndexAddOption, MergeAnalysis, Repository, WorktreeAddOptions,
    build::CheckoutBuilder,
};

use super::{current_branch, err, open_repo, repo_root};

/// Where oxi keeps the work trees it creates.
fn worktrees_dir() -> PathBuf {
    crate::app_dirs::data_dir().join("worktrees")
}

/// A linked work tree and the checkout it branches from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeInfo {
    /// The work tree's branch.
    pub branch: String,
    /// The main checkout's root.
    pub main_root: PathBuf,
    /// The branch checked out in the main checkout (what "Merge" merges into).
    pub main_branch: String,
}

/// Create a work tree with a new branch `oxi/<name>` off the current `HEAD` of the repository
/// containing `cwd`. Returns the work tree folder; when `cwd` is a subfolder of the repository,
/// the same subfolder inside the new work tree.
pub fn create(cwd: &Path) -> Result<PathBuf, String> {
    let repo = open_repo(&cwd.to_string_lossy())?;
    if repo.is_worktree() {
        return Err("This folder is already a work tree; create it from the main checkout".into());
    }
    let root = repo_root(&repo)?.to_path_buf();
    let head = repo
        .head()
        .and_then(|h| h.peel_to_commit())
        .map_err(|_| "The repository has no commits yet".to_string())?;
    let repo_name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".into());
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let (name, folder) = (1..100)
        .map(|n| {
            let name = if n == 1 {
                stamp.clone()
            } else {
                format!("{stamp}-{n}")
            };
            let folder = worktrees_dir().join(format!("{repo_name}-{name}"));
            (name, folder)
        })
        .find(|(name, folder)| {
            !folder.exists()
                && repo
                    .find_branch(&format!("oxi/{name}"), BranchType::Local)
                    .is_err()
                && repo
                    .find_worktree(&worktree_name(&repo_name, name))
                    .is_err()
        })
        .ok_or("Could not pick a free work tree name")?;
    std::fs::create_dir_all(worktrees_dir()).map_err(|e| e.to_string())?;
    let branch = repo
        .branch(&format!("oxi/{name}"), &head, false)
        .map_err(err)?;
    let mut opts = WorktreeAddOptions::new();
    opts.reference(Some(branch.get()));
    repo.worktree(&worktree_name(&repo_name, &name), &folder, Some(&opts))
        .map_err(err)?;
    let relative = cwd
        .canonicalize()
        .ok()
        .and_then(|c| {
            root.canonicalize()
                .ok()
                .and_then(|r| c.strip_prefix(r).ok().map(Path::to_path_buf))
        })
        .unwrap_or_default();
    Ok(folder.join(relative))
}

fn worktree_name(repo_name: &str, name: &str) -> String {
    format!("oxi-{repo_name}-{name}")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// Details of the linked work tree containing `path`; `None` for a main checkout or no repo.
pub fn info(path: &Path) -> Option<WorktreeInfo> {
    let repo = open_repo(&path.to_string_lossy()).ok()?;
    if !repo.is_worktree() {
        return None;
    }
    let main_root = repo.commondir().parent()?.to_path_buf();
    let main = Repository::open(&main_root).ok()?;
    Some(WorktreeInfo {
        branch: current_branch(&repo),
        main_root,
        main_branch: current_branch(&main),
    })
}

/// Commit everything in the work tree at `path` (if anything changed), then merge its branch
/// into the branch checked out in the main checkout. The main checkout must have no uncommitted
/// changes to tracked files. Returns a short summary for the user.
pub fn merge_into_main(path: &Path, message: &str) -> Result<String, String> {
    let wt = open_repo(&path.to_string_lossy())?;
    if !wt.is_worktree() {
        return Err("Not a work tree".into());
    }
    let committed = commit_all(&wt, message)?;
    let theirs_id = wt
        .head()
        .and_then(|h| h.peel_to_commit())
        .map_err(err)?
        .id();
    let main_root = wt
        .commondir()
        .parent()
        .ok_or("Cannot find the main checkout")?
        .to_path_buf();
    let main = Repository::open(&main_root).map_err(err)?;
    let dirty = main
        .statuses(Some(git2::StatusOptions::new().include_untracked(false)))
        .map_err(err)?
        .iter()
        .any(|s| s.status() != git2::Status::CURRENT);
    if dirty {
        return Err(format!(
            "{} has uncommitted changes; commit or stash them before merging",
            main_root.display()
        ));
    }
    let main_branch = current_branch(&main);
    // Objects are tied to the handle that loaded them; work through the main checkout's.
    let theirs = main.find_commit(theirs_id).map_err(err)?;
    let ours = main.head().and_then(|h| h.peel_to_commit()).map_err(err)?;
    let annotated = main.find_annotated_commit(theirs.id()).map_err(err)?;
    let (analysis, _) = main.merge_analysis(&[&annotated]).map_err(err)?;
    let prefix = if committed {
        "Committed the work tree's changes and merged"
    } else {
        "Merged"
    };
    if analysis.contains(MergeAnalysis::ANALYSIS_UP_TO_DATE) {
        return Ok(format!("{main_branch} already contains this work"));
    }
    let (tree, message) = if analysis.contains(MergeAnalysis::ANALYSIS_FASTFORWARD) {
        (theirs.tree().map_err(err)?, None)
    } else {
        let mut index = main.merge_commits(&ours, &theirs, None).map_err(err)?;
        if index.has_conflicts() {
            return Err(format!(
                "Merging into {main_branch} has conflicts; resolve them from a terminal (git merge {})",
                current_branch(&wt)
            ));
        }
        let tree_id = index.write_tree_to(&main).map_err(err)?;
        (
            main.find_tree(tree_id).map_err(err)?,
            Some(format!("Merge branch '{}'", current_branch(&wt))),
        )
    };
    // Update the files first (relative to the current HEAD), then move the branch.
    main.checkout_tree(tree.as_object(), Some(CheckoutBuilder::new().safe()))
        .map_err(err)?;
    let new_head = match message {
        None => theirs.id(),
        Some(message) => {
            let sig = main.signature().map_err(err)?;
            main.commit(None, &sig, &sig, &message, &tree, &[&ours, &theirs])
                .map_err(err)?
        }
    };
    main.head()
        .map_err(err)?
        .set_target(new_head, "oxi: merge work tree")
        .map_err(err)?;
    Ok(format!("{prefix} into {main_branch}"))
}

/// Stage and commit every change in `repo`'s work tree. Returns whether a commit was made.
fn commit_all(repo: &Repository, message: &str) -> Result<bool, String> {
    let mut index = repo.index().map_err(err)?;
    index
        .add_all(["*"], IndexAddOption::DEFAULT, None)
        .map_err(err)?;
    index.update_all(["*"], None).map_err(err)?;
    let tree_id = index.write_tree().map_err(err)?;
    let parent = repo.head().and_then(|h| h.peel_to_commit()).map_err(err)?;
    if parent.tree_id() == tree_id {
        return Ok(false);
    }
    index.write().map_err(err)?;
    let tree = repo.find_tree(tree_id).map_err(err)?;
    let sig = repo.signature().map_err(err)?;
    repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &[&parent])
        .map_err(err)?;
    Ok(true)
}

/// What removing a work tree would lose, so the user can see it before confirming.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemovalRisk {
    /// The work tree's branch (`HEAD` when it is detached).
    pub branch: String,
    /// Tracked files with uncommitted changes, staged or not.
    pub modified: Vec<String>,
    /// Untracked files (folders as one entry); ignored files are not counted.
    pub untracked: Vec<String>,
    /// Commits on the branch that no other branch, remote branch or tag contains, newest first,
    /// as "<short id> <summary>". At most [`MAX_UNMERGED_LISTED`].
    pub unmerged: Vec<String>,
    /// More unmerged commits than are listed.
    pub more_unmerged: bool,
    /// `HEAD` is detached: its commits have no branch to keep them.
    pub detached: bool,
}

/// Unmerged commits listed by [`removal_risk`]; more are only flagged.
pub const MAX_UNMERGED_LISTED: usize = 20;

impl RemovalRisk {
    /// Deleting the folder loses uncommitted or untracked files.
    pub fn loses_files(&self) -> bool {
        !self.modified.is_empty() || !self.untracked.is_empty()
    }

    /// Deleting the branch (or a detached `HEAD`) loses commits.
    pub fn loses_commits(&self) -> bool {
        !self.unmerged.is_empty()
    }

    /// Whether removing with `opts` (without `force`) would lose anything.
    pub fn blocks(&self, opts: RemoveOptions) -> bool {
        self.loses_files() || (self.loses_commits() && (!opts.keep_branch || self.detached))
    }
}

/// How [`remove`] treats a work tree with work that would be lost.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RemoveOptions {
    /// Keep the branch (only the folder and git's record of it go), so its commits stay.
    pub keep_branch: bool,
    /// Remove even when uncommitted files or unmerged commits would be lost.
    pub force: bool,
}

/// What removing the work tree at `path` would lose (see [`RemovalRisk`]).
pub fn removal_risk(path: &Path) -> Result<RemovalRisk, String> {
    let wt = open_repo(&path.to_string_lossy())?;
    if !wt.is_worktree() {
        return Err("Not a work tree".into());
    }
    let mut risk = RemovalRisk {
        branch: current_branch(&wt),
        detached: wt.head_detached().unwrap_or(false),
        ..Default::default()
    };
    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(false)
        .include_ignored(false)
        .exclude_submodules(true);
    for entry in wt.statuses(Some(&mut opts)).map_err(err)?.iter() {
        let Ok(file) = entry.path().map(str::to_owned) else {
            continue;
        };
        let status = entry.status();
        if status == git2::Status::WT_NEW {
            risk.untracked.push(file);
        } else if !status.is_ignored() && status != git2::Status::CURRENT {
            risk.modified.push(file);
        }
    }
    let Ok(head) = wt.head().and_then(|h| h.peel_to_commit()) else {
        return Ok(risk);
    };
    // Commits only this branch keeps: everything reachable from no other ref.
    let mut walk = wt.revwalk().map_err(err)?;
    walk.push(head.id()).map_err(err)?;
    let own_ref = wt
        .head()
        .ok()
        .and_then(|h| h.name().ok().map(str::to_owned));
    for reference in wt.references().map_err(err)?.flatten() {
        let Ok(name) = reference.name() else {
            continue;
        };
        let other_branch = name.starts_with("refs/heads/") && Some(name) != own_ref.as_deref();
        let keeps =
            other_branch || name.starts_with("refs/remotes/") || name.starts_with("refs/tags/");
        if keeps && let Ok(commit) = reference.peel_to_commit() {
            walk.hide(commit.id()).map_err(err)?;
        }
    }
    // The main checkout's HEAD may be detached on a commit no branch names.
    if let Some(main_root) = wt.commondir().parent()
        && let Ok(main) = Repository::open(main_root)
        && let Ok(main_head) = main.head().and_then(|h| h.peel_to_commit())
    {
        walk.hide(main_head.id()).map_err(err)?;
    }
    for (i, oid) in walk.enumerate() {
        if i == MAX_UNMERGED_LISTED {
            risk.more_unmerged = true;
            break;
        }
        let oid = oid.map_err(err)?;
        let commit = wt.find_commit(oid).map_err(err)?;
        let id = oid.to_string();
        risk.unmerged.push(format!(
            "{} {}",
            &id[..7.min(id.len())],
            commit.summary().ok().flatten().unwrap_or("").trim()
        ));
    }
    Ok(risk)
}

/// Delete the work tree at `path` (its folder and git's record of it) and, unless
/// `opts.keep_branch`, its branch. Without `opts.force` it refuses, checking again at this
/// moment, when that would lose uncommitted files or commits no other branch contains.
pub fn remove(path: &Path, opts: RemoveOptions) -> Result<(), String> {
    if !opts.force {
        let risk = removal_risk(path)?;
        if risk.blocks(opts) {
            return Err(if risk.loses_files() {
                format!(
                    "{} has uncommitted or untracked files ({}); the work tree was kept",
                    path.display(),
                    risk.modified.len() + risk.untracked.len()
                )
            } else {
                format!(
                    "{} has commits that are on no other branch; the work tree was kept",
                    risk.branch
                )
            });
        }
    }
    let wt = open_repo(&path.to_string_lossy())?;
    if !wt.is_worktree() {
        return Err("Not a work tree".into());
    }
    let folder = repo_root(&wt)?.to_path_buf();
    let branch = current_branch(&wt);
    let main_root = wt
        .commondir()
        .parent()
        .ok_or("Cannot find the main checkout")?
        .to_path_buf();
    drop(wt);
    let main = Repository::open(&main_root).map_err(err)?;
    let folder_canon = folder.canonicalize().unwrap_or_else(|_| folder.clone());
    let names = main.worktrees().map_err(err)?;
    for name in names.iter().flatten().flatten() {
        let Ok(worktree) = main.find_worktree(name) else {
            continue;
        };
        let same = worktree
            .path()
            .canonicalize()
            .is_ok_and(|p| p == folder_canon);
        if same {
            std::fs::remove_dir_all(&folder).map_err(|e| e.to_string())?;
            worktree
                .prune(Some(
                    git2::WorktreePruneOptions::new()
                        .valid(true)
                        .working_tree(true),
                ))
                .map_err(err)?;
            break;
        }
    }
    if !opts.keep_branch
        && let Ok(mut b) = main.find_branch(&branch, BranchType::Local)
    {
        let _ = b.delete();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempRepo(PathBuf);

    impl Drop for TempRepo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn init() -> (TempRepo, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "oxi-worktree-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let repo = Repository::init(&root).unwrap();
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "Oxi Test").unwrap();
        config.set_str("user.email", "oxi@example.com").unwrap();
        // Windows runners default to core.autocrlf=true, which would turn the
        // merged checkout into CRLF.
        config.set_bool("core.autocrlf", false).unwrap();
        std::fs::write(root.join("a.txt"), "one\n").unwrap();
        commit_all(&repo, "initial").unwrap_or_else(|_| {
            // First commit has no parent.
            let mut index = repo.index().unwrap();
            index.add_all(["*"], IndexAddOption::DEFAULT, None).unwrap();
            index.write().unwrap();
            let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
            let sig = repo.signature().unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
                .unwrap();
            true
        });
        (TempRepo(root.clone()), root)
    }

    #[test]
    fn create_merge_and_remove_a_work_tree() {
        let (_guard, root) = init();
        let folder = create(&root).unwrap();
        let _cleanup = TempRepo(folder.clone());
        let info = info(&folder).unwrap();
        assert!(info.branch.starts_with("oxi/"), "{}", info.branch);
        assert_eq!(
            info.main_root.canonicalize().unwrap(),
            root.canonicalize().unwrap()
        );
        assert!(super::info(&root).is_none());

        std::fs::write(folder.join("a.txt"), "two\n").unwrap();
        std::fs::write(folder.join("b.txt"), "new\n").unwrap();
        let summary = merge_into_main(&folder, "agent work").unwrap();
        assert!(summary.contains("Committed"), "{summary}");
        assert_eq!(
            std::fs::read_to_string(root.join("a.txt")).unwrap(),
            "two\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("b.txt")).unwrap(),
            "new\n"
        );

        assert_eq!(
            removal_risk(&folder).unwrap().unmerged,
            Vec::<String>::new()
        );
        remove(&folder, RemoveOptions::default()).unwrap();
        assert!(!folder.exists());
        let main = Repository::open(&root).unwrap();
        assert!(main.find_branch(&info.branch, BranchType::Local).is_err());
    }

    #[test]
    fn removal_lists_what_would_be_lost_and_refuses_without_force() {
        let (_guard, root) = init();
        let folder = create(&root).unwrap();
        let _cleanup = TempRepo(folder.clone());
        let branch = info(&folder).unwrap().branch;
        assert_eq!(
            removal_risk(&folder).unwrap(),
            RemovalRisk {
                branch: branch.clone(),
                ..Default::default()
            },
            "a fresh work tree loses nothing"
        );

        std::fs::write(folder.join("a.txt"), "edited\n").unwrap();
        std::fs::write(folder.join("new.txt"), "new\n").unwrap();
        std::fs::write(folder.join(".gitignore"), "build/\n").unwrap();
        std::fs::create_dir_all(folder.join("build")).unwrap();
        std::fs::write(folder.join("build/out"), "x").unwrap();
        let risk = removal_risk(&folder).unwrap();
        assert_eq!(risk.modified, ["a.txt"]);
        let mut untracked = risk.untracked.clone();
        untracked.sort();
        assert_eq!(
            untracked,
            [".gitignore", "new.txt"],
            "ignored files are not listed"
        );
        let error = remove(&folder, RemoveOptions::default()).unwrap_err();
        assert!(error.contains("uncommitted"), "{error}");
        assert!(folder.join("new.txt").exists(), "nothing was deleted");

        // Committed on the branch only: deleting the branch would lose it.
        let wt = Repository::open(&folder).unwrap();
        commit_all(&wt, "agent work").unwrap();
        let risk = removal_risk(&folder).unwrap();
        assert!(!risk.loses_files());
        assert_eq!(risk.unmerged.len(), 1);
        assert!(
            risk.unmerged[0].ends_with(" agent work"),
            "{:?}",
            risk.unmerged
        );
        assert!(remove(&folder, RemoveOptions::default()).is_err());

        // Keeping the branch keeps the commit, so nothing is lost.
        remove(
            &folder,
            RemoveOptions {
                keep_branch: true,
                force: false,
            },
        )
        .unwrap();
        assert!(!folder.exists());
        let main = Repository::open(&root).unwrap();
        assert!(main.find_branch(&branch, BranchType::Local).is_ok());
    }

    #[test]
    fn forced_removal_deletes_a_dirty_work_tree_and_its_branch() {
        let (_guard, root) = init();
        let folder = create(&root).unwrap();
        let _cleanup = TempRepo(folder.clone());
        let branch = info(&folder).unwrap().branch;
        std::fs::write(folder.join("scratch.txt"), "unsaved\n").unwrap();
        remove(
            &folder,
            RemoveOptions {
                keep_branch: false,
                force: true,
            },
        )
        .unwrap();
        assert!(!folder.exists());
        let main = Repository::open(&root).unwrap();
        assert!(main.find_branch(&branch, BranchType::Local).is_err());
    }

    #[test]
    fn merge_refuses_a_dirty_main_checkout() {
        let (_guard, root) = init();
        let folder = create(&root).unwrap();
        let _cleanup = TempRepo(folder.clone());
        std::fs::write(folder.join("a.txt"), "agent\n").unwrap();
        std::fs::write(root.join("a.txt"), "local edit\n").unwrap();
        let error = merge_into_main(&folder, "agent work").unwrap_err();
        assert!(error.contains("uncommitted"), "{error}");
        assert_eq!(
            std::fs::read_to_string(root.join("a.txt")).unwrap(),
            "local edit\n"
        );
    }
}
