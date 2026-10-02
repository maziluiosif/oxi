//! Branch comparison, like GitLens "Compare with" or a GitHub pull request: the commits on
//! `HEAD` that the base branch lacks, and every file that differs between their merge base
//! and the work tree (committed and uncommitted changes alike), so the files stay editable.

use std::collections::HashMap;

use git2::{BranchType, DiffFindOptions, DiffOptions, Patch, Repository, Sort};

use super::{
    FULL_FILE_CONTEXT, GitCommit, GitLineChange, GitLineKind, MAX_FILE_DIFF_CHARS, current_branch,
    diff_text_limited, err, open_repo,
};

/// Commits listed at most; a base that is far behind would otherwise walk the whole history.
const MAX_COMPARE_COMMITS: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompareFile {
    pub path: String,
    /// The pre-rename path, for renames.
    pub old_path: Option<String>,
    pub status: char,
    pub added: usize,
    pub deleted: usize,
}

#[derive(Debug, Clone, Default)]
pub struct GitCompare {
    /// The base it was computed against (a local or remote branch name).
    pub base: String,
    /// Branches that can serve as the base, local first, then remote.
    pub bases: Vec<String>,
    pub branch: String,
    pub merge_base: String,
    pub commits: Vec<GitCommit>,
    /// More commits than [`MAX_COMPARE_COMMITS`] exist.
    pub commits_truncated: bool,
    pub files: Vec<CompareFile>,
    /// Work-tree lines that differ from the merge base, for the editor gutter.
    pub line_changes: HashMap<String, Vec<GitLineChange>>,
    pub error: Option<String>,
}

/// Compare the current branch (and its work tree) against `base`; an empty `base` picks the
/// repository's default branch.
pub fn compare(cwd: &str, base: &str) -> GitCompare {
    let repo = match open_repo(cwd) {
        Ok(repo) if !repo.is_bare() => repo,
        Ok(_) => return failed(base, "Bare repositories are not supported".into()),
        Err(error) => return failed(base, error),
    };
    let branch = current_branch(&repo);
    let bases = base_candidates(&repo, &branch);
    let base = if base.is_empty() {
        default_base(&repo, &bases).unwrap_or_default()
    } else {
        base.to_owned()
    };
    let mut result = GitCompare {
        base: base.clone(),
        bases,
        branch,
        ..Default::default()
    };
    if base.is_empty() {
        result.error = Some("No other branch to compare with".into());
        return result;
    }
    if let Err(error) = fill(&repo, &base, &mut result) {
        result.error = Some(error);
    }
    result
}

fn failed(base: &str, error: String) -> GitCompare {
    GitCompare {
        base: base.to_owned(),
        error: Some(error),
        ..Default::default()
    }
}

fn fill(repo: &Repository, base: &str, out: &mut GitCompare) -> Result<(), String> {
    let base_oid = resolve(repo, base)?;
    let head_oid = repo
        .head()
        .and_then(|head| head.peel_to_commit())
        .map_err(err)?
        .id();
    let merge_base = repo.merge_base(head_oid, base_oid).map_err(err)?;
    out.merge_base = merge_base.to_string();

    let mut walk = repo.revwalk().map_err(err)?;
    walk.push(head_oid).map_err(err)?;
    walk.hide(merge_base).map_err(err)?;
    walk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME)
        .map_err(err)?;
    for oid in walk.filter_map(Result::ok) {
        if out.commits.len() == MAX_COMPARE_COMMITS {
            out.commits_truncated = true;
            break;
        }
        if let Ok(commit) = repo.find_commit(oid) {
            out.commits.push(super::commit_entry(&commit));
        }
    }

    let tree = repo
        .find_commit(merge_base)
        .and_then(|c| c.tree())
        .map_err(err)?;
    let mut opts = DiffOptions::new();
    opts.context_lines(0)
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .show_untracked_content(true);
    let mut diff = repo
        .diff_tree_to_workdir_with_index(Some(&tree), Some(&mut opts))
        .map_err(err)?;
    let mut find = DiffFindOptions::new();
    find.renames(true).for_untracked(true);
    let _ = diff.find_similar(Some(&mut find));

    for index in 0..diff.deltas().len() {
        let Some(delta) = diff.get_delta(index) else {
            continue;
        };
        let new_path = delta.new_file().path().map(path_string);
        let old_path = delta.old_file().path().map(path_string);
        let Some(path) = new_path.clone().or_else(|| old_path.clone()) else {
            continue;
        };
        let status = match delta.status() {
            git2::Delta::Added | git2::Delta::Untracked => 'A',
            git2::Delta::Deleted => 'D',
            git2::Delta::Renamed => 'R',
            git2::Delta::Copied => 'C',
            git2::Delta::Typechange => 'T',
            git2::Delta::Conflicted => 'U',
            _ => 'M',
        };
        let (mut added, mut deleted) = (0, 0);
        if let Ok(Some(patch)) = Patch::from_diff(&diff, index) {
            if let Ok((_, adds, dels)) = patch.line_stats() {
                (added, deleted) = (adds, dels);
            }
            if status != 'D' {
                let lines = out.line_changes.entry(path.clone()).or_default();
                for hunk in 0..patch.num_hunks() {
                    let Ok((hunk, _)) = patch.hunk(hunk) else {
                        continue;
                    };
                    let kind = if hunk.old_lines() == 0 {
                        GitLineKind::Added
                    } else {
                        GitLineKind::Modified
                    };
                    let start = hunk.new_start() as usize;
                    lines.extend((start..start + hunk.new_lines() as usize).map(|n| {
                        GitLineChange {
                            line: n.saturating_sub(1),
                            kind,
                        }
                    }));
                }
            }
        }
        out.files.push(CompareFile {
            old_path: (status == 'R' || status == 'C')
                .then_some(old_path)
                .flatten()
                .filter(|old| *old != path),
            path,
            status,
            added,
            deleted,
        });
    }
    out.files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(())
}

/// One file's change since the merge base with `base`, with the whole file as context.
pub(super) fn compare_file_diff(
    repo: &Repository,
    base: &str,
    path: &str,
    old_path: Option<&str>,
) -> Result<String, String> {
    let base_oid = resolve(repo, base)?;
    let head_oid = repo
        .head()
        .and_then(|head| head.peel_to_commit())
        .map_err(err)?
        .id();
    let merge_base = repo.merge_base(head_oid, base_oid).map_err(err)?;
    let tree = repo
        .find_commit(merge_base)
        .and_then(|c| c.tree())
        .map_err(err)?;
    let mut opts = DiffOptions::new();
    opts.context_lines(FULL_FILE_CONTEXT)
        .include_untracked(true)
        .show_untracked_content(true)
        .disable_pathspec_match(true)
        .pathspec(path);
    if let Some(old_path) = old_path {
        opts.pathspec(old_path);
    }
    let mut diff = repo
        .diff_tree_to_workdir_with_index(Some(&tree), Some(&mut opts))
        .map_err(err)?;
    if old_path.is_some() {
        let mut find = DiffFindOptions::new();
        find.renames(true).for_untracked(true);
        let _ = diff.find_similar(Some(&mut find));
    }
    diff_text_limited(&diff, MAX_FILE_DIFF_CHARS)
}

fn resolve(repo: &Repository, name: &str) -> Result<git2::Oid, String> {
    repo.revparse_single(name)
        .and_then(|object| object.peel_to_commit())
        .map(|commit| commit.id())
        .map_err(|e| format!("Cannot resolve {name}: {}", e.message()))
}

fn path_string(path: &std::path::Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn base_candidates(repo: &Repository, current: &str) -> Vec<String> {
    let mut out = Vec::new();
    for kind in [BranchType::Local, BranchType::Remote] {
        let mut names = repo
            .branches(Some(kind))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|(b, _)| b.name().ok().flatten().map(str::to_owned))
            .filter(|name| name != current && !name.ends_with("/HEAD"))
            .collect::<Vec<_>>();
        names.sort();
        out.extend(names);
    }
    out
}

/// The branch a pull request would target: the remote's default branch (like GitHub), then
/// a local `main`/`master`.
fn default_base(repo: &Repository, candidates: &[String]) -> Option<String> {
    let remote_head = repo
        .find_reference("refs/remotes/origin/HEAD")
        .ok()
        .and_then(|r| r.symbolic_target().ok().flatten().map(str::to_owned))
        .and_then(|target| target.strip_prefix("refs/remotes/").map(str::to_owned));
    remote_head
        .into_iter()
        .chain(
            [
                "origin/main",
                "origin/master",
                "main",
                "master",
                "develop",
                "dev",
            ]
            .map(str::to_owned),
        )
        .find(|name| candidates.contains(name))
        .or_else(|| candidates.first().cloned())
}
