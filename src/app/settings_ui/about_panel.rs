//! About, updates, and diagnostics settings page.

use eframe::egui::{self, RichText, Ui};

use crate::theme::*;
use crate::ui::chrome::{card_frame, hairline, settings_caption, settings_section_title};

use super::super::OxiApp;

impl OxiApp {
    pub(super) fn render_settings_about_panel(&mut self, ui: &mut Ui) {
        settings_section_title(ui, "About", Some("Version and updates."));
        card_frame().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("oxi").size(FS_H1).color(c_text()).strong());
                ui.add_space(8.0);
                ui.label(
                    RichText::new(format!("Version {}", crate::update::APP_VERSION))
                        .size(FS_SMALL)
                        .color(c_text_muted()),
                );
            });
            ui.add_space(2.0);
            ui.label(
                RichText::new("Standalone coding agent chat UI.")
                    .size(FS_TINY)
                    .color(c_text_faint()),
            );

            ui.add_space(10.0);
            hairline(ui);
            ui.add_space(8.0);
            settings_caption(ui, "Updates");
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        !self.conv.update_checking,
                        crate::ui::chrome::ghost_button_widget("Check for updates", false),
                    )
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
                    self.ensure_update_checked(ui.ctx(), true);
                }
                ui.add_space(8.0);
                if self.conv.update_checking {
                    ui.label(
                        RichText::new("Checking…")
                            .size(FS_TINY)
                            .color(c_text_muted()),
                    );
                } else if let Some(info) = self.update_available().cloned() {
                    ui.label(
                        RichText::new(format!("Update available: v{}", info.version))
                            .size(FS_TINY)
                            .color(c_accent())
                            .strong(),
                    );
                    ui.add_space(6.0);
                    if ui
                        .add(crate::ui::chrome::primary_button_widget("View release"))
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .clicked()
                    {
                        let _ = webbrowser::open(&info.html_url);
                    }
                } else {
                    match &self.conv.update_result {
                        Some(Ok(_)) => {
                            ui.label(
                                RichText::new("You're up to date.")
                                    .size(FS_TINY)
                                    .color(c_text_muted()),
                            );
                        }
                        Some(Err(_)) => {
                            ui.label(
                                RichText::new("Couldn't check for updates.")
                                    .size(FS_TINY)
                                    .color(c_text_muted()),
                            );
                        }
                        None => {}
                    }
                }
            });
            ui.label(
                RichText::new("Checked once at startup against the latest GitHub release.")
                    .size(FS_TINY)
                    .color(c_text_faint()),
            );

            ui.add_space(10.0);
            hairline(ui);
            ui.add_space(8.0);
            settings_caption(ui, "Diagnostics");
            ui.add_space(4.0);
            let config_path = crate::settings::AppSettings::config_path();
            let config_dir = config_path
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."));
            ui.label(
                RichText::new(format!(
                    "OS: {} · Architecture: {}\nConfig: {}\nWorkspace: {}",
                    std::env::consts::OS,
                    std::env::consts::ARCH,
                    config_dir.display(),
                    self.active_workspace().root_path
                ))
                .size(FS_TINY)
                .monospace()
                .color(c_text_muted()),
            );
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
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
                    let _ = webbrowser::open(&format!("file://{}", config_dir.display()));
                }
                let crash_log = config_dir.join("crash.log");
                if crash_log.is_file()
                    && crate::ui::chrome::ghost_button(ui, "Open crash log", false).clicked()
                {
                    let _ = webbrowser::open(&format!("file://{}", crash_log.display()));
                }
            });

            ui.add_space(10.0);
            hairline(ui);
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if crate::ui::chrome::ghost_button(ui, "GitHub", false).clicked() {
                    let _ = webbrowser::open(crate::update::REPO_URL);
                }
                ui.add_space(4.0);
                if crate::ui::chrome::ghost_button(ui, "Changelog", false).clicked() {
                    let _ = webbrowser::open(&format!(
                        "{}/blob/master/CHANGELOG.md",
                        crate::update::REPO_URL
                    ));
                }
            });
        });
    }
}
