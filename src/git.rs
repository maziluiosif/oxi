//! Native Git integration for the source-control panel.
//!
//! All repository operations use `git2`/libgit2. No `git` executable is spawned. Work runs on a
//! background thread so repository and network I/O never blocks egui.

use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use git2::{
    BranchType, Cred, CredentialType, Diff, DiffFormat, DiffOptions, FetchOptions, IndexAddOption,
    ObjectType, Oid, PushOptions, RemoteCallbacks, Repository, ResetType, Sort, Status,
    StatusOptions, build::CheckoutBuilder,
};

const MAX_DIFF_CHARS: usize = 200_000;

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
}

#[derive(Debug, Clone)]
pub enum GitOp {
    Refresh,
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
    pub tx: Sender<GitOp>,
    pub rx: Receiver<GitState>,
}

impl GitChannels {
    pub fn new(cwd: String, ctx: egui::Context) -> Self {
        let (op_tx, op_rx) = mpsc::channel();
        let (snap_tx, snap_rx) = mpsc::channel();
        let _ = thread::Builder::new()
            .name("oxi-git".into())
            .spawn(move || git_worker(cwd, op_rx, snap_tx, ctx));
        Self {
            tx: op_tx,
            rx: snap_rx,
        }
    }
}

fn git_worker(cwd: String, rx: Receiver<GitOp>, tx: Sender<GitState>, ctx: egui::Context) {
    let mut cwd = cwd;
    let _ = tx.send(GitState {
        busy: true,
        last_op: Some("refresh".into()),
        ..Default::default()
    });
    for op in rx {
        if let GitOp::SetCwd(path) = op {
            cwd = path;
            let _ = tx.send(handle_op(&cwd, GitOp::Refresh));
            ctx.request_repaint();
            continue;
        }
        let _ = tx.send(GitState {
            busy: true,
            last_op: Some(label_op(&op).into()),
            ..Default::default()
        });
        let _ = tx.send(handle_op(&cwd, op));
        ctx.request_repaint();
    }
}

fn label_op(op: &GitOp) -> &'static str {
    match op {
        GitOp::Refresh => "refresh",
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
    let mut opts = DiffOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .show_untracked_content(true);
    if let Some(path) = path {
        opts.pathspec(path);
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
    let mut bytes = Vec::new();
    diff.print(DiffFormat::Patch, |_delta, _hunk, line| {
        if matches!(line.origin(), '+' | '-' | ' ') {
            bytes.push(line.origin() as u8);
        }
        bytes.extend_from_slice(line.content());
        true
    })
    .map_err(err)?;
    Ok(truncate(&String::from_utf8_lossy(&bytes), MAX_DIFF_CHARS))
}

fn show_diff(repo: &Repository, path: &str, staged: bool) -> String {
    make_diff(repo, staged, Some(path))
        .and_then(|d| diff_text(&d))
        .unwrap_or_else(|e| e)
}

fn working_tree_line_changes(
    repo: &Repository,
    unstaged: &[GitEntry],
) -> HashMap<String, Vec<GitLineChange>> {
    let mut changes = HashMap::new();
    let mut line_opts = DiffOptions::new();
    line_opts
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .context_lines(0);
    let tree = head_tree(repo);
    if let Ok(diff) = repo.diff_tree_to_workdir_with_index(tree.as_ref(), Some(&mut line_opts)) {
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
    let line_changes = working_tree_line_changes(repo, &unstaged);
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
            GitOp::Refresh => {}
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

fn github_credentials() -> (String, String) {
    let settings = crate::settings::AppSettings::load();
    let secrets = crate::secrets::load_unified();
    (
        settings.github_username.trim().to_owned(),
        secrets.github_token.trim().to_owned(),
    )
}

fn remote_callbacks() -> RemoteCallbacks<'static> {
    let (configured_username, token) = github_credentials();
    let mut callbacks = RemoteCallbacks::new();
    callbacks.credentials(move |url, username_url, allowed| {
        // GitHub accepts a PAT as the HTTP password and requires a non-empty username. Use the
        // configured account name when available; otherwise derive it from an HTTPS GitHub URL.
        let url_username = url::Url::parse(url)
            .ok()
            .map(|u| u.username().to_owned())
            .filter(|u| !u.is_empty());
        let username = if !configured_username.is_empty() {
            configured_username.as_str()
        } else {
            username_url
                .or(url_username.as_deref())
                .unwrap_or("x-access-token")
        };
        if allowed.contains(CredentialType::USER_PASS_PLAINTEXT) && !token.is_empty() {
            return Cred::userpass_plaintext(username, &token);
        }
        if allowed.contains(CredentialType::SSH_KEY) {
            return Cred::ssh_key_from_agent(username_url.unwrap_or("git"));
        }
        if allowed.contains(CredentialType::USERNAME) {
            return Cred::username(username);
        }
        Cred::default()
    });
    callbacks
}

fn remote_url(repo: &Repository, name: &str) -> String {
    repo.find_remote(name)
        .ok()
        .and_then(|r| r.url().ok().map(str::to_owned))
        .unwrap_or_default()
}

fn remote_error(repo: &Repository, remote: &str, operation: &str, error: git2::Error) -> String {
    let url = remote_url(repo, remote);
    let is_github_https =
        url.starts_with("https://github.com/") || url.starts_with("http://github.com/");
    let is_403 = error.code() == git2::ErrorCode::Auth || error.message().contains("403");
    if is_github_https && is_403 {
        let (_, token) = github_credentials();
        if token.is_empty() {
            return format!(
                "GitHub rejected {operation}: no token is configured. Open Settings → GitHub and save a personal access token with repository Contents: Read and write permission."
            );
        }
        return format!(
            "GitHub rejected {operation} with HTTP 403. Authentication reached GitHub, but this token cannot write to the repository. For a fine-grained token, grant access to this repository and Repository permissions → Contents: Read and write. For an organization repository, also authorize the token for SSO and check that the organization approved it. Remote: {url}"
        );
    }
    format!("{operation} failed for {url}: {error}")
}

fn fetch(repo: &Repository) -> Result<(), String> {
    let mut remote = repo.find_remote("origin").map_err(err)?;
    let mut options = FetchOptions::new();
    options.remote_callbacks(remote_callbacks());
    remote
        .fetch(&[] as &[&str], Some(&mut options), None)
        .map_err(|e| remote_error(repo, "origin", "fetch", e))
}

/// Integrate the current branch's upstream after a fetch. Fast-forwards when possible and
/// creates a regular two-parent merge commit when local and remote histories diverged.
///
/// A dirty worktree is rejected before integration. If libgit2 detects merge conflicts, the
/// attempted merge is rolled back to the original HEAD so a network button can never leave the
/// user's repository half-merged.
fn integrate_upstream(repo: &Repository) -> Result<(), String> {
    let branch = current_branch(repo);
    if branch.is_empty() || branch == "HEAD" {
        return Err("Cannot pull while HEAD is detached".into());
    }
    let local = repo.find_branch(&branch, BranchType::Local).map_err(err)?;
    let upstream = local
        .upstream()
        .map_err(|_| "The current branch has no upstream".to_string())?;
    let upstream_name = upstream
        .name()
        .ok()
        .flatten()
        .unwrap_or("upstream")
        .to_owned();
    let upstream_ref = upstream.get();
    let upstream_oid = upstream_ref.target().ok_or("Upstream has no target")?;
    let annotated = repo
        .reference_to_annotated_commit(upstream_ref)
        .map_err(err)?;
    let (analysis, _) = repo.merge_analysis(&[&annotated]).map_err(err)?;
    if analysis.is_up_to_date() {
        return Ok(());
    }

    let refname = format!("refs/heads/{branch}");
    if analysis.is_fast_forward() {
        // Update the worktree to the fetched commit *before* moving the branch reference, using a
        // safe checkout. While HEAD still points at the old commit, libgit2 uses it as the merge
        // base: files the fast-forward does not touch keep their local modifications (matching
        // `git pull`, which does not require a clean tree), and a locally-modified file that the
        // fast-forward would overwrite aborts the checkout instead of being clobbered. Only once
        // the worktree is in place do we advance the reference, so an unrelated dirty file never
        // blocks the pull and a relevant one is never lost.
        //
        // (Moving the reference first would force a checkout: HEAD would already match the target,
        // so a safe checkout would treat the still-old index as staged changes and preserve it
        // instead of completing the fast-forward — which is why this must run in this order.)
        let target = repo.find_object(upstream_oid, None).map_err(err)?;
        let mut checkout = CheckoutBuilder::new();
        checkout.safe();
        repo.checkout_tree(&target, Some(&mut checkout))
            .map_err(|e| {
                format!(
                    "Cannot pull: your local changes to files updated by {upstream_name} would be overwritten. Commit, stash, or discard them first. ({e})"
                )
            })?;
        repo.reference(&refname, upstream_oid, true, "pull: fast-forward")
            .map_err(err)?;
        return repo.set_head(&refname).map_err(err);
    }

    let original = repo.head().and_then(|h| h.peel_to_commit()).map_err(err)?;
    let remote_commit = repo.find_commit(upstream_oid).map_err(err)?;
    // Resolve identity before changing index/worktree, so a missing author config cannot leave a
    // merge in progress.
    let signature = author_signature(repo)?;
    let mut checkout = CheckoutBuilder::new();
    checkout.safe();
    if let Err(error) = repo.merge(&[&annotated], None, Some(&mut checkout)) {
        // A safe merge checkout aborts if a locally-modified file would be overwritten. Nothing
        // was committed, but libgit2 may have recorded merge state; clear it so the repository is
        // left exactly as it was found.
        let _ = repo.cleanup_state();
        return Err(format!(
            "Cannot pull: your local changes to files updated by {upstream_name} would be overwritten. Commit, stash, or discard them first. ({error})"
        ));
    }

    let mut index = repo.index().map_err(err)?;
    if index.has_conflicts() {
        // Restore both index and worktree. Conflict resolution UI is intentionally not forced on
        // the user as a side effect of pressing Push.
        let object = original.as_object();
        let rollback = repo.reset(object, ResetType::Hard, None).map_err(err);
        let _ = repo.cleanup_state();
        rollback?;
        return Err(format!(
            "Local and {upstream_name} have conflicting changes. Automatic sync was rolled back safely; merge the branches manually or resolve the conflicting commits before pushing."
        ));
    }

    let merge_result = (|| -> Result<(), String> {
        let tree_oid = index.write_tree().map_err(err)?;
        let tree = repo.find_tree(tree_oid).map_err(err)?;
        let message = format!("Merge {upstream_name} into {branch}");
        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            &message,
            &tree,
            &[&original, &remote_commit],
        )
        .map_err(err)?;
        repo.cleanup_state().map_err(err)
    })();
    if let Err(error) = merge_result {
        let rollback = repo
            .reset(original.as_object(), ResetType::Hard, None)
            .map_err(err);
        let _ = repo.cleanup_state();
        rollback?;
        return Err(format!(
            "Automatic sync failed and was rolled back safely: {error}"
        ));
    }
    Ok(())
}

fn pull(repo: &Repository) -> Result<(), String> {
    fetch(repo)?;
    integrate_upstream(repo)
}

fn push_once(repo: &Repository, branch: &str) -> Result<(), git2::Error> {
    let mut remote = repo.find_remote("origin")?;
    let mut options = PushOptions::new();
    options.remote_callbacks(remote_callbacks());
    let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
    remote.push(&[&refspec], Some(&mut options))
}

fn push(repo: &Repository) -> Result<(), String> {
    let branch = current_branch(repo);
    if branch.is_empty() || branch == "HEAD" {
        return Err("Cannot push while HEAD is detached".into());
    }

    match push_once(repo, &branch) {
        Ok(()) => {}
        Err(error) if error.code() == git2::ErrorCode::NotFastForward => {
            // Match the useful part of `git push`: refresh the remote tracking branch, safely
            // integrate it, then retry. Never force-push and never overwrite remote history.
            fetch(repo)?;
            integrate_upstream(repo)?;
            push_once(repo, &branch).map_err(|e| remote_error(repo, "origin", "push", e))?;
        }
        Err(error) => return Err(remote_error(repo, "origin", "push", error)),
    }

    let mut local = repo.find_branch(&branch, BranchType::Local).map_err(err)?;
    if local.upstream().is_err() {
        local
            .set_upstream(Some(&format!("origin/{branch}")))
            .map_err(err)?;
    }
    Ok(())
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
