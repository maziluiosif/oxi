//! Git identity and GitHub authentication settings.

use super::super::OxiApp;
use crate::theme::*;
use crate::ui::chrome::{
    card_frame, field_hint, field_label, field_label_first, ghost_button, settings_card_header,
    settings_section_title, settings_text_field,
};
use eframe::egui::{RichText, Ui};

impl OxiApp {
    pub(super) fn render_settings_github_panel(&mut self, ui: &mut Ui) {
        settings_section_title(
            ui,
            "GitHub",
            Some("Authenticate native Git push, pull, and fetch without installing the Git CLI."),
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
                "Native Git engine",
                Some("Status, diffs, commits, branches and network operations run through bundled libgit2."),
            );
            ui.label(
                RichText::new("HTTPS GitHub remotes use the token above. SSH remotes use your running SSH agent. TLS certificates remain verified by libgit2.")
                    .size(FS_SMALL)
                    .color(c_text_muted()),
            );
        });
    }
}
