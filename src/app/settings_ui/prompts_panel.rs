//! Agent and commit-message prompt settings.

use super::super::OxiApp;
use super::provider_catalog::PROVIDER_GROUPS;
use crate::settings::LlmProviderKind;
use crate::ui::chrome::{
    CONTROL_W, ghost_button, setting_row, setting_select, setting_switch, settings_disclosure,
    settings_section, settings_section_title, settings_text_area, settings_text_field_width,
};
use crate::ui::select_menu::SelectOption;
use eframe::egui::Ui;

impl OxiApp {
    pub(super) fn render_settings_prompts_panel(&mut self, ui: &mut Ui) {
        settings_section_title(
            ui,
            "Prompts",
            Some(
                "What oxi's own agent is told, and the helper model that names chats and drafts commit messages.",
            ),
        );

        settings_section(ui, "Agent system prompt", None, |ui| {
            setting_switch(
                ui,
                &mut self.conv.settings.include_agents_md,
                "Include workspace AGENTS.md",
                Some("Appends the project's root AGENTS.md instructions."),
            );
            setting_switch(
                ui,
                &mut self.conv.settings.include_oxi_rules,
                "Include .oxi/rules and .cursor/rules",
                Some("Appends the markdown rules in those folders."),
            );
            settings_disclosure(ui, "Edit the system prompt", &["System prompt"], |ui| {
                setting_row(
                    ui,
                    "System prompt",
                    Some("{tools_list} is replaced by the enabled tools."),
                    |ui| {
                        if ghost_button(ui, "Reset to default", false).clicked() {
                            self.conv.settings.system_prompt =
                                crate::agent::prompt::DEFAULT_AGENT_SYSTEM_PROMPT.to_string();
                        }
                    },
                );
                settings_text_area(
                    ui,
                    &mut self.conv.settings.system_prompt,
                    crate::agent::prompt::DEFAULT_AGENT_SYSTEM_PROMPT,
                    20,
                );
                ui.add_space(8.0);
            });
        });

        settings_section(
            ui,
            "Commit messages & chat titles",
            Some("A helper model, separate from the agent."),
            |ui| {
                setting_switch(
                    ui,
                    &mut self.conv.settings.auto_title_chats,
                    "Name new chats with this model",
                    Some(
                        "After the first reply, a short summary replaces the title. Claude Code and Codex use their own CLI login (claude -p, codex exec); other ACP agents can't. Renamed chats are kept.",
                    ),
                );

                let current = self.conv.settings.commit_msg_provider;
                let current_label = current.map_or("Active provider", |provider| provider.label());
                let kinds: Vec<Option<LlmProviderKind>> = std::iter::once(None)
                    .chain(
                        PROVIDER_GROUPS
                            .iter()
                            .flat_map(|(_, providers)| providers.iter().copied().map(Some)),
                    )
                    .collect();
                let picked = setting_select(
                    ui,
                    "commit_message_provider",
                    "Provider",
                    Some("Also drafts commit messages from the staged diff in the git panel."),
                    current_label,
                    || {
                        let mut options =
                            vec![SelectOption::new("Active provider", current.is_none())];
                        for (group, providers) in PROVIDER_GROUPS {
                            for &kind in *providers {
                                options.push(
                                    SelectOption::new(kind.label(), current == Some(kind))
                                        .group(Some((*group).to_owned())),
                                );
                            }
                        }
                        options
                    },
                );
                if let Some(kind) = picked.and_then(|i| kinds.get(i)).copied() {
                    if kind.is_none() {
                        self.conv.settings.commit_msg_model_id.clear();
                    }
                    self.conv.settings.commit_msg_provider = kind;
                }
                if let Some(kind) = self.conv.settings.commit_msg_provider {
                    let hint = self.conv.settings.provider(kind).model_id.clone();
                    setting_row(
                        ui,
                        "Model",
                        Some("Empty uses the provider's model. A small, fast one is plenty."),
                        |ui| {
                            settings_text_field_width(
                                ui,
                                &mut self.conv.settings.commit_msg_model_id,
                                &hint,
                                CONTROL_W - 18.0,
                            )
                        },
                    );
                }

                settings_disclosure(
                    ui,
                    "Edit the commit message prompt",
                    &["Commit message prompt"],
                    |ui| {
                        setting_row(ui, "Commit message prompt", None, |ui| {
                            if ghost_button(ui, "Reset to default", false).clicked() {
                                self.conv.settings.commit_msg_system_prompt =
                                    crate::settings::DEFAULT_COMMIT_MSG_SYSTEM_PROMPT.to_string();
                            }
                        });
                        settings_text_area(
                            ui,
                            &mut self.conv.settings.commit_msg_system_prompt,
                            crate::settings::DEFAULT_COMMIT_MSG_SYSTEM_PROMPT,
                            8,
                        );
                        ui.add_space(8.0);
                    },
                );
            },
        );
    }
}
