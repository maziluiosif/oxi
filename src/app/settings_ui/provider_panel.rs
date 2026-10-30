//! Per-provider model, endpoint, credential, and reasoning configuration.

use eframe::egui::{RichText, Ui};

use crate::settings::LlmProviderKind;
use crate::theme::*;
use crate::ui::chrome::{
    CONTROL_W, alert_banner, ghost_button, setting_block, setting_row, setting_select,
    setting_switch, settings_disclosure, settings_password_field, settings_section,
    settings_text_field, settings_text_field_width,
};
use crate::ui::select_menu::{SelectMenu, SelectOption};

use super::super::OxiApp;

impl OxiApp {
    pub(super) fn render_provider_config(&mut self, ui: &mut Ui, kind: LlmProviderKind) {
        // oxi runs these models itself, so model id, endpoint and key are derived from the
        // guided setup instead of being typed in.
        if kind.is_managed_hf() {
            self.render_managed_hf_setup(ui, kind);
            return;
        }

        // ── Model ──────────────────────────────────────────────────────────
        // ACP agents advertise their own models and thinking levels, which the composer
        // offers; only Claude Code needs a field, for ids its adapter doesn't list.
        if kind == LlmProviderKind::ClaudeCodeAcp {
            settings_section(
                ui,
                "Model",
                Some("Pick the model and effort in the composer."),
                |ui| {
                    setting_row(
                        ui,
                        "Model id",
                        Some(
                            "Follows the composer's picker. Type an id the agent doesn't list to launch with it.",
                        ),
                        |ui| {
                            settings_text_field_width(
                                ui,
                                &mut self.conv.settings.provider_mut(kind).model_id,
                                "e.g. claude-opus-5",
                                CONTROL_W - 18.0,
                            )
                        },
                    );
                },
            );
        } else if !kind.is_acp() {
            self.render_model_card(ui, kind);
        }

        // ── Connection ─────────────────────────────────────────────────────
        settings_section(ui, "Connection", None, |ui| {
            if kind.is_acp() {
                // ACP launches a subprocess instead of hitting an HTTP endpoint.
                let default_command = self.conv.settings.provider(kind).default_acp_command();
                setting_block(
                    ui,
                    "Agent command",
                    Some("Empty runs the recommended default."),
                    |ui| {
                        settings_text_field(
                            ui,
                            &mut self.conv.settings.provider_mut(kind).acp_command,
                            default_command,
                        );
                        if kind == LlmProviderKind::AntigravityAcp {
                            ui.add_space(6.0);
                            ui.label(
                                RichText::new(
                                    "The official server is downloaded on first launch. Sign in \
                                     with Google in the browser when asked, or give a Gemini API \
                                     key below.",
                                )
                                .size(FS_TINY)
                                .color(c_text_muted()),
                            );
                            ui.hyperlink_to(
                                RichText::new("Antigravity authentication and plans").size(FS_TINY),
                                "https://antigravity.google/docs/ide/extensions/zed/",
                            );
                        }
                    },
                );
                let status = self.fetch_status_text(kind);
                setting_row(
                    ui,
                    "Check that it starts",
                    Some(
                        status
                            .as_deref()
                            .unwrap_or("Failures include installation hints."),
                    ),
                    |ui| {
                        if ghost_button(ui, "Test launch", false).clicked() {
                            self.spawn_model_fetch(ui.ctx(), kind);
                        }
                    },
                );
                self.fetch_error(ui, kind);
            } else if kind.has_custom_endpoint() {
                setting_block(ui, "Base URL", Some("Empty uses the default."), |ui| {
                    settings_text_field(
                        ui,
                        &mut self.conv.settings.provider_mut(kind).base_url,
                        kind.default_base_url(),
                    );
                    if let Some(warning) = self.conv.settings.provider(kind).base_url_warning() {
                        ui.add_space(4.0);
                        alert_banner(ui, warning, false);
                    }
                });
                let mut accept_invalid = self.conv.settings.provider(kind).allows_self_signed_tls();
                if setting_switch(
                    ui,
                    &mut accept_invalid,
                    "Accept self-signed TLS certificates",
                    Some("Only for a server you control on a trusted network: anyone on the path could read the API key."),
                )
                .changed()
                {
                    self.conv.settings.provider_mut(kind).allow_self_signed_tls =
                        Some(accept_invalid);
                }
            } else {
                setting_row(
                    ui,
                    "Endpoint",
                    Some("The official one. Use OpenAI or Anthropic Compatible for a custom URL."),
                    |ui| {
                        ui.label(
                            RichText::new(kind.default_base_url())
                                .size(FS_SMALL)
                                .color(c_text_muted())
                                .monospace(),
                        )
                    },
                );
            }

            let key_hint = match kind {
                LlmProviderKind::OpenAi => "OpenAI API key",
                LlmProviderKind::OpenRouter => "OpenRouter API key",
                LlmProviderKind::AzureOpenAi => "Azure API key (or AZURE_OPENAI_API_KEY)",
                LlmProviderKind::CustomAnthropic => "Anthropic-compatible API key",
                LlmProviderKind::GptCodex => "OpenAI API key for Codex fallback",
                LlmProviderKind::OpenCodeGo => "OpenCode Go API key",
                LlmProviderKind::MetaMuse => "Meta Model API key (or MODEL_API_KEY)",
                LlmProviderKind::LmStudio => "Optional (LM Studio ignores it)",
                LlmProviderKind::LlamaCpp => "Optional: the --api-key given to llama-server",
                LlmProviderKind::Ollama => "Optional (Ollama ignores it by default)",
                LlmProviderKind::LocalHf | LlmProviderKind::RemoteHf => {
                    "Optional (llama-server usually ignores it)"
                }
                LlmProviderKind::ClaudeCodeAcp => {
                    "Optional ANTHROPIC_API_KEY (else Claude Code's own login is used)"
                }
                LlmProviderKind::CursorAcp => "Cursor CLI uses its own login",
                LlmProviderKind::OpenCodeAcp => {
                    "OpenCode uses its own login (`opencode auth login`)"
                }
                LlmProviderKind::CodexAcp => {
                    "Optional OPENAI_API_KEY / CODEX_API_KEY (else Codex login is used)"
                }
                LlmProviderKind::MuseCodeAcp => {
                    "Optional META_API_KEY, billed pay-as-you-go (else `muse login` is used)"
                }
                LlmProviderKind::CopilotAcp => {
                    "Optional COPILOT_GITHUB_TOKEN (else Copilot CLI's own login is used)"
                }
                LlmProviderKind::AntigravityAcp => {
                    "Optional GEMINI_API_KEY (else Google account sign-in is used)"
                }
                LlmProviderKind::Router => "",
            };
            let key_help = if kind.is_acp() {
                "Kept in the OS keychain."
            } else {
                "Kept in the OS keychain. Empty falls back to the environment variable."
            };
            setting_block(ui, "API key / token", Some(key_help), |ui| {
                settings_password_field(
                    ui,
                    &mut self.conv.settings.provider_mut(kind).api_key,
                    key_hint,
                )
            });

            if kind == LlmProviderKind::OpenRouter {
                settings_disclosure(ui, "OpenRouter headers", &[], |ui| {
                    let config = self.conv.settings.provider_mut(kind);
                    setting_row(
                        ui,
                        "HTTP-Referer",
                        Some("Your app's URL, for OpenRouter rankings."),
                        |ui| {
                            settings_text_field_width(
                                ui,
                                &mut config.openrouter_http_referer,
                                "https://\u{2026}",
                                CONTROL_W - 18.0,
                            )
                        },
                    );
                    setting_row(ui, "X-Title", Some("Your app's name."), |ui| {
                        settings_text_field_width(
                            ui,
                            &mut config.openrouter_title,
                            "oxi",
                            CONTROL_W - 18.0,
                        )
                    });
                });
            }
        });

        // ── Compute target ─────────────────────────────────────────────────
        if matches!(
            kind,
            LlmProviderKind::LmStudio | LlmProviderKind::LlamaCpp | LlmProviderKind::Ollama
        ) {
            self.render_compute_target_section(ui, kind);
        }
    }

    /// One line on the provider's model fetch (also the ACP launch check), if there is one.
    fn fetch_status_text(&self, kind: LlmProviderKind) -> Option<String> {
        let f = self.conv.fetched_models.get(&kind)?;
        if f.error.is_some() {
            None
        } else if f.loading {
            Some(
                if kind.is_acp() {
                    "Starting agent\u{2026}"
                } else {
                    "Loading models\u{2026}"
                }
                .to_owned(),
            )
        } else if !f.models.is_empty() {
            Some(if kind.is_acp() {
                format!("Agent runs \u{00b7} {} models", f.models.len())
            } else {
                format!("{} models available", f.models.len())
            })
        } else {
            None
        }
    }

    /// The model fetch's error, as a banner.
    fn fetch_error(&self, ui: &mut Ui, kind: LlmProviderKind) {
        if let Some(e) = self
            .conv
            .fetched_models
            .get(&kind)
            .and_then(|f| f.error.clone())
        {
            ui.add_space(2.0);
            alert_banner(ui, &e, true);
            ui.add_space(8.0);
        }
    }

    /// Model id, context window and effort for providers oxi talks to directly.
    fn render_model_card(&mut self, ui: &mut Ui, kind: LlmProviderKind) {
        settings_section(ui, "Model", None, |ui| {
            let fetched = self
                .conv
                .fetched_models
                .get(&kind)
                .map(|f| f.models.clone())
                .unwrap_or_default();
            let status = self.fetch_status_text(kind);
            let current = self.conv.settings.provider(kind).model_id.clone();
            setting_row(
                ui,
                "Model",
                Some(
                    status
                        .as_deref()
                        .unwrap_or("Load the provider's list with the refresh button."),
                ),
                |ui| {
                    if crate::ui::chrome::icon_button(ui, ICON_REFRESH, 26.0, false)
                        .on_hover_text("Load available models from provider")
                        .clicked()
                    {
                        self.spawn_model_fetch(ui.ctx(), kind);
                    }
                    ui.add_space(4.0);
                    if fetched.is_empty() {
                        settings_text_field_width(
                            ui,
                            &mut self.conv.settings.provider_mut(kind).model_id,
                            "e.g. gpt-4o-mini",
                            CONTROL_W - 18.0,
                        );
                    } else {
                        let label = if current.is_empty() {
                            "Choose a model"
                        } else {
                            current.as_str()
                        };
                        if let Some(i) = SelectMenu::new(("model", kind.slug()), label)
                            .framed(CONTROL_W)
                            .show(ui, || {
                                fetched
                                    .iter()
                                    .map(|m| SelectOption::new(m.clone(), *m == current))
                                    .collect()
                            })
                        {
                            self.conv.settings.provider_mut(kind).model_id = fetched[i].clone();
                        }
                    }
                },
            );
            self.fetch_error(ui, kind);
            // The list replaces the free-text field once it arrives, so keep a way to use an
            // id the provider doesn't advertise.
            if !fetched.is_empty() {
                settings_disclosure(ui, "Use a model that isn't listed", &[], |ui| {
                    setting_row(ui, "Model id", None, |ui| {
                        settings_text_field_width(
                            ui,
                            &mut self.conv.settings.provider_mut(kind).model_id,
                            "e.g. claude-opus-5",
                            CONTROL_W - 18.0,
                        )
                    });
                });
            }

            let cw = self.conv.settings.provider(kind).context_window;
            let resolved = self
                .conv
                .settings
                .provider(kind)
                .effective_context_window(self.conv.settings.context_window_default);
            let help = format!(
                "Empty takes it from the model catalog. In use: {} tokens.",
                group_thousands(resolved as u64)
            );
            setting_row(ui, "Context window", Some(&help), |ui| {
                // Empty when automatic, so the hint shows what "auto" resolves to.
                let mut value = cw.map(|n| n.to_string()).unwrap_or_default();
                let hint = format!("auto ({})", group_thousands(resolved as u64));
                if settings_text_field_width(ui, &mut value, &hint, 120.0).changed() {
                    let parsed = value.trim().parse::<usize>().ok();
                    self.conv.settings.provider_mut(kind).context_window =
                        parsed.filter(|&n| n > 0);
                }
            });

            if matches!(
                kind,
                LlmProviderKind::OpenAi
                    | LlmProviderKind::GptCodex
                    | LlmProviderKind::OpenCodeGo
                    | LlmProviderKind::AzureOpenAi
                    | LlmProviderKind::CustomAnthropic
            ) {
                let is_gpt = matches!(
                    kind,
                    LlmProviderKind::OpenAi
                        | LlmProviderKind::GptCodex
                        | LlmProviderKind::AzureOpenAi
                );
                let current = self.conv.settings.provider(kind).effort.clone();
                let values: &[(&str, &str)] = if is_gpt {
                    &[
                        ("", "default"),
                        ("low", "low"),
                        ("medium", "medium"),
                        ("high", "high"),
                    ]
                } else {
                    &[
                        ("", "default (high)"),
                        ("low", "low"),
                        ("medium", "medium"),
                        ("high", "high"),
                        ("xhigh", "xhigh"),
                        ("max", "max"),
                    ]
                };
                let selected = values
                    .iter()
                    .find(|(value, _)| *value == current)
                    .map(|(_, label)| *label)
                    .unwrap_or("default");
                if let Some(i) = setting_select(
                    ui,
                    ("effort", kind.slug()),
                    if is_gpt { "Reasoning level" } else { "Effort" },
                    Some(if is_gpt {
                        "For GPT reasoning models (gpt-5, o-series) and ChatGPT Codex."
                    } else {
                        "For Claude 4.6+ adaptive-thinking models."
                    }),
                    selected,
                    || {
                        values
                            .iter()
                            .map(|(value, label)| SelectOption::new(*label, current == *value))
                            .collect()
                    },
                ) {
                    self.conv.settings.provider_mut(kind).effort = values[i].0.to_string();
                }
            }
        });
    }
}
