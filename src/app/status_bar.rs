//! Persistent bottom status bar: thin strip below the terminal panel with quick
//! toggles for the chats/explorer/search sidebar, Settings, terminal/git panels, the active chat's
//! context and usage limits, and the current git branch.

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

                    // Settings takes over the window, so no sidebar view reads as active.
                    let settings_on = self.conv.settings_page.open;
                    let sidebar_on = !settings_on
                        && self.conv.sidebar.open
                        && self.conv.sidebar.mode == crate::app::state::SidebarMode::Chats;
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

                    let explorer_on = !settings_on
                        && self.conv.sidebar.open
                        && self.conv.sidebar.mode == crate::app::state::SidebarMode::Explorer;
                    if crate::ui::chrome::icon_button_plain(ui, ICON_EXPLORER, 20.0, explorer_on)
                        .on_hover_text(if explorer_on {
                            "Hide workspace explorer (Cmd/Ctrl+E)"
                        } else {
                            "Open workspace explorer (Cmd/Ctrl+E)"
                        })
                        .clicked()
                    {
                        self.request_settings_exit(
                            crate::app::state::SettingsExitAction::ToggleExplorer,
                        );
                    }

                    let search_on = !settings_on
                        && self.conv.sidebar.open
                        && self.conv.sidebar.mode == crate::app::state::SidebarMode::Search;
                    if crate::ui::chrome::icon_button_plain(ui, ICON_SEARCH, 20.0, search_on)
                        .on_hover_text(if search_on {
                            "Hide search (Cmd/Ctrl+Shift+F)"
                        } else {
                            "Find in files (Cmd/Ctrl+Shift+F)"
                        })
                        .clicked()
                    {
                        self.request_settings_exit(
                            crate::app::state::SettingsExitAction::ShowSearch { toggle: true },
                        );
                    }

                    // Settings sits with the views it replaces, VS Code's gear at the foot
                    // of the activity bar: reachable from every view, even with the
                    // sidebar hidden.
                    if crate::ui::chrome::icon_button_plain(ui, ICON_SETTINGS, 20.0, settings_on)
                        .on_hover_text(if settings_on {
                            "Close settings (Esc)"
                        } else {
                            "Settings (Cmd/Ctrl+,)"
                        })
                        .clicked()
                    {
                        self.toggle_settings_page();
                    }

                    let file_shown = self.conv.editor.active.is_some()
                        && !self.conv.settings_page.open
                        && !self.conv.editor.diff_tab_active;
                    if file_shown {
                        ui.add_space(10.0);
                        self.render_caret_status(ui);
                    }
                    if !settings_on {
                        ui.add_space(10.0);
                        self.render_chat_status(ui);
                    }

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let activity_on = super::activity_window::activity_window_open(ui.ctx());
                        let recording = self.conv.settings.activity_log_enabled;
                        if crate::ui::chrome::icon_button_plain(
                            ui,
                            ICON_ACTIVITY,
                            20.0,
                            activity_on || recording,
                        )
                        .on_hover_text(if recording {
                            "Activity log (recording)"
                        } else {
                            "Activity log: inspect raw requests, responses and tool calls"
                        })
                        .clicked()
                        {
                            super::activity_window::toggle_activity_window(ui.ctx());
                        }
                        let term_on = self.conv.terminal_panel.open;
                        if crate::ui::chrome::icon_button_plain(ui, ICON_TERMINAL, 20.0, term_on)
                            .on_hover_text("Toggle terminal panel (Cmd/Ctrl+`)")
                            .clicked()
                        {
                            self.request_settings_exit(
                                crate::app::state::SettingsExitAction::ToggleTerminal,
                            );
                        }
                        let git_on = self.conv.git_ui.open;
                        if crate::ui::chrome::icon_button_plain(ui, ICON_GIT, 20.0, git_on)
                            .on_hover_text(if git_on {
                                "Hide git panel (Cmd/Ctrl+Shift+B)"
                            } else {
                                "Open git panel (Cmd/Ctrl+Shift+B)"
                            })
                            .clicked()
                        {
                            self.request_settings_exit(
                                crate::app::state::SettingsExitAction::ToggleGitPanel,
                            );
                        }

                        if file_shown {
                            ui.add_space(8.0);
                            self.render_file_status(ui);
                            ui.add_space(12.0);
                        }
                        self.render_update_status(ui);

                        if self.conv.git.repo {
                            self.render_pr_pill(ui);
                            let branches_on =
                                self.conv.git_ui.open && self.conv.git_ui.tab == GitTab::Branches;
                            let mut label = self.conv.git.branch.clone();
                            if self.conv.git.ahead > 0 {
                                label.push_str(&format!(" \u{2191}{}", self.conv.git.ahead));
                            }
                            if self.conv.git.behind > 0 {
                                label.push_str(&format!(" \u{2193}{}", self.conv.git.behind));
                            }
                            if ui
                                .add(
                                    egui::Label::new(
                                        egui::RichText::new(label).size(FS_TINY).color(
                                            if branches_on {
                                                c_accent()
                                            } else {
                                                c_text_muted()
                                            },
                                        ),
                                    )
                                    .sense(egui::Sense::click()),
                                )
                                .on_hover_cursor(egui::CursorIcon::PointingHand)
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
                        }
                    });
                });
            });
    }
}

impl OxiApp {
    /// "Restart to update" once a new release is installed; progress while it downloads; a link
    /// to About when an update exists that oxi will not install by itself.
    fn render_update_status(&mut self, ui: &mut egui::Ui) {
        use crate::app::state::UpdateInstall;

        let (label, hover, accent) = match &self.conv.update.install {
            UpdateInstall::Ready { version, .. } => (
                format!("Restart to update to v{version}"),
                if self.any_waiting_response() {
                    format!("v{version} is installed. Restarting stops the running reply.")
                } else {
                    format!("v{version} is installed. Restart oxi to use it.")
                },
                true,
            ),
            UpdateInstall::Installing(version) => (
                format!("Updating to v{version}\u{2026}"),
                "Downloading the update in the background".to_owned(),
                false,
            ),
            _ => match self.update_available() {
                Some(info) => (
                    format!("v{} available", info.version),
                    "Open About to update".to_owned(),
                    true,
                ),
                None => return,
            },
        };
        let response = ui
            .add(
                egui::Label::new(egui::RichText::new(label).size(FS_TINY).color(if accent {
                    c_accent()
                } else {
                    c_text_muted()
                }))
                .sense(egui::Sense::click()),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text(hover);
        if response.clicked() {
            if matches!(self.conv.update.install, UpdateInstall::Ready { .. }) {
                self.restart_to_update(ui.ctx());
            } else {
                self.conv.settings_page.tab = crate::app::state::SettingsTab::About;
                self.open_settings_page();
            }
        }
        ui.add_space(6.0);
    }
}
