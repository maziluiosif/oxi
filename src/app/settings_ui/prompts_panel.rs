//! Agent and commit-message prompt settings.

use super::super::OxiApp;
use super::provider_catalog::PROVIDER_GROUPS;
use crate::theme::*;
use crate::ui::chrome::{
    card_frame, field_hint, field_label, ghost_button, settings_caption, settings_card_header,
    settings_section_title, settings_text_area, settings_text_field_width,
};
use eframe::egui::{self, Align, Layout, RichText, Ui};

impl OxiApp {
    pub(super) fn render_settings_prompts_panel(&mut self, ui: &mut Ui) {
        settings_section_title(
            ui,
            "Prompts",
            Some("Edit the agent system prompt and the commit-message generator."),
        );

        // Agent system prompt section
        card_frame().show(ui, |ui| {
            settings_card_header(
                ui,
                "Agent system prompt",
                Some("Single editable prompt. Use {tools_list} to inject currently enabled tools."),
            );
            let mut include_agents_md = self.conv.settings.include_agents_md;
            if ui
                .checkbox(
                    &mut include_agents_md,
                    RichText::new("Include workspace AGENTS.md")
                        .size(FS_SMALL)
                        .color(c_text()),
                )
                .on_hover_text(
                    "When on, oxi appends root-level AGENTS.md project instructions to the agent system prompt.",
                )
                .changed()
            {
                self.conv.settings.include_agents_md = include_agents_md;
            }
            let mut include_oxi_rules = self.conv.settings.include_oxi_rules;
            if ui
                .checkbox(
                    &mut include_oxi_rules,
                    RichText::new("Include .oxi/rules and .cursor/rules")
                        .size(FS_SMALL)
                        .color(c_text()),
                )
                .on_hover_text(
                    "When on, oxi appends markdown rules from .oxi/rules/ and .cursor/rules/ to the agent system prompt.",
                )
                .changed()
            {
                self.conv.settings.include_oxi_rules = include_oxi_rules;
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ghost_button(ui, "Reset to default", false)
                        .on_hover_text("Replace with the built-in agent system prompt")
                        .clicked()
                    {
                        self.conv.settings.system_prompt =
                            crate::agent::prompt::DEFAULT_AGENT_SYSTEM_PROMPT.to_string();
                    }
                });
            });
            settings_text_area(
                ui,
                &mut self.conv.settings.system_prompt,
                crate::agent::prompt::DEFAULT_AGENT_SYSTEM_PROMPT,
                20,
            );
        });

        // Commit-message generator section
        ui.add_space(16.0);
        card_frame().show(ui, |ui| {
            settings_card_header(
                ui,
                "Commit message generator",
                Some(
                    "The Generate button in the git panel drafts a commit message from the staged diff. Uses its own provider/model and prompt, separate from the agent.",
                ),
            );

            settings_caption(ui, "Provider");
            ui.add_space(4.0);
            let current = self.conv.settings.commit_msg_provider;
            let current_label = current.map_or("Use active provider", |provider| provider.label());
            egui::ComboBox::from_id_salt("commit_message_provider_combo")
                .selected_text(current_label)
                .width(320.0)
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(current.is_none(), "Use active provider")
                        .clicked()
                    {
                        self.conv.settings.commit_msg_provider = None;
                        self.conv.settings.commit_msg_model_id.clear();
                    }
                    ui.separator();
                    for (group_label, providers) in PROVIDER_GROUPS {
                        ui.label(
                            RichText::new(*group_label)
                                .size(FS_TINY)
                                .color(c_text_faint())
                                .strong(),
                        );
                        for &kind in *providers {
                            if ui
                                .selectable_label(current == Some(kind), kind.label())
                                .clicked()
                            {
                                self.conv.settings.commit_msg_provider = Some(kind);
                            }
                        }
                        ui.add_space(4.0);
                    }
                });
            if let Some(kind) = self.conv.settings.commit_msg_provider {
                field_label(ui, "Model (empty = provider's selected model)");
                let hint = self.conv.settings.provider(kind).model_id.clone();
                settings_text_field_width(
                    ui,
                    &mut self.conv.settings.commit_msg_model_id,
                    &hint,
                    320.0,
                );
                field_hint(
                    ui,
                    "Tip: choose a cheap/fast model for commit messages (e.g. claude-haiku-4-5 or a small local coder).",
                );
            }

            ui.add_space(10.0);
            ui.horizontal(|ui| {
                settings_caption(ui, "System prompt");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ghost_button(ui, "Reset to default", false).clicked() {
                        self.conv.settings.commit_msg_system_prompt =
                            crate::settings::DEFAULT_COMMIT_MSG_SYSTEM_PROMPT.to_string();
                    }
                });
            });
            ui.add_space(4.0);
            settings_text_area(
                ui,
                &mut self.conv.settings.commit_msg_system_prompt,
                crate::settings::DEFAULT_COMMIT_MSG_SYSTEM_PROMPT,
                8,
            );
        });
    }
}
