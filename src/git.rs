//! Native Git integration for the source-control panel.
//!
//! All repository operations use `git2`/libgit2. No `git` executable is spawned unless the user
//! opts into system Git for network operations (see `git/system.rs`). Work runs on a background
//! thread so repository and network I/O never blocks egui.

use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

#[path = "git/network.rs"]
mod network;
use network::{fetch, pull, push};

#[path = "git/system.rs"]
mod system;
pub use system::version as system_git_version;

#[cfg(test)]
#[path = "git/tests.rs"]
mod tests;

use git2::{
    BranchType, Diff, DiffFormat, DiffOptions, IndexAddOption, ObjectType, Oid, Repository, Sort,
    Status, StatusOptions, build::CheckoutBuilder,
};

const MAX_DIFF_CHARS: usize = 200_000;
/// Single-file diffs include every unchanged line, so they get a larger budget.
const MAX_FILE_DIFF_CHARS: usize = 2_000_000;
const FULL_FILE_CONTEXT: u32 = 1_000_000;

#[derive(Debug, Clone, Default)]
pub struct GitEntry {
    pub path: String,
    #[allow(dead_code)]
    pub code: String,
    pub status: char,
    #[allow(dead_code)]
    pub conflict: bool,
}

#[derive(Debug, Clone)]
pub struct GitCommit {
    pub hash: String,
    pub date: String,
    pub author: String,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitLineKind {
    Added,
    Modified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GitLineChange {
    pub line: usize,
    pub kind: GitLineKind,
}

#[derive(Debug, Clone, Default)]
pub struct GitState {
    pub repo: bool,
    pub branch: String,
    pub branches: Vec<String>,
    pub ahead: usize,
    pub behind: usize,
    pub staged: Vec<GitEntry>,
    pub unstaged: Vec<GitEntry>,
    pub line_changes: HashMap<String, Vec<GitLineChange>>,
    pub log: Vec<GitCommit>,
    pub diff: Option<(String, String)>,
    pub error: Option<String>,
    pub busy: bool,
    pub last_op: Option<String>,
    pub current_diff_path: Option<String>,
    pub current_diff_staged: Option<bool>,
    pub commit_diff: Option<String>,
    /// Counts diff-view changes (open file/commit, close, ops that reset the view). A state
    /// whose generation is older than the one already shown must not replace its diff.
    pub view_generation: u64,
    /// Only `diff`, `current_diff_*` and `error` are meaningful: an answer of the diff worker,
    /// merged into the last full snapshot.
    pub diff_only: bool,
}

#[derive(Debug, Clone)]
pub enum GitOp {
    Refresh,
    AutoRefresh,
    Stage(Vec<String>),
    Unstage(Vec<String>),
    Discard(Vec<String>),
    Commit(String),
    Checkout(String),
    NewBranch(String),
    ShowCommit(String),
    ShowDiff { path: String, staged: bool },
    ClearDiff,
    Pull,
    Push,
    Fetch,
    SetCwd(String),
    CollectCommitDiff,
}

pub struct GitChannels {
    pub tx: GitSender,
    pub rx: Receiver<GitState>,
}

/// What the diff view shows, shared by the UI-side sender and both workers.
#[derive(Default)]
struct ViewState {
    cwd: String,
    view: Option<GitOp>,
    generation: u64,
}

/// Routes UI requests: opening a file or commit diff goes to a dedicated diff worker, so a
/// click is answered at once instead of queueing behind a whole-repository status scan (an
/// auto refresh) and then running one itself; that took seconds in large repositories.
#[derive(Clone)]
pub struct GitSender {
    main: Sender<(GitOp, u64)>,
    diff: Sender<(GitOp, u64)>,
    shared: Arc<Mutex<ViewState>>,
}

impl GitSender {
    pub fn send(&self, op: GitOp) -> Result<(), mpsc::SendError<GitOp>> {
        let mut shared = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        let (target, generation) = match &op {
            GitOp::ShowDiff { .. } | GitOp::ShowCommit(_) | GitOp::ClearDiff => {
                shared.generation += 1;
                shared.view = (!matches!(op, GitOp::ClearDiff)).then(|| op.clone());
                (&self.diff, shared.generation)
            }
            // Re-reads the current view when it runs.
            GitOp::AutoRefresh => (&self.main, 0),
            other => {
                if let GitOp::SetCwd(path) = other {
                    shared.cwd = path.clone();
                }
                // Every other operation answers with a snapshot without a diff.
                shared.generation += 1;
                shared.view = None;
                (&self.main, shared.generation)
            }
        };
        drop(shared);
        target
            .send((op, generation))
            .map_err(|error| mpsc::SendError(error.0.0))
    }
}

impl GitChannels {
    pub fn new(cwd: String, ctx: egui::Context) -> Self {
        let (op_tx, op_rx) = mpsc::channel();
        let (diff_tx, diff_rx) = mpsc::channel();
        let (snap_tx, snap_rx) = mpsc::channel();
        let shared = Arc::new(Mutex::new(ViewState {
            cwd: cwd.clone(),
            ..Default::default()
        }));
        {
            let (shared, snap_tx, ctx) = (Arc::clone(&shared), snap_tx.clone(), ctx.clone());
            let _ = thread::Builder::new()
                .name("oxi-git-diff".into())
                .spawn(move || diff_worker(diff_rx, snap_tx, shared, ctx));
        }
        {
            let shared = Arc::clone(&shared);
            let _ = thread::Builder::new()
                .name("oxi-git".into())
                .spawn(move || git_worker(cwd, op_rx, snap_tx, shared, ctx));
        }
        Self {
            tx: GitSender {
                main: op_tx,
                diff: diff_tx,
                shared,
            },
            rx: snap_rx,
        }
    }
}

fn git_worker(
    cwd: String,
    rx: Receiver<(GitOp, u64)>,
    tx: Sender<GitState>,
    shared: Arc<Mutex<ViewState>>,
    ctx: egui::Context,
) {
    let mut cwd = cwd;
    let _ = tx.send(GitState {
        busy: true,
        last_op: Some("refresh".into()),
        ..Default::default()
    });
    for (op, generation) in rx {
        if let GitOp::SetCwd(path) = op {
            cwd = path;
            let mut state = handle_op(&cwd, GitOp::Refresh);
            state.view_generation = generation;
            let _ = tx.send(state);
            ctx.request_repaint();
            continue;
        }
        if matches!(op, GitOp::AutoRefresh) {
            let (view, generation) = {
                let shared = shared.lock().unwrap_or_else(|e| e.into_inner());
                (shared.view.clone(), shared.generation)
            };
            let mut state = auto_refresh(&cwd, view.as_ref());
            state.view_generation = generation;
            let _ = tx.send(state);
        } else {
            let _ = tx.send(GitState {
                busy: true,
                last_op: Some(label_op(&op).into()),
                ..Default::default()
            });
            let collecting_diff = matches!(op, GitOp::CollectCommitDiff);
            let mut state = handle_op(&cwd, op);
            if collecting_diff {
                state.last_op = Some("collect commit diff".into());
            }
            state.view_generation = generation;
            let _ = tx.send(state);
        }
        ctx.request_repaint();
    }
}

/// Answers diff-view requests. Only the newest queued request is computed: clicking through
/// a list of files or commits never builds the diffs that were already skipped past.
fn diff_worker(
    rx: Receiver<(GitOp, u64)>,
    tx: Sender<GitState>,
    shared: Arc<Mutex<ViewState>>,
    ctx: egui::Context,
) {
    while let Ok(mut job) = rx.recv() {
        while let Ok(newer) = rx.try_recv() {
            job = newer;
        }
        let (op, generation) = job;
        let cwd = {
            let shared = shared.lock().unwrap_or_else(|e| e.into_inner());
            if shared.generation != generation {
                // Superseded by an operation on the main worker (which resets the view).
                continue;
            }
            shared.cwd.clone()
        };
        let mut state = view_diff(&cwd, op);
        state.view_generation = generation;
        state.diff_only = true;
        state.last_op = Some("diff".into());
        let _ = tx.send(state);
        ctx.request_repaint();
    }
}

/// The diff-view part of a snapshot for `op`, without the repository status.
fn view_diff(cwd: &str, op: GitOp) -> GitState {
    let repo = match open_repo(cwd) {
        Ok(repo) if !repo.is_bare() => repo,
        _ => return GitState::default(),
    };
    match op {
        GitOp::ShowDiff { path, staged } => GitState {
            diff: Some((
                if staged {
                    format!("Staged: {path}")
                } else {
                    path.clone()
                },
                show_diff(&repo, &path, staged),
            )),
            current_diff_path: Some(path),
            current_diff_staged: Some(staged),
            ..Default::default()
        },
        GitOp::ShowCommit(hash) => match show_commit(&repo, &hash) {
            Ok(text) => GitState {
                diff: Some((format!("Commit {hash}"), text)),
                current_diff_path: Some(hash),
                current_diff_staged: Some(true),
                ..Default::default()
            },
            Err(error) => GitState {
                error: Some(error),
                ..Default::default()
            },
        },
        _ => GitState::default(),
    }
}

fn auto_refresh(cwd: &str, diff_view: Option<&GitOp>) -> GitState {
    let mut state = handle_op(cwd, diff_view.cloned().unwrap_or(GitOp::Refresh));
    state.last_op = Some("auto refresh".into());
    state
}

fn label_op(op: &GitOp) -> &'static str {
    match op {
        GitOp::Refresh | GitOp::AutoRefresh => "refresh",
        GitOp::Stage(_) => "stage",
        GitOp::Unstage(_) => "unstage",
        GitOp::Discard(_) => "discard",
        GitOp::Commit(_) => "commit",
        GitOp::Checkout(_) => "checkout",
        GitOp::NewBranch(_) => "new branch",
        GitOp::ShowCommit(_) => "show",
        GitOp::ShowDiff { .. } | GitOp::ClearDiff | GitOp::CollectCommitDiff => "diff",
        GitOp::Pull => "pull",
        GitOp::Push => "push",
        GitOp::Fetch => "fetch",
        GitOp::SetCwd(_) => "switch",
    }
}

fn open_repo(cwd: &str) -> Result<Repository, String> {
    Repository::discover(cwd).map_err(|e| e.message().to_string())
}

fn repo_root(repo: &Repository) -> Result<&Path, String> {
    repo.workdir()
        .ok_or_else(|| "Bare repositories are not supported".into())
}

fn current_branch(repo: &Repository) -> String {
    repo.head()
        .ok()
        .and_then(|h| h.shorthand().ok().map(str::to_owned))
        .unwrap_or_default()
}

fn list_branches(repo: &Repository) -> Vec<String> {
    let mut names = repo
        .branches(Some(BranchType::Local))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|(b, _)| b.name().ok().flatten().map(str::to_owned))
        .collect::<Vec<_>>();
    names.sort();
    names
}

fn ahead_behind(repo: &Repository) -> (usize, usize) {
    let Ok(head) = repo.head() else { return (0, 0) };
    let Some(local_oid) = head.target() else {
        return (0, 0);
    };
    let Ok(name) = head.shorthand() else {
        return (0, 0);
    };
    let Ok(upstream) = repo
        .find_branch(name, BranchType::Local)
        .and_then(|b| b.upstream())
    else {
        return (0, 0);
    };
    let Some(upstream_oid) = upstream.get().target() else {
        return (0, 0);
    };
    repo.graph_ahead_behind(local_oid, upstream_oid)
        .unwrap_or((0, 0))
}

fn status_entries(repo: &Repository) -> Result<(Vec<GitEntry>, Vec<GitEntry>), String> {
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .renames_head_to_index(true)
        .renames_index_to_workdir(true);
    let statuses = repo.statuses(Some(&mut opts)).map_err(err)?;
    let mut staged = Vec::new();
    let mut unstaged = Vec::new();
    for entry in statuses.iter() {
        let status = entry.status();
        let path = entry.path().unwrap_or("(non-UTF-8 path)").to_owned();
        let conflict = status.contains(Status::CONFLICTED);
        let index_status = if status.contains(Status::INDEX_NEW) {
            Some('A')
        } else if status.contains(Status::INDEX_MODIFIED) {
            Some('M')
        } else if status.contains(Status::INDEX_DELETED) {
            Some('D')
        } else if status.contains(Status::INDEX_RENAMED) {
            Some('R')
        } else if status.contains(Status::INDEX_TYPECHANGE) {
            Some('T')
        } else {
            None
        };
        let work_status = if status.contains(Status::WT_NEW) {
            Some('?')
        } else if status.contains(Status::WT_MODIFIED) {
            Some('M')
        } else if status.contains(Status::WT_DELETED) {
            Some('D')
        } else if status.contains(Status::WT_RENAMED) {
            Some('R')
        } else if status.contains(Status::WT_TYPECHANGE) {
            Some('T')
        } else if conflict {
            Some('U')
        } else {
            None
        };
        if let Some(s) = index_status {
            staged.push(GitEntry {
                path: path.clone(),
                code: format!("{s} "),
                status: s,
                conflict,
            });
        }
        if let Some(s) = work_status {
            unstaged.push(GitEntry {
                path,
                code: format!(" {s}"),
                status: s,
                conflict,
            });
        }
    }
    Ok((staged, unstaged))
}

fn log_entries(repo: &Repository) -> Vec<GitCommit> {
    let Ok(mut walk) = repo.revwalk() else {
        return Vec::new();
    };
    if walk.push_head().is_err() {
        return Vec::new();
    }
    let _ = walk.set_sorting(Sort::TIME);
    walk.take(60)
        .filter_map(Result::ok)
        .filter_map(|oid| repo.find_commit(oid).ok())
        .map(|c| {
            let secs = c.time().seconds();
            let date = chrono::DateTime::from_timestamp(secs, 0)
                .map(|d| d.format("%Y-%m-%d").to_string())
                .unwrap_or_default();
            GitCommit {
                hash: c.id().to_string(),
                date,
                author: c.author().name().unwrap_or("Unknown").to_owned(),
                message: c.summary().ok().flatten().unwrap_or("").to_owned(),
            }
        })
        .collect()
}

fn head_tree(repo: &Repository) -> Option<git2::Tree<'_>> {
    repo.head().ok()?.peel_to_tree().ok()
}

fn make_diff<'a>(
    repo: &'a Repository,
    staged: bool,
    path: Option<&str>,
) -> Result<Diff<'a>, String> {
    make_diff_with_context(repo, staged, path, 3)
}

fn make_diff_with_context<'a>(
    repo: &'a Repository,
    staged: bool,
    path: Option<&str>,
    context_lines: u32,
) -> Result<Diff<'a>, String> {
    let mut opts = DiffOptions::new();
    opts.context_lines(context_lines)
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .show_untracked_content(true);
    if let Some(path) = path {
        // A literal path lets libgit2 visit only that file instead of the whole work tree.
        opts.pathspec(path).disable_pathspec_match(true);
    }
    if staged {
        let tree = head_tree(repo);
        repo.diff_tree_to_index(tree.as_ref(), None, Some(&mut opts))
            .map_err(err)
    } else {
        repo.diff_index_to_workdir(None, Some(&mut opts))
            .map_err(err)
    }
}

fn diff_text(diff: &Diff<'_>) -> Result<String, String> {
    diff_text_limited(diff, MAX_DIFF_CHARS)
}

fn diff_text_limited(diff: &Diff<'_>, max_chars: usize) -> Result<String, String> {
    let mut bytes = Vec::new();
    diff.print(DiffFormat::Patch, |_delta, _hunk, line| {
        if matches!(line.origin(), '+' | '-' | ' ') {
            bytes.push(line.origin() as u8);
        }
        bytes.extend_from_slice(line.content());
        true
    })
    .map_err(err)?;
    Ok(truncate(&String::from_utf8_lossy(&bytes), max_chars))
}

/// One file's diff with the whole file as context: the diff view folds unchanged regions
/// itself and can expand them, like an editor's diff view.
fn show_diff(repo: &Repository, path: &str, staged: bool) -> String {
    make_diff_with_context(repo, staged, Some(path), FULL_FILE_CONTEXT)
        .and_then(|d| diff_text_limited(&d, MAX_FILE_DIFF_CHARS))
        .unwrap_or_else(|e| e)
}

fn working_tree_line_changes(
    repo: &Repository,
    staged: &[GitEntry],
    unstaged: &[GitEntry],
) -> HashMap<String, Vec<GitLineChange>> {
    let mut changes = HashMap::new();
    // Status already knows which files changed. Diffing just those (as literal paths) avoids a
    // second scan of the whole work tree, which dominated refreshes in large repositories.
    let changed: Vec<&str> = staged
        .iter()
        .chain(unstaged)
        .filter(|entry| entry.status != '?' && entry.status != 'D')
        .map(|entry| entry.path.as_str())
        .collect();
    let mut line_opts = DiffOptions::new();
    line_opts.context_lines(0).disable_pathspec_match(true);
    for path in &changed {
        line_opts.pathspec(path);
    }
    let tree = head_tree(repo);
    if !changed.is_empty()
        && let Ok(diff) = repo.diff_tree_to_workdir_with_index(tree.as_ref(), Some(&mut line_opts))
    {
        let _ = diff.foreach(
            &mut |_delta, _| true,
            None,
            Some(&mut |delta, hunk| {
                let Some(path) = delta.new_file().path().or_else(|| delta.old_file().path()) else {
                    return true;
                };
                let kind = if hunk.old_lines() == 0 {
                    GitLineKind::Added
                } else {
                    GitLineKind::Modified
                };
                let list = changes
                    .entry(path.to_string_lossy().into_owned())
                    .or_insert_with(Vec::new);
                for n in hunk.new_start()..hunk.new_start().saturating_add(hunk.new_lines()) {
                    list.push(GitLineChange {
                        line: (n as usize).saturating_sub(1),
                        kind,
                    });
                }
                true
            }),
            None,
        );
    }
    if let Ok(root) = repo_root(repo) {
        for entry in unstaged.iter().filter(|e| e.status == '?') {
            if let Ok(content) = std::fs::read_to_string(root.join(&entry.path)) {
                changes.insert(
                    entry.path.clone(),
                    (0..content.split('\n').count().max(1))
                        .map(|line| GitLineChange {
                            line,
                            kind: GitLineKind::Added,
                        })
                        .collect(),
                );
            }
        }
    }
    changes
}

fn snapshot(
    repo: &Repository,
    diff_pref: Option<(String, bool)>,
    preset: Option<(String, String)>,
    error: Option<String>,
) -> GitState {
    let branch = current_branch(repo);
    let branches = list_branches(repo);
    let (ahead, behind) = ahead_behind(repo);
    let (staged, unstaged) = status_entries(repo).unwrap_or_default();
    let line_changes = working_tree_line_changes(repo, &staged, &unstaged);
    let diff = preset.or_else(|| {
        diff_pref.as_ref().map(|(p, s)| {
            (
                if *s {
                    format!("Staged: {p}")
                } else {
                    p.clone()
                },
                show_diff(repo, p, *s),
            )
        })
    });
    GitState {
        repo: true,
        branch,
        branches,
        ahead,
        behind,
        staged,
        unstaged,
        line_changes,
        log: log_entries(repo),
        diff,
        error,
        busy: false,
        last_op: None,
        current_diff_path: diff_pref.as_ref().map(|x| x.0.clone()),
        current_diff_staged: diff_pref.map(|x| x.1),
        commit_diff: None,
        view_generation: 0,
        diff_only: false,
    }
}

fn non_repo(error: Option<String>) -> GitState {
    GitState {
        error,
        ..Default::default()
    }
}

fn handle_op(cwd: &str, op: GitOp) -> GitState {
    let repo = match open_repo(cwd) {
        Ok(repo) if !repo.is_bare() => repo,
        Ok(_) => return non_repo(Some("Bare repositories are not supported".into())),
        Err(_) => return non_repo(None),
    };
    let result: Result<Option<GitState>, String> = (|| {
        match op {
            GitOp::Refresh | GitOp::AutoRefresh => {}
            GitOp::Stage(paths) => stage(&repo, &paths)?,
            GitOp::Unstage(paths) => unstage(&repo, &paths)?,
            GitOp::Discard(paths) => discard(&repo, &paths)?,
            GitOp::Commit(message) => commit(&repo, &message)?,
            GitOp::Checkout(branch) => checkout_branch(&repo, &branch, false)?,
            GitOp::NewBranch(branch) => checkout_branch(&repo, &branch, true)?,
            GitOp::ShowDiff { path, staged } => {
                return Ok(Some(snapshot(&repo, Some((path, staged)), None, None)));
            }
            GitOp::ClearDiff => return Ok(Some(snapshot(&repo, None, None, None))),
            GitOp::ShowCommit(hash) => {
                let text = show_commit(&repo, &hash)?;
                return Ok(Some(snapshot(
                    &repo,
                    Some((hash.clone(), true)),
                    Some((format!("Commit {hash}"), text)),
                    None,
                )));
            }
            GitOp::Fetch => fetch(&repo)?,
            GitOp::Pull => pull(&repo)?,
            GitOp::Push => push(&repo)?,
            GitOp::CollectCommitDiff => {
                let staged = diff_text(&make_diff(&repo, true, None)?)?;
                let combined = if staged.trim().is_empty() {
                    diff_text(&make_diff(&repo, false, None)?)?
                } else {
                    staged
                };
                let mut state = snapshot(&repo, None, None, None);
                if !combined.trim().is_empty() {
                    state.commit_diff = Some(combined);
                }
                return Ok(Some(state));
            }
            GitOp::SetCwd(_) => {}
        }
        Ok(None)
    })();
    match result {
        Ok(Some(state)) => state,
        Ok(None) => snapshot(&repo, None, None, None),
        Err(e) => snapshot(&repo, None, None, Some(e)),
    }
}

fn stage(repo: &Repository, paths: &[String]) -> Result<(), String> {
    let mut index = repo.index().map_err(err)?;
    index
        .add_all(
            paths.iter().map(String::as_str),
            IndexAddOption::DEFAULT,
            None,
        )
        .map_err(err)?;
    index.write().map_err(err)
}

fn unstage(repo: &Repository, paths: &[String]) -> Result<(), String> {
    if repo.is_empty().unwrap_or(true) {
        let mut index = repo.index().map_err(err)?;
        for path in paths {
            let _ = index.remove_path(Path::new(path));
        }
        return index.write().map_err(err);
    }
    let head = repo
        .head()
        .and_then(|h| h.peel(ObjectType::Commit))
        .map_err(err)?;
    repo.reset_default(Some(&head), paths.iter().map(String::as_str))
        .map_err(err)
}

fn discard(repo: &Repository, paths: &[String]) -> Result<(), String> {
    let root = repo_root(repo)?.canonicalize().map_err(|e| e.to_string())?;
    let mut checkout = CheckoutBuilder::new();
    checkout.force();
    for path in paths {
        if repo
            .status_file(Path::new(path))
            .map_err(err)?
            .contains(Status::WT_NEW)
        {
            let candidate = root.join(path);
            let parent = candidate
                .parent()
                .ok_or("Invalid path")?
                .canonicalize()
                .map_err(|e| e.to_string())?;
            if !parent.starts_with(&root) {
                return Err("Refusing to remove a path outside the repository".into());
            }
            if candidate.is_dir() {
                std::fs::remove_dir_all(candidate).map_err(|e| e.to_string())?;
            } else {
                std::fs::remove_file(candidate).map_err(|e| e.to_string())?;
            }
        } else {
            checkout.path(path);
        }
    }
    repo.checkout_index(None, Some(&mut checkout)).map_err(err)
}

fn commit(repo: &Repository, message: &str) -> Result<(), String> {
    if message.trim().is_empty() {
        return Err("Commit message is empty".into());
    }
    let sig = author_signature(repo)?;
    let mut index = repo.index().map_err(err)?;
    let tree_oid = index.write_tree().map_err(err)?;
    let tree = repo.find_tree(tree_oid).map_err(err)?;
    let parents = repo
        .head()
        .ok()
        .and_then(|h| h.peel_to_commit().ok())
        .into_iter()
        .collect::<Vec<_>>();
    let parent_refs = parents.iter().collect::<Vec<_>>();
    repo.commit(
        Some("HEAD"),
        &sig,
        &sig,
        message.trim(),
        &tree,
        &parent_refs,
    )
    .map_err(err)?;
    Ok(())
}

fn checkout_branch(repo: &Repository, name: &str, create: bool) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("Branch name is empty".into());
    }
    if create {
        let head = repo.head().and_then(|h| h.peel_to_commit()).map_err(err)?;
        repo.branch(name, &head, false).map_err(err)?;
    }
    let reference = format!("refs/heads/{name}");
    let obj = repo.revparse_single(&reference).map_err(err)?;
    let mut checkout = CheckoutBuilder::new();
    checkout.safe();
    repo.checkout_tree(&obj, Some(&mut checkout)).map_err(err)?;
    repo.set_head(&reference).map_err(err)
}

fn show_commit(repo: &Repository, hash: &str) -> Result<String, String> {
    let oid = Oid::from_str(hash).map_err(err)?;
    let commit = repo.find_commit(oid).map_err(err)?;
    let tree = commit.tree().map_err(err)?;
    let parent_tree = commit.parent(0).ok().and_then(|p| p.tree().ok());
    let diff = repo
        .diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None)
        .map_err(err)?;
    let mut out = format!(
        "commit {}\nAuthor: {}\nDate:   {}\n\n    {}\n\n",
        commit.id(),
        commit.author(),
        chrono::DateTime::from_timestamp(commit.time().seconds(), 0)
            .map(|d| d.to_rfc2822())
            .unwrap_or_default(),
        commit.message().unwrap_or("")
    );
    out.push_str(&diff_text(&diff)?);
    Ok(truncate(&out, MAX_DIFF_CHARS))
}

fn author_signature(repo: &Repository) -> Result<git2::Signature<'static>, String> {
    let settings = crate::settings::AppSettings::load();
    let name = settings.git_author_name.trim();
    let email = settings.git_author_email.trim();
    if !name.is_empty() && !email.is_empty() {
        return git2::Signature::now(name, email).map_err(err);
    }
    let signature = repo.signature().map_err(|e| {
        format!(
            "Git author identity is not configured. Set name and email in Settings → GitHub: {e}"
        )
    })?;
    git2::Signature::now(
        signature.name().unwrap_or(""),
        signature.email().unwrap_or(""),
    )
    .map_err(err)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        format!(
            "{}\n… [truncated]\n",
            s.chars().take(max).collect::<String>()
        )
    }
}

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}
