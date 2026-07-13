//! Per-provider config editing: model/base-URL/API-key fields, the Local/Remote(SSH)
//! compute target (with SSH password storage and "Test connection"), Codex OAuth sign-in,
//! and background model-list fetching.

use eframe::egui::{self, Align, Layout, RichText, Ui};

use crate::oauth::{
    OAuthUiMsg, clear_codex, ensure_codex_access_token, load_oauth_store, save_oauth_store,
};
use crate::settings::{ComputeLocation, LlmProviderKind, ProviderConfig, SshConfig};
use crate::theme::*;
use crate::ui::chrome::{
    alert_banner, card_frame, field_hint, field_label, field_label_first, ghost_button,
    nested_card_frame, pill_tab, settings_caption, settings_card_header, settings_password_field,
    settings_text_field, settings_text_field_width,
};

use super::super::task_runner::spawn_async_task;
use super::super::{ModelFetchMsg, OxiApp, SshTestMsg};
use super::layout::{active_pill, inactive_pill};

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
            ui.horizontal(|ui| {
                let refresh = ui
                    .with_layout(Layout::right_to_left(Align::Center), |ui| {
                        crate::ui::chrome::icon_button(ui, ICON_REFRESH, 26.0, false)
                            .on_hover_text("Load available models from provider")
                            .clicked()
                    })
                    .inner;
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
                                let _ = ui.selectable_label(false, format!("{current} (custom)"));
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
                LlmProviderKind::LocalHf => "Optional (llama-server usually ignores it)",
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

        // ── Compute target / Local HF ──────────────────────────────────────
        if kind == LlmProviderKind::LocalHf {
            self.render_compute_target_section(ui, kind);
            self.render_local_hf_section(ui);
        } else if kind == LlmProviderKind::LmStudio || kind == LlmProviderKind::Ollama {
            self.render_compute_target_section(ui, kind);
        }
    }

    /// "Local" vs "Remote (SSH)" compute target, shown only for self-hosted runtimes
    /// (LM Studio / Ollama / Local HF) where running on another host over SSH is meaningful.
    fn render_compute_target_section(&mut self, ui: &mut Ui, kind: LlmProviderKind) {
        ui.add_space(12.0);
        card_frame().show(ui, |ui| {
            settings_card_header(
                ui,
                "Compute target",
                Some("Where the model runtime listens: this machine, or another host via SSH tunnel."),
            );
            let is_remote = matches!(
                self.conv.settings.provider(kind).location,
                ComputeLocation::RemoteSsh(_)
            );
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                if pill_tab(ui, "Local", !is_remote) && is_remote {
                    self.conv.settings.provider_mut(kind).location = ComputeLocation::Local;
                }
                if pill_tab(ui, "Remote (SSH)", is_remote) && !is_remote {
                    self.conv.settings.provider_mut(kind).location =
                        ComputeLocation::RemoteSsh(SshConfig {
                            remote_runtime_port: kind.default_remote_runtime_port(),
                            ..SshConfig::default()
                        });
                }
            });

            if let ComputeLocation::RemoteSsh(cfg) =
                &mut self.conv.settings.provider_mut(kind).location
            {
                ui.add_space(8.0);
                ui.label(
                    RichText::new(
                        if kind == LlmProviderKind::LocalHf {
                            "Runs the oxi-managed HF model on another host over SSH. oxi can install llama-server, download GGUF files, start/stop the runtime, and tunnel chat to it."
                        } else {
                            "Runs the model on another host (e.g. a machine on your LAN) over SSH. The runtime must listen on 127.0.0.1 there; oxi forwards a local port to it."
                        },
                    )
                    .size(FS_TINY)
                    .color(c_text_faint()),
                );
                ui.add_space(6.0);

                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        field_label_first(ui, "SSH host");
                        settings_text_field_width(
                            ui,
                            &mut cfg.host,
                            "192.168.1.10 or myhost.local",
                            220.0,
                        );
                    });
                    ui.add_space(8.0);
                    ui.vertical(|ui| {
                        field_label_first(ui, "SSH port");
                        let mut port_str = cfg.port.to_string();
                        if settings_text_field_width(ui, &mut port_str, "22", 80.0).changed()
                            && let Ok(p) = port_str.trim().parse::<u16>()
                        {
                            cfg.port = p;
                        }
                    });
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        field_label_first(ui, "SSH user");
                        settings_text_field_width(ui, &mut cfg.user, "e.g. ioan", 220.0);
                    });
                    ui.add_space(8.0);
                    ui.vertical(|ui| {
                        field_label_first(ui, "Remote runtime port");
                        let mut rport_str = cfg.remote_runtime_port.to_string();
                        if settings_text_field_width(ui, &mut rport_str, "11434", 80.0).changed()
                            && let Ok(p) = rport_str.trim().parse::<u16>()
                        {
                            cfg.remote_runtime_port = p;
                        }
                    });
                });
            }
        });

        if !matches!(
            self.conv.settings.provider(kind).location,
            ComputeLocation::RemoteSsh(_)
        ) {
            return;
        }

        // Lazily load the saved password (if any) into the in-memory draft on first touch.
        self.conv
            .ssh_password_drafts
            .entry(kind)
            .or_insert_with(|| {
                let creds = crate::compute::load_ssh_credentials();
                creds.get(kind.slug()).unwrap_or_default().to_string()
            });

        ui.add_space(12.0);
        card_frame().show(ui, |ui| {
            settings_card_header(
                ui,
                "SSH credentials",
                Some("Password is stored in the OS keychain, never in settings.json."),
            );
            field_label_first(ui, "SSH password");
            let changed = {
                let pw = self.conv.ssh_password_drafts.get_mut(&kind).unwrap();
                settings_password_field(ui, pw, "SSH password").changed()
            };
            if changed {
                let pw = self
                    .conv
                    .ssh_password_drafts
                    .get(&kind)
                    .cloned()
                    .unwrap_or_default();
                let mut creds = crate::compute::load_ssh_credentials();
                creds.set(kind.slug(), pw);
                if let Err(e) = crate::compute::save_ssh_credentials(&creds) {
                    self.run_state_mut(self.active_session_key()).stream_error =
                        Some(format!("Save SSH password: {e}"));
                }
            }

            ui.add_space(8.0);
            // Clone the status out first so rendering it doesn't hold an immutable borrow of
            // `self.conv` while the buttons need `&mut self`.
            let status = self.conv.ssh_test.get(&kind).cloned();
            let pinned = self
                .conv
                .settings
                .provider(kind)
                .ssh_config()
                .and_then(|c| c.pinned_host_key.clone());
            let mut rerun_test = false;
            let mut accept_key: Option<String> = None;
            ui.horizontal(|ui| {
                if ghost_button(ui, "Test connection", false).clicked() {
                    rerun_test = true;
                }
                ui.add_space(8.0);
                if let Some(status) = &status {
                    if status.loading {
                        ui.label(
                            RichText::new("Connecting…")
                                .size(FS_TINY)
                                .color(c_text_muted()),
                        );
                    } else if let Some(Ok(port)) = &status.result {
                        ui.label(
                            RichText::new(format!("Connected (local tunnel port {port})"))
                                .size(FS_TINY)
                                .color(c_accent()),
                        );
                    }
                }
            });
            if let Some(status) = &status
                && let Some(Err(err)) = &status.result
            {
                ui.add_space(6.0);
                match err {
                    crate::compute::TunnelError::HostKeyMismatch { pinned, observed } => {
                        alert_banner(
                            ui,
                            &format!(
                                "Host key changed! Pinned {pinned}, server now presents \
                                 {observed}. Accept only if you know the host was rebuilt.",
                            ),
                            true,
                        );
                        ui.add_space(6.0);
                        if ghost_button(ui, "Accept new key", false).clicked() {
                            accept_key = Some(observed.clone());
                        }
                    }
                    e => alert_banner(ui, &e.to_string(), true),
                }
            }
            if let Some(fp) = &pinned {
                let short = fp.get(..23).unwrap_or(fp.as_str());
                ui.label(
                    RichText::new(format!("Host key pinned: {short}…"))
                        .size(FS_TINY)
                        .color(c_text_faint()),
                );
            }
            if let Some(fp) = accept_key {
                if let ComputeLocation::RemoteSsh(cfg) =
                    &mut self.conv.settings.provider_mut(kind).location
                {
                    cfg.pinned_host_key = Some(fp);
                }
                if let Err(e) = self.conv.settings.save() {
                    self.run_state_mut(self.active_session_key()).stream_error =
                        Some(format!("Save settings: {e}"));
                }
                rerun_test = true;
            }
            if rerun_test {
                self.spawn_ssh_test(ui.ctx(), kind);
            }
        });
    }

    /// Kick off a background SSH "Test connection" check for `kind`'s `RemoteSsh` config,
    /// if one isn't already in flight. Results arrive on `conv.ssh_test_rx` and are
    /// drained each frame.
    fn spawn_ssh_test(&mut self, ctx: &egui::Context, kind: LlmProviderKind) {
        let Some(cfg) = self.conv.settings.provider(kind).ssh_config().cloned() else {
            return;
        };
        let password = self
            .conv
            .ssh_password_drafts
            .get(&kind)
            .cloned()
            .unwrap_or_default();

        let entry = self.conv.ssh_test.entry(kind).or_default();
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
        let err_ctx = ctx.clone();
        spawn_async_task(
            move |err| {
                let _ = err_tx.send(SshTestMsg {
                    provider: kind,
                    result: Err(crate::compute::TunnelError::Other(err)),
                });
                err_ctx.request_repaint();
            },
            move |rt| {
                let r = rt
                    .block_on(tunnels.ensure_tunnel(kind.slug(), &cfg, &password))
                    .map(|ok| ok.local_port);
                let _ = tx.send(SshTestMsg {
                    provider: kind,
                    result: r,
                });
                ctx.request_repaint();
            },
        );
    }

    /// Pin host keys observed on successful SSH connects (trust-on-first-use). Drains the
    /// tunnel manager's observed-fingerprint map each frame; for any provider whose
    /// `SshConfig` has no pinned key yet, records the observed fingerprint and saves
    /// settings. Already-pinned providers are left untouched — a mismatch never reaches a
    /// successful connect, so an attacker key can't silently overwrite an existing pin.
    pub(crate) fn pin_observed_host_keys(&mut self) {
        let observed = self.tunnels.take_observed_host_keys();
        if observed.is_empty() {
            return;
        }
        let mut changed = false;
        for (slug, fp) in observed {
            let Some(kind) = LlmProviderKind::ALL.into_iter().find(|k| k.slug() == slug) else {
                continue;
            };
            if let ComputeLocation::RemoteSsh(cfg) =
                &mut self.conv.settings.provider_mut(kind).location
                && cfg.pinned_host_key.is_none()
            {
                cfg.pinned_host_key = Some(fp);
                changed = true;
            }
        }
        if changed && let Err(e) = self.conv.settings.save() {
            self.run_state_mut(self.active_session_key()).stream_error =
                Some(format!("Save settings: {e}"));
        }
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
                    let entry = self.conv.ssh_test.entry(msg.provider).or_default();
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

    pub(super) fn render_codex_oauth_section(&mut self, ui: &mut Ui) {
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
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
                    self.spawn_codex_oauth(ui.ctx());
                }
                if self.conv.oauth_busy {
                    ui.add(egui::Spinner::new().size(13.0).color(c_text_muted()));
                    ui.label(
                        RichText::new("Waiting for the browser sign-in…")
                            .size(FS_TINY)
                            .color(c_text_muted()),
                    );
                }
                if ui
                    .add_enabled(
                        signed_in,
                        crate::ui::chrome::ghost_button_widget("Sign out", false),
                    )
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
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
    /// Ensure the active provider's model catalog has been fetched at least once
    /// (e.g. on startup) so the composer model dropdown offers the full list
    /// instead of falling back to just the current model id.
    pub(crate) fn ensure_active_models_fetched(&mut self, ctx: &egui::Context) {
        let kind = self.conv.settings.active_provider;
        // Already fetched, in flight, or failed? Then nothing to do — don't auto-retry every
        // frame (a failed retry would re-launch the ACP subprocess on each repaint). The manual
        // refresh button clears the error and re-fetches.
        if let Some(f) = self.conv.fetched_models.get(&kind)
            && (f.loading || !f.models.is_empty() || f.error.is_some())
        {
            return;
        }
        self.spawn_model_fetch(ctx, kind);
    }

    /// Warm the Claude Code (ACP) subprocess for the active session in the background and load
    /// its advertised model list, if a fetch isn't already in flight. Reuses the same per-session
    /// key as [`Self::send_prompt_payload`] so the warmed subprocess is reused by the first
    /// prompt. Results arrive on `conv.model_rxs` (as a [`ModelFetchMsg`]) and are drained each
    /// frame like an HTTP model fetch.
    fn spawn_acp_warm(&mut self, ctx: &egui::Context, kind: LlmProviderKind) {
        let entry = self.conv.fetched_models.entry(kind).or_default();
        if entry.loading {
            return;
        }
        entry.loading = true;
        entry.error = None;

        let cfg = self.conv.settings.provider(kind).clone();
        let key = self.active_session_key();
        let session_file = self.conv.workspaces[key.workspace_idx].sessions[key.session_idx]
            .session_file
            .clone();
        let acp_session_key = session_file
            .unwrap_or_else(|| format!("mem:{}:{}", key.workspace_idx, key.session_idx));
        let cwd =
            std::path::PathBuf::from(self.conv.workspaces[key.workspace_idx].root_path.trim());
        let mut env = Vec::new();
        let api_key = cfg.api_key.trim();
        if !api_key.is_empty() {
            env.push(("ANTHROPIC_API_KEY".to_string(), api_key.to_string()));
        }
        let command_line = cfg.effective_acp_command();
        let acp = self.acp.clone();

        let (tx, rx) = std::sync::mpsc::channel::<ModelFetchMsg>();
        self.conv.model_rxs.push(rx);
        let ctx = ctx.clone();
        let err_tx = tx.clone();
        let err_ctx = ctx.clone();
        spawn_async_task(
            move |err| {
                let _ = err_tx.send(ModelFetchMsg {
                    provider: kind,
                    result: Err(err),
                });
                err_ctx.request_repaint();
            },
            move |rt| {
                let r = rt.block_on(acp.warm(crate::agent::acp::AcpWarm {
                    session_key: acp_session_key,
                    cwd,
                    command_line,
                    env,
                    model: cfg.model_id.clone(),
                }));
                let _ = tx.send(ModelFetchMsg {
                    provider: kind,
                    result: r,
                });
                ctx.request_repaint();
            },
        );
    }

    /// Kick off a background `/v1/models` fetch for `kind`, if one isn't already in
    /// flight. Results arrive on `conv.model_rxs` and are drained each frame.
    pub(crate) fn spawn_model_fetch(&mut self, ctx: &egui::Context, kind: LlmProviderKind) {
        // Claude Code (ACP) has no HTTP `/v1/models` endpoint — the agent advertises its models
        // over the protocol, so warm the subprocess and read them from `session/new` instead.
        if kind == LlmProviderKind::ClaudeCodeAcp {
            self.spawn_acp_warm(ctx, kind);
            return;
        }
        let cfg = self.conv.settings.provider(kind).clone();
        let entry = self.conv.fetched_models.entry(kind).or_default();
        if entry.loading {
            return;
        }
        entry.loading = true;
        entry.error = None;

        let (tx, rx) = std::sync::mpsc::channel::<ModelFetchMsg>();
        self.conv.model_rxs.push(rx);
        let ctx = ctx.clone();
        let err_tx = tx.clone();
        let err_ctx = ctx.clone();
        let tunnels = self.tunnels.clone();
        spawn_async_task(
            move |err| {
                let _ = err_tx.send(ModelFetchMsg {
                    provider: kind,
                    result: Err(err),
                });
                err_ctx.request_repaint();
            },
            move |rt| {
                let client = match reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(30))
                    .tls_danger_accept_invalid_certs(cfg.provider.allows_self_signed_tls())
                    .build()
                {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = tx.send(ModelFetchMsg {
                            provider: kind,
                            result: Err(e.to_string()),
                        });
                        ctx.request_repaint();
                        return;
                    }
                };
                let mut oauth = load_oauth_store();
                let (base, key, extra) = if cfg.provider == LlmProviderKind::GptCodex
                    && oauth.openai_codex.is_some()
                {
                    let creds = match rt.block_on(ensure_codex_access_token(&client, &mut oauth)) {
                        Ok(creds) => creds,
                        Err(e) => {
                            let _ = tx.send(ModelFetchMsg {
                                provider: kind,
                                result: Err(e),
                            });
                            ctx.request_repaint();
                            return;
                        }
                    };
                    let base = if cfg.base_url.trim().is_empty() {
                        "https://chatgpt.com/backend-api/codex".to_string()
                    } else {
                        cfg.effective_base_url()
                    };
                    let extra = vec![
                        ("ChatGPT-Account-ID".to_string(), creds.1),
                        ("originator".to_string(), "codex_cli_rs".to_string()),
                    ];
                    (base, creds.0, extra)
                } else {
                    let base = match rt.block_on(crate::compute::resolve_base_url(&cfg, &tunnels)) {
                        Ok(b) => b,
                        Err(e) => {
                            let _ = tx.send(ModelFetchMsg {
                                provider: kind,
                                result: Err(e),
                            });
                            ctx.request_repaint();
                            return;
                        }
                    };
                    // OpenCode Go expects /v1/models but its default base lacks /v1.
                    let base = if cfg.provider == LlmProviderKind::OpenCodeGo
                        && !base.trim_end_matches('/').ends_with("/v1")
                    {
                        format!("{}/v1", base.trim_end_matches('/'))
                    } else {
                        base
                    };
                    // Anthropic-compatible APIs typically expose `/v1/models` relative to the
                    // same root used for `/v1/messages`; `fetch_models` appends that suffix.
                    let extra = if cfg.provider == LlmProviderKind::OpenRouter {
                        crate::agent::runner::openrouter_extra_headers(&cfg)
                    } else {
                        Vec::new()
                    };
                    let key = match resolve_fetch_key(&cfg) {
                        Ok(k) => k,
                        Err(e) => {
                            let _ = tx.send(ModelFetchMsg {
                                provider: kind,
                                result: Err(e),
                            });
                            ctx.request_repaint();
                            return;
                        }
                    };
                    (base, key, extra)
                };
                let r = rt.block_on(crate::agent::fetch_models(&client, &base, &key, &extra));
                let r = r.map(|ms| ms.into_iter().map(|m| m.id).collect::<Vec<_>>());
                let _ = tx.send(ModelFetchMsg {
                    provider: kind,
                    result: r,
                });
                ctx.request_repaint();
            },
        );
    }
}

/// Resolve the bearer key to use for a model-list fetch (mirrors the runner's auth fallbacks).
fn resolve_fetch_key(cfg: &ProviderConfig) -> Result<String, String> {
    let key = cfg.api_key.trim();
    if !key.is_empty() {
        return Ok(key.to_string());
    }
    match cfg.provider {
        LlmProviderKind::OpenAi | LlmProviderKind::GptCodex => {
            std::env::var("OPENAI_API_KEY").map_err(|_| "Set an API key to list models.".into())
        }
        LlmProviderKind::OpenRouter => {
            std::env::var("OPENROUTER_API_KEY").map_err(|_| "Set an API key to list models.".into())
        }
        LlmProviderKind::CustomAnthropic => std::env::var("CUSTOM_ANTHROPIC_API_KEY")
            .or_else(|_| std::env::var("ANTHROPIC_API_KEY"))
            .map_err(|_| "Set an API key to list models.".into()),
        LlmProviderKind::AzureOpenAi => std::env::var("AZURE_OPENAI_API_KEY")
            .map_err(|_| "Set an API key to list models.".into()),
        // OpenCode Go, LM Studio, and Ollama may expose the model list without auth.
        LlmProviderKind::OpenCodeGo | LlmProviderKind::LmStudio | LlmProviderKind::Ollama => {
            Ok(String::new())
        }
        LlmProviderKind::LocalHf | LlmProviderKind::ClaudeCodeAcp => Ok(String::new()),
    }
}
