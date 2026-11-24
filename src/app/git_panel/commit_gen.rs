//! Git worker channel plumbing (spawn/refresh/drain) and the AI commit-message
//! generator that rides on top of it.

use std::time::{Duration, Instant};

use eframe::egui;

use crate::git::{BRANCH_REFRESH, GitOp, GitState};

use super::super::OxiApp;
use super::super::state::{ConfirmAction, SidebarMode};

const REFRESH_INTERVAL: Duration = Duration::from_secs(2);

impl OxiApp {
    /// Toggles the git panel. `None` toggles it on whatever tab it shows; `Some(tab)` switches
    /// an open panel to `tab` and only hides it when `tab` is already showing.
    pub(crate) fn toggle_git_panel_tab(&mut self, tab: Option<super::GitTab>) {
        self.close_settings_page();

        if self.conv.git_ui.open && tab.is_none_or(|tab| self.conv.git_ui.tab == tab) {
            self.conv.git_ui.open = false;
            self.conv.settings.git_open = false;
            self.focus_active_view_next_frame();
            if let Err(e) = self.conv.settings.save() {
                self.run_state_mut(self.active_session_key()).stream_error =
                    Some(format!("Save settings: {e}"));
            }
            return;
        }

        if let Some(tab) = tab {
            self.conv.git_ui.tab = tab;
        }
        if !self.conv.git_ui.open {
            self.conv.git_ui.open = true;
            self.conv.settings.git_open = true;
            if let Err(e) = self.conv.settings.save() {
                self.run_state_mut(self.active_session_key()).stream_error =
                    Some(format!("Save settings: {e}"));
            }
        }
        self.ensure_git_channels();
        let _ = self.conv.git_ui.tx.as_ref().map(|t| t.send(GitOp::Refresh));
        self.focus_active_view_next_frame();
    }

    pub(crate) fn bind_git_ctx(&mut self, ctx: &egui::Context) {
        self.conv.git_ctx = ctx.clone();
    }

    fn has_git_workspace(&self) -> bool {
        // Index zero is the hidden scratch workspace while it still exists.
        !self.conv.no_workspace || self.conv.active_workspace != 0
    }

    fn needs_full_git_snapshot(&self) -> bool {
        !self.conv.settings_page.open
            && (self.conv.git_ui.open
                || (self.conv.sidebar.open && self.conv.sidebar.mode == SidebarMode::Explorer)
                || self.conv.diff_view.open
                || self
                    .conv
                    .editor
                    .active_document()
                    .is_some_and(|document| !document.is_scratchpad))
    }

    /// Tell the git worker the active workspace changed, so it re-roots and
    /// refreshes. Called from `select_workspace` / new-workspace flows.
    pub(crate) fn refresh_git_cwd(&mut self) {
        self.conv.git_ui.last_auto_refresh = None;
        self.conv.git_ui.compare = Default::default();
        if !self.has_git_workspace() {
            let git_ui = &mut self.conv.git_ui;
            git_ui.tx = None;
            git_ui.rx = None;
            git_ui.auto_refresh_pending = false;
            git_ui.pending_branch_delete = None;
            git_ui.commit_gen_pending = false;
            git_ui.commit_gen_rx = None;
            git_ui.commit_gen_error = None;
            self.restore_commit_gen_stash();
            self.conv.git = GitState::default();
            self.conv.diff_view = Default::default();
            self.conv.editor.diff_tab_active = false;
            if matches!(
                self.conv.confirm_prompt,
                Some(ConfirmAction::GitDiscard { .. } | ConfirmAction::GitForceDeleteBranch { .. })
            ) {
                self.conv.confirm_prompt = None;
            }
            return;
        }
        if self.conv.git_ui.rx.is_none() {
            self.start_git_worker();
            return;
        }
        let cwd = self.active_workspace().root_path.clone();
        let _ = self
            .conv
            .git_ui
            .tx
            .as_ref()
            .map(|t| t.send(GitOp::SetCwd(cwd)));
    }

    /// Make sure the git worker thread exists and is rooted at the active workspace.
    /// `ensure_git_channels` lazily creates it using the real egui context.
    pub(crate) fn ensure_git_channels(&mut self) {
        if self.has_git_workspace() && self.conv.git_ui.rx.is_none() {
            let cwd = self.active_workspace().root_path.clone();
            let chan = crate::git::GitChannels::new(cwd, self.conv.git_ctx.clone());
            self.conv.git_ui.tx = Some(chan.tx);
            self.conv.git_ui.rx = Some(chan.rx);
            // Optimistic busy marker so the panel doesn't flash "not a repo" before
            // the first snapshot arrives.
            self.conv.git.busy = true;
            self.conv.git.last_op = Some("refresh".to_string());
        }
    }

    /// Start asynchronously so the status bar can show the branch on launch.
    pub(crate) fn start_git_worker(&mut self) {
        if self.has_git_workspace() && self.conv.git_ui.rx.is_none() {
            self.ensure_git_channels();
            self.queue_auto_refresh(self.needs_full_git_snapshot());
        }
    }

    pub(crate) fn poll_git_changes(&mut self, ctx: &egui::Context) {
        if !ctx.input(|i| i.focused)
            || !self.has_git_workspace()
            || self.conv.git.busy
            || self.conv.git_ui.auto_refresh_pending
            || self.conv.git_ui.commit_gen_pending
        {
            return;
        }
        let full = self.needs_full_git_snapshot();
        if let Some((at, previous_full)) = self.conv.git_ui.last_auto_refresh
            && full == previous_full
            && let Some(wait) = REFRESH_INTERVAL.checked_sub(at.elapsed())
        {
            ctx.request_repaint_after(wait);
            return;
        }
        self.queue_auto_refresh(full);
    }

    fn queue_auto_refresh(&mut self, full: bool) {
        let op = if full {
            GitOp::AutoRefresh
        } else {
            GitOp::RefreshBranch
        };
        if let Some(tx) = &self.conv.git_ui.tx
            && tx.send(op).is_ok()
        {
            self.conv.git_ui.auto_refresh_pending = true;
            self.conv.git_ui.last_auto_refresh = Some((Instant::now(), full));
        }
    }

    pub(crate) fn drain_git(&mut self, ctx: &egui::Context) {
        self.drain_git_compare();
        let Some(rx) = self.conv.git_ui.rx.as_ref() else {
            return;
        };
        let mut latest: Option<GitState> = None;
        // The diff collected for the commit-message generator can arrive on any drained
        // state, so capture it across all of them rather than only the last one.
        let mut collected_diff: Option<String> = None;
        let mut saw_final_snapshot = false;
        while let Ok(mut state) = rx.try_recv() {
            if state.last_op.as_deref() == Some(BRANCH_REFRESH) {
                self.conv.git_ui.auto_refresh_pending = false;
                let current = latest.as_mut().unwrap_or(&mut self.conv.git);
                current.repo = state.repo;
                current.branch = state.branch;
                current.ahead = state.ahead;
                current.behind = state.behind;
                current.busy = false;
                ctx.request_repaint();
                continue;
            }
            if state.last_op.as_deref() == Some("auto refresh") {
                self.conv.git_ui.auto_refresh_pending = false;
                if state.error.is_none() {
                    state.error = latest.as_ref().unwrap_or(&self.conv.git).error.clone();
                }
            }
            if !state.busy && state.last_op.as_deref() == Some("collect commit diff") {
                saw_final_snapshot = true;
            }
            let previous = latest.as_ref().unwrap_or(&self.conv.git);
            if state.diff_only {
                // The diff worker answers with just the diff view: keep everything else.
                let mut merged = previous.clone();
                if state.view_generation >= previous.view_generation {
                    merged.diff = state.diff;
                    merged.current_diff_path = state.current_diff_path;
                    merged.current_diff_staged = state.current_diff_staged;
                    merged.view_generation = state.view_generation;
                    if state.error.is_some() {
                        merged.error = state.error;
                    }
                }
                merged.commit_diff = None;
                latest = Some(merged);
                continue;
            }
            if !state.busy && state.view_generation < previous.view_generation {
                // Computed before a newer file/commit was opened: its diff is stale.
                state.diff = previous.diff.clone();
                state.current_diff_path = previous.current_diff_path.clone();
                state.current_diff_staged = previous.current_diff_staged;
                state.view_generation = previous.view_generation;
            }
            if state.busy {
                // The worker emits a lightweight "busy" snapshot before each git op.
                // Keep the last real snapshot's content in place while only updating
                // the busy marker; otherwise the diff view (and sidebar lists) briefly
                // disappear until the final snapshot arrives, which looks like flicker
                // when switching between files/commits.
                let previous = latest.as_ref().unwrap_or(&self.conv.git);
                state.repo = previous.repo;
                state.branch = previous.branch.clone();
                state.branches = previous.branches.clone();
                state.ahead = previous.ahead;
                state.behind = previous.behind;
                state.staged = previous.staged.clone();
                state.unstaged = previous.unstaged.clone();
                state.line_changes = previous.line_changes.clone();
                state.log = previous.log.clone();
                state.diff = previous.diff.clone();
                state.error = previous.error.clone();
                state.current_diff_path = previous.current_diff_path.clone();
                state.current_diff_staged = previous.current_diff_staged;
                state.view_generation = previous.view_generation;
            }
            if let Some(diff) = &state.commit_diff {
                collected_diff = Some(diff.clone());
            }
            latest = Some(state);
        }
        if let Some(state) = latest {
            if !state.busy {
                self.conv.git_ui.epoch += 1;
            }
            self.conv.git = state;
            self.settle_branch_delete();
            ctx.request_repaint();
        }
        if let Some(diff) = collected_diff
            && self.conv.git_ui.commit_gen_pending
        {
            self.start_commit_gen(&diff);
            ctx.request_repaint();
        } else if self.conv.git_ui.commit_gen_pending && saw_final_snapshot && !self.conv.git.busy {
            // The diff collection finished without producing a diff (empty tree or a git
            // error): stop the "Generating…" state instead of leaving it stuck.
            self.conv.git_ui.commit_gen_pending = false;
            self.conv.git_ui.commit_gen_error = Some(
                self.conv
                    .git
                    .error
                    .clone()
                    .unwrap_or_else(|| "No changes to summarize.".to_string()),
            );
            ctx.request_repaint();
        }
    }

    /// Follow up a safe branch delete: git refusing it as unmerged turns into a force-delete
    /// confirmation; the branch disappearing or any other error ends it.
    fn settle_branch_delete(&mut self) {
        let Some(name) = self.conv.git_ui.pending_branch_delete.as_ref() else {
            return;
        };
        if self.conv.git.busy {
            return;
        }
        let unmerged = self
            .conv
            .git
            .error
            .as_deref()
            .is_some_and(|e| e.starts_with(crate::git::UNMERGED_BRANCH_ERROR));
        if unmerged {
            let name = name.clone();
            self.conv.git.error = None;
            self.conv.git_ui.pending_branch_delete = None;
            self.request_confirm(crate::app::state::ConfirmAction::GitForceDeleteBranch { name });
        } else if self.conv.git.error.is_some() || !self.conv.git.branches.contains(name) {
            self.conv.git_ui.pending_branch_delete = None;
        }
    }

    /// Kick off the LLM completion for the commit message once the diff is in hand.
    fn start_commit_gen(&mut self, diff: &str) {
        self.conv.git_ui.commit_gen_pending = false;
        let config = crate::router::helper_config(
            &self.conv.settings,
            self.conv.settings.commit_msg_config(),
            diff.len(),
        );
        let system_prompt = self.conv.settings.commit_msg_system_prompt.clone();
        let user_prompt =
            format!("Write a git commit message for the following diff.\n\n```diff\n{diff}\n```");
        let (rx, _handle) = crate::agent::spawn_completion(crate::agent::CompleteRequest {
            config,
            system_prompt,
            user_prompt,
            max_chars: Some(1500),
            effort_override: Some("low".to_string()),
        });
        // Stash whatever the user already typed: the stream writes into the field,
        // and a failed generation must not cost them their own draft.
        self.conv.git_ui.commit_gen_stash =
            Some(std::mem::take(&mut self.conv.git_ui.commit_message));
        self.conv.git_ui.commit_gen_error = None;
        self.conv.git_ui.commit_gen_rx = Some(rx);
    }

    /// Put the user's pre-generation draft back after a failed/aborted generation.
    fn restore_commit_gen_stash(&mut self) {
        if let Some(prev) = self.conv.git_ui.commit_gen_stash.take() {
            self.conv.git_ui.commit_message = prev;
        }
    }

    /// Drain streamed commit-message deltas into the composer. Called each frame.
    pub(crate) fn drain_commit_gen(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.conv.git_ui.commit_gen_rx.as_ref() else {
            return;
        };
        let mut done = false;
        loop {
            match rx.try_recv() {
                Ok(crate::agent::CompleteEvent::Delta(d)) => {
                    self.conv.git_ui.commit_message.push_str(&d);
                    ctx.request_repaint();
                }
                Ok(crate::agent::CompleteEvent::Done(result)) => {
                    match result {
                        Ok(text) => {
                            let trimmed = text.trim();
                            if !trimmed.is_empty() {
                                self.conv.git_ui.commit_message = trimmed.to_string();
                                self.conv.git_ui.commit_gen_stash = None;
                            } else {
                                self.restore_commit_gen_stash();
                            }
                        }
                        Err(e) => {
                            self.conv.git_ui.commit_gen_error = Some(e);
                            self.restore_commit_gen_stash();
                        }
                    }
                    done = true;
                    ctx.request_repaint();
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    // Worker died without a terminal event: tell the user instead of
                    // silently leaving a half-written (or empty) message behind.
                    self.conv.git_ui.commit_gen_error =
                        Some("Generation stopped unexpectedly.".to_string());
                    self.restore_commit_gen_stash();
                    done = true;
                    break;
                }
            }
        }
        if done {
            self.conv.git_ui.commit_gen_rx = None;
        }
    }

    /// True while a commit message is being collected or generated.
    pub(super) fn commit_gen_active(&self) -> bool {
        self.conv.git_ui.commit_gen_pending || self.conv.git_ui.commit_gen_rx.is_some()
    }

    /// Send an op to the git worker; surfaces a visible error instead of silently
    /// dropping the request when the worker channel is gone.
    pub(crate) fn request(&mut self, op: GitOp) {
        if !self.has_git_workspace() {
            return;
        }
        let sent = self
            .conv
            .git_ui
            .tx
            .as_ref()
            .is_some_and(|t| t.send(op).is_ok());
        if !sent {
            self.conv.git.error =
                Some("Git worker is not running — reopen the panel to restart it.".to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_worker_is_detached_after_removing_the_last_workspace() {
        crate::secrets::use_mock_store();
        let mut app = OxiApp::new();
        app.conv.workspaces.truncate(1);
        app.conv.active_workspace = 0;
        app.conv.no_workspace = false;
        app.ensure_git_channels();
        assert!(app.conv.git_ui.tx.is_some());
        app.conv.git.repo = true;
        app.conv.git.branch = "removed-branch".into();
        app.conv.git_ui.auto_refresh_pending = true;
        app.conv.confirm_prompt = Some(ConfirmAction::GitDiscard {
            paths: vec!["file.txt".into()],
        });

        app.delete_workspace(0);
        assert!(app.conv.no_workspace);
        assert!(app.conv.git_ui.tx.is_none());
        assert!(app.conv.git_ui.rx.is_none());
        assert!(!app.conv.git_ui.auto_refresh_pending);
        assert!(!app.conv.git.repo);
        assert!(app.conv.git.branch.is_empty());
        assert!(app.conv.confirm_prompt.is_none());

        app.start_git_worker();
        app.ensure_git_channels();
        app.request(GitOp::Refresh);
        assert!(app.conv.git_ui.tx.is_none());
        assert!(app.conv.git.error.is_none());
    }
}
