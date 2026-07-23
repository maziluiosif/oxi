//! Provider selection settings page.

use super::super::OxiApp;
use super::provider_catalog::{PROVIDER_GROUPS, provider_blurb};
use crate::settings::LlmProviderKind;
use crate::theme::*;
use crate::ui::chrome::{card_frame, settings_caption, settings_section_title};
use eframe::egui::{self, Align, Layout, RichText, Ui};

impl OxiApp {
    pub(super) fn render_settings_providers_panel(&mut self, ui: &mut Ui) {
        settings_section_title(
            ui,
            "Models & providers",
            Some("Pick a backend, configure credentials, then make it active for new chats."),
        );

        // Active provider summary strip — answers "what am I using?" without hunting pills.
        {
            let active = self.conv.settings.active_provider;
            let model = self.conv.settings.provider(active).model_id.clone();
            card_frame().show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new("Currently active")
                                .size(FS_TINY)
                                .color(c_text_faint()),
                        );
                        ui.add_space(2.0);
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(active.label())
                                    .size(FS_BODY)
                                    .color(c_text())
                                    .strong(),
                            );
                            if !model.is_empty() {
                                ui.label(
                                    RichText::new(format!("/ {model}"))
                                        .size(FS_SMALL)
                                        .color(c_text_muted())
                                        .monospace(),
                                );
                            }
                        });
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        super::layout::active_pill(ui, "Active");
                    });
                });
            });
        }

        ui.add_space(12.0);
        settings_caption(ui, "Configure provider");
        ui.add_space(4.0);
        let current_provider = self.conv.settings_provider_tab;
        egui::ComboBox::from_id_salt("settings_provider_picker")
            .selected_text(current_provider.label())
            .width(320.0)
            .show_ui(ui, |ui| {
                for (group_label, providers) in PROVIDER_GROUPS {
                    ui.label(
                        RichText::new(*group_label)
                            .size(FS_TINY)
                            .color(c_text_faint())
                            .strong(),
                    );
                    for &provider in *providers {
                        if ui
                            .selectable_label(provider == current_provider, provider.label())
                            .clicked()
                        {
                            self.conv.settings_provider_tab = provider;
                        }
                    }
                    ui.add_space(4.0);
                }
            });
        ui.add_space(10.0);

        let provider = self.conv.settings_provider_tab;
        card_frame().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(provider.label())
                        .size(FS_BODY)
                        .color(c_text())
                        .strong(),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let is_active = self.conv.settings.active_provider == provider;
                    if is_active {
                        super::layout::active_pill(ui, "Active");
                    } else if crate::ui::chrome::primary_button(ui, "Make active")
                        .on_hover_text("Use this provider for new chats")
                        .clicked()
                    {
                        self.conv.settings.active_provider = provider;
                    }
                });
            });
            ui.add_space(2.0);
            ui.label(
                RichText::new(provider_blurb(provider))
                    .size(FS_TINY)
                    .color(c_text_muted()),
            );
        });
        ui.add_space(12.0);

        self.render_provider_config(ui, provider);

        // Provider OAuth (single section below the config, for clarity)
        if provider == LlmProviderKind::GptCodex {
            ui.add_space(12.0);
            settings_caption(ui, "Sign-in");
            ui.add_space(6.0);
            self.render_codex_oauth_section(ui);
        }

        ui.add_space(10.0);
        ui.label(
            RichText::new(
                "Empty API key falls back to environment variables. OAuth still takes precedence where available. Keys are stored in the OS keychain, not in settings.json.",
            )
            .size(FS_TINY)
            .color(c_text_faint()),
        );
    }
}
