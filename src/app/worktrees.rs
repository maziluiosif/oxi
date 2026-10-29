//! Chats in git work trees: "New chat in a work tree" makes a branch + folder and opens it as a
//! workspace, so the agent works away from the main checkout; "Merge" brings the result back.

use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use eframe::egui;

use super::OxiApp;
use super::state::Workspace;

/// The check behind a confirmation that would delete a work tree: what would be lost
/// (see [`crate::git::worktree::removal_risk`]) and how the user chose to handle it.
pub(crate) struct WorktreeRiskCheck {
    /// Root of the work tree workspace being checked.
    pub root: String,
    /// `None` while the check runs.
    pub result: Option<Result<crate::git::worktree::RemovalRisk, String>>,
    rx: Option<Receiver<Result<crate::git::worktree::RemovalRisk, String>>>,
    /// Keep the branch, and so its commits; the default when it has commits no other branch has.
    pub keep_branch: bool,
    /// Remove even though the listed work is lost. Only ever set by the user.
    pub force: bool,
}

impl WorktreeRiskCheck {
    pub(crate) fn options(&self) -> crate::git::worktree::RemoveOptions {
        crate::git::worktree::RemoveOptions {
            keep_branch: self.keep_branch,
            force: self.force,
        }
    }

    /// Whether removing now is allowed: the check found nothing that would be lost with these
    /// options, or the user chose to delete it anyway.
    pub(crate) fn allows_removal(&self) -> bool {
        match &self.result {
            None => false,
            Some(Ok(risk)) => self.force || !risk.blocks(self.options()),
            Some(Err(_)) => self.force,
        }
    }
}

/// Result of a background work tree operation, applied on the UI thread.
pub(crate) enum WorktreeResult {
    Created(Result<PathBuf, String>),
    Merged(Result<String, String>),
}

impl OxiApp {
    /// Create a work tree off workspace `wi`'s repository in the background, then open a chat
    /// in it.
    pub(crate) fn start_worktree_chat(&mut self, wi: usize) {
        self.conv.workspaces[wi].sidebar_folded = false;
        let root = PathBuf::from(&self.conv.workspaces[wi].root_path);
        if self.start_worktree_chat_at(root) {
            self.sync_workspaces_to_settings();
        }
    }

    /// Create a work tree off the repository at `root`; false when another operation runs.
    pub(crate) fn start_worktree_chat_at(&mut self, root: PathBuf) -> bool {
        if !self.conv.worktree_ops.is_empty() {
            self.notify_composer("A work tree operation is already in progress…");
            return false;
        }
        self.spawn_worktree_op(move || {
            WorktreeResult::Created(crate::git::worktree::create(&root))
        });
        self.notify_composer("Creating a work tree…");
        true
    }

    /// Commit the work tree's changes and merge its branch into the main checkout.
    pub(crate) fn merge_worktree(&mut self, wi: usize) {
        let root = PathBuf::from(&self.conv.workspaces[wi].root_path);
        let title = self.conv.workspaces[wi]
            .sessions
            .get(self.conv.workspaces[wi].active)
            .map(|s| s.title.trim().to_string())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "Agent work".to_string());
        self.spawn_worktree_op(move || {
            WorktreeResult::Merged(crate::git::worktree::merge_into_main(&root, &title))
        });
    }

    /// Find out, in the background, what removing work tree workspace `wi` would lose.
    pub(crate) fn start_worktree_risk_check(&mut self, wi: usize) {
        let Some(workspace) = self.conv.workspaces.get(wi) else {
            return;
        };
        if workspace.worktree.is_none() {
            self.conv.worktree_risk = None;
            return;
        }
        let root = workspace.root_path.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = self.conv.git_ctx.clone();
        let path = PathBuf::from(&root);
        std::thread::spawn(move || {
            let _ = tx.send(crate::git::worktree::removal_risk(&path));
            ctx.request_repaint();
        });
        self.conv.worktree_risk = Some(WorktreeRiskCheck {
            root,
            result: None,
            rx: Some(rx),
            keep_branch: false,
            force: false,
        });
    }

    /// Pick up a finished removal check.
    pub(crate) fn poll_worktree_risk(&mut self) {
        let Some(check) = self.conv.worktree_risk.as_mut() else {
            return;
        };
        let Some(rx) = check.rx.as_ref() else {
            return;
        };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err("The check stopped unexpectedly".to_string())
            }
        };
        if let Ok(risk) = &result {
            check.keep_branch = risk.loses_commits() && !risk.detached;
        }
        check.result = Some(result);
        check.rx = None;
    }

    /// Delete work tree workspace `wi` (folder and, unless `opts.keep_branch`, its branch) and
    /// drop it from the sidebar. The removal itself checks again for work that would be lost.
    pub(crate) fn remove_worktree(&mut self, wi: usize, opts: crate::git::worktree::RemoveOptions) {
        let Some(workspace) = self.conv.workspaces.get(wi) else {
            return;
        };
        let launch_dir = std::env::current_dir()
            .ok()
            .and_then(|dir| std::fs::canonicalize(dir).ok());
        if launch_dir.is_some_and(|dir| dir.starts_with(&workspace.root_path)) {
            self.notify_composer("oxi was opened in this work tree; remove it from another window");
            return;
        }
        let root = PathBuf::from(&workspace.root_path);
        for si in 0..workspace.sessions.len() {
            let key = self.session_key(wi, si);
            let acp_key = self.acp_session_key(key);
            self.acp.close(&acp_key);
        }
        self.delete_workspace(wi);
        // Deleting the folder can take seconds (build output, node_modules).
        self.spawn_chore("Could not remove the work tree", move || {
            crate::git::worktree::remove(&root, opts)
        });
    }

    fn spawn_worktree_op(&mut self, op: impl FnOnce() -> WorktreeResult + Send + 'static) {
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = self.conv.git_ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(op());
            ctx.request_repaint();
        });
        self.conv.worktree_ops.push(rx);
    }

    pub(crate) fn drain_worktree_ops(&mut self, ctx: &egui::Context) {
        if self.conv.worktree_ops.is_empty() {
            return;
        }
        let mut done = Vec::new();
        let ops: Vec<Receiver<WorktreeResult>> = std::mem::take(&mut self.conv.worktree_ops);
        for rx in ops {
            match rx.try_recv() {
                Ok(result) => done.push(result),
                Err(std::sync::mpsc::TryRecvError::Empty) => self.conv.worktree_ops.push(rx),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {}
            }
        }
        for result in done {
            match result {
                WorktreeResult::Created(Ok(path)) => self.open_worktree_workspace(path),
                WorktreeResult::Created(Err(e)) => {
                    self.conv.pending_split = None;
                    self.notify_composer(format!("Could not create a work tree: {e}"));
                }
                WorktreeResult::Merged(Ok(summary)) => {
                    self.request(crate::git::GitOp::Refresh);
                    self.conv.explorer.cache.invalidate();
                    self.notify_composer(summary);
                }
                WorktreeResult::Merged(Err(e)) => {
                    self.notify_composer(format!("Merge failed: {e}"));
                }
            }
            ctx.request_repaint();
        }
    }

    fn open_worktree_workspace(&mut self, path: PathBuf) {
        let path = path.to_string_lossy().into_owned();
        let sessions = Self::initial_workspace_sessions(&path, self.conn.no_session);
        let worktree = crate::git::worktree::info(std::path::Path::new(&path));
        if let Some(info) = &worktree {
            for ws in &mut self.conv.workspaces {
                if ws.worktree.is_none()
                    && std::path::Path::new(&ws.root_path).starts_with(&info.main_root)
                {
                    ws.sidebar_folded = false;
                }
            }
        }
        self.conv.workspaces.push(Workspace {
            root_path: path,
            sessions,
            active: 0,
            sidebar_folded: false,
            pinned: Vec::new(),
            folded_groups: Vec::new(),
            worktree,
        });
        self.select_workspace(self.conv.workspaces.len() - 1);
        self.sync_workspaces_to_settings();
        self.new_chat();
        self.finish_pending_split();
    }
}

/// Sidebar label of a work tree workspace: the main checkout's folder and the branch.
pub(crate) fn worktree_label(info: &crate::git::worktree::WorktreeInfo) -> String {
    let repo = info
        .main_root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    format!("{repo} · {}", info.branch)
}
