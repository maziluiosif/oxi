//! Git worker channel plumbing (spawn/refresh/drain) and the AI commit-message
//! generator that rides on top of it.

use eframe::egui;

use crate::git::{GitOp, GitState};

use super::super::OxiApp;

impl OxiApp {
    pub(crate) fn toggle_git_panel_tab(&mut self, tab: super::GitTab) {
        self.close_settings_page();

        if self.conv.git_open && self.conv.git_tab == tab {
            self.conv.git_open = false;
            self.conv.settings.git_open = false;
            self.focus_active_view_next_frame();
            if let Err(e) = self.conv.settings.save() {
                self.run_state_mut(self.active_session_key()).stream_error =
                    Some(format!("Save settings: {e}"));
            }
            return;
        }

        self.conv.git_tab = tab;
        if !self.conv.git_open {
            self.conv.git_open = true;
            self.conv.settings.git_open = true;
            if let Err(e) = self.conv.settings.save() {
                self.run_state_mut(self.active_session_key()).stream_error =
                    Some(format!("Save settings: {e}"));
            }
        }
        self.ensure_git_channels();
        let _ = self.conv.git_tx.as_ref().map(|t| t.send(GitOp::Refresh));
        self.focus_active_view_next_frame();
    }

    pub(crate) fn bind_git_ctx(&mut self, ctx: &egui::Context) {
        self.conv.git_ctx = ctx.clone();
    }

    /// Tell the git worker the active workspace changed, so it re-roots and
    /// refreshes. Called from `select_workspace` / new-workspace flows.
    pub(crate) fn refresh_git_cwd(&mut self) {
        if self.conv.git_rx.is_none() {
            return;
        }
        let cwd = self.active_workspace().root_path.clone();
        let _ = self
            .conv
            .git_tx
            .as_ref()
            .map(|t| t.send(GitOp::SetCwd(cwd)));
    }

    /// Make sure the git worker thread exists and is rooted at the active workspace.
    /// `ensure_git_channels` lazily creates it using the real egui context.
    pub(crate) fn ensure_git_channels(&mut self) {
        if self.conv.git_rx.is_none() {
            let cwd = self.active_workspace().root_path.clone();
            let chan = crate::git::GitChannels::new(cwd, self.conv.git_ctx.clone());
            self.conv.git_tx = Some(chan.tx);
            self.conv.git_rx = Some(chan.rx);
            // Optimistic busy marker so the panel doesn't flash "not a repo" before
            // the first snapshot arrives.
            self.conv.git.busy = true;
            self.conv.git.last_op = Some("refresh".to_string());
        }
    }

    pub(crate) fn drain_git(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.conv.git_rx.as_ref() else {
            return;
        };
        let mut latest: Option<GitState> = None;
        // The diff collected for the commit-message generator can arrive on any drained
        // state, so capture it across all of them rather than only the last one.
        let mut collected_diff: Option<String> = None;
        let mut saw_final_snapshot = false;
        while let Ok(mut state) = rx.try_recv() {
            if !state.busy {
                saw_final_snapshot = true;
            }
            if state.busy {
                // The worker emits a lightweight "busy" snapshot before each git op.
                // Keep the last real snapshot's content in place while only updating
                // the busy marker; otherwise the diff view (and sidebar lists) briefly
                // disappear until the final snapshot arrives, which looks like flicker
                // when switching between files/commits.
                let previous = &self.conv.git;
                state.repo = previous.repo;
                state.branch = previous.branch.clone();
                state.branches = previous.branches.clone();
                state.ahead = previous.ahead;
                state.behind = previous.behind;
                state.staged = previous.staged.clone();
                state.unstaged = previous.unstaged.clone();
                state.log = previous.log.clone();
                state.diff = previous.diff.clone();
                state.error = previous.error.clone();
                state.current_diff_path = previous.current_diff_path.clone();
                state.current_diff_staged = previous.current_diff_staged;
            }
            if let Some(diff) = &state.commit_diff {
                collected_diff = Some(diff.clone());
            }
            latest = Some(state);
        }
        if let Some(state) = latest {
            self.conv.git = state;
            ctx.request_repaint();
        }
        if let Some(diff) = collected_diff
            && self.conv.commit_gen_pending
        {
            self.start_commit_gen(&diff);
            ctx.request_repaint();
        } else if self.conv.commit_gen_pending && saw_final_snapshot && !self.conv.git.busy {
            // The diff collection finished without producing a diff (empty tree or a git
            // error): stop the "Generating…" state instead of leaving it stuck.
            self.conv.commit_gen_pending = false;
            self.conv.commit_gen_error = Some(
                self.conv
                    .git
                    .error
                    .clone()
                    .unwrap_or_else(|| "No changes to summarize.".to_string()),
            );
            ctx.request_repaint();
        }
    }

    /// Kick off the LLM completion for the commit message once the diff is in hand.
    fn start_commit_gen(&mut self, diff: &str) {
        self.conv.commit_gen_pending = false;
        let config = self.conv.settings.commit_msg_config();
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
        self.conv.commit_gen_stash = Some(std::mem::take(&mut self.conv.git_commit_message));
        self.conv.commit_gen_error = None;
        self.conv.commit_gen_rx = Some(rx);
    }

    /// Put the user's pre-generation draft back after a failed/aborted generation.
    fn restore_commit_gen_stash(&mut self) {
        if let Some(prev) = self.conv.commit_gen_stash.take() {
            self.conv.git_commit_message = prev;
        }
    }

    /// Drain streamed commit-message deltas into the composer. Called each frame.
    pub(crate) fn drain_commit_gen(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.conv.commit_gen_rx.as_ref() else {
            return;
        };
        let mut done = false;
        loop {
            match rx.try_recv() {
                Ok(crate::agent::CompleteEvent::Delta(d)) => {
                    self.conv.git_commit_message.push_str(&d);
                    ctx.request_repaint();
                }
                Ok(crate::agent::CompleteEvent::Done(result)) => {
                    match result {
                        Ok(text) => {
                            let trimmed = text.trim();
                            if !trimmed.is_empty() {
                                self.conv.git_commit_message = trimmed.to_string();
                                self.conv.commit_gen_stash = None;
                            } else {
                                self.restore_commit_gen_stash();
                            }
                        }
                        Err(e) => {
                            self.conv.commit_gen_error = Some(e);
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
                    self.conv.commit_gen_error =
                        Some("Generation stopped unexpectedly.".to_string());
                    self.restore_commit_gen_stash();
                    done = true;
                    break;
                }
            }
        }
        if done {
            self.conv.commit_gen_rx = None;
        }
    }

    /// True while a commit message is being collected or generated.
    pub(super) fn commit_gen_active(&self) -> bool {
        self.conv.commit_gen_pending || self.conv.commit_gen_rx.is_some()
    }

    /// Send an op to the git worker; surfaces a visible error instead of silently
    /// dropping the request when the worker channel is gone.
    pub(crate) fn request(&mut self, op: GitOp) {
        let sent = self
            .conv
            .git_tx
            .as_ref()
            .is_some_and(|t| t.send(op).is_ok());
        if !sent {
            self.conv.git.error =
                Some("Git worker is not running — reopen the panel to restart it.".to_string());
        }
    }
}
