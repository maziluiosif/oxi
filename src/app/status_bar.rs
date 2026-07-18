//! Persistent bottom status bar: thin strip below the terminal panel with quick
//! toggles for the chats/explorer sidebar, terminal/git panels, and the current git branch.

use eframe::egui::{self, Align, Layout, Stroke};

use crate::theme::*;

use super::OxiApp;

const STATUS_BAR_H: f32 = 24.0;

impl OxiApp {
    /// Render the bottom status bar (call before the terminal panel / `CentralPanel`
    /// so it claims the very bottom strip of the window).
    pub(crate) fn render_status_bar(&mut self, ui: &mut egui::Ui) {
        use crate::app::git_panel::GitTab;

        egui::Panel::bottom("status_bar")
            .resizable(false)
            .exact_size(STATUS_BAR_H)
            .frame(
                egui::Frame::new()
                    .fill(c_bg_sidebar())
                    .stroke(Stroke::new(1.0, c_border_subtle()))
                    .inner_margin(egui::Margin::symmetric(6, 0)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;

                    let sidebar_on = self.conv.sidebar_open
                        && self.conv.sidebar_mode == crate::app::state::SidebarMode::Chats;
                    if crate::ui::chrome::icon_button_plain(ui, ICON_MENU, 20.0, sidebar_on)
                        .on_hover_text(if sidebar_on {
                            "Hide sidebar (Cmd/Ctrl+B)"
                        } else {
                            "Show sidebar (Cmd/Ctrl+B)"
                        })
                        .clicked()
                    {
                        self.request_settings_exit(
                            crate::app::state::SettingsExitAction::ToggleSidebar,
                        );
                    }

                    let explorer_on = self.conv.sidebar_open
                        && self.conv.sidebar_mode == crate::app::state::SidebarMode::Explorer;
                    if crate::ui::chrome::icon_button_plain(ui, ICON_EXPLORER, 20.0, explorer_on)
                        .on_hover_text(if explorer_on {
                            "Hide workspace explorer"
                        } else {
                            "Open workspace explorer"
                        })
                        .clicked()
                    {
                        self.request_settings_exit(
                            crate::app::state::SettingsExitAction::ToggleExplorer,
                        );
                    }

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let term_on = self.conv.terminal_open;
                        if crate::ui::chrome::icon_button_plain(ui, ICON_TERMINAL, 20.0, term_on)
                            .on_hover_text("Toggle terminal panel (Cmd/Ctrl+`)")
                            .clicked()
                        {
                            self.request_settings_exit(
                                crate::app::state::SettingsExitAction::ToggleTerminal,
                            );
                        }
                        let branches_on =
                            self.conv.git_open && self.conv.git_tab == GitTab::Branches;
                        if crate::ui::chrome::icon_button_plain(ui, ICON_BRANCH, 20.0, branches_on)
                            .on_hover_text(if branches_on {
                                "Hide git panel"
                            } else {
                                "Open git branches"
                            })
                            .clicked()
                        {
                            self.request_settings_exit(
                                crate::app::state::SettingsExitAction::ToggleGitBranches,
                            );
                        }

                        let changes_on = self.conv.git_open && self.conv.git_tab == GitTab::Changes;
                        if crate::ui::chrome::icon_button_plain(ui, ICON_GIT, 20.0, changes_on)
                            .on_hover_text(if changes_on {
                                "Hide git panel (Cmd/Ctrl+Shift+B)"
                            } else {
                                "Open git changes (Cmd/Ctrl+Shift+B)"
                            })
                            .clicked()
                        {
                            self.request_settings_exit(
                                crate::app::state::SettingsExitAction::ToggleGitChanges,
                            );
                        }

                        if self.conv.git.repo {
                            let mut label = self.conv.git.branch.clone();
                            if self.conv.git.ahead > 0 {
                                label.push_str(&format!(" \u{2191}{}", self.conv.git.ahead));
                            }
                            if self.conv.git.behind > 0 {
                                label.push_str(&format!(" \u{2193}{}", self.conv.git.behind));
                            }
                            ui.label(
                                egui::RichText::new(label)
                                    .size(FS_TINY)
                                    .color(c_text_muted()),
                            )
                            .on_hover_text("Current branch");
                        }
                    });
                });
            });
    }
}
