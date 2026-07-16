//! Git integration (source-control panel): shells out to the `git` CLI inside the
//! active workspace root and exposes typed snapshots (branch, staged/unstaged
//! changes, history, diffs) plus mutating ops (stage, unstage, commit, checkout).
//!
//! Execution happens on a background thread so the UI never blocks. The UI thread
//! sends a [`GitOp`] over the request channel and the worker replies with a full
//! [`GitState`] snapshot (after-action) over the response channel.

use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

/// Maximum rendered diff size (chars) before truncation — keeps giant blobs from
/// stalling the UI thread that paints the diff.
const MAX_DIFF_CHARS: usize = 200_000;

/// One paired change entry. `code` is the porcelain `XY` two-char status string.
#[derive(Debug, Clone, Default)]
pub struct GitEntry {
    pub path: String,
    /// Raw porcelain `XY` two-char code (kept for debugging / future use).
    #[allow(dead_code)]
    pub code: String,
    /// Single-letter status used for the badge / diff selection (e.g. `M`, `A`, `D`, `?`).
    pub status: char,
    /// True for unmerged (conflict) entries.
    #[allow(dead_code)]
    pub conflict: bool,
}

/// One commit in `git log`.
#[derive(Debug, Clone)]
pub struct GitCommit {
    pub hash: String,
    pub date: String,
    pub author: String,
    pub message: String,
}

#[derive(Debug, Clone, Default)]
pub struct GitState {
    /// `false` when the workspace isn't inside a git worktree.
    pub repo: bool,
    pub branch: String,
    pub branches: Vec<String>,
    pub ahead: usize,
    pub behind: usize,
    pub staged: Vec<GitEntry>,
    pub unstaged: Vec<GitEntry>,
    pub log: Vec<GitCommit>,
    /// Currently viewed diff: `(title, unified diff text)`.
    pub diff: Option<(String, String)>,
    pub error: Option<String>,
    pub busy: bool,
    pub last_op: Option<String>,
    pub current_diff_path: Option<String>,
    pub current_diff_staged: Option<bool>,
    /// Combined staged (+ unstaged fallback) diff text collected for the "generate commit
    /// message" feature. Populated by [`GitOp::CollectCommitDiff`]; the UI copies it and
    /// kicks off an LLM completion.
    pub commit_diff: Option<String>,
}

/// Operations the UI can request. Parameterized where needed.
#[derive(Debug, Clone)]
pub enum GitOp {
    /// Refresh status / branches / log / current branch.
    Refresh,
    Stage(Vec<String>),
    Unstage(Vec<String>),
    /// Discard working-tree changes (checkout -- / clean -f) per path.
    Discard(Vec<String>),
    Commit(String),
    Checkout(String),
    NewBranch(String),
    ShowCommit(String),
    ShowDiff {
        path: String,
        staged: bool,
    },
    ClearDiff,
    Pull,
    Push,
    Fetch,
    /// Change the working directory the worker shells out in.
    SetCwd(String),
    /// Gather a combined diff of staged changes (falls back to unstaged when nothing is
    /// staged) into [`GitState::commit_diff`] for the commit-message generator.
    CollectCommitDiff,
}

/// A request/response pair owned by the app.
pub struct GitChannels {
    pub tx: Sender<GitOp>,
    pub rx: Receiver<GitState>,
}

impl GitChannels {
    /// Build a fresh channel pair (UI keeps `tx`, worker reports back on `rx`).
    pub fn new(cwd: String, ctx: egui::Context) -> Self {
        let (op_tx, op_rx) = mpsc::channel::<GitOp>();
        let (snap_tx, snap_rx) = mpsc::channel::<GitState>();
        thread::Builder::new()
            .name("oxi-git".to_string())
            .spawn(move || git_worker(cwd, op_rx, snap_tx, ctx))
            .ok();
        Self {
            tx: op_tx,
            rx: snap_rx,
        }
    }
}

fn git_worker(cwd: String, op_rx: Receiver<GitOp>, snap_tx: Sender<GitState>, ctx: egui::Context) {
    use std::cell::RefCell;
    // The worktree root the worker shells out in; can change via `SetCwd`.
    let root_cell: RefCell<String> = RefCell::new(git_root(&cwd).unwrap_or_else(|| cwd.clone()));

    // Initial busy marker the UI shows while the first snapshot is being built.
    let init = GitState {
        busy: true,
        last_op: Some("refresh".to_string()),
        ..Default::default()
    };
    let _ = snap_tx.send(init);

    for op in op_rx.iter() {
        if let GitOp::SetCwd(new_cwd) = &op {
            *root_cell.borrow_mut() = git_root(new_cwd).unwrap_or_else(|| new_cwd.clone());
            let state = handle_op(&root_cell.borrow(), GitOp::Refresh);
            let _ = snap_tx.send(state);
            ctx.request_repaint();
            continue;
        }
        let labeled = label_op(&op);
        let busy = GitState {
            busy: true,
            last_op: Some(labeled.clone()),
            ..GitState::default()
        };
        let _ = snap_tx.send(busy);
        let state = handle_op(&root_cell.borrow(), op);
        let _ = snap_tx.send(state);
        ctx.request_repaint();
    }
}

fn label_op(op: &GitOp) -> String {
    match op {
        GitOp::Refresh => "refresh".to_string(),
        GitOp::Stage(_) => "stage".to_string(),
        GitOp::Unstage(_) => "unstage".to_string(),
        GitOp::Discard(_) => "discard".to_string(),
        GitOp::Commit(_) => "commit".to_string(),
        GitOp::Checkout(_) => "checkout".to_string(),
        GitOp::NewBranch(_) => "new branch".to_string(),
        GitOp::ShowCommit(_) => "show".to_string(),
        GitOp::ShowDiff { .. } => "diff".to_string(),
        GitOp::ClearDiff => "diff".to_string(),
        GitOp::Pull => "pull".to_string(),
        GitOp::Push => "push".to_string(),
        GitOp::Fetch => "fetch".to_string(),
        GitOp::SetCwd(_) => "switch".to_string(),
        GitOp::CollectCommitDiff => "diff".to_string(),
    }
}

/// Run `git` with the given args inside `cwd`, returning combined stdout/stderr.
fn git(cwd: &str, args: &[&str]) -> (bool, String) {
    let mut cmd = Command::new("git");
    cmd.current_dir(cwd);
    cmd.args(args);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    // On Windows a GUI process has no console, so each `git` spawn would otherwise pop
    // (and immediately close) a `cmd`-style window. A single sidebar refresh shells out
    // ~7 times, producing the flurry of flashing terminals users see when opening a repo.
    // `CREATE_NO_WINDOW` keeps the child headless.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    match cmd.output() {
        Ok(out) => {
            let mut s = String::from_utf8_lossy(&out.stdout).to_string();
            let err = String::from_utf8_lossy(&out.stderr);
            if !err.trim().is_empty() {
                if !s.is_empty() && !s.ends_with('\n') {
                    s.push('\n');
                }
                s.push_str(&err);
            }
            (out.status.success(), s)
        }
        Err(e) => (false, format!("git: {e}")),
    }
}

fn git_root(cwd: &str) -> Option<String> {
    let (ok, out) = git(cwd, &["rev-parse", "--show-toplevel"]);
    let out = out.trim();
    if ok && !out.is_empty() {
        Some(out.to_string())
    } else {
        None
    }
}

fn is_repo(root: &str) -> bool {
    git(root, &["rev-parse", "--is-inside-work-tree"]).0
}

fn current_branch(root: &str) -> String {
    let (ok, out) = git(root, &["rev-parse", "--abbrev-ref", "HEAD"]);
    if ok {
        out.trim().to_string()
    } else {
        String::new()
    }
}

fn list_branches(root: &str) -> Vec<String> {
    let (ok, out) = git(root, &["branch", "--format=%(refname:short)"]);
    if !ok {
        return Vec::new();
    }
    out.lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

fn ahead_behind(root: &str, branch: &str) -> (usize, usize) {
    if branch.is_empty() {
        return (0, 0);
    }
    // @{u} works only when an upstream is configured.
    let (ok, out) = git(
        root,
        &["rev-list", "--left-right", "--count", "@{u}...HEAD"],
    );
    if !ok {
        return (0, 0);
    }
    let mut it = out.split_whitespace();
    let behind = it.next().and_then(|n| n.parse::<usize>().ok()).unwrap_or(0);
    let ahead = it.next().and_then(|n| n.parse::<usize>().ok()).unwrap_or(0);
    (ahead, behind)
}

/// Parse `git status --porcelain=v1` into staged + unstaged buckets.
fn parse_porcelain(root: &str) -> (Vec<GitEntry>, Vec<GitEntry>) {
    let (_ok, out) = git(root, &["status", "--porcelain=v1", "-z"]);
    let mut staged: Vec<GitEntry> = Vec::new();
    let mut unstaged: Vec<GitEntry> = Vec::new();
    if out.is_empty() {
        return (staged, unstaged);
    }

    // `-z` separates records with NUL bytes. Rename/copy entries use two NUL
    // records: `<XY> <orig>\0<new>\0`.
    let bytes = out.as_bytes();
    let mut idx = 0usize;
    while idx < bytes.len() {
        // extract a NUL-delimited token
        let start = idx;
        while idx < bytes.len() && bytes[idx] != 0 {
            idx += 1;
        }
        let token = String::from_utf8_lossy(&bytes[start..idx]).to_string();
        if idx < bytes.len() {
            idx += 1; // skip the NUL
        }
        if token.is_empty() {
            continue;
        }
        if token.len() < 3 {
            continue;
        }
        let x = token.as_bytes()[0] as char;
        let y = token.as_bytes()[1] as char;
        let rest = &token[3..];
        let conflict =
            matches!(x, 'U' | 'A' | 'D') && matches!(y, 'U' | 'A' | 'D') || x == 'U' || y == 'U';

        // Rename: old path is `rest`, new path is the next token.
        let (path, _consumed_extra) = if x == 'R' || x == 'C' || y == 'R' || y == 'C' {
            // next NUL token is the destination path
            let dstart = idx;
            while idx < bytes.len() && bytes[idx] != 0 {
                idx += 1;
            }
            let dest = String::from_utf8_lossy(&bytes[dstart..idx]).to_string();
            if idx < bytes.len() {
                idx += 1;
            }
            (dest, true)
        } else {
            (rest.to_string(), false)
        };

        if x != ' ' && x != '?' {
            staged.push(GitEntry {
                path: path.clone(),
                code: format!("{x}{y}"),
                status: x,
                conflict,
            });
        }
        if y != ' ' {
            unstaged.push(GitEntry {
                path: path.clone(),
                code: format!("{x}{y}"),
                status: y,
                conflict,
            });
        } else if x == '?' && y == '?' {
            // untracked — only show in unstaged
        }
    }
    (staged, unstaged)
}

fn parse_log(root: &str) -> Vec<GitCommit> {
    let sep = "%x01"; // SOH record separator
    let field_sep = "%x02"; // STX
    let format = format!("%H{field_sep}%ad{field_sep}%an{field_sep}%s{sep}");
    let (_ok, out) = git(
        root,
        &[
            "log",
            "-n",
            "60",
            "--date=short",
            &format!("--pretty={format}"),
        ],
    );
    let mut entries = Vec::new();
    for rec in out.split('\u{1}') {
        let rec = rec.trim().trim_start_matches('\u{2}');
        if rec.is_empty() {
            continue;
        }
        let parts: Vec<&str> = rec.splitn(4, '\u{2}').collect();
        if parts.len() < 4 {
            continue;
        }
        entries.push(GitCommit {
            hash: parts[0].to_string(),
            date: parts[1].to_string(),
            author: parts[2].to_string(),
            message: parts[3].to_string(),
        });
    }
    entries
}

fn show_diff(root: &str, path: &str, staged: bool) -> String {
    let mut args = vec!["diff", "--no-color"];
    if staged {
        args.push("--cached");
    }
    args.push("--");
    args.push(path);
    let (_ok, out) = git(root, &args);
    truncate(&out, 200_000)
}

fn untracked_full(root: &str, path: &str) -> String {
    // Render the entire file as additions for untracked entries.
    let full = std::path::Path::new(root).join(path);
    match std::fs::read_to_string(&full) {
        Ok(content) => {
            let mut s = String::from("--- /dev/null\n+++ b/");
            s.push_str(path);
            s.push('\n');
            for line in content.lines() {
                s.push('+');
                s.push_str(line);
                s.push('\n');
            }
            truncate(&s, 200_000)
        }
        Err(e) => format!("(cannot read file: {e})"),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let head: String = s.chars().take(max).collect();
        format!("{head}\n… [truncated]\n")
    }
}

/// Build a full snapshot of the repository. If `after_op` was a mutating op we
/// may carry its error message. `diff_path` lets us refresh the on-screen diff
/// after the working tree changed.
fn snapshot(root: &str, diff_pref: Option<(String, bool)>, error: Option<String>) -> GitState {
    snapshot_with(root, diff_pref, None, error)
}
/// Like [`snapshot`] but with an optional precomputed (title, text) diff that replaces
/// the working-tree/file diff (used for `git show <commit>`).
fn snapshot_with(
    root: &str,
    diff_pref: Option<(String, bool)>,
    preset_diff: Option<(String, String)>,
    error: Option<String>,
) -> GitState {
    if !is_repo(root) {
        return GitState {
            repo: false,
            error,
            ..Default::default()
        };
    }
    let branch = current_branch(root);
    let branches = list_branches(root);
    let (ahead, behind) = ahead_behind(root, &branch);
    let (staged, unstaged) = parse_porcelain(root);
    let log = parse_log(root);

    let diff = if let Some(preset) = preset_diff {
        Some(preset)
    } else {
        diff_pref.as_ref().map(|(p, staged)| {
            let untracked = !staged && unstaged.iter().any(|e| e.status == '?' && e.path == *p);
            let text = if *staged {
                show_diff(root, p, true)
            } else if untracked {
                untracked_full(root, p)
            } else {
                show_diff(root, p, false)
            };
            let title = if *staged {
                format!("Staged: {p}")
            } else {
                p.clone()
            };
            (title, text)
        })
    };

    let current_diff_path = diff_pref.as_ref().map(|(p, _)| p.clone());
    let current_diff_staged = diff_pref.map(|(_, s)| s);

    GitState {
        repo: true,
        branch,
        branches,
        ahead,
        behind,
        staged,
        unstaged,
        log,
        diff,
        error,
        busy: false,
        last_op: None,
        current_diff_path,
        current_diff_staged,
        commit_diff: None,
    }
}

fn handle_op(root: &str, op: GitOp) -> GitState {
    let mut error: Option<String> = None;
    let mut diff_pref: Option<(String, bool)> = None;

    match op.clone() {
        GitOp::Refresh => {}
        GitOp::Stage(paths) => {
            let mut args = vec!["add", "--"];
            for p in &paths {
                args.push(p.as_str());
            }
            let (ok, out) = git(root, &args);
            if !ok {
                error = Some(out.trim().to_string());
            }
        }
        GitOp::Unstage(paths) => {
            let mut args = vec!["reset", "HEAD", "--"];
            for p in &paths {
                args.push(p.as_str());
            }
            let (ok, out) = git(root, &args);
            if !ok {
                error = Some(out.trim().to_string());
            }
        }
        GitOp::Discard(paths) => {
            for p in &paths {
                // We must classify each path; easiest: do both a checkout and a clean.
                // To be safe, only `clean` untracked ones.
                let is_untracked = {
                    let (_ok, out) = git(root, &["status", "--porcelain=v1", "--", p]);
                    out.starts_with("??")
                };
                if is_untracked {
                    let (ok, out) = git(root, &["clean", "-f", "--", p]);
                    if !ok {
                        error = Some(out.trim().to_string());
                    }
                } else {
                    let (ok, out) = git(root, &["checkout", "--", p]);
                    if !ok {
                        error = Some(out.trim().to_string());
                    }
                }
            }
        }
        GitOp::Commit(message) => {
            if message.trim().is_empty() {
                error = Some("Commit message is empty".to_string());
            } else {
                let (ok, out) = git(root, &["commit", "-m", message.trim()]);
                if !ok {
                    error = Some(out.trim().to_string());
                }
            }
        }
        GitOp::Checkout(branch) => {
            let (ok, out) = git(root, &["checkout", &branch]);
            if !ok {
                error = Some(out.trim().to_string());
            }
        }
        GitOp::NewBranch(name) => {
            let (ok, out) = git(root, &["checkout", "-b", &name]);
            if !ok {
                error = Some(out.trim().to_string());
            }
        }
        GitOp::ShowDiff { path, staged } => {
            if path.is_empty() {
                // Treat an empty-path request as a clear.
                return snapshot(root, None, error);
            }
            diff_pref = Some((path, staged));
        }
        GitOp::ClearDiff => {
            return snapshot(root, None, error);
        }
        GitOp::ShowCommit(hash) => {
            if hash.is_empty() {
                return snapshot(root, None, error);
            }
            let (ok, out) = git(
                root,
                &["show", "--no-color", "--patch", "--stat=200", &hash],
            );
            if ok {
                let text = truncate(&out, MAX_DIFF_CHARS);
                let title = format!("Commit {hash}");
                diff_pref = Some((hash, true));
                return snapshot_with(root, diff_pref, Some((title, text)), error);
            } else {
                error = Some(out.trim().to_string());
            }
        }
        GitOp::Pull => {
            let (ok, out) = git(root, &["pull", "--ff-only"]);
            if !ok {
                error = Some(out.trim().to_string());
            }
        }
        GitOp::Push => {
            // A brand-new branch has no upstream yet, so a bare `git push` fails with
            // "no upstream branch". Detect that and push with `--set-upstream origin
            // <branch>` so the branch publishes and starts tracking in one shot.
            let has_upstream = git(
                root,
                &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
            )
            .0;
            let (ok, out) = if has_upstream {
                git(root, &["push"])
            } else {
                let (bok, branch) = git(root, &["rev-parse", "--abbrev-ref", "HEAD"]);
                let branch = branch.trim();
                if !bok || branch.is_empty() || branch == "HEAD" {
                    (
                        false,
                        "Cannot determine the current branch to push.".to_string(),
                    )
                } else {
                    git(root, &["push", "--set-upstream", "origin", branch])
                }
            };
            if !ok {
                error = Some(out.trim().to_string());
            }
        }
        GitOp::Fetch => {
            let (ok, out) = git(root, &["fetch"]);
            if !ok {
                error = Some(out.trim().to_string());
            }
        }
        GitOp::SetCwd(_) => {
            // Handled by the worker loop before dispatch; never reached here.
        }
        GitOp::CollectCommitDiff => {
            // Prefer the staged diff; if nothing is staged, fall back to the working-tree
            // diff so the generator still has something to summarize.
            let (ok, staged_out) = git(root, &["diff", "--cached", "--no-color"]);
            if !ok {
                error = Some(staged_out.trim().to_string());
            }
            let combined = if staged_out.trim().is_empty() {
                let (ok2, unstaged_out) = git(root, &["diff", "--no-color"]);
                if !ok2 && error.is_none() {
                    error = Some(unstaged_out.trim().to_string());
                }
                unstaged_out
            } else {
                staged_out
            };
            let mut state = snapshot(root, None, error);
            let trimmed = truncate(&combined, MAX_DIFF_CHARS);
            state.commit_diff = if trimmed.trim().is_empty() {
                None
            } else {
                Some(trimmed)
            };
            return state;
        }
    }

    // After a ShowDiff request carry the requested diff back, surviving even if the
    // file momentarily leaves the index; other ops just snapshot.
    if matches!(op, GitOp::ShowDiff { .. }) {
        snapshot_keep(root, diff_pref, error)
    } else {
        snapshot(root, diff_pref, error)
    }
}

fn snapshot_keep(root: &str, diff_pref: Option<(String, bool)>, error: Option<String>) -> GitState {
    let diff_clone = diff_pref.clone();
    let mut s = snapshot(root, diff_pref, error);
    // ensure selected diff survives even if the file disappeared from staged/unstaged
    if s.diff.is_none()
        && let Some((p, staged)) = diff_clone
    {
        let text = if staged {
            show_diff(root, &p, true)
        } else {
            show_diff(root, &p, false)
        };
        s.diff = Some((p.clone(), text));
        s.current_diff_path = Some(p);
        s.current_diff_staged = Some(staged);
    }
    s
}
