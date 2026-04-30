//! Settings page: profiles panel, system prompt panel, OAuth sections.

use eframe::egui::{
    self, Align, Button, Color32, Frame, Layout, Margin, RichText, Rounding, ScrollArea, Sense,
    Stroke, TextEdit, Ui,
};

use crate::oauth::{clear_codex, clear_copilot, load_oauth_store, save_oauth_store, OAuthUiMsg};
use crate::settings::{LlmProviderKind, ALL_TOOL_NAMES};
use crate::theme::{
    C_ACCENT, C_BG_ELEVATED, C_BG_ELEVATED_2, C_BG_MAIN, C_BG_SIDEBAR, C_BORDER, C_BORDER_SUBTLE,
    C_ROW_ACTIVE, C_ROW_HOVER, C_SUCCESS, C_TEXT, C_TEXT_FAINT, C_TEXT_MUTED, FS_BODY, FS_SMALL,
    FS_TINY, SIDEBAR_RESIZE_SEP_W,
};
use crate::ui::chrome::{
    card_frame, field_label, ghost_button, hairline, nested_card_frame, pill_tab, primary_button,
    settings_caption, settings_nav_row, settings_section_title,
};

use super::state::SettingsTab;
use super::task_runner::spawn_async_task;
use super::OxiApp;

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
                        .fill(C_BG_SIDEBAR)
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
                Stroke::new(1.0, C_BORDER_SUBTLE),
            );
            ui.add_space(SIDEBAR_RESIZE_SEP_W);

            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), full_h),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    Frame::none().fill(C_BG_MAIN).show(ui, |ui| {
                        self.render_settings_header(ui);
                        ScrollArea::vertical()
                            .id_salt("settings_page_scroll")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                Frame::none()
                                    .inner_margin(Margin {
                                        left: 36.0,
                                        right: 36.0,
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
            .fill(C_BG_MAIN)
            .inner_margin(Margin {
                left: 36.0,
                right: 24.0,
                top: 16.0,
                bottom: 14.0,
            })
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Settings").size(20.0).color(C_TEXT).strong());
                    ui.add_space(10.0);
                    ui.label(
                        RichText::new("Preferences for oxi")
                            .size(FS_SMALL)
                            .color(C_TEXT_MUTED),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add(
                                Button::new(
                                    RichText::new("✕  Close").size(FS_SMALL).color(C_TEXT_MUTED),
                                )
                                .fill(C_BG_ELEVATED)
                                .stroke(Stroke::new(1.0, C_BORDER_SUBTLE))
                                .rounding(7.0)
                                .min_size(egui::vec2(0.0, 26.0)),
                            )
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
            Stroke::new(1.0, C_BORDER_SUBTLE),
        );
    }

    fn render_settings_sidebar(&mut self, ui: &mut Ui) {
        ui.set_min_width(ui.max_rect().width());

        if ui
            .add(
                Button::new(
                    RichText::new("←  Back to chat")
                        .size(FS_SMALL)
                        .color(C_TEXT_MUTED),
                )
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
            (SettingsTab::Profiles, "⚙", "Profiles & models"),
            (SettingsTab::Prompt, "✎", "System prompt"),
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
                .color(C_TEXT_FAINT)
                .monospace(),
            );
            ui.horizontal(|ui| {
                ui.label(RichText::new("●").size(FS_TINY).color(C_SUCCESS));
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Auto-saved")
                        .size(FS_TINY)
                        .color(C_TEXT_MUTED),
                );
            });
            ui.add_space(2.0);
        });
    }

    fn render_settings_body(&mut self, ui: &mut Ui) {
        match self.conv.settings_tab {
            SettingsTab::Profiles => self.render_settings_profiles_panel(ui),
            SettingsTab::Prompt => self.render_settings_system_prompt_panel(ui),
        }
    }

    fn render_settings_profiles_panel(&mut self, ui: &mut Ui) {
        settings_section_title(
            ui,
            "Profiles & models",
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
                    .color(C_TEXT)
                    .strong(),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if primary_button(ui, "+ Add profile")
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
                        .color(C_TEXT_MUTED),
                );
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Click \"+ Add profile\" above to create one.")
                        .size(FS_TINY)
                        .color(C_TEXT_FAINT),
                );
            });
            ui.add_space(12.0);
        }

        for idx in profile_indices {
            self.render_profile_card(ui, idx);
            ui.add_space(10.0);
        }

        // Provider OAuth (single section below cards, for clarity)
        match provider {
            LlmProviderKind::GitHubCopilot => {
                self.render_copilot_oauth_section(ui);
                ui.add_space(10.0);
            }
            LlmProviderKind::GptCodex => {
                self.render_codex_oauth_section(ui);
                ui.add_space(10.0);
            }
            _ => {}
        }

        // Tools section
        ui.add_space(8.0);
        hairline(ui);
        ui.add_space(18.0);
        settings_section_title(
            ui,
            "Tools",
            Some("Enable or disable the tools the agent is allowed to call."),
        );
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
        });
        ui.add_space(10.0);
        ui.label(
            RichText::new(
                "If a profile key is empty, the app falls back to environment variables. \
                 OAuth still takes precedence where available.",
            )
            .size(FS_TINY)
            .color(C_TEXT_FAINT),
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
                let dot_col = if selected { C_ACCENT } else { C_TEXT_FAINT };
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
                        .color(C_TEXT_MUTED),
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
                    } else if ui
                        .add(
                            Button::new(RichText::new("Make active").size(FS_SMALL).color(C_TEXT))
                                .fill(C_BG_ELEVATED_2)
                                .stroke(Stroke::new(1.0, C_BORDER_SUBTLE))
                                .rounding(7.0)
                                .min_size(egui::vec2(0.0, 26.0)),
                        )
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

            // Model id
            field_label(ui, "Model id");
            ui.add(
                TextEdit::singleline(&mut self.conv.settings.profiles[idx].model_id)
                    .desired_width(f32::INFINITY)
                    .hint_text("e.g. gpt-4o-mini or claude-sonnet-4")
                    .margin(Margin::symmetric(8.0, 5.0)),
            );

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
                        LlmProviderKind::GitHubCopilot => "GitHub Copilot token / PAT",
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
        });

        if make_active_clicked {
            let id = self.conv.settings.profiles[idx].id.clone();
            self.conv.settings.set_active_profile(&id);
        }
        if delete_clicked {
            let id = self.conv.settings.profiles[idx].id.clone();
            self.conv.settings.remove_profile(&id);
        }
    }

    fn render_settings_system_prompt_panel(&mut self, ui: &mut Ui) {
        settings_section_title(
            ui,
            "System prompt",
            Some("Single editable prompt. Use {tools_list} to inject the currently enabled tools."),
        );

        card_frame().show(ui, |ui| {
            settings_caption(ui, "System prompt template");
            ui.add(
                TextEdit::multiline(&mut self.conv.settings.system_prompt)
                    .desired_width(f32::INFINITY)
                    .desired_rows(20)
                    .margin(Margin::symmetric(8.0, 6.0))
                    .hint_text(crate::agent::prompt::DEFAULT_AGENT_SYSTEM_PROMPT),
            );
        });
        ui.add_space(8.0);
        ui.label(
            RichText::new("Tip: changes are saved automatically.")
                .size(FS_TINY)
                .color(C_TEXT_FAINT),
        );
    }

    // ── OAuth sections ────────────────────────────────────────────────────────

    pub(crate) fn render_copilot_oauth_section(&mut self, ui: &mut Ui) {
        let oauth = load_oauth_store();
        let signed_in = oauth.github_copilot.is_some();
        card_frame().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("GitHub Copilot OAuth")
                        .size(FS_BODY)
                        .color(C_TEXT)
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
                RichText::new(format!(
                    "Tokens stored in {}",
                    crate::oauth::oauth_config_path().display()
                ))
                .size(FS_TINY)
                .color(C_TEXT_FAINT),
            );
            ui.add_space(10.0);

            field_label(ui, "Optional Enterprise hostname (blank = github.com)");
            ui.add(
                TextEdit::singleline(&mut self.conv.copilot_enterprise_domain)
                    .desired_width(f32::INFINITY)
                    .hint_text("e.g. company.ghe.com")
                    .margin(Margin::symmetric(8.0, 5.0)),
            );
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        !self.conv.oauth_busy,
                        Button::new(
                            RichText::new("Sign in with GitHub")
                                .size(FS_SMALL)
                                .color(Color32::WHITE),
                        )
                        .fill(C_ACCENT)
                        .stroke(Stroke::NONE)
                        .rounding(7.0)
                        .min_size(egui::vec2(0.0, 28.0)),
                    )
                    .clicked()
                {
                    self.spawn_github_oauth(ui.ctx());
                }
                if ui
                    .add_enabled(signed_in, {
                        Button::new(RichText::new("Sign out").size(FS_SMALL).color(C_TEXT))
                            .fill(C_BG_ELEVATED_2)
                            .stroke(Stroke::new(1.0, C_BORDER_SUBTLE))
                            .rounding(7.0)
                            .min_size(egui::vec2(0.0, 28.0))
                    })
                    .clicked()
                {
                    let mut s = load_oauth_store();
                    clear_copilot(&mut s);
                    let _ = save_oauth_store(&s);
                    self.conv.oauth_last_message = Some("Signed out GitHub Copilot.".into());
                }
            });
            if let Some((ref url, ref code)) = self.conv.oauth_device_copilot {
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!("Open {url}\nEnter code: {code}"))
                        .size(FS_TINY)
                        .color(C_ACCENT),
                );
            }
            if let Some(ref msg) = self.conv.oauth_last_message {
                ui.add_space(6.0);
                ui.label(RichText::new(msg).size(FS_TINY).color(C_TEXT_MUTED));
            }
        });
    }

    pub(crate) fn render_codex_oauth_section(&mut self, ui: &mut Ui) {
        let oauth = load_oauth_store();
        let signed_in = oauth.openai_codex.is_some();
        card_frame().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("ChatGPT / Codex OAuth")
                        .size(FS_BODY)
                        .color(C_TEXT)
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
                    .color(C_TEXT_FAINT),
            );
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        !self.conv.oauth_busy,
                        Button::new(
                            RichText::new("Sign in with ChatGPT")
                                .size(FS_SMALL)
                                .color(Color32::WHITE),
                        )
                        .fill(C_ACCENT)
                        .stroke(Stroke::NONE)
                        .rounding(7.0)
                        .min_size(egui::vec2(0.0, 28.0)),
                    )
                    .clicked()
                {
                    self.spawn_codex_oauth(ui.ctx());
                }
                if ui
                    .add_enabled(signed_in, {
                        Button::new(RichText::new("Sign out").size(FS_SMALL).color(C_TEXT))
                            .fill(C_BG_ELEVATED_2)
                            .stroke(Stroke::new(1.0, C_BORDER_SUBTLE))
                            .rounding(7.0)
                            .min_size(egui::vec2(0.0, 28.0))
                    })
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
                ui.label(RichText::new(msg).size(FS_TINY).color(C_TEXT_MUTED));
            }
        });
    }

    // ── OAuth spawn helpers ───────────────────────────────────────────────────

    fn spawn_github_oauth(&mut self, ctx: &egui::Context) {
        if self.conv.oauth_busy {
            return;
        }
        self.conv.oauth_busy = true;
        self.conv.oauth_last_message = None;
        let (tx, rx) = std::sync::mpsc::channel();
        self.conn.oauth_rx = Some(rx);
        let ctx = ctx.clone();
        let ent = self.conv.copilot_enterprise_domain.clone();
        spawn_async_task(
            {
                let tx = tx.clone();
                let ctx = ctx.clone();
                move |err| {
                    let _ = tx.send(OAuthUiMsg::GitHubDone(Err(err)));
                    ctx.request_repaint();
                }
            },
            move |rt| {
                let tx2 = tx.clone();
                let r = rt.block_on(crate::oauth::login_github_copilot(&ent, tx2));
                let _ = tx.send(OAuthUiMsg::GitHubDone(r));
                ctx.request_repaint();
            },
        );
    }

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
}

/// Small "Active" / "Signed in" pill.
fn active_pill(ui: &mut Ui, text: &str) {
    Frame::none()
        .fill(Color32::from_rgba_unmultiplied(
            C_ACCENT.r(),
            C_ACCENT.g(),
            C_ACCENT.b(),
            32,
        ))
        .stroke(Stroke::new(
            1.0,
            Color32::from_rgba_unmultiplied(C_ACCENT.r(), C_ACCENT.g(), C_ACCENT.b(), 90),
        ))
        .rounding(999.0)
        .inner_margin(Margin::symmetric(10.0, 3.0))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(FS_TINY).color(C_ACCENT).strong());
        });
}

fn inactive_pill(ui: &mut Ui, text: &str) {
    Frame::none()
        .fill(C_BG_ELEVATED_2)
        .stroke(Stroke::new(1.0, C_BORDER_SUBTLE))
        .rounding(999.0)
        .inner_margin(Margin::symmetric(10.0, 3.0))
        .show(ui, |ui| {
            ui.label(
                RichText::new(text)
                    .size(FS_TINY)
                    .color(C_TEXT_MUTED)
                    .strong(),
            );
        });
}

fn tool_chip(ui: &mut Ui, name: &str, enabled: bool) -> egui::Response {
    let icon = if enabled { "✓" } else { "·" };
    let label_fid = egui::FontId::proportional(FS_SMALL);
    let label_galley = ui
        .painter()
        .layout_no_wrap(name.to_string(), label_fid.clone(), C_TEXT);
    let icon_galley = ui
        .painter()
        .layout_no_wrap(icon.to_string(), label_fid.clone(), C_ACCENT);

    let pad = egui::vec2(12.0, 6.0);
    let icon_gap = 8.0;
    let size = egui::vec2(
        icon_galley.rect.width() + icon_gap + label_galley.rect.width() + pad.x * 2.0,
        label_galley.rect.height().max(icon_galley.rect.height()) + pad.y * 2.0,
    );
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let hovered = response.hovered();
    let (fill, stroke_col, text_col) = if enabled && hovered {
        (C_ROW_ACTIVE, C_BORDER, C_TEXT)
    } else if enabled {
        (C_ROW_ACTIVE, C_BORDER_SUBTLE, C_TEXT)
    } else if hovered {
        (C_ROW_HOVER, C_BORDER_SUBTLE, C_TEXT_MUTED)
    } else {
        (C_BG_ELEVATED_2, C_BORDER_SUBTLE, C_TEXT_MUTED)
    };
    let r = Rounding::same(999.0);
    ui.painter().rect_filled(rect, r, fill);
    ui.painter()
        .rect_stroke(rect, r, Stroke::new(1.0, stroke_col));
    let icon_col = if enabled { C_ACCENT } else { C_TEXT_FAINT };
    let top = rect.center().y - label_galley.rect.height() * 0.5;
    let icon_x = rect.left() + pad.x;
    ui.painter()
        .galley(egui::pos2(icon_x, top), icon_galley, icon_col);
    let label_x = icon_x + 14.0;
    ui.painter()
        .galley(egui::pos2(label_x, top), label_galley, text_col);
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}
