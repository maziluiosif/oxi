//! Background provider model discovery, including ACP warming.

use eframe::egui;

use crate::app::task_runner::spawn_async_task;
use crate::oauth::{ensure_codex_access_token, load_oauth_store};
use crate::settings::{LlmProviderKind, ProviderConfig};

use super::super::{ModelFetchMsg, OxiApp};

impl OxiApp {
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
        // Remote HF's useful catalog is the GGUF files downloaded on the SSH host. It must be
        // available while llama-server is stopped (so a model can be selected and started),
        // therefore list the managed directory over SSH rather than opening a runtime tunnel and
        // calling `/v1/models`.
        if kind == LlmProviderKind::RemoteHf {
            if self.conv.local_models.remote_list_loading {
                return;
            }
            self.spawn_remote_list(ctx);
            return;
        }
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
        LlmProviderKind::LocalHf | LlmProviderKind::RemoteHf | LlmProviderKind::ClaudeCodeAcp => {
            Ok(String::new())
        }
    }
}
