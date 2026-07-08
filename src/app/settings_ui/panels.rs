//! The three top-level settings panels: Models & providers, Agent, and Appearance.

use eframe::egui::{self, Align, Layout, Margin, RichText, TextEdit, Ui};

use crate::settings::{ALL_TOOL_NAMES, LlmProviderKind};
use crate::theme::*;
use crate::ui::chrome::{
    card_frame, field_label, hairline, pill_tab, settings_caption, settings_section_title,
};

use super::super::OxiApp;
use super::layout::tool_chip;

impl OxiApp {
    pub(super) fn render_settings_providers_panel(&mut self, ui: &mut Ui) {
        settings_section_title(
            ui,
            "Models & providers",
            Some("Configure LLM providers, API keys, and the default model."),
        );

        // Provider pill bar
        settings_caption(ui, "Provider");
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            for provider in LlmProviderKind::ALL {
                let selected = self.conv.settings_provider_tab == provider;
                if pill_tab(ui, provider.label(), selected) {
                    self.conv.settings_provider_tab = provider;
                }
            }
        });
        ui.add_space(16.0);

        let provider = self.conv.settings_provider_tab;
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
                } else if crate::ui::chrome::ghost_button(ui, "Make active", false)
                    .on_hover_text("Use this provider for new chats")
                    .clicked()
                {
                    self.conv.settings.active_provider = provider;
                }
            });
        });
        ui.add_space(10.0);

        self.render_provider_config(ui, provider);
        ui.add_space(10.0);

        // Provider OAuth (single section below the config, for clarity)
        if provider == LlmProviderKind::GptCodex {
            ui.add_space(6.0);
            settings_caption(ui, "OAuth");
            ui.add_space(6.0);
            self.render_codex_oauth_section(ui);
            ui.add_space(10.0);
        }

        ui.add_space(6.0);
        ui.label(
            RichText::new(
                "If the API key is empty, the app falls back to environment variables. \
                 OAuth still takes precedence where available.",
            )
            .size(FS_TINY)
            .color(c_text_faint()),
        );
    }

    pub(super) fn render_settings_agent_panel(&mut self, ui: &mut Ui) {
        // Tools section
        settings_section_title(
            ui,
            "Agent",
            Some("Control which tools the agent can call, approval behavior, and web search."),
        );
        settings_caption(ui, "Tools");
        ui.add_space(4.0);
        card_frame().show(ui, |ui| {
            let n = ALL_TOOL_NAMES.len();
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(10.0, 8.0);
                for (i, name) in ALL_TOOL_NAMES.iter().enumerate().take(n) {
                    let enabled = self.conv.settings.tools_enabled[i];
                    if tool_chip(ui, name, enabled).clicked() {
                        self.conv.settings.tools_enabled[i] = !enabled;
                    }
                }
            });
            ui.add_space(10.0);
            hairline(ui);
            ui.add_space(8.0);
            let mut require_write_edit_approval = self.conv.settings.require_write_edit_approval;
            if ui
                .checkbox(
                    &mut require_write_edit_approval,
                    RichText::new("Ask before write / edit")
                        .size(FS_SMALL)
                        .color(c_text()),
                )
                .on_hover_text(
                    "When on, the agent pauses for your approval before each write/edit tool call.",
                )
                .changed()
            {
                self.conv.settings.require_write_edit_approval = require_write_edit_approval;
            }
            let mut require_bash_approval = self.conv.settings.require_bash_approval;
            if ui
                .checkbox(
                    &mut require_bash_approval,
                    RichText::new("Ask before bash")
                        .size(FS_SMALL)
                        .color(c_text()),
                )
                .on_hover_text(
                    "When on, the agent pauses for your approval before each bash tool call.",
                )
                .changed()
            {
                self.conv.settings.require_bash_approval = require_bash_approval;
            }
            ui.add_space(10.0);
            hairline(ui);
            ui.add_space(8.0);
            field_label(ui, "Max tool calls per run (0 = unlimited)");
            let mut max_rounds = self.conv.settings.max_tool_rounds.to_string();
            let resp = ui.add(
                TextEdit::singleline(&mut max_rounds)
                    .desired_width(180.0)
                    .hint_text("0")
                    .margin(Margin::symmetric(8, 5)),
            );
            if resp.changed() {
                let trimmed = max_rounds.trim();
                if trimmed.is_empty() {
                    self.conv.settings.max_tool_rounds = 0;
                } else if let Ok(n) = trimmed.parse::<u32>() {
                    self.conv.settings.max_tool_rounds = n;
                }
            }
            ui.label(
                RichText::new(
                    "Caps the number of tool-call rounds in a single agent run. 0 disables the cap (unlimited, the default).",
                )
                .size(FS_TINY)
                .color(c_text_muted()),
            );

            ui.add_space(10.0);
            hairline(ui);
            ui.add_space(8.0);
            field_label(ui, "Bash timeout cap (seconds)");
            let mut bash_cap = self.conv.settings.bash_timeout_cap_secs.to_string();
            let resp = ui.add(
                TextEdit::singleline(&mut bash_cap)
                    .desired_width(180.0)
                    .hint_text("300")
                    .margin(Margin::symmetric(8, 5)),
            );
            if resp.changed()
                && let Ok(n) = bash_cap.trim().parse::<u32>()
                && n >= 1
            {
                self.conv.settings.bash_timeout_cap_secs = n.clamp(5, 3600);
            }
            ui.label(
                RichText::new(
                    "Upper bound for a single bash tool call. The model's own timeout argument is clamped to this (5–3600s).",
                )
                .size(FS_TINY)
                .color(c_text_muted()),
            );
        });

        // Web search section
        ui.add_space(16.0);
        settings_caption(ui, "Web search");
        ui.add_space(4.0);
        card_frame().show(ui, |ui| {
            ui.label(
                RichText::new("Search backend (web_search)")
                    .size(FS_SMALL)
                    .color(c_text()),
            );
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let current = self.conv.settings.web_search_backend;
                for b in crate::settings::WebSearchBackend::ALL {
                    if pill_tab(ui, b.label(), b == current) && b != current {
                        self.conv.settings.web_search_backend = b;
                    }
                }
            });
            ui.add_space(6.0);
            match self.conv.settings.web_search_backend {
                crate::settings::WebSearchBackend::Bing => {
                    ui.label(
                        RichText::new(
                            "Zero-config. Searches Bing's stable RSS results feed with no API \
                             key or setup. No fallback is used; if Bing fails, the error is shown.",
                        )
                        .size(FS_TINY)
                        .color(c_text_muted()),
                    );
                }
                crate::settings::WebSearchBackend::DuckDuckGo => {
                    ui.label(
                        RichText::new(
                            "Zero-config. Searches DuckDuckGo's HTML endpoint with no API key \
                             or setup. May rate-limit under heavy use. Note: DuckDuckGo now \
                             serves a bot-challenge page, so Bing is recommended instead.",
                        )
                        .size(FS_TINY)
                        .color(c_text_muted()),
                    );
                }
                crate::settings::WebSearchBackend::SearXng => {
                    ui.label(
                        RichText::new("SearXNG instance URL")
                            .size(FS_TINY)
                            .color(c_text_muted()),
                    );
                    ui.add_space(4.0);
                    ui.add(
                        TextEdit::singleline(&mut self.conv.settings.searxng_url)
                            .hint_text("https://searxng.example.com")
                            .desired_width(f32::INFINITY),
                    )
                    .on_hover_text(
                        "Base URL of your SearXNG instance. Its JSON output format must be \
                         enabled (search.formats: [html, json]). No fallback is used; if this \
                         URL is missing or invalid, the error is shown.",
                    );
                    ui.add_space(4.0);
                    if self.conv.settings.searxng_url.trim().is_empty() {
                        ui.label(
                            RichText::new(
                                "No URL set — web_search will report a configuration error until \
                                 you add one.",
                            )
                            .size(FS_TINY)
                            .color(c_text_faint()),
                        );
                    }
                }
            }
        });

        ui.add_space(8.0);
        ui.label(
            RichText::new("Tip: changes are saved automatically.")
                .size(FS_TINY)
                .color(c_text_faint()),
        );
    }

    pub(super) fn render_settings_prompts_panel(&mut self, ui: &mut Ui) {
        settings_section_title(
            ui,
            "Prompts",
            Some("Edit the agent system prompt and the commit-message generator."),
        );

        // Agent system prompt section
        settings_caption(ui, "Agent system prompt");
        ui.add_space(4.0);
        card_frame().show(ui, |ui| {
            ui.label(
                RichText::new(
                    "Single editable prompt. Use {tools_list} to inject the currently enabled tools.",
                )
                .size(FS_TINY)
                .color(c_text_muted()),
            );
            ui.add_space(4.0);
            ui.add(
                TextEdit::multiline(&mut self.conv.settings.system_prompt)
                    .desired_width(f32::INFINITY)
                    .desired_rows(20)
                    .margin(Margin::symmetric(8, 6))
                    .hint_text(crate::agent::prompt::DEFAULT_AGENT_SYSTEM_PROMPT),
            );
        });

        // Commit-message generator section
        ui.add_space(16.0);
        settings_caption(ui, "Commit message generator");
        ui.add_space(4.0);
        card_frame().show(ui, |ui| {
            ui.label(
                RichText::new(
                    "The Generate button in the git panel drafts a commit message from \
                     the staged diff. Pick which provider and model it uses and its own \
                     system prompt, kept separate from the agent prompt above.",
                )
                .size(FS_TINY)
                .color(c_text_muted()),
            );

            ui.add_space(8.0);
            settings_caption(ui, "Provider");
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let current = self.conv.settings.commit_msg_provider;
                if pill_tab(ui, "Active provider", current.is_none()) && current.is_some() {
                    self.conv.settings.commit_msg_provider = None;
                    self.conv.settings.commit_msg_model_id.clear();
                }
                for kind in LlmProviderKind::ALL {
                    if pill_tab(ui, kind.label(), current == Some(kind)) && current != Some(kind) {
                        self.conv.settings.commit_msg_provider = Some(kind);
                    }
                }
            });
            if let Some(kind) = self.conv.settings.commit_msg_provider {
                ui.add_space(6.0);
                field_label(ui, "Model (empty = provider's selected model)");
                let hint = self.conv.settings.provider(kind).model_id.clone();
                ui.add(
                    TextEdit::singleline(&mut self.conv.settings.commit_msg_model_id)
                        .desired_width(320.0)
                        .hint_text(hint)
                        .margin(Margin::symmetric(8, 5)),
                );
                ui.label(
                    RichText::new("Tip: choose a cheap/fast model for commit messages (for example claude-haiku-4-5 or a small local coder model).")
                        .size(FS_TINY)
                        .color(c_text_muted()),
                );
            }

            ui.add_space(10.0);
            settings_caption(ui, "System prompt");
            ui.add_space(4.0);
            ui.add(
                TextEdit::multiline(&mut self.conv.settings.commit_msg_system_prompt)
                    .desired_width(f32::INFINITY)
                    .desired_rows(8)
                    .margin(Margin::symmetric(8, 6))
                    .hint_text(crate::settings::DEFAULT_COMMIT_MSG_SYSTEM_PROMPT),
            );
        });

        ui.add_space(8.0);
        ui.label(
            RichText::new("Tip: changes are saved automatically.")
                .size(FS_TINY)
                .color(c_text_faint()),
        );
    }

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

    pub(super) fn render_settings_appearance_panel(&mut self, ui: &mut Ui) {
        settings_section_title(
            ui,
            "Appearance",
            Some("Switch the color theme. Built-in themes plus any custom themes found on disk."),
        );
        card_frame().show(ui, |ui| {
            let themes = crate::theme::available_themes();
            let current = self.conv.settings.theme_id.clone();
            settings_caption(ui, "Theme");
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                for t in &themes {
                    if pill_tab(ui, &t.name, t.id == current) && t.id != current {
                        self.conv.settings.theme_id = t.id.clone();
                        crate::theme::apply_theme(ui.ctx(), &t.id);
                    }
                }
            });

            ui.add_space(12.0);
            let current_density = self.conv.settings.ui_density;
            settings_caption(ui, "Text size");
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                for d in crate::settings::UiDensity::ALL {
                    if pill_tab(ui, d.label(), d == current_density) && d != current_density {
                        self.conv.settings.ui_density = d;
                        ui.ctx().set_zoom_factor(d.zoom_factor());
                    }
                }
            });

            ui.add_space(12.0);
            settings_caption(ui, "Chat width");
            ui.add(
                egui::Slider::new(
                    &mut self.conv.settings.chat_column_max_width,
                    CHAT_COLUMN_WIDTH_MIN..=CHAT_COLUMN_WIDTH_MAX,
                )
                .suffix("px"),
            );
            ui.label(
                RichText::new("Max width of the message column. Raise it to fill a wide screen or the space freed by hiding the sidebar/git panel.")
                    .size(FS_TINY)
                    .color(c_text_faint()),
            );
        });
        ui.add_space(10.0);
        ui.label(
            RichText::new(format!(
                "Add a custom theme by dropping a <name>.json file in {}.",
                crate::theme::custom_themes_dir().display()
            ))
            .size(FS_TINY)
            .color(c_text_faint()),
        );
    }
}
