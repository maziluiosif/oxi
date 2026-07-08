//! Persistent bottom status bar: thin strip below the terminal panel with quick
//! toggles for the sidebar/terminal/git panels and the current git branch.

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

                    let sidebar_on = self.conv.sidebar_open;
                    if crate::ui::chrome::icon_button_plain(ui, ICON_MENU, 20.0, sidebar_on)
                        .on_hover_text(if sidebar_on {
                            "Hide sidebar"
                        } else {
                            "Show sidebar"
                        })
                        .clicked()
                    {
                        self.request_settings_exit(
                            crate::app::state::SettingsExitAction::ToggleSidebar,
                        );
                    }

                    let settings_on = self.conv.settings_open;
                    if crate::ui::chrome::icon_button_plain(ui, ICON_SETTINGS, 20.0, settings_on)
                        .on_hover_text(if settings_on {
                            "Back to chat"
                        } else {
                            "Open settings"
                        })
                        .clicked()
                    {
                        if settings_on {
                            self.request_settings_exit(
                                crate::app::state::SettingsExitAction::BackToChat,
                            );
                        } else {
                            self.open_settings_page();
                        }
                    }

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let term_on = self.conv.terminal_open;
                        if crate::ui::chrome::icon_button_plain(ui, ICON_TERMINAL, 20.0, term_on)
                            .on_hover_text("Toggle terminal panel")
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
                                "Hide git panel"
                            } else {
                                "Open git changes"
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
