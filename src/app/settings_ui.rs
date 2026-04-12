//! Settings page: profiles panel, system prompt panel, OAuth sections.

use eframe::egui::{
    self, Align, Button, Color32, Frame, Layout, Margin, RichText, Rounding, ScrollArea, Sense,
    Stroke, TextEdit, Ui,
};

use crate::oauth::{clear_codex, clear_copilot, load_oauth_store, save_oauth_store, OAuthUiMsg};
use crate::settings::{LlmProviderKind, ALL_TOOL_NAMES};
use crate::theme::{
    C_BG_ELEVATED, C_BG_MAIN, C_BG_SIDEBAR, C_BORDER_SUBTLE, C_ROW_ACTIVE, C_ROW_HOVER,
    C_TEXT, C_TEXT_MUTED, CHAT_FRAME_BOTTOM, CHAT_FRAME_TOP, CHAT_VIEW_MARGIN_LEFT,
    CHAT_VIEW_MARGIN_RIGHT, FS_SMALL, FS_TINY, SIDEBAR_RESIZE_SEP_W,
};

use super::task_runner::spawn_async_task;
use super::OxiApp;
use super::state::SettingsTab;

impl OxiApp {
    pub(crate) fn render_settings_page(&mut self, ui: &mut Ui) {
        let settings_before = self.conv.settings.clone();
        const SIDEBAR_W_MIN: f32 = 120.0;
        const SIDEBAR_W_MAX: f32 = 520.0;
        let full_h = ui.available_height();

        ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
            ui.set_min_height(full_h);
            ui.spacing_mut().item_spacing.x = 0.0;

            if self.conv.sidebar_open {
                let w = self.conv.sidebar_width.clamp(SIDEBAR_W_MIN, SIDEBAR_W_MAX);
                ui.allocate_ui_with_layout(
                    egui::vec2(w, full_h),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        Frame::none()
                            .fill(C_BG_SIDEBAR)
                            .inner_margin(Margin {
                                left: 8.0,
                                right: 6.0,
                                top: 6.0,
                                bottom: 8.0,
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
                let sep_rect = egui::Rect::from_min_max(
                    egui::pos2(boundary_x - SIDEBAR_RESIZE_SEP_W * 0.5, ui.min_rect().top()),
                    egui::pos2(
                        boundary_x + SIDEBAR_RESIZE_SEP_W * 0.5,
                        ui.min_rect().top() + full_h,
                    ),
                );
                let sep = ui.interact(sep_rect, ui.id().with("settings_sidebar_sep"), Sense::drag());
                if sep.dragged() {
                    self.conv.sidebar_width = (self.conv.sidebar_width + sep.drag_delta().x)
                        .clamp(SIDEBAR_W_MIN, SIDEBAR_W_MAX);
                }
                if sep.hovered() || sep.dragged() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                }
                ui.painter().vline(
                    boundary_x,
                    sep_rect.y_range(),
                    Stroke::new(1.0, C_BORDER_SUBTLE),
                );
            }

            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), full_h),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    Frame::none()
                        .fill(C_BG_MAIN)
                        .inner_margin(Margin {
                            left: CHAT_VIEW_MARGIN_LEFT,
                            right: CHAT_VIEW_MARGIN_RIGHT,
                            top: CHAT_FRAME_TOP,
                            bottom: CHAT_FRAME_BOTTOM,
                        })
                        .show(ui, |ui| {
                            ScrollArea::vertical()
                                .id_salt("settings_page_scroll")
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    self.render_settings_body(ui);
                                });
                            ui.expand_to_include_rect(ui.max_rect());
                        });
                    ui.expand_to_include_rect(ui.max_rect());
                },
            );
        });

        if self.conv.settings != settings_before {
            if let Err(e) = self.conv.settings.save() {
                self.run_state_mut(self.active_session_key()).stream_error = Some(format!("Save settings: {e}"));
            }
        }
    }

    fn render_settings_sidebar(&mut self, ui: &mut Ui) {
        ui.set_min_width(ui.max_rect().width());
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 5.0;
            if ui
                .add(
                    Button::new(RichText::new("← Back").size(FS_TINY).color(C_TEXT))
                        .frame(false)
                        .fill(Color32::TRANSPARENT),
                )
                .on_hover_text("Back to chat")
                .clicked()
            {
                self.conv.settings_open = false;
            }
        });
        ui.add_space(8.0);
        ui.label(RichText::new("SETTINGS").size(FS_TINY).color(C_TEXT_MUTED));
        ui.add_space(8.0);

        for (tab, label) in [
            (SettingsTab::Profiles, "Profiles"),
            (SettingsTab::Prompt, "System prompt"),
        ] {
            let selected = self.conv.settings_tab == tab;
            let row_w = ui.available_width();
            let (rect, response) = ui.allocate_exact_size(egui::vec2(row_w, 28.0), Sense::click());
            let hovered = response.hovered();
            let fill = if selected {
                C_ROW_ACTIVE
            } else if hovered {
                C_ROW_HOVER
            } else {
                Color32::TRANSPARENT
            };
            ui.painter().rect_filled(rect, Rounding::same(6.0), fill);
            ui.allocate_new_ui(
                egui::UiBuilder::new().max_rect(rect.shrink2(egui::vec2(10.0, 4.0))),
                |ui| {
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        ui.label(RichText::new(label).size(FS_SMALL).color(C_TEXT));
                    });
                },
            );
            if hovered {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if response.clicked() {
                self.conv.settings_tab = tab;
            }
            ui.add_space(4.0);
        }

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(8.0);
        ui.label(
            RichText::new("Settings are saved automatically")
                .size(FS_TINY)
                .color(C_TEXT_MUTED),
        );
        ui.label(
            RichText::new("Stored in ~/.config/oxi/settings.json")
                .size(FS_TINY)
                .color(C_TEXT_MUTED),
        );
        ui.expand_to_include_rect(ui.max_rect());
    }

    fn render_settings_body(&mut self, ui: &mut Ui) {
        match self.conv.settings_tab {
            SettingsTab::Profiles => self.render_settings_profiles_panel(ui),
            SettingsTab::Prompt => self.render_settings_system_prompt_panel(ui),
        }
    }

    fn render_settings_profiles_panel(&mut self, ui: &mut Ui) {
        ui.label(
            RichText::new("Profiles")
                .size(FS_SMALL)
                .color(C_TEXT)
                .strong(),
        );
        ui.add_space(8.0);

        // Provider tab bar
        ui.horizontal_wrapped(|ui| {
            for provider in LlmProviderKind::ALL {
                let selected = self.conv.settings_provider_tab == provider;
                if ui
                    .selectable_label(selected, provider.label())
                    .on_hover_text("Show profiles for this provider")
                    .clicked()
                {
                    self.conv.settings_provider_tab = provider;
                }
            }
        });

        ui.add_space(10.0);
        let provider = self.conv.settings_provider_tab;
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("{} profiles", provider.label()))
                    .size(FS_SMALL)
                    .color(C_TEXT),
            );
            if ui.button("+ Add profile").clicked() {
                let id = self.conv.settings.add_profile(provider);
                self.conv.settings.set_active_profile(&id);
            }
        });
        ui.add_space(8.0);

        let profile_indices: Vec<usize> = self
            .conv
            .settings
            .profiles
            .iter()
            .enumerate()
            .filter(|(_, p)| p.provider == provider)
            .map(|(i, _)| i)
            .collect();

        for idx in profile_indices {
            let mut delete_clicked = false;
            let active_id = self.conv.settings.active_profile_id.clone();

            Frame::none()
                .fill(C_BG_ELEVATED)
                .stroke(Stroke::new(1.0, C_BORDER_SUBTLE))
                .rounding(8.0)
                .inner_margin(Margin::symmetric(10.0, 8.0))
                .show(ui, |ui| {
                    let prov = self.conv.settings.profiles[idx].provider;
                    ui.horizontal_wrapped(|ui| {
                        let profile = &mut self.conv.settings.profiles[idx];
                        let selected = active_id == profile.id;
                        if ui.selectable_label(selected, "Active").clicked() {
                            self.conv.settings.active_profile_id = profile.id.clone();
                        }
                        ui.add(
                            TextEdit::singleline(&mut profile.name)
                                .desired_width(180.0)
                                .hint_text("Profile name")
                                .margin(Margin::symmetric(4.0, 2.0)),
                        );
                        ui.label(
                            RichText::new(profile.provider.label())
                                .size(FS_TINY)
                                .color(C_TEXT_MUTED),
                        );
                        if ui.button("Delete").clicked() {
                            delete_clicked = true;
                        }
                    });

                    ui.add_space(6.0);
                    ui.label(RichText::new("Model id").size(FS_TINY).color(C_TEXT_MUTED));
                    ui.add(
                        TextEdit::singleline(&mut self.conv.settings.profiles[idx].model_id)
                            .desired_width(f32::INFINITY)
                            .hint_text("e.g. gpt-4o-mini or claude-sonnet-4")
                            .margin(Margin::symmetric(4.0, 2.0)),
                    );
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new("Base URL (optional)")
                            .size(FS_TINY)
                            .color(C_TEXT_MUTED),
                    );
                    ui.add(
                        TextEdit::singleline(&mut self.conv.settings.profiles[idx].base_url)
                            .desired_width(f32::INFINITY)
                            .hint_text("Leave empty for provider default")
                            .margin(Margin::symmetric(4.0, 2.0)),
                    );
                    ui.add_space(6.0);
                    ui.label(RichText::new("API key / token").size(FS_TINY).color(C_TEXT_MUTED));
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
                            .margin(Margin::symmetric(4.0, 2.0)),
                    );

                    if prov == LlmProviderKind::OpenRouter {
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new("Optional OpenRouter headers")
                                .size(FS_TINY)
                                .color(C_TEXT_MUTED),
                        );
                        ui.add(
                            TextEdit::singleline(
                                &mut self.conv.settings.profiles[idx].openrouter_http_referer,
                            )
                            .desired_width(f32::INFINITY)
                            .hint_text("HTTP-Referer")
                            .margin(Margin::symmetric(4.0, 2.0)),
                        );
                        ui.add_space(6.0);
                        ui.add(
                            TextEdit::singleline(
                                &mut self.conv.settings.profiles[idx].openrouter_title,
                            )
                            .desired_width(f32::INFINITY)
                            .hint_text("X-Title")
                            .margin(Margin::symmetric(4.0, 2.0)),
                        );
                    }
                });

            // OAuth sections per provider (rendered outside the profile card)
            let prov = self.conv.settings.profiles[idx].provider;
            if prov == LlmProviderKind::GitHubCopilot {
                self.render_copilot_oauth_section(ui);
                ui.add_space(8.0);
            }
            if prov == LlmProviderKind::GptCodex {
                self.render_codex_oauth_section(ui);
                ui.add_space(8.0);
            }
            if delete_clicked {
                let id = self.conv.settings.profiles[idx].id.clone();
                self.conv.settings.remove_profile(&id);
            }
            ui.add_space(8.0);
        }

        // Tools section
        ui.separator();
        ui.add_space(8.0);
        ui.label(RichText::new("Tools").size(FS_SMALL).color(C_TEXT).strong());
        ui.add_space(4.0);
        ui.label(
            RichText::new("Enable or disable tools the agent can call.")
                .size(FS_TINY)
                .color(C_TEXT_MUTED),
        );
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            for (i, name) in ALL_TOOL_NAMES.iter().enumerate() {
                ui.checkbox(&mut self.conv.settings.tools_enabled[i], *name);
            }
        });
        ui.add_space(8.0);
        ui.label(
            RichText::new(
                "If a profile key is empty, the app falls back to environment variables. \
                 OAuth still takes precedence where available.",
            )
            .size(FS_TINY)
            .color(C_TEXT_MUTED),
        );
    }

    fn render_settings_system_prompt_panel(&mut self, ui: &mut Ui) {
        ui.label(
            RichText::new("System prompt")
                .size(FS_SMALL)
                .color(C_TEXT)
                .strong(),
        );
        ui.add_space(6.0);
        ui.label(
            RichText::new(
                "Single editable prompt. Use {tools_list} to inject the enabled tools automatically.",
            )
            .size(FS_TINY)
            .color(C_TEXT_MUTED),
        );
        ui.add_space(8.0);
        ui.label(
            RichText::new("System prompt template")
                .size(FS_TINY)
                .color(C_TEXT_MUTED),
        );
        ui.add(
            TextEdit::multiline(&mut self.conv.settings.system_prompt)
                .desired_width(f32::INFINITY)
                .desired_rows(18)
                .margin(Margin::symmetric(4.0, 4.0))
                .hint_text(crate::agent::prompt::DEFAULT_AGENT_SYSTEM_PROMPT),
        );
    }

    // ── OAuth sections ────────────────────────────────────────────────────────

    pub(crate) fn render_copilot_oauth_section(&mut self, ui: &mut Ui) {
        ui.add_space(10.0);
        ui.separator();
        ui.add_space(8.0);
        let oauth = load_oauth_store();
        ui.label(
            RichText::new("GitHub Copilot OAuth")
                .size(FS_SMALL)
                .color(C_TEXT)
                .strong(),
        );
        ui.label(
            RichText::new(format!(
                "Tokens file: {}",
                crate::oauth::oauth_config_path().display()
            ))
            .size(FS_TINY)
            .color(C_TEXT_MUTED),
        );
        ui.add_space(6.0);
        ui.label(
            RichText::new("Optional Enterprise hostname (blank = github.com)")
                .size(FS_TINY)
                .color(C_TEXT_MUTED),
        );
        ui.add(
            TextEdit::singleline(&mut self.conv.copilot_enterprise_domain)
                .desired_width(f32::INFINITY)
                .hint_text("e.g. company.ghe.com")
                .margin(Margin::symmetric(4.0, 2.0)),
        );
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!self.conv.oauth_busy, Button::new("Sign in with GitHub"))
                .clicked()
            {
                self.spawn_github_oauth(ui.ctx());
            }
            if ui
                .add_enabled(oauth.github_copilot.is_some(), Button::new("Sign out"))
                .clicked()
            {
                let mut s = load_oauth_store();
                clear_copilot(&mut s);
                let _ = save_oauth_store(&s);
                self.conv.oauth_last_message = Some("Signed out GitHub Copilot.".into());
            }
        });
        if let Some((ref url, ref code)) = self.conv.oauth_device_copilot {
            ui.label(
                RichText::new(format!("Open {url}\nEnter code: {code}"))
                    .size(FS_TINY)
                    .color(crate::theme::C_ACCENT),
            );
        }
        if let Some(ref msg) = self.conv.oauth_last_message {
            ui.label(RichText::new(msg).size(FS_TINY).color(C_TEXT));
        }
    }

    pub(crate) fn render_codex_oauth_section(&mut self, ui: &mut Ui) {
        ui.add_space(10.0);
        ui.separator();
        ui.add_space(8.0);
        let oauth = load_oauth_store();
        ui.label(
            RichText::new("ChatGPT / Codex OAuth")
                .size(FS_SMALL)
                .color(C_TEXT)
                .strong(),
        );
        ui.label(
            RichText::new("Browser + localhost:1455 callback")
                .size(FS_TINY)
                .color(C_TEXT_MUTED),
        );
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!self.conv.oauth_busy, Button::new("Sign in with ChatGPT"))
                .clicked()
            {
                self.spawn_codex_oauth(ui.ctx());
            }
            if ui
                .add_enabled(oauth.openai_codex.is_some(), Button::new("Sign out"))
                .clicked()
            {
                let mut s = load_oauth_store();
                clear_codex(&mut s);
                let _ = save_oauth_store(&s);
                self.conv.oauth_last_message = Some("Signed out Codex OAuth.".into());
            }
        });
        if let Some(ref msg) = self.conv.oauth_last_message {
            ui.label(RichText::new(msg).size(FS_TINY).color(C_TEXT));
        }
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
