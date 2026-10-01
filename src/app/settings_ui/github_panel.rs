//! Git identity and GitHub authentication settings.

use super::super::OxiApp;
use crate::theme::*;
use crate::ui::chrome::{
    card_frame, field_hint, field_label, field_label_first, ghost_button, settings_card_header,
    settings_section_title, settings_text_field,
};
use eframe::egui::{self, RichText, Ui};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// `git --version` results keyed by the configured executable path. Probed on a worker thread so
/// typing a path never blocks the UI on process spawns.
#[derive(Clone)]
enum GitProbe {
    Pending,
    Done(Result<String, String>),
}

fn git_probe(ctx: &egui::Context, configured: &str) -> GitProbe {
    static PROBES: OnceLock<Mutex<HashMap<String, GitProbe>>> = OnceLock::new();
    let probes = PROBES.get_or_init(Default::default);
    let key = configured.trim().to_owned();
    let Ok(mut cache) = probes.lock() else {
        return GitProbe::Pending;
    };
    if let Some(state) = cache.get(&key) {
        return state.clone();
    }
    cache.insert(key.clone(), GitProbe::Pending);
    drop(cache);
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let result = crate::git::system_git_version(&key);
        if let Ok(mut cache) = probes.lock() {
            cache.insert(key, GitProbe::Done(result));
        }
        ctx.request_repaint();
    });
    GitProbe::Pending
}

impl OxiApp {
    pub(super) fn render_settings_github_panel(&mut self, ui: &mut Ui) {
        settings_section_title(
            ui,
            "GitHub",
            Some(
                "Authenticate push, pull, and fetch with a GitHub token, or use the Git installed on this machine.",
            ),
        );

        card_frame().show(ui, |ui| {
            settings_card_header(
                ui,
                "Commit identity",
                Some("Author name and email written into commits created by oxi."),
            );
            field_label_first(ui, "Author name");
            settings_text_field(ui, &mut self.conv.settings.git_author_name, "Your name");
            field_label(ui, "Author email");
            settings_text_field(
                ui,
                &mut self.conv.settings.git_author_email,
                "you@example.com",
            );
            field_hint(
                ui,
                "Leave both empty to use identity from the repository or global Git config.",
            );
        });

        ui.add_space(12.0);
        card_frame().show(ui, |ui| {
            settings_card_header(
                ui,
                "GitHub authentication",
                Some("Use a fine-grained personal access token with access to the repositories you work with."),
            );
            field_label_first(ui, "GitHub username");
            settings_text_field(ui, &mut self.conv.settings.github_username, "octocat");
            field_hint(ui, "Your GitHub account name, not your email address.");
            field_label(ui, "Personal access token");
            crate::ui::chrome::settings_password_field(
                ui,
                &mut self.conv.settings.github_token,
                "github_pat_… or ghp_…",
            );
            field_hint(
                ui,
                "The token is stored only in your OS keychain. Fine-grained tokens need access to the repository and Contents: Read and write permission. Organization tokens may also require SSO authorization and administrator approval.",
            );
            if self.conv.settings.git_use_system_cli {
                field_hint(
                    ui,
                    "System Git is enabled below, so this token is not used for push, pull, or fetch.",
                );
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if self.conv.settings.github_token.trim().is_empty() {
                    super::layout::inactive_pill(ui, "Not configured");
                } else {
                    super::layout::active_pill(ui, "Configured");
                }
                if ghost_button(ui, "Create token on GitHub", false).clicked() {
                    let _ = webbrowser::open("https://github.com/settings/personal-access-tokens/new?name=oxi&description=Native%20Git%20push%20from%20oxi&contents=write");
                }
                if !self.conv.settings.github_token.is_empty()
                    && ghost_button(ui, "Clear token", true).clicked()
                {
                    self.conv.settings.github_token.clear();
                }
            });
        });

        ui.add_space(12.0);
        card_frame().show(ui, |ui| {
            settings_card_header(
                ui,
                "System Git",
                Some("For remotes that reject token authentication, such as company servers."),
            );
            let mut use_system = self.conv.settings.git_use_system_cli;
            if ui
                .checkbox(
                    &mut use_system,
                    RichText::new("Use system Git for push, pull, and fetch")
                        .size(FS_SMALL)
                        .color(c_text()),
                )
                .changed()
            {
                self.conv.settings.git_use_system_cli = use_system;
            }
            field_hint(
                ui,
                "Use the system git installation and its credentials (SSH keys, credential manager) instead of the token. Status, diffs, commits, and merges still run natively.",
            );
            field_label(ui, "Git executable");
            settings_text_field(
                ui,
                &mut self.conv.settings.git_executable,
                "Auto-detect (git on PATH)",
            );
            ui.add_space(4.0);
            match git_probe(ui.ctx(), &self.conv.settings.git_executable) {
                GitProbe::Pending => field_hint(ui, "Detecting Git…"),
                GitProbe::Done(Ok(version)) => {
                    ui.horizontal(|ui| {
                        super::layout::active_pill(ui, "Found");
                        ui.label(RichText::new(version).size(FS_SMALL).color(c_text_muted()));
                    });
                }
                GitProbe::Done(Err(error)) => {
                    ui.horizontal(|ui| {
                        super::layout::inactive_pill(ui, "Not found");
                    });
                    field_hint(ui, &error);
                }
            }
        });

        ui.add_space(12.0);
        card_frame().show(ui, |ui| {
            settings_card_header(
                ui,
                "Native Git engine",
                Some("Status, diffs, commits, branches and network operations run through bundled libgit2."),
            );
            ui.label(
                RichText::new("Unless system Git is enabled, HTTPS GitHub remotes use the token above and SSH remotes use your running SSH agent. TLS certificates remain verified by libgit2.")
                    .size(FS_SMALL)
                    .color(c_text_muted()),
            );
        });
    }
}
