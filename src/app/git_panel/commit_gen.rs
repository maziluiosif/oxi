//! Git worker channel plumbing (spawn/refresh/drain) and the AI commit-message
//! generator that rides on top of it.

use eframe::egui;

use crate::git::{GitOp, GitState};

use super::super::OxiApp;

impl OxiApp {
    pub(crate) fn toggle_git_panel(&mut self) {
        self.conv.git_open = !self.conv.git_open;
        self.conv.settings.git_open = self.conv.git_open;
        if let Err(e) = self.conv.settings.save() {
            self.run_state_mut(self.active_session_key()).stream_error =
                Some(format!("Save settings: {e}"));
        }
        if self.conv.git_open {
            // Ensure the worker exists and request an initial snapshot.
            self.ensure_git_channels();
            let _ = self.conv.git_tx.as_ref().map(|t| t.send(GitOp::Refresh));
        }
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
    pub(super) fn ensure_git_channels(&mut self) {
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
        while let Ok(state) = rx.try_recv() {
            if let Some(diff) = &state.commit_diff {
                collected_diff = Some(diff.clone());
            }
            latest = Some(state);
        }
        if let Some(state) = latest {
            self.conv.git = state;
            ctx.request_repaint();
        }
        if let Some(diff) = collected_diff {
            if self.conv.commit_gen_pending {
                self.start_commit_gen(&diff);
                ctx.request_repaint();
            }
        }
    }

    /// Kick off the LLM completion for the commit message once the diff is in hand.
    fn start_commit_gen(&mut self, diff: &str) {
        self.conv.commit_gen_pending = false;
        let Some(profile) = self.conv.settings.commit_msg_profile().cloned() else {
            self.conv.commit_gen_error =
                Some("No provider profile configured for commit messages.".to_string());
            return;
        };
        let system_prompt = self.conv.settings.commit_msg_system_prompt.clone();
        let user_prompt =
            format!("Write a git commit message for the following diff.\n\n```diff\n{diff}\n```");
        let (rx, _handle) = crate::agent::spawn_completion(crate::agent::CompleteRequest {
            profile,
            system_prompt,
            user_prompt,
            max_chars: Some(1500),
        });
        self.conv.git_commit_message.clear();
        self.conv.commit_gen_error = None;
        self.conv.commit_gen_rx = Some(rx);
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
                            }
                        }
                        Err(e) => self.conv.commit_gen_error = Some(e),
                    }
                    done = true;
                    ctx.request_repaint();
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
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

    pub(super) fn request(&self, op: GitOp) {
        let _ = self.conv.git_tx.as_ref().map(|t| t.send(op));
    }
}
