//! About, updates, and diagnostics settings page.

use eframe::egui::{self, RichText, Ui};

use crate::theme::*;
use crate::ui::chrome::{
    setting_row, setting_switch, settings_disclosure, settings_section, settings_section_title,
};

use super::super::OxiApp;
use super::super::state::UpdateInstall;

/// License texts shipped with the bundled fonts (see `assets/fonts/README.md`).
const FONT_LICENSES: &[(&str, &str)] = &[
    ("Inter", include_str!("../../../assets/fonts/Inter-OFL.txt")),
    (
        "JetBrains Mono",
        include_str!("../../../assets/fonts/JetBrainsMono-OFL.txt"),
    ),
    (
        "IBM Plex Sans",
        include_str!("../../../assets/fonts/IBMPlexSans-OFL.txt"),
    ),
    ("Lilex", include_str!("../../../assets/fonts/Lilex-OFL.txt")),
    (
        "Noto Sans, Noto Emoji, scripts and symbols",
        include_str!("../../../assets/fonts/NotoScripts-OFL.txt"),
    ),
    (
        "Ubuntu Mono",
        include_str!("../../../assets/fonts/UbuntuMono-UFL.txt"),
    ),
    (
        "Symbols Nerd Font",
        include_str!("../../../assets/fonts/SymbolsNerdFont-LICENSE.txt"),
    ),
];

impl OxiApp {
    pub(super) fn render_settings_about_panel(&mut self, ui: &mut Ui) {
        settings_section_title(
            ui,
            "About",
            Some(&format!(
                "oxi {} \u{00b7} coding agent chat",
                crate::update::APP_VERSION
            )),
        );

        settings_section(
            ui,
            "Updates",
            Some("Checked at startup and every few hours against the latest GitHub release."),
            |ui| {
                let status = if self.conv.update.checking {
                    "Checking\u{2026}".to_owned()
                } else if self.update_available().is_some() {
                    String::new()
                } else {
                    match &self.conv.update.result {
                        Some(Ok(_)) => "You're up to date.".to_owned(),
                        Some(Err(_)) => "Couldn't check for updates.".to_owned(),
                        None => String::new(),
                    }
                };
                let version = format!("Version {}", crate::update::APP_VERSION);
                setting_row(ui, &version, Some(&status), |ui| {
                    if ui
                        .add_enabled(
                            !self.conv.update.checking,
                            crate::ui::chrome::ghost_button_widget("Check for updates", false),
                        )
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .clicked()
                    {
                        self.ensure_update_checked(ui.ctx(), true);
                    }
                });
                if !self.conv.update.checking
                    && let Some(info) = self.update_available().cloned()
                {
                    ui.horizontal(|ui| self.render_update_install(ui, &info));
                    ui.add_space(8.0);
                }
                if let UpdateInstall::Failed { error, .. } = &self.conv.update.install {
                    ui.label(
                        RichText::new(format!("Update failed: {error}"))
                            .size(FS_TINY)
                            .color(c_error_fg()),
                    );
                }
                if crate::update::can_self_update() {
                    setting_switch(
                        ui,
                        &mut self.conv.settings.auto_update,
                        "Install updates automatically",
                        Some(
                            "Downloaded in the background; they take effect the next time oxi starts.",
                        ),
                    );
                }
                setting_row(ui, "Links", None, |ui| {
                    if crate::ui::chrome::ghost_button(ui, "Changelog", false).clicked() {
                        let _ = webbrowser::open(&format!(
                            "{}/blob/master/CHANGELOG.md",
                            crate::update::REPO_URL
                        ));
                    }
                    if crate::ui::chrome::ghost_button(ui, "GitHub", false).clicked() {
                        let _ = webbrowser::open(crate::update::REPO_URL);
                    }
                });
            },
        );

        let config_path = crate::settings::AppSettings::config_path();
        let config_dir = config_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .to_path_buf();
        settings_section(ui, "Diagnostics", None, |ui| {
            ui.add_space(4.0);
            ui.label(
                RichText::new(format!(
                    "OS: {} \u{00b7} {}\nConfig: {}\nWorkspace: {}",
                    std::env::consts::OS,
                    std::env::consts::ARCH,
                    config_dir.display(),
                    self.active_workspace().root_path
                ))
                .size(FS_TINY)
                .monospace()
                .color(c_text_muted()),
            );
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if crate::ui::chrome::ghost_button(ui, "Copy diagnostics", false).clicked() {
                    let report = format!(
                        "oxi {}\nOS: {} {}\nConfig: {}\nWorkspace: {}\nProvider: {}\nModel: {}\nGit repository: {}",
                        crate::update::APP_VERSION,
                        std::env::consts::OS,
                        std::env::consts::ARCH,
                        config_dir.display(),
                        self.active_workspace().root_path,
                        self.conv.settings.active_provider.label(),
                        self.conv.settings.active_config().model_id,
                        self.conv.git.repo,
                    );
                    ui.ctx().copy_text(report);
                }
                if crate::ui::chrome::ghost_button(ui, "Open config folder", false).clicked() {
                    crate::os_open::open_path(&config_dir);
                }
                let log = crate::logging::log_path();
                if log.is_file() && crate::ui::chrome::ghost_button(ui, "Open log", false).clicked()
                {
                    crate::os_open::open_path(&log);
                }
                let crash_log = crate::logging::crash_log_path();
                if crash_log.is_file()
                    && crate::ui::chrome::ghost_button(ui, "Open crash log", false).clicked()
                {
                    crate::os_open::open_path(&crash_log);
                }
            });
            ui.add_space(10.0);
        });

        settings_section(ui, "Keyboard shortcuts", None, |ui| {
            ui.add_space(6.0);
            render_shortcuts(ui);
            ui.add_space(6.0);
        });

        settings_section(ui, "Bundled font licenses", None, |ui| {
            for &(name, license) in FONT_LICENSES {
                settings_disclosure(ui, name, &[], |ui| {
                    ui.label(RichText::new(license).size(FS_TINY).color(c_text_muted()));
                    ui.add_space(8.0);
                });
            }
        });
    }
}

impl OxiApp {
    /// The "update available" row: install progress, restart, or the install / release buttons.
    fn render_update_install(&mut self, ui: &mut Ui, info: &crate::update::ReleaseInfo) {
        let ready = matches!(
            &self.conv.update.install,
            UpdateInstall::Ready { version, .. } if *version == info.version
        );
        let installing = matches!(self.conv.update.install, UpdateInstall::Installing(_));
        let status = if ready {
            format!("v{} installed", info.version)
        } else if installing {
            format!("Installing v{}\u{2026}", info.version)
        } else {
            format!("Update available: v{}", info.version)
        };
        ui.label(
            RichText::new(status)
                .size(FS_TINY)
                .color(c_accent())
                .strong(),
        );
        ui.add_space(6.0);
        if ready {
            if ui
                .add(crate::ui::chrome::primary_button_widget("Restart now"))
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
            {
                self.restart_to_update(ui.ctx());
            }
        } else if !installing
            && crate::update::can_self_update()
            && ui
                .add(crate::ui::chrome::primary_button_widget("Install"))
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
        {
            self.install_update(ui.ctx(), true);
        }
        if crate::ui::chrome::ghost_button(ui, "View release", false).clicked() {
            let _ = webbrowser::open(&info.html_url);
        }
    }
}

/// Two-column reference of the app's keyboard shortcuts (see `handle_global_shortcuts`).
fn render_shortcuts(ui: &mut Ui) {
    let mac = cfg!(target_os = "macos");
    // macOS spells chords with modifier glyphs (⌘⇧B), elsewhere with words (Ctrl+Shift+B),
    // matching the hints elsewhere in the app.
    let (cmd, shift, alt) = if mac {
        ("⌘", "⇧", "⌥")
    } else {
        ("Ctrl+", "Shift+", "Alt+")
    };
    let replace = if mac {
        format!("{cmd}{alt}F")
    } else {
        format!("{cmd}H")
    };
    let goto_line = if mac {
        "⌃G".to_owned()
    } else {
        format!("{cmd}P :")
    };
    // macOS keeps ⌘` for cycling windows, so the terminal also answers to VS Code's ⌃`.
    let terminal = if mac {
        "⌃`".to_owned()
    } else {
        format!("{cmd}`")
    };
    let (next_tab, nth_tab) = if mac {
        (format!("{cmd}{shift}] / ["), format!("{cmd}1…9"))
    } else {
        ("Ctrl+PgDn / PgUp".to_owned(), format!("{alt}1…9"))
    };
    let modification = if mac {
        "⌃. / ⌃,".to_owned()
    } else {
        format!("F7 / {shift}F7")
    };
    let shortcuts: [(String, &str); 40] = [
        (format!("{cmd}N"), "New chat"),
        (format!("{cmd},"), "Settings"),
        (format!("{cmd}."), "Stop the running reply"),
        ("Enter".into(), "Send message"),
        (format!("{shift}Enter"), "New line in the message"),
        ("↑ / ↓".into(), "Previous / next sent message"),
        (
            format!("{cmd}Enter"),
            "While the agent answers: queue instead of steering (or the reverse)",
        ),
        (
            format!("{cmd}{shift}Enter"),
            "Steer the first queued message into the reply",
        ),
        (format!("{alt}↑"), "Edit the last queued message"),
        (format!("{cmd}{alt}Enter"), "Send, then start another chat"),
        (
            format!("{cmd}{shift}S"),
            "Stash the draft / bring the last one back",
        ),
        ("Hold Space".into(), "Dictate (when voice is on)"),
        (format!("{cmd}B"), "Show or hide chats"),
        (format!("{cmd}E"), "Show or hide the file explorer"),
        (format!("{cmd}{shift}B"), "Show or hide source control"),
        (terminal, "Show or hide the terminal"),
        (
            format!("{cmd}{shift}P"),
            "Command palette (every menu command)",
        ),
        (format!("{cmd}P"), "Open a file in the workspace"),
        (format!("{cmd}R"), "Go to symbol in the file"),
        (format!("{cmd}{shift}R"), "Go to symbol in the project"),
        (goto_line, "Go to line"),
        ("F12".into(), "Go to definition"),
        (format!("{cmd}{shift}N"), "Open global scratchpad"),
        (format!("{cmd}S"), "Save the open file"),
        (format!("{cmd}W"), "Close the editor tab"),
        (format!("{cmd}F / {replace}"), "Find / find and replace"),
        (format!("{cmd}G / {cmd}{shift}G"), "Next / previous match"),
        (format!("{cmd}/"), "Toggle line comment"),
        (format!("{cmd}{shift}D"), "Duplicate line or selection"),
        (next_tab, "Next / previous editor tab"),
        (nth_tab, "Go to editor tab 1–8, or the last"),
        (modification, "Next / previous Git change in the file"),
        (
            format!("{cmd}= / {cmd}- / {cmd}0"),
            "Editor font size (window zoom elsewhere)",
        ),
        (
            format!("{cmd}D"),
            "Select the word, then add its next occurrence",
        ),
        (
            if mac {
                "⌃⌘G".to_owned()
            } else {
                format!("{alt}F3")
            },
            "Select every occurrence",
        ),
        (format!("{cmd}{shift}L"), "Split the selection into lines"),
        (
            if mac {
                "⌃⇧↑ / ↓".to_owned()
            } else {
                "Ctrl+Alt+Up / Down".to_owned()
            },
            "Add a caret on the line above / below",
        ),
        (
            if mac {
                "⌥ drag".to_owned()
            } else {
                "Alt drag".to_owned()
            },
            "Column selection",
        ),
        (
            if mac {
                format!("{cmd}{alt}[ / ]")
            } else {
                "Ctrl+Shift+[ / ]".to_owned()
            },
            "Fold / unfold (⌘K ⌘0 unfolds all)",
        ),
        (format!("{cmd}F2 / F2"), "Toggle bookmark / next bookmark"),
    ];
    egui::Grid::new("about_shortcuts")
        .num_columns(2)
        .spacing(egui::vec2(18.0, 6.0))
        .show(ui, |ui| {
            for (keys, action) in shortcuts {
                ui.label(crate::ui::chrome::shortcut_job(&keys, FS_TINY, c_text()));
                ui.label(RichText::new(action).size(FS_TINY).color(c_text_muted()));
                ui.end_row();
            }
        });
}
