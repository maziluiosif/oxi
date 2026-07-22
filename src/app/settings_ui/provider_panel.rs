//! Per-provider model, endpoint, credential, and reasoning configuration.

use eframe::egui::{self, Align, Layout, RichText, Ui};

use crate::settings::{ComputeLocation, LlmProviderKind, ProviderConfig, SshConfig};
use crate::theme::*;
use crate::ui::chrome::{
    alert_banner, card_frame, field_hint, field_label, field_label_first, nested_card_frame,
    settings_caption, settings_card_header, settings_password_field, settings_text_field,
    settings_text_field_width,
};

use super::super::OxiApp;

impl OxiApp {
    pub(super) fn render_provider_config(&mut self, ui: &mut Ui, kind: LlmProviderKind) {
        // ── Model ──────────────────────────────────────────────────────────
        card_frame().show(ui, |ui| {
            settings_card_header(ui, "Model", Some("Primary model id and context window for this provider."));

            field_label_first(ui, "Model id");
            let have = self
                .conv
                .fetched_models
                .get(&kind)
                .is_some_and(|f| !f.models.is_empty());
            // Refresh button pinned to the right edge; the model field/combo fills the
            // rest of the row on the left. Laying the whole row out right-to-left keeps
            // the field flush-left instead of leaving it stranded on the right.
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let refresh = crate::ui::chrome::icon_button(ui, ICON_REFRESH, 26.0, false)
                        .on_hover_text("Load available models from provider")
                        .clicked();
                    if have {
                        let fetched = self
                            .conv
                            .fetched_models
                            .get(&kind)
                            .map(|f| f.models.clone())
                            .unwrap_or_default();
                        let current = self.conv.settings.provider(kind).model_id.clone();
                        let label = if current.is_empty() {
                            "(custom)".to_string()
                        } else {
                            current.clone()
                        };
                        egui::ComboBox::from_id_salt(("model_combo", kind.slug()))
                            .selected_text(label)
                            .width(ui.available_width())
                            .show_ui(ui, |ui| {
                                if !current.is_empty() && fetched.iter().all(|x| x != &current) {
                                    let _ =
                                        ui.selectable_label(false, format!("{current} (custom)"));
                                }
                                for m in &fetched {
                                    if ui.selectable_label(*m == current, m.clone()).clicked() {
                                        self.conv.settings.provider_mut(kind).model_id = m.clone();
                                    }
                                }
                            });
                    } else {
                        settings_text_field(
                            ui,
                            &mut self.conv.settings.provider_mut(kind).model_id,
                            "e.g. gpt-4o-mini or kimi-k2.7-code",
                        );
                    }
                    if refresh {
                        self.spawn_model_fetch(ui.ctx(), kind);
                    }
                });
            });
            // Status line for the model fetch.
            if let Some(f) = self.conv.fetched_models.get(&kind) {
                if let Some(e) = f.error.clone() {
                    ui.add_space(4.0);
                    alert_banner(ui, &e, true);
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
                let cw = self.conv.settings.provider(kind).context_window;
                let resolved = self
                    .conv
                    .settings
                    .provider(kind)
                    .effective_context_window(self.conv.settings.context_window_default);
                field_label(ui, "Context window (tokens, 0 = auto)");
                ui.horizontal(|ui| {
                    let mut value = cw.unwrap_or(0).to_string();
                    let hint = format!("auto ({resolved})");
                    if settings_text_field_width(ui, &mut value, &hint, 160.0).changed() {
                        let parsed = value.trim().parse::<usize>().ok();
                        self.conv.settings.provider_mut(kind).context_window =
                            parsed.filter(|&n| n > 0);
                    }
                    if crate::ui::chrome::ghost_button(ui, "Auto", false)
                        .on_hover_text("Resolve context window from the model catalog")
                        .clicked()
                    {
                        self.conv.settings.provider_mut(kind).context_window = None;
                    }
                    ui.label(
                        RichText::new(format!("effective: {resolved}"))
                            .size(FS_TINY)
                            .color(c_text_muted()),
                    );
                });
            }

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
                    LlmProviderKind::OpenAi | LlmProviderKind::GptCodex | LlmProviderKind::AzureOpenAi
                );
                field_label(
                    ui,
                    if is_gpt {
                        "Thinking / reasoning level"
                    } else {
                        "Claude effort (4.6+ adaptive thinking)"
                    },
                );
                let current = self.conv.settings.provider(kind).effort.clone();
                let values: &[(&str, &str)] = if is_gpt {
                    &[("", "default"), ("low", "low"), ("medium", "medium"), ("high", "high")]
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
                egui::ComboBox::from_id_salt(("effort_combo", kind.slug()))
                    .selected_text(selected)
                    .width(180.0)
                    .show_ui(ui, |ui| {
                        for (value, label) in values {
                            if ui.selectable_label(current == *value, *label).clicked() {
                                self.conv.settings.provider_mut(kind).effort = value.to_string();
                            }
                        }
                    });
                field_hint(
                    ui,
                    if is_gpt {
                        "Sent as reasoning_effort for GPT reasoning models (gpt-5/o-series) and as reasoning.effort for ChatGPT Codex."
                    } else {
                        "Sent as output_config.effort only for Claude 4.6+ adaptive-thinking models."
                    },
                );
            }
        });

        // ── Connection ─────────────────────────────────────────────────────
        ui.add_space(12.0);
        card_frame().show(ui, |ui| {
            settings_card_header(
                ui,
                "Connection",
                Some("Endpoint and credentials. Secrets stay in the OS keychain."),
            );

            if kind == LlmProviderKind::ClaudeCodeAcp {
                // ACP launches a subprocess instead of hitting an HTTP endpoint.
                field_label_first(ui, "Agent command");
                settings_text_field(
                    ui,
                    &mut self.conv.settings.provider_mut(kind).acp_command,
                    ProviderConfig::DEFAULT_ACP_COMMAND,
                );
                field_hint(
                    ui,
                    "Shell command for the Claude Code ACP adapter (default: npx @agentclientprotocol/claude-agent-acp). Model is applied via ANTHROPIC_MODEL; changing it restarts the agent.",
                );
            } else {
                field_label_first(ui, "Base URL (optional)");
                settings_text_field(
                    ui,
                    &mut self.conv.settings.provider_mut(kind).base_url,
                    kind.default_base_url(),
                );
            }

            field_label(ui, "API key / token");
            let key_hint = match kind {
                LlmProviderKind::OpenAi => "OpenAI API key",
                LlmProviderKind::OpenRouter => "OpenRouter API key",
                LlmProviderKind::AzureOpenAi => "Azure API key (or AZURE_OPENAI_API_KEY)",
                LlmProviderKind::CustomAnthropic => "Anthropic-compatible API key",
                LlmProviderKind::GptCodex => "OpenAI API key for Codex fallback",
                LlmProviderKind::OpenCodeGo => "OpenCode Go API key",
                LlmProviderKind::LmStudio => "Optional (LM Studio ignores it)",
                LlmProviderKind::Ollama => "Optional (Ollama ignores it by default)",
                LlmProviderKind::LocalHf | LlmProviderKind::RemoteHf => {
                    "Optional (llama-server usually ignores it)"
                }
                LlmProviderKind::ClaudeCodeAcp => {
                    "Optional ANTHROPIC_API_KEY (else Claude Code's own login is used)"
                }
            };
            settings_password_field(
                ui,
                &mut self.conv.settings.provider_mut(kind).api_key,
                key_hint,
            );

            if kind == LlmProviderKind::OpenRouter {
                ui.add_space(10.0);
                nested_card_frame().show(ui, |ui| {
                    settings_caption(ui, "Optional OpenRouter headers");
                    settings_text_field(
                        ui,
                        &mut self
                            .conv
                            .settings
                            .provider_mut(kind)
                            .openrouter_http_referer,
                        "HTTP-Referer",
                    );
                    ui.add_space(4.0);
                    settings_text_field(
                        ui,
                        &mut self.conv.settings.provider_mut(kind).openrouter_title,
                        "X-Title",
                    );
                });
            }
        });

        // ── Compute target / managed HF runtimes ───────────────────────────
        if kind == LlmProviderKind::LocalHf {
            self.render_local_hf_section(ui, kind);
        } else if kind == LlmProviderKind::RemoteHf {
            if !matches!(
                self.conv.settings.provider(kind).location,
                ComputeLocation::RemoteSsh(_)
            ) {
                self.conv.settings.provider_mut(kind).location =
                    ComputeLocation::RemoteSsh(SshConfig {
                        remote_runtime_port: kind.default_remote_runtime_port(),
                        ..SshConfig::default()
                    });
            }
            self.render_compute_target_section(ui, kind);
            self.render_local_hf_section(ui, kind);
        } else if kind == LlmProviderKind::LmStudio || kind == LlmProviderKind::Ollama {
            self.render_compute_target_section(ui, kind);
        }
    }
}
