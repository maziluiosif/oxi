//! Source-control panel (right side): changes, commit, branch switching, history, diff.
//!
//! Split by responsibility: [`commit_gen`] (git worker channel plumbing + the AI
//! commit-message generator), [`changes`] (header, tabs, the Changes tab, and the diff
//! viewer), and [`refs`] (branches + history tabs). This file keeps the shared `GitTab`
//! type, panel width bounds, and the top-level [`OxiApp::render_git_panel`] orchestrator
//! that dispatches into the three tabs.

mod changes;
mod commit_gen;
mod refs;

use eframe::egui::{self, Margin, RichText, Ui};

use crate::git::GitOp;
use crate::theme::*;

use super::OxiApp;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GitTab {
    #[default]
    Changes,
    Branches,
    History,
}

pub const GIT_W_MIN: f32 = 240.0;
pub const GIT_W_MAX: f32 = 640.0;

impl OxiApp {
    /// Render the right-side git column (allocated after the chat area).
    pub(crate) fn render_git_panel(&mut self, ui: &mut Ui, full_h: f32) {
        let _ = full_h;

        // The panel can come up open straight from settings (git_open persisted), so
        // make sure the worker exists and we don't sit on a stale "Not a git repository"
        // default state.
        if self.conv.git_rx.is_none() {
            self.ensure_git_channels();
            let _ = self.conv.git_tx.as_ref().map(|t| t.send(GitOp::Refresh));
        }
        ui.set_min_width(ui.max_rect().width());
        ui.set_min_height(ui.max_rect().height());

        egui::Frame::new()
            .fill(c_bg_sidebar())
            .inner_margin(Margin {
                left: 8,
                right: 8,
                top: 8,
                bottom: 8,
            })
            .show(ui, |ui| {
                ui.set_min_width(ui.max_rect().width());
                ui.set_min_height(ui.max_rect().height());

                self.render_git_header(ui);
                ui.add_space(8.0);
                self.render_git_tabs(ui);
                ui.add_space(6.0);

                if !self.conv.git.repo && !self.conv.git.busy {
                    ui.add_space(12.0);
                    ui.label(
                        RichText::new("Not a git repository")
                            .size(FS_SMALL)
                            .color(c_text_muted()),
                    );
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(format!(
                            "Workspace root isn't inside a git worktree:\n{}",
                            self.active_workspace().root_path
                        ))
                        .size(FS_TINY)
                        .color(c_text_faint()),
                    );
                    return;
                }

                if let Some(err) = self.conv.git.error.clone() {
                    crate::ui::chrome::alert_banner(ui, &err, true);
                    ui.add_space(6.0);
                }

                match self.conv.git_tab {
                    GitTab::Changes => self.render_git_changes(ui),
                    GitTab::Branches => self.render_git_branches(ui),
                    GitTab::History => self.render_git_history(ui),
                }
            });
        ui.expand_to_include_rect(ui.max_rect());
    }
}
