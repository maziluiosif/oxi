//! Settings page: profiles panel, system prompt panel, OAuth sections.

use eframe::egui::{
    self, Align, Button, Color32, FontId, Frame, Layout, Margin, RichText, Rounding, ScrollArea,
    Sense, Stroke, TextEdit, Ui,
};

use crate::oauth::{clear_codex, load_oauth_store, save_oauth_store, OAuthUiMsg};
use crate::settings::{
    ComputeLocation, LlmProviderKind, ProviderProfile, SshConfig, ALL_TOOL_NAMES,
};
use crate::theme::*;
use crate::ui::chrome::{
    card_frame, field_label, ghost_button, hairline, nested_card_frame, pill_tab,
    primary_button_icon, settings_caption, settings_nav_row, settings_section_title,
};

use super::state::SettingsTab;
use super::task_runner::spawn_async_task;
use super::{ModelFetchMsg, OxiApp, SshTestMsg};

const SETTINGS_CONTENT_MAX: f32 = 820.0;

impl OxiApp {
    pub(crate) fn render_settings_page(&mut self, ui: &mut Ui) {
        let settings_before = self.conv.settings.clone();
        const SIDEBAR_W_MIN: f32 = 180.0;
        const SIDEBAR_W_MAX: f32 = 320.0;
        let full_h = ui.available_height();

        ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
            ui.set_min_height(full_h);
            ui.spacing_mut().item_spacing.x = 0.0;

            let w = self.conv.sidebar_width.clamp(SIDEBAR_W_MIN, SIDEBAR_W_MAX);
            ui.allocate_ui_with_layout(
                egui::vec2(w, full_h),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    Frame::none()
                        .fill(c_bg_sidebar())
                        .inner_margin(Margin {
                            left: 12.0,
                            right: 10.0,
                            top: 12.0,
                            bottom: 12.0,
                        })
                        .show(ui, |ui| {
                            ui.set_min_width(ui.max_rect().width());
                            ui.set_min_height(ui.max_rect().height());
                            self.render_settings_sidebar(ui);
                        });
                    ui.expand_to_include_rect(ui.max_rect());
                },
            );

            let boundary_x = ui.cursor().min.x;
            ui.painter().vline(
                boundary_x,
                egui::Rangef::new(ui.min_rect().top(), ui.min_rect().top() + full_h),
                Stroke::new(1.0, c_border_subtle()),
            );
            ui.add_space(SIDEBAR_RESIZE_SEP_W);

            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), full_h),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    Frame::none().fill(c_bg_main()).show(ui, |ui| {
                        self.render_settings_header(ui);
                        ScrollArea::vertical()
                            .id_salt("settings_page_scroll")
                            .auto_shrink([false, false])
                            .scroll_bar_visibility(
                                egui::scroll_area::ScrollBarVisibility::AlwaysVisible,
                            )
                            .show(ui, |ui| {
                                Frame::none()
                                    .inner_margin(Margin {
                                        left: 36.0,
                                        // The reserved 10px scroll gutter adds to this;
                                        // 26 + 10 keeps optical symmetry with the left.
                                        right: 26.0,
                                        top: 24.0,
                                        bottom: 48.0,
                                    })
                                    .show(ui, |ui| {
                                        ui.set_max_width(SETTINGS_CONTENT_MAX);
                                        self.render_settings_body(ui);
                                    });
                            });
                        ui.expand_to_include_rect(ui.max_rect());
                    });
                    ui.expand_to_include_rect(ui.max_rect());
                },
            );
        });

        if self.conv.settings != settings_before {
            if let Err(e) = self.conv.settings.save() {
                self.run_state_mut(self.active_session_key()).stream_error =
                    Some(format!("Save settings: {e}"));
            }
        }
    }

    fn render_settings_header(&mut self, ui: &mut Ui) {
        Frame::none()
            .fill(c_bg_main())
            .inner_margin(Margin {
                left: 36.0,
                right: 24.0,
                top: 16.0,
                bottom: 14.0,
            })
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Settings")
                            .size(FS_H1)
                            .color(c_text())
                            .strong(),
                    );
                    ui.add_space(10.0);
                    ui.label(
                        RichText::new("Preferences for oxi")
                            .size(FS_SMALL)
                            .color(c_text_muted()),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if crate::ui::chrome::ghost_button_icon(ui, ICON_CLOSE, "Close", false)
                            .on_hover_text("Back to chat")
                            .clicked()
                        {
                            self.conv.settings_open = false;
                        }
                    });
                });
            });
        ui.painter().hline(
            ui.min_rect().x_range(),
            ui.cursor().min.y,
            Stroke::new(1.0, c_border_subtle()),
        );
    }

    fn render_settings_sidebar(&mut self, ui: &mut Ui) {
        ui.set_min_width(ui.max_rect().width());

        if ui
            .add(
                Button::new(crate::ui::chrome::icon_label_job(
                    ICON_CHEVRON_LEFT,
                    "Back to chat",
                    FS_SMALL,
                    c_text_muted(),
                ))
                .frame(false)
                .fill(Color32::TRANSPARENT),
            )
            .on_hover_text("Close settings")
            .clicked()
        {
            self.conv.settings_open = false;
        }

        ui.add_space(18.0);
        settings_caption(ui, "Settings");
        ui.add_space(4.0);

        let items = [
            (SettingsTab::Providers, ICON_PROVIDERS, "Models & providers"),
            (SettingsTab::Agent, ICON_AGENT, "Agent"),
            (SettingsTab::Appearance, ICON_APPEARANCE, "Appearance"),
        ];
        for (tab, icon, label) in items {
            let selected = self.conv.settings_tab == tab;
            let response = settings_nav_row(ui, icon, label, selected);
            if response.clicked() {
                self.conv.settings_tab = tab;
            }
            ui.add_space(2.0);
        }

        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            ui.add_space(4.0);
            ui.label(
                RichText::new(format!(
                    "~/{}",
                    crate::settings::AppSettings::config_path()
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("settings.json")
                ))
                .size(FS_TINY)
                .color(c_text_faint())
                .monospace(),
            );
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(ICON_CHECK_CIRCLE)
                        .font(FontId::new(FS_TINY, icon_font()))
                        .color(c_success()),
                );
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Auto-saved")
                        .size(FS_TINY)
                        .color(c_text_muted()),
                );
            });
            ui.add_space(2.0);
        });
    }

    fn render_settings_body(&mut self, ui: &mut Ui) {
        match self.conv.settings_tab {
            SettingsTab::Providers => self.render_settings_providers_panel(ui),
            SettingsTab::Agent => self.render_settings_agent_panel(ui),
            SettingsTab::Appearance => self.render_settings_appearance_panel(ui),
        }
    }

    fn render_settings_providers_panel(&mut self, ui: &mut Ui) {
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
                RichText::new(format!("{} profiles", provider.label()))
                    .size(FS_BODY)
                    .color(c_text())
                    .strong(),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if primary_button_icon(ui, ICON_PLUS, "Add profile")
                    .on_hover_text("Create a new profile for this provider")
                    .clicked()
                {
                    let id = self.conv.settings.add_profile(provider);
                    self.conv.settings.set_active_profile(&id);
                }
            });
        });
        ui.add_space(10.0);

        let profile_indices: Vec<usize> = self
            .conv
            .settings
            .profiles
            .iter()
            .enumerate()
            .filter(|(_, p)| p.provider == provider)
            .map(|(i, _)| i)
            .collect();

        if profile_indices.is_empty() {
            card_frame().show(ui, |ui| {
                ui.label(
                    RichText::new("No profiles for this provider yet.")
                        .size(FS_SMALL)
                        .color(c_text_muted()),
                );
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Click \"+ Add profile\" above to create one.")
                        .size(FS_TINY)
                        .color(c_text_faint()),
                );
            });
            ui.add_space(12.0);
        }

        for idx in profile_indices {
            self.render_profile_card(ui, idx);
            ui.add_space(10.0);
        }

        // Provider OAuth (single section below cards, for clarity)
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
                "If a profile key is empty, the app falls back to environment variables. \
                 OAuth still takes precedence where available.",
            )
            .size(FS_TINY)
            .color(c_text_faint()),
        );
    }

    fn render_settings_agent_panel(&mut self, ui: &mut Ui) {
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
            let mut require_approval = self.conv.settings.require_approval;
            if ui
                .checkbox(
                    &mut require_approval,
                    RichText::new("Ask before running bash / write / edit")
                        .size(FS_SMALL)
                        .color(c_text()),
                )
                .on_hover_text(
                    "When on, the agent pauses for your approval before each mutating tool call.",
                )
                .changed()
            {
                self.conv.settings.require_approval = require_approval;
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
                    .margin(Margin::symmetric(8.0, 5.0)),
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
        });

        // Web search section
        ui.add_space(16.0);
        settings_caption(ui, "Web search");
        ui.add_space(4.0);
        card_frame().show(ui, |ui| {
            ui.label(
                RichText::new("SearXNG URL (web_search)")
                    .size(FS_SMALL)
                    .color(c_text()),
            );
            ui.add_space(4.0);
            ui.add(
                TextEdit::singleline(&mut self.conv.settings.searxng_url)
                    .hint_text("Empty = DuckDuckGo (no setup) — or https://searxng.example.com")
                    .desired_width(f32::INFINITY),
            )
            .on_hover_text(
                "Optional. Leave empty to search via DuckDuckGo with no configuration. \
                 To use your own SearXNG instance instead, set its base URL here — its JSON \
                 output format must be enabled (search.formats: [html, json]).",
            );
        });

        // System prompt section
        ui.add_space(16.0);
        settings_caption(ui, "System prompt");
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
                    .margin(Margin::symmetric(8.0, 6.0))
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
                     the staged diff. Pick which provider profile it uses and its own system \
                     prompt, kept separate from the agent prompt above.",
                )
                .size(FS_TINY)
                .color(c_text_muted()),
            );

            ui.add_space(8.0);
            settings_caption(ui, "Model profile");
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let current = self.conv.settings.commit_msg_profile_id.clone();
                if pill_tab(ui, "Active profile", current.trim().is_empty())
                    && !current.trim().is_empty()
                {
                    self.conv.settings.commit_msg_profile_id.clear();
                }
                let profiles: Vec<(String, String)> = self
                    .conv
                    .settings
                    .profiles
                    .iter()
                    .map(|p| (p.id.clone(), p.name.clone()))
                    .collect();
                for (id, name) in profiles {
                    if pill_tab(ui, &name, id == current) && id != current {
                        self.conv.settings.commit_msg_profile_id = id;
                    }
                }
            });

            ui.add_space(10.0);
            settings_caption(ui, "System prompt");
            ui.add_space(4.0);
            ui.add(
                TextEdit::multiline(&mut self.conv.settings.commit_msg_system_prompt)
                    .desired_width(f32::INFINITY)
                    .desired_rows(8)
                    .margin(Margin::symmetric(8.0, 6.0))
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

    fn render_settings_appearance_panel(&mut self, ui: &mut Ui) {
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

    fn render_profile_card(&mut self, ui: &mut Ui, idx: usize) {
        let mut delete_clicked = false;
        let mut make_active_clicked = false;
        let active_id = self.conv.settings.active_profile_id.clone();
        let prov = self.conv.settings.profiles[idx].provider;
        let selected = active_id == self.conv.settings.profiles[idx].id;

        card_frame().show(ui, |ui| {
            // Header: status dot, name editor, "Active" pill, delete
            ui.horizontal(|ui| {
                let dot_col = if selected { c_accent() } else { c_text_faint() };
                ui.label(RichText::new("●").size(FS_BODY).color(dot_col));
                ui.add_space(4.0);

                ui.add(
                    TextEdit::singleline(&mut self.conv.settings.profiles[idx].name)
                        .desired_width(220.0)
                        .hint_text("Profile name")
                        .margin(Margin::symmetric(8.0, 4.0)),
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new(prov.label())
                        .size(FS_TINY)
                        .color(c_text_muted()),
                );

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ghost_button(ui, "Delete", true)
                        .on_hover_text("Remove this profile")
                        .clicked()
                    {
                        delete_clicked = true;
                    }
                    ui.add_space(6.0);
                    if selected {
                        // "Active" indicator pill (non-interactive)
                        active_pill(ui, "Active");
                    } else if crate::ui::chrome::ghost_button(ui, "Make active", false)
                        .on_hover_text("Use this profile for new chats")
                        .clicked()
                    {
                        make_active_clicked = true;
                    }
                });
            });

            ui.add_space(4.0);
            hairline(ui);
            ui.add_space(4.0);

            // Model id ─ with dropdown of models fetched from the provider's /v1/models.
            field_label(ui, "Model id");
            let pid = self.conv.settings.profiles[idx].id.clone();
            let have = self
                .conv
                .fetched_models
                .get(&pid)
                .is_some_and(|f| !f.models.is_empty());
            ui.horizontal(|ui| {
                if have {
                    let fetched = self
                        .conv
                        .fetched_models
                        .get(&pid)
                        .map(|f| f.models.clone())
                        .unwrap_or_default();
                    let current = self.conv.settings.profiles[idx].model_id.clone();
                    let label = if current.is_empty() {
                        "(custom)".to_string()
                    } else {
                        current.clone()
                    };
                    egui::ComboBox::from_id_salt(("model_combo", idx))
                        .selected_text(label)
                        .width(ui.available_width() - 30.0)
                        .show_ui(ui, |ui| {
                            if !current.is_empty() && fetched.iter().all(|x| x != &current) {
                                let _ = ui.selectable_label(false, format!("{current} (custom)"));
                            }
                            for m in &fetched {
                                if ui.selectable_label(*m == current, m.clone()).clicked() {
                                    self.conv.settings.profiles[idx].model_id = m.clone();
                                }
                            }
                        });
                } else {
                    ui.add(
                        TextEdit::singleline(&mut self.conv.settings.profiles[idx].model_id)
                            .desired_width(ui.available_width() - 30.0)
                            .hint_text("e.g. gpt-4o-mini or kimi-k2.7-code")
                            .margin(Margin::symmetric(8.0, 5.0)),
                    );
                }
                if crate::ui::chrome::icon_button(ui, ICON_REFRESH, 26.0, false)
                    .on_hover_text("Load available models from provider")
                    .clicked()
                {
                    self.spawn_model_fetch(ui.ctx(), idx);
                }
            });
            // Status line for the model fetch.
            if let Some(f) = self.conv.fetched_models.get(&pid) {
                if let Some(e) = &f.error {
                    ui.label(RichText::new(e).size(FS_TINY).color(c_danger()));
                } else if f.loading {
                    ui.label(
                        RichText::new("Loading models…")
                            .size(FS_TINY)
                            .color(c_text_muted()),
                    );
                } else if !f.models.is_empty() {
                    ui.label(
                        RichText::new(format!("{} models available", f.models.len()))
                            .size(FS_TINY)
                            .color(c_text_muted()),
                    );
                }
            }
            // Context window (auto from catalog; editable override).
            {
                let cw = self.conv.settings.profiles[idx].context_window;
                let resolved = self.conv.settings.profiles[idx]
                    .effective_context_window(self.conv.settings.context_window_default);
                field_label(ui, "Context window (tokens, 0 = auto)");
                ui.horizontal(|ui| {
                    let mut value = cw.unwrap_or(0).to_string();
                    let resp = ui.add(
                        TextEdit::singleline(&mut value)
                            .desired_width(160.0)
                            .hint_text(format!("auto ({resolved})"))
                            .margin(Margin::symmetric(8.0, 5.0)),
                    );
                    if resp.changed() {
                        let parsed = value.trim().parse::<usize>().ok();
                        self.conv.settings.profiles[idx].context_window =
                            parsed.and_then(|n| if n > 0 { Some(n) } else { None });
                    }
                    if crate::ui::chrome::ghost_button(ui, "Auto", false)
                        .on_hover_text("Resolve context window from the model catalog")
                        .clicked()
                    {
                        self.conv.settings.profiles[idx].context_window = None;
                    }
                    ui.label(
                        RichText::new(format!("effective: {resolved}"))
                            .size(FS_TINY)
                            .color(c_text_muted()),
                    );
                });
            }

            // Base URL
            field_label(ui, "Base URL (optional)");
            ui.add(
                TextEdit::singleline(&mut self.conv.settings.profiles[idx].base_url)
                    .desired_width(f32::INFINITY)
                    .hint_text(prov.default_base_url())
                    .margin(Margin::symmetric(8.0, 5.0)),
            );

            // API key
            field_label(ui, "API key / token");
            ui.add(
                TextEdit::singleline(&mut self.conv.settings.profiles[idx].api_key)
                    .password(true)
                    .desired_width(f32::INFINITY)
                    .hint_text(match prov {
                        LlmProviderKind::OpenAi => "OpenAI API key",
                        LlmProviderKind::OpenRouter => "OpenRouter API key",
                        LlmProviderKind::GptCodex => "OpenAI API key for Codex fallback",
                        LlmProviderKind::OpenCodeGo => "OpenCode Go API key",
                        LlmProviderKind::LmStudio => "Optional (LM Studio ignores it)",
                        LlmProviderKind::Ollama => "Optional (Ollama ignores it by default)",
                    })
                    .margin(Margin::symmetric(8.0, 5.0)),
            );

            if prov == LlmProviderKind::OpenRouter {
                ui.add_space(8.0);
                nested_card_frame().show(ui, |ui| {
                    settings_caption(ui, "Optional OpenRouter headers");
                    ui.add(
                        TextEdit::singleline(
                            &mut self.conv.settings.profiles[idx].openrouter_http_referer,
                        )
                        .desired_width(f32::INFINITY)
                        .hint_text("HTTP-Referer")
                        .margin(Margin::symmetric(8.0, 5.0)),
                    );
                    ui.add_space(4.0);
                    ui.add(
                        TextEdit::singleline(
                            &mut self.conv.settings.profiles[idx].openrouter_title,
                        )
                        .desired_width(f32::INFINITY)
                        .hint_text("X-Title")
                        .margin(Margin::symmetric(8.0, 5.0)),
                    );
                });
            }

            if prov == LlmProviderKind::LmStudio || prov == LlmProviderKind::Ollama {
                self.render_compute_target_section(ui, idx);
            }
        });

        if make_active_clicked {
            let id = self.conv.settings.profiles[idx].id.clone();
            self.conv.settings.set_active_profile(&id);
        }
        if delete_clicked {
            let id = self.conv.settings.profiles[idx].id.clone();
            self.conv.settings.remove_profile(&id);
            self.conv.ssh_password_drafts.remove(&id);
            self.conv.ssh_test.remove(&id);
            let mut creds = crate::compute::load_ssh_credentials();
            creds.clear(&id);
            let _ = crate::compute::save_ssh_credentials(&creds);
        }
    }

    /// "Local" vs "Remote (SSH)" compute target for a profile, shown only for self-hosted
    /// runtimes (LM Studio / Ollama) where running on another host over SSH is meaningful.
    fn render_compute_target_section(&mut self, ui: &mut Ui, idx: usize) {
        ui.add_space(8.0);
        nested_card_frame().show(ui, |ui| {
            settings_caption(ui, "Compute target");
            let is_remote = matches!(
                self.conv.settings.profiles[idx].location,
                ComputeLocation::RemoteSsh(_)
            );
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                if pill_tab(ui, "Local", !is_remote) && is_remote {
                    self.conv.settings.profiles[idx].location = ComputeLocation::Local;
                }
                if pill_tab(ui, "Remote (SSH)", is_remote) && !is_remote {
                    let remote_runtime_port = self.conv.settings.profiles[idx]
                        .provider
                        .default_remote_runtime_port();
                    self.conv.settings.profiles[idx].location =
                        ComputeLocation::RemoteSsh(SshConfig {
                            remote_runtime_port,
                            ..SshConfig::default()
                        });
                }
            });

            if let ComputeLocation::RemoteSsh(cfg) = &mut self.conv.settings.profiles[idx].location
            {
                ui.add_space(8.0);
                ui.label(
                    RichText::new(
                        "Runs the model on another host (e.g. a machine on your LAN) reached over SSH. \
                         The runtime must be listening on 127.0.0.1 on that host; oxi \
                         forwards a local port to it.",
                    )
                    .size(FS_TINY)
                    .color(c_text_faint()),
                );
                ui.add_space(6.0);

                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        field_label(ui, "SSH host");
                        ui.add(
                            TextEdit::singleline(&mut cfg.host)
                                .desired_width(200.0)
                                .hint_text("192.168.1.10 or myhost.local")
                                .margin(Margin::symmetric(8.0, 5.0)),
                        );
                    });
                    ui.add_space(8.0);
                    ui.vertical(|ui| {
                        field_label(ui, "SSH port");
                        let mut port_str = cfg.port.to_string();
                        if ui
                            .add(
                                TextEdit::singleline(&mut port_str)
                                    .desired_width(70.0)
                                    .margin(Margin::symmetric(8.0, 5.0)),
                            )
                            .changed()
                        {
                            if let Ok(p) = port_str.trim().parse::<u16>() {
                                cfg.port = p;
                            }
                        }
                    });
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        field_label(ui, "SSH user");
                        ui.add(
                            TextEdit::singleline(&mut cfg.user)
                                .desired_width(200.0)
                                .hint_text("e.g. ioan")
                                .margin(Margin::symmetric(8.0, 5.0)),
                        );
                    });
                    ui.add_space(8.0);
                    ui.vertical(|ui| {
                        field_label(ui, "Remote runtime port");
                        let mut rport_str = cfg.remote_runtime_port.to_string();
                        if ui
                            .add(
                                TextEdit::singleline(&mut rport_str)
                                    .desired_width(70.0)
                                    .margin(Margin::symmetric(8.0, 5.0)),
                            )
                            .changed()
                        {
                            if let Ok(p) = rport_str.trim().parse::<u16>() {
                                cfg.remote_runtime_port = p;
                            }
                        }
                    });
                });
            }
        });

        if !matches!(
            self.conv.settings.profiles[idx].location,
            ComputeLocation::RemoteSsh(_)
        ) {
            return;
        }
        let pid = self.conv.settings.profiles[idx].id.clone();

        // Lazily load the saved password (if any) into the in-memory draft on first touch.
        if !self.conv.ssh_password_drafts.contains_key(&pid) {
            let creds = crate::compute::load_ssh_credentials();
            let pw = creds.get(&pid).unwrap_or_default().to_string();
            self.conv.ssh_password_drafts.insert(pid.clone(), pw);
        }

        ui.add_space(8.0);
        nested_card_frame().show(ui, |ui| {
            field_label(ui, "SSH password");
            let changed = {
                let pw = self.conv.ssh_password_drafts.get_mut(&pid).unwrap();
                ui.add(
                    TextEdit::singleline(pw)
                        .password(true)
                        .desired_width(240.0)
                        .hint_text("SSH password")
                        .margin(Margin::symmetric(8.0, 5.0)),
                )
                .changed()
            };
            ui.label(
                RichText::new(
                    "Stored in ssh_credentials.json (plaintext, like oauth.json), \
                     never in settings.json.",
                )
                .size(FS_TINY)
                .color(c_text_faint()),
            );
            if changed {
                let pw = self
                    .conv
                    .ssh_password_drafts
                    .get(&pid)
                    .cloned()
                    .unwrap_or_default();
                let mut creds = crate::compute::load_ssh_credentials();
                creds.set(pid.clone(), pw);
                if let Err(e) = crate::compute::save_ssh_credentials(&creds) {
                    self.run_state_mut(self.active_session_key()).stream_error =
                        Some(format!("Save SSH password: {e}"));
                }
            }

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ghost_button(ui, "Test connection", false).clicked() {
                    self.spawn_ssh_test(ui.ctx(), idx);
                }
                ui.add_space(8.0);
                if let Some(status) = self.conv.ssh_test.get(&pid) {
                    if status.loading {
                        ui.label(
                            RichText::new("Connecting…")
                                .size(FS_TINY)
                                .color(c_text_muted()),
                        );
                    } else if let Some(result) = &status.result {
                        match result {
                            Ok(port) => {
                                ui.label(
                                    RichText::new(format!("Connected (local tunnel port {port})"))
                                        .size(FS_TINY)
                                        .color(c_accent()),
                                );
                            }
                            Err(e) => {
                                ui.label(RichText::new(e).size(FS_TINY).color(c_danger()));
                            }
                        }
                    }
                }
            });
        });
    }

    /// Kick off a background SSH "Test connection" check for the profile at `idx`'s
    /// `RemoteSsh` config, if one isn't already in flight. Results arrive on
    /// `conv.ssh_test_rx` and are drained each frame.
    fn spawn_ssh_test(&mut self, ctx: &egui::Context, idx: usize) {
        let Some(profile) = self.conv.settings.profiles.get(idx) else {
            return;
        };
        let Some(cfg) = profile.ssh_config().cloned() else {
            return;
        };
        let pid = profile.id.clone();
        let password = self
            .conv
            .ssh_password_drafts
            .get(&pid)
            .cloned()
            .unwrap_or_default();

        let entry = self.conv.ssh_test.entry(pid.clone()).or_default();
        if entry.loading {
            return;
        }
        entry.loading = true;
        entry.result = None;

        let (tx, rx) = std::sync::mpsc::channel::<SshTestMsg>();
        self.conv.ssh_test_rx = Some(rx);
        let ctx = ctx.clone();
        let tunnels = self.tunnels.clone();
        let err_tx = tx.clone();
        let err_pid = pid.clone();
        let err_ctx = ctx.clone();
        spawn_async_task(
            move |err| {
                let _ = err_tx.send(SshTestMsg {
                    profile_id: err_pid,
                    result: Err(err),
                });
                err_ctx.request_repaint();
            },
            move |rt| {
                let r = rt.block_on(tunnels.ensure_tunnel(&pid, &cfg, &password));
                let _ = tx.send(SshTestMsg {
                    profile_id: pid,
                    result: r,
                });
                ctx.request_repaint();
            },
        );
    }

    /// Drain background SSH "Test connection" results into `conv.ssh_test`. Mirrors
    /// [`Self::drain_models`].
    pub(crate) fn drain_ssh_test(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.conv.ssh_test_rx.take() else {
            return;
        };
        let mut repainted = false;
        loop {
            match rx.try_recv() {
                Ok(msg) => {
                    let entry = self.conv.ssh_test.entry(msg.profile_id).or_default();
                    entry.loading = false;
                    entry.result = Some(msg.result);
                    repainted = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    self.conv.ssh_test_rx = Some(rx);
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            }
        }
        if repainted {
            ctx.request_repaint();
        }
    }

    // ── OAuth sections ────────────────────────────────────────────────────────

    pub(crate) fn render_codex_oauth_section(&mut self, ui: &mut Ui) {
        let oauth = load_oauth_store();
        let signed_in = oauth.openai_codex.is_some();
        card_frame().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("ChatGPT / Codex OAuth")
                        .size(FS_BODY)
                        .color(c_text())
                        .strong(),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if signed_in {
                        active_pill(ui, "Signed in");
                    } else {
                        inactive_pill(ui, "Signed out");
                    }
                });
            });
            ui.add_space(2.0);
            ui.label(
                RichText::new("Browser + localhost:1455 callback")
                    .size(FS_TINY)
                    .color(c_text_faint()),
            );
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        !self.conv.oauth_busy,
                        crate::ui::chrome::primary_button_widget("Sign in with ChatGPT"),
                    )
                    .clicked()
                {
                    self.spawn_codex_oauth(ui.ctx());
                }
                if ui
                    .add_enabled(
                        signed_in,
                        crate::ui::chrome::ghost_button_widget("Sign out", false),
                    )
                    .clicked()
                {
                    let mut s = load_oauth_store();
                    clear_codex(&mut s);
                    let _ = save_oauth_store(&s);
                    self.conv.oauth_last_message = Some("Signed out Codex OAuth.".into());
                }
            });
            if let Some(ref msg) = self.conv.oauth_last_message {
                ui.add_space(6.0);
                ui.label(RichText::new(msg).size(FS_TINY).color(c_text_muted()));
            }
        });
    }

    // ── OAuth spawn helpers ───────────────────────────────────────────────────

    fn spawn_codex_oauth(&mut self, ctx: &egui::Context) {
        if self.conv.oauth_busy {
            return;
        }
        self.conv.oauth_busy = true;
        self.conv.oauth_last_message = None;
        let (tx, rx) = std::sync::mpsc::channel();
        self.conn.oauth_rx = Some(rx);
        let ctx = ctx.clone();
        spawn_async_task(
            {
                let tx = tx.clone();
                let ctx = ctx.clone();
                move |err| {
                    let _ = tx.send(OAuthUiMsg::CodexDone(Err(err)));
                    ctx.request_repaint();
                }
            },
            move |rt| {
                let tx2 = tx.clone();
                let r = rt.block_on(crate::oauth::login_openai_codex(tx2));
                let _ = tx.send(OAuthUiMsg::CodexDone(r));
                ctx.request_repaint();
            },
        );
    }

    // ── Model list fetch ─────────────────────────────────────────────────────
    /// Kick off a background `/v1/models` fetch for the profile at `idx`, if one isn't
    /// already in flight. Results arrive on `conv.model_rx` and are drained each frame.
    fn spawn_model_fetch(&mut self, ctx: &egui::Context, idx: usize) {
        let profile = match self.conv.settings.profiles.get(idx) {
            Some(p) => p.clone(),
            None => return,
        };
        let entry = self
            .conv
            .fetched_models
            .entry(profile.id.clone())
            .or_default();
        if entry.loading {
            return;
        }
        entry.loading = true;
        entry.error = None;

        let (tx, rx) = std::sync::mpsc::channel::<ModelFetchMsg>();
        // Keep only the most recent receiver live (single global channel).
        self.conv.model_rx = Some(rx);
        let ctx = ctx.clone();
        let profile_id = profile.id.clone();
        let err_tx = tx.clone();
        let err_pid = profile_id.clone();
        let err_ctx = ctx.clone();
        let tunnels = self.tunnels.clone();
        spawn_async_task(
            move |err| {
                let _ = err_tx.send(ModelFetchMsg {
                    profile_id: err_pid,
                    result: Err(err),
                });
                err_ctx.request_repaint();
            },
            move |rt| {
                let client = match reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(30))
                    .danger_accept_invalid_certs(profile.provider.allows_self_signed_tls())
                    .build()
                {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = tx.send(ModelFetchMsg {
                            profile_id,
                            result: Err(e.to_string()),
                        });
                        ctx.request_repaint();
                        return;
                    }
                };
                let base = match rt.block_on(crate::compute::resolve_base_url(&profile, &tunnels)) {
                    Ok(b) => b,
                    Err(e) => {
                        let _ = tx.send(ModelFetchMsg {
                            profile_id,
                            result: Err(e),
                        });
                        ctx.request_repaint();
                        return;
                    }
                };
                // OpenCode Go expects /v1/models but its default base lacks /v1.
                let base = if profile.provider == LlmProviderKind::OpenCodeGo
                    && !base.trim_end_matches('/').ends_with("/v1")
                {
                    format!("{}/v1", base.trim_end_matches('/'))
                } else {
                    base
                };
                let extra = if profile.provider == LlmProviderKind::OpenRouter {
                    crate::agent::runner::openrouter_extra_headers(&profile)
                } else {
                    Vec::new()
                };
                let key = match resolve_fetch_key(&profile) {
                    Ok(k) => k,
                    Err(e) => {
                        let _ = tx.send(ModelFetchMsg {
                            profile_id,
                            result: Err(e),
                        });
                        ctx.request_repaint();
                        return;
                    }
                };
                let r = rt.block_on(crate::agent::fetch_models(&client, &base, &key, &extra));
                let r = r.map(|ms| ms.into_iter().map(|m| m.id).collect::<Vec<_>>());
                let _ = tx.send(ModelFetchMsg {
                    profile_id,
                    result: r,
                });
                ctx.request_repaint();
            },
        );
    }
}

/// Resolve the bearer key to use for a model-list fetch (mirrors the runner's auth fallbacks).
fn resolve_fetch_key(profile: &ProviderProfile) -> Result<String, String> {
    let key = profile.api_key.trim();
    if !key.is_empty() {
        return Ok(key.to_string());
    }
    match profile.provider {
        LlmProviderKind::OpenAi | LlmProviderKind::GptCodex => {
            std::env::var("OPENAI_API_KEY").map_err(|_| "Set an API key to list models.".into())
        }
        LlmProviderKind::OpenRouter => {
            std::env::var("OPENROUTER_API_KEY").map_err(|_| "Set an API key to list models.".into())
        }
        // OpenCode Go, LM Studio, and Ollama expose the model list without auth.
        LlmProviderKind::OpenCodeGo | LlmProviderKind::LmStudio | LlmProviderKind::Ollama => {
            Ok(String::new())
        }
    }
}

/// Small "Active" / "Signed in" pill.
fn active_pill(ui: &mut Ui, text: &str) {
    Frame::none()
        .fill(Color32::from_rgba_unmultiplied(
            c_accent().r(),
            c_accent().g(),
            c_accent().b(),
            32,
        ))
        .stroke(Stroke::new(
            1.0,
            Color32::from_rgba_unmultiplied(c_accent().r(), c_accent().g(), c_accent().b(), 90),
        ))
        .rounding(999.0)
        .inner_margin(Margin::symmetric(10.0, 3.0))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(FS_TINY).color(c_accent()).strong());
        });
}

fn inactive_pill(ui: &mut Ui, text: &str) {
    Frame::none()
        .fill(c_bg_elevated_2())
        .stroke(Stroke::new(1.0, c_border_subtle()))
        .rounding(999.0)
        .inner_margin(Margin::symmetric(10.0, 3.0))
        .show(ui, |ui| {
            ui.label(
                RichText::new(text)
                    .size(FS_TINY)
                    .color(c_text_muted())
                    .strong(),
            );
        });
}

fn tool_chip(ui: &mut Ui, name: &str, enabled: bool) -> egui::Response {
    let icon = if enabled { ICON_CHECK } else { "·" };
    let label_fid = egui::FontId::proportional(FS_SMALL);
    let icon_fid = egui::FontId::new(FS_SMALL, icon_font());
    let label_galley = ui
        .painter()
        .layout_no_wrap(name.to_string(), label_fid.clone(), c_text());
    let icon_galley = ui
        .painter()
        .layout_no_wrap(icon.to_string(), icon_fid, c_accent());

    let pad = egui::vec2(12.0, 6.0);
    let icon_gap = 8.0;
    let size = egui::vec2(
        icon_galley.rect.width() + icon_gap + label_galley.rect.width() + pad.x * 2.0,
        label_galley.rect.height().max(icon_galley.rect.height()) + pad.y * 2.0,
    );
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let hovered = response.hovered();
    let (fill, stroke_col, text_col) = if enabled && hovered {
        (c_row_active(), c_border(), c_text())
    } else if enabled {
        (c_row_active(), c_border_subtle(), c_text())
    } else if hovered {
        (c_row_hover(), c_border_subtle(), c_text_muted())
    } else {
        (c_bg_elevated_2(), c_border_subtle(), c_text_muted())
    };
    let r = Rounding::same(999.0);
    ui.painter().rect_filled(rect, r, fill);
    ui.painter()
        .rect_stroke(rect, r, Stroke::new(1.0, stroke_col));
    let icon_col = if enabled { c_accent() } else { c_text_faint() };
    let top = rect.center().y - label_galley.rect.height() * 0.5;
    let icon_x = rect.left() + pad.x;
    let label_x = icon_x + icon_galley.rect.width() + icon_gap;
    ui.painter()
        .galley(egui::pos2(icon_x, top), icon_galley, icon_col);
    ui.painter()
        .galley(egui::pos2(label_x, top), label_galley, text_col);
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}
