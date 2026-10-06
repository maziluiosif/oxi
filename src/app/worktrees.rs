//! Chats in git work trees: "New chat in a work tree" makes a branch + folder and opens it as a
//! workspace, so the agent works away from the main checkout; "Merge" brings the result back.

use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use eframe::egui;

use super::OxiApp;
use super::state::Workspace;

/// Result of a background work tree operation, applied on the UI thread.
pub(crate) enum WorktreeResult {
    Created(Result<PathBuf, String>),
    Merged(Result<String, String>),
}

impl OxiApp {
    /// Create a work tree off workspace `wi`'s repository in the background, then open a chat
    /// in it.
    pub(crate) fn start_worktree_chat(&mut self, wi: usize) {
        let root = PathBuf::from(&self.conv.workspaces[wi].root_path);
        self.spawn_worktree_op(move || {
            WorktreeResult::Created(crate::git::worktree::create(&root))
        });
        self.notify_composer("Creating a work tree…");
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

    /// Delete work tree workspace `wi` (folder and branch) and drop it from the sidebar.
    pub(crate) fn remove_worktree(&mut self, wi: usize) {
        let Some(workspace) = self.conv.workspaces.get(wi) else {
            return;
        };
        if wi == 0 {
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
        if let Err(e) = crate::git::worktree::remove(&root) {
            self.notify_composer(format!("Could not remove the work tree: {e}"));
        }
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
                    self.notify_composer(format!("Could not create a work tree: {e}"));
                }
                WorktreeResult::Merged(Ok(summary)) => {
                    self.request(crate::git::GitOp::Refresh);
                    self.conv.explorer_cache.invalidate();
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
