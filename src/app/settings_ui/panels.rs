//! The three top-level settings panels: Models & providers, Agent, and Appearance.

use eframe::egui::{self, Align, Layout, RichText, Ui};

use crate::settings::{ALL_TOOL_NAMES, LlmProviderKind};
use crate::theme::*;
use crate::ui::chrome::{
    card_frame, field_hint, field_label, field_label_first, ghost_button, hairline, pill_tab,
    settings_caption, settings_card_header, settings_section_title, settings_text_area,
    settings_text_field, settings_text_field_width,
};

use super::super::OxiApp;
use super::layout::tool_chip;

/// Provider groups keep the picker skimmable (10 pills in one row is hard to scan).
const PROVIDER_GROUPS: &[(&str, &[LlmProviderKind])] = &[
    (
        "Local / self-hosted",
        &[
            LlmProviderKind::LocalHf,
            LlmProviderKind::Ollama,
            LlmProviderKind::LmStudio,
        ],
    ),
    (
        "Hosted APIs",
        &[
            LlmProviderKind::OpenAi,
            LlmProviderKind::OpenRouter,
            LlmProviderKind::AzureOpenAi,
            LlmProviderKind::CustomAnthropic,
            LlmProviderKind::GptCodex,
            LlmProviderKind::OpenCodeGo,
        ],
    ),
    ("External agents", &[LlmProviderKind::ClaudeCodeAcp]),
];

/// Tool chips grouped by intent so the Agent panel is scannable.
const TOOL_GROUPS: &[(&str, &[&str])] = &[
    ("Read", &["read", "grep", "find", "ls", "codebase_search"]),
    ("Write", &["write", "edit"]),
    ("Shell", &["bash"]),
    ("Git", &["git_status", "git_diff"]),
    ("Web", &["web_search", "web_fetch"]),
];

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

        ui.add_space(16.0);
        settings_caption(ui, "Choose provider");
        ui.add_space(4.0);

        // Grouped pill bars instead of one dense wrapped row of 10 providers.
        for (group_label, providers) in PROVIDER_GROUPS {
            ui.label(
                RichText::new(*group_label)
                    .size(FS_TINY)
                    .color(c_text_muted()),
            );
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                for &provider in *providers {
                    let selected = self.conv.settings_provider_tab == provider;
                    if pill_tab(ui, provider.label(), selected) {
                        self.conv.settings_provider_tab = provider;
                    }
                }
            });
            ui.add_space(10.0);
        }

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

    pub(super) fn render_settings_agent_panel(&mut self, ui: &mut Ui) {
        settings_section_title(
            ui,
            "Tools & safety",
            Some(
                "Control which tools the agent can call, when it must ask first, and how web search works.",
            ),
        );

        // ── Tools ──────────────────────────────────────────────────────────
        card_frame().show(ui, |ui| {
            settings_card_header(
                ui,
                "Enabled tools",
                Some("Click a chip to toggle. Disabled tools are hidden from the model."),
            );
            for (gi, (group, names)) in TOOL_GROUPS.iter().enumerate() {
                if gi > 0 {
                    ui.add_space(10.0);
                }
                ui.label(RichText::new(*group).size(FS_TINY).color(c_text_muted()));
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(8.0, 6.0);
                    for name in *names {
                        let Some(i) = ALL_TOOL_NAMES.iter().position(|n| n == name) else {
                            continue;
                        };
                        let enabled = self.conv.settings.tools_enabled[i];
                        if tool_chip(ui, name, enabled).clicked() {
                            self.conv.settings.tools_enabled[i] = !enabled;
                        }
                    }
                });
            }
        });

        // ── MCP servers ────────────────────────────────────────────────────
        ui.add_space(12.0);
        card_frame().show(ui, |ui| {
            settings_card_header(
                ui,
                "MCP servers",
                Some("Stdio MCP servers. Tools appear as mcp_<name>_<tool> in the agent."),
            );
            let mut remove_idx: Option<usize> = None;
            let n = self.conv.settings.mcp_servers.len();
            for i in 0..n {
                let server = &mut self.conv.settings.mcp_servers[i];
                ui.horizontal(|ui| {
                    ui.checkbox(&mut server.enabled, "");
                    settings_text_field_width(ui, &mut server.name, "name", 100.0);
                    settings_text_field_width(ui, &mut server.command, "command", 120.0);
                    let mut args = server.args.join(" ");
                    if settings_text_field_width(ui, &mut args, "args…", 180.0).changed() {
                        server.args = args.split_whitespace().map(str::to_string).collect();
                    }
                    if ghost_button(ui, "Remove", true).clicked() {
                        remove_idx = Some(i);
                    }
                });
                ui.add_space(4.0);
            }
            if let Some(i) = remove_idx {
                self.conv.settings.mcp_servers.remove(i);
            }
            if ghost_button(ui, "Add MCP server", false).clicked() {
                self.conv
                    .settings
                    .mcp_servers
                    .push(crate::settings::McpServerConfig::default());
            }
        });

        // ── Approvals ──────────────────────────────────────────────────────
        ui.add_space(12.0);
        card_frame().show(ui, |ui| {
            settings_card_header(
                ui,
                "Approvals",
                Some("When on, the agent pauses and asks before each matching tool call."),
            );
            let mut require_write_edit_approval = self.conv.settings.require_write_edit_approval;
            if ui
                .checkbox(
                    &mut require_write_edit_approval,
                    RichText::new("Ask before file changes")
                        .size(FS_SMALL)
                        .color(c_text()),
                )
                .on_hover_text(
                    "When on, the agent pauses before write, edit, delete, move, or mkdir tool calls.",
                )
                .changed()
            {
                self.conv.settings.require_write_edit_approval = require_write_edit_approval;
            }
            let mut require_bash_approval = self.conv.settings.require_bash_approval;
            if ui
                .checkbox(
                    &mut require_bash_approval,
                    RichText::new("Ask before bash").size(FS_SMALL).color(c_text()),
                )
                .on_hover_text(
                    "When on, the agent pauses for your approval before each bash tool call.",
                )
                .changed()
            {
                self.conv.settings.require_bash_approval = require_bash_approval;
            }
            ui.add_space(4.0);
            ui.label(
                RichText::new(
                    "Bash is not sandboxed; the approval prompt is the real safety boundary. Read-only tools never require approval.",
                )
                .size(FS_TINY)
                .color(c_text_faint()),
            );
        });

        // ── Limits ─────────────────────────────────────────────────────────
        ui.add_space(12.0);
        card_frame().show(ui, |ui| {
            settings_card_header(
                ui,
                "Limits",
                Some("Caps that keep a runaway agent loop from going forever."),
            );

            field_label_first(ui, "Max tool calls per run (0 = unlimited)");
            let mut max_rounds = self.conv.settings.max_tool_rounds.to_string();
            if settings_text_field_width(ui, &mut max_rounds, "0", 180.0).changed() {
                let trimmed = max_rounds.trim();
                if trimmed.is_empty() {
                    self.conv.settings.max_tool_rounds = 0;
                } else if let Ok(n) = trimmed.parse::<u32>() {
                    self.conv.settings.max_tool_rounds = n;
                }
            }
            field_hint(
                ui,
                "Caps tool-call rounds in a single agent run. 0 disables the cap.",
            );

            field_label(ui, "Bash timeout cap (seconds)");
            let mut bash_cap = self.conv.settings.bash_timeout_cap_secs.to_string();
            if settings_text_field_width(ui, &mut bash_cap, "300", 180.0).changed()
                && let Ok(n) = bash_cap.trim().parse::<u32>()
                && n >= 1
            {
                self.conv.settings.bash_timeout_cap_secs = n.clamp(5, 3600);
            }
            field_hint(
                ui,
                "Upper bound for one bash call (5–3600s). The model's own timeout is clamped to this.",
            );
        });

        // ── Web search ─────────────────────────────────────────────────────
        ui.add_space(12.0);
        card_frame().show(ui, |ui| {
            settings_card_header(
                ui,
                "Web search",
                Some("Backend used by the web_search tool."),
            );
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let current = self.conv.settings.web_search_backend;
                for b in crate::settings::WebSearchBackend::ALL {
                    if pill_tab(ui, b.label(), b == current) && b != current {
                        self.conv.settings.web_search_backend = b;
                    }
                }
            });
            ui.add_space(8.0);
            match self.conv.settings.web_search_backend {
                crate::settings::WebSearchBackend::Bing => {
                    ui.label(
                        RichText::new(
                            "Zero-config. Uses Bing's RSS results feed. No fallback if Bing fails.",
                        )
                        .size(FS_TINY)
                        .color(c_text_muted()),
                    );
                }
                crate::settings::WebSearchBackend::DuckDuckGo => {
                    ui.label(
                        RichText::new(
                            "Zero-config. DuckDuckGo HTML endpoint — may serve a bot challenge; Bing is usually more reliable.",
                        )
                        .size(FS_TINY)
                        .color(c_text_muted()),
                    );
                }
                crate::settings::WebSearchBackend::SearXng => {
                    field_label_first(ui, "SearXNG instance URL");
                    settings_text_field(
                        ui,
                        &mut self.conv.settings.searxng_url,
                        "https://searxng.example.com",
                    )
                    .on_hover_text(
                        "Base URL of your SearXNG instance. JSON output must be enabled (search.formats: [html, json]).",
                    );
                    if self.conv.settings.searxng_url.trim().is_empty() {
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(
                                "No URL set — web_search will report a configuration error.",
                            )
                            .size(FS_TINY)
                            .color(c_text_faint()),
                        );
                    } else {
                        field_hint(ui, "Requires JSON format enabled on the instance.");
                    }
                }
            }
        });
    }

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
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let current = self.conv.settings.commit_msg_provider;
                if pill_tab(ui, "Active provider", current.is_none()) && current.is_some() {
                    self.conv.settings.commit_msg_provider = None;
                    self.conv.settings.commit_msg_model_id.clear();
                }
            });
            ui.add_space(8.0);
            for (group_label, providers) in PROVIDER_GROUPS {
                ui.label(
                    RichText::new(*group_label)
                        .size(FS_TINY)
                        .color(c_text_muted()),
                );
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let current = self.conv.settings.commit_msg_provider;
                    for &kind in *providers {
                        if pill_tab(ui, kind.label(), current == Some(kind))
                            && current != Some(kind)
                        {
                            self.conv.settings.commit_msg_provider = Some(kind);
                        }
                    }
                });
                ui.add_space(8.0);
            }
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
            let config_dir = config_path.parent().unwrap_or_else(|| std::path::Path::new("."));
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

    /// Register the current font choice and rebuild egui's font atlas so the change shows
    /// immediately (theme stays as-is).
    fn apply_font_selection(&self, ctx: &egui::Context) {
        crate::theme::set_active_fonts(crate::theme::FontSelection {
            ui: self.conv.settings.ui_font.clone(),
            mono: self.conv.settings.mono_font.clone(),
        });
        crate::theme::setup_style(ctx);
    }

    pub(super) fn render_settings_appearance_panel(&mut self, ui: &mut Ui) {
        settings_section_title(
            ui,
            "Appearance",
            Some("Theme, text size, and chat column width."),
        );
        card_frame().show(ui, |ui| {
            let themes = crate::theme::available_themes();
            let current = self.conv.settings.theme_id.clone();
            settings_card_header(
                ui,
                "Theme",
                Some("Built-in themes plus any custom JSON themes."),
            );
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                for t in &themes {
                    if pill_tab(ui, &t.name, t.id == current) && t.id != current {
                        self.conv.settings.theme_id = t.id.clone();
                        crate::theme::apply_theme(ui.ctx(), &t.id);
                    }
                }
            });

            ui.add_space(14.0);
            hairline(ui);
            ui.add_space(12.0);
            settings_card_header(
                ui,
                "Font",
                Some("Interface and code fonts. System fonts are detected automatically."),
            );
            settings_caption(ui, "Interface");
            ui.add_space(4.0);
            let current = self.conv.settings.ui_font.clone();
            let current_name = crate::theme::ui_font_options()
                .iter()
                .find(|opt| opt.id == current)
                .map(|opt| opt.name.as_str())
                .unwrap_or("Default");
            let mut selected = None;
            egui::ComboBox::from_id_salt("interface_font_combo")
                .selected_text(current_name)
                .width(320.0)
                .height(360.0)
                .show_ui(ui, |ui| {
                    let search_id = egui::Id::new("interface_font_search");
                    let mut search = ui
                        .ctx()
                        .data(|data| data.get_temp::<String>(search_id))
                        .unwrap_or_default();
                    ui.add(
                        egui::TextEdit::singleline(&mut search)
                            .hint_text("Search fonts…")
                            .desired_width(290.0),
                    );
                    ui.ctx()
                        .data_mut(|data| data.insert_temp(search_id, search.clone()));
                    ui.separator();
                    let needle = search.trim().to_lowercase();
                    for opt in crate::theme::ui_font_options().iter().filter(|opt| {
                        needle.is_empty() || opt.name.to_lowercase().contains(&needle)
                    }) {
                        if ui.selectable_label(opt.id == current, &opt.name).clicked() {
                            selected = Some(opt.id.clone());
                        }
                    }
                });
            if let Some(id) = selected.filter(|id| id != &current) {
                self.conv.settings.ui_font = id;
                self.apply_font_selection(ui.ctx());
            }

            ui.add_space(8.0);
            settings_caption(ui, "Code / monospace");
            ui.add_space(4.0);
            let current = self.conv.settings.mono_font.clone();
            let current_name = crate::theme::mono_font_options()
                .iter()
                .find(|opt| opt.id == current)
                .map(|opt| opt.name.as_str())
                .unwrap_or("Default");
            let mut selected = None;
            egui::ComboBox::from_id_salt("monospace_font_combo")
                .selected_text(current_name)
                .width(320.0)
                .height(360.0)
                .show_ui(ui, |ui| {
                    let search_id = egui::Id::new("monospace_font_search");
                    let mut search = ui
                        .ctx()
                        .data(|data| data.get_temp::<String>(search_id))
                        .unwrap_or_default();
                    ui.add(
                        egui::TextEdit::singleline(&mut search)
                            .hint_text("Search fonts…")
                            .desired_width(290.0),
                    );
                    ui.ctx()
                        .data_mut(|data| data.insert_temp(search_id, search.clone()));
                    ui.separator();
                    let needle = search.trim().to_lowercase();
                    for opt in crate::theme::mono_font_options().iter().filter(|opt| {
                        needle.is_empty() || opt.name.to_lowercase().contains(&needle)
                    }) {
                        if ui.selectable_label(opt.id == current, &opt.name).clicked() {
                            selected = Some(opt.id.clone());
                        }
                    }
                });
            if let Some(id) = selected.filter(|id| id != &current) {
                self.conv.settings.mono_font = id;
                self.apply_font_selection(ui.ctx());
            }

            ui.add_space(14.0);
            hairline(ui);
            ui.add_space(12.0);
            let current_density = self.conv.settings.ui_density;
            settings_card_header(
                ui,
                "Text size",
                Some("Scales the whole UI (density / zoom)."),
            );
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                for d in crate::settings::UiDensity::ALL {
                    if pill_tab(ui, d.label(), d == current_density) && d != current_density {
                        self.conv.settings.ui_density = d;
                        ui.ctx().set_zoom_factor(d.zoom_factor());
                    }
                }
            });

            ui.add_space(14.0);
            hairline(ui);
            ui.add_space(12.0);
            settings_card_header(
                ui,
                "Chat width",
                Some("Max width of the message column on wide screens."),
            );
            ui.add(
                egui::Slider::new(
                    &mut self.conv.settings.chat_column_max_width,
                    CHAT_COLUMN_WIDTH_MIN..=CHAT_COLUMN_WIDTH_MAX,
                )
                .suffix("px"),
            );
            field_hint(ui, "Raise it when the sidebar or git panel is hidden.");
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

fn provider_blurb(kind: LlmProviderKind) -> &'static str {
    match kind {
        LlmProviderKind::LocalHf => {
            "Download GGUF models from HuggingFace and run them via oxi-managed llama-server."
        }
        LlmProviderKind::Ollama => {
            "Talk to a local or LAN Ollama server (OpenAI-compatible /v1 API)."
        }
        LlmProviderKind::LmStudio => "Talk to a local or LAN LM Studio server (OpenAI-compatible).",
        LlmProviderKind::OpenAi => "OpenAI Chat Completions API.",
        LlmProviderKind::OpenRouter => "OpenRouter multi-model router.",
        LlmProviderKind::AzureOpenAi => "Azure OpenAI deployment endpoint.",
        LlmProviderKind::CustomAnthropic => "Any Anthropic Messages-compatible endpoint.",
        LlmProviderKind::GptCodex => "ChatGPT / Codex via OAuth or OpenAI API-key fallback.",
        LlmProviderKind::OpenCodeGo => "OpenCode Go subscription endpoint.",
        LlmProviderKind::ClaudeCodeAcp => {
            "Drive Claude Code as an external agent over the Agent Client Protocol."
        }
    }
}
