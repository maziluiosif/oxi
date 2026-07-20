//! Spawn background agent run (tokio + mpsc).

use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};

use crate::agent::anthropic::run_anthropic_loop;
use crate::agent::approval::{ApprovalDecision, ApprovalGate, ApprovalPolicy};
use crate::agent::codex_responses::run_codex_responses_loop;
use crate::agent::events::{AgentEvent, AgentOutcome};
use crate::agent::history::{
    build_openai_messages, trim_wire_history_to_budget, user_content_to_openai,
};
use crate::agent::loop_ctx::LoopCtx;
use crate::agent::openai::{run_azure_chat_loop, run_chat_loop};
use crate::agent::tools::{ToolEnv, tool_definitions_json};
use crate::model::{ChatMessage, WireCache};
use crate::oauth::{ensure_codex_access_token, load_oauth_store};
use crate::settings::{AppSettings, LlmProviderKind, ProviderConfig};

const WIRE_CACHE_SCHEMA_VERSION: u8 = 1;

pub fn wire_fingerprint_for(
    settings: &AppSettings,
    system: &str,
    tools: &[serde_json::Value],
) -> String {
    let cfg = settings.active_config();
    let protocol = match cfg.provider {
        LlmProviderKind::CustomAnthropic => "anthropic-messages",
        LlmProviderKind::GptCodex => "codex-or-openai",
        LlmProviderKind::OpenCodeGo if opencode_go_model_uses_anthropic(&cfg.model_id) => {
            "anthropic-messages"
        }
        LlmProviderKind::ClaudeCodeAcp => "acp",
        _ => "openai-chat",
    };
    let canonical = serde_json::json!({
        "schema_version": WIRE_CACHE_SCHEMA_VERSION,
        "protocol": protocol,
        "provider": cfg.provider.slug(),
        "model": cfg.model_id,
        "base_url": cfg.effective_base_url().trim_end_matches('/'),
        "system": system,
        "tools": tools,
    });
    let digest = Sha256::digest(serde_json::to_vec(&canonical).unwrap_or_default());
    let hex = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("v{WIRE_CACHE_SCHEMA_VERSION}:sha256:{hex}")
}

fn finish_with_error(tx: &Sender<AgentEvent>, msg: impl Into<String>) {
    let _ = tx.send(AgentEvent::Finished(AgentOutcome::Failed {
        error: msg.into(),
    }));
}

pub(super) fn configured_openai_key(cfg: &ProviderConfig) -> Result<String, String> {
    let key = cfg.api_key.trim();
    if !key.is_empty() {
        return Ok(key.to_string());
    }
    std::env::var("OPENAI_API_KEY")
        .map_err(|_| "Set OpenAI API key in Settings or OPENAI_API_KEY, or sign in with ChatGPT (Codex) OAuth.".into())
}

pub(super) fn configured_azure_openai_key(cfg: &ProviderConfig) -> Result<String, String> {
    let key = cfg.api_key.trim();
    if !key.is_empty() {
        return Ok(key.to_string());
    }
    std::env::var("AZURE_OPENAI_API_KEY").map_err(|_| {
        "Set Azure OpenAI API key in Settings or AZURE_OPENAI_API_KEY in the environment.".into()
    })
}

pub(super) fn azure_openai_api_version() -> String {
    std::env::var("AZURE_OPENAI_API_VERSION").unwrap_or_else(|_| "2024-10-21".to_string())
}

pub(super) fn configured_custom_anthropic_key(cfg: &ProviderConfig) -> Result<String, String> {
    let key = cfg.api_key.trim();
    if !key.is_empty() {
        return Ok(key.to_string());
    }
    std::env::var("CUSTOM_ANTHROPIC_API_KEY")
        .or_else(|_| std::env::var("ANTHROPIC_API_KEY"))
        .map_err(|_| "Set Custom Anthropic API key in Settings or CUSTOM_ANTHROPIC_API_KEY / ANTHROPIC_API_KEY in the environment.".into())
}

pub(super) fn configured_openrouter_key(cfg: &ProviderConfig) -> Result<String, String> {
    let key = cfg.api_key.trim();
    if !key.is_empty() {
        return Ok(key.to_string());
    }
    std::env::var("OPENROUTER_API_KEY").map_err(|_| {
        "Set OpenRouter API key in Settings or OPENROUTER_API_KEY in the environment.".into()
    })
}

pub(super) fn configured_opencode_go_key(cfg: &ProviderConfig) -> Result<String, String> {
    let key = cfg.api_key.trim();
    if !key.is_empty() {
        return Ok(key.to_string());
    }
    std::env::var("OPENCODE_GO_API_KEY").or_else(|_| std::env::var("OPENCODE_API_KEY")).map_err(|_| {
        "Set OpenCode Go API key in Settings or OPENCODE_GO_API_KEY / OPENCODE_API_KEY in the environment."
            .into()
    })
}

/// LM Studio's local server ignores the bearer token, so an API key is optional. Use the
/// configured value (or `LMSTUDIO_API_KEY`) if present, otherwise fall back to an empty key.
pub(super) fn configured_lmstudio_key(cfg: &ProviderConfig) -> String {
    let key = cfg.api_key.trim();
    if !key.is_empty() {
        return key.to_string();
    }
    std::env::var("LMSTUDIO_API_KEY").unwrap_or_default()
}

/// Ollama's local server has no auth by default, so an API key is optional. Use the
/// configured value (or `OLLAMA_API_KEY`) if present, otherwise fall back to an empty key.
pub(super) fn configured_ollama_key(cfg: &ProviderConfig) -> String {
    let key = cfg.api_key.trim();
    if !key.is_empty() {
        return key.to_string();
    }
    std::env::var("OLLAMA_API_KEY").unwrap_or_default()
}

pub(super) fn opencode_go_model_uses_anthropic(model: &str) -> bool {
    let m = model
        .trim()
        .strip_prefix("opencode-go/")
        .unwrap_or(model.trim())
        .to_ascii_lowercase();
    m.starts_with("minimax-") || m.starts_with("qwen")
}

pub fn openrouter_extra_headers(cfg: &ProviderConfig) -> Vec<(String, String)> {
    let mut h = Vec::new();
    let referer = if cfg.openrouter_http_referer.trim().is_empty() {
        std::env::var("OPENROUTER_HTTP_REFERER").ok()
    } else {
        Some(cfg.openrouter_http_referer.trim().to_string())
    };
    if let Some(r) = referer.filter(|s| !s.is_empty()) {
        h.push(("HTTP-Referer".to_string(), r));
    }
    let title = if cfg.openrouter_title.trim().is_empty() {
        std::env::var("OPENROUTER_TITLE").ok()
    } else {
        Some(cfg.openrouter_title.trim().to_string())
    };
    if let Some(t) = title.filter(|s| !s.is_empty()) {
        h.push(("X-Title".to_string(), t));
    }
    h
}

/// Immutable snapshot for one agent run.
pub struct AgentRunRequest {
    pub settings: AppSettings,
    pub tunnels: crate::compute::TunnelManager,
    pub acp: crate::agent::acp::AcpManager,
    pub acp_session_key: String,
    pub cwd: PathBuf,
    pub chat_for_history: Vec<ChatMessage>,
    pub approval_rx: Receiver<ApprovalDecision>,
    pub cancel: Arc<AtomicBool>,
    pub wire_candidate: Option<WireCache>,
    pub chars_per_token: f32,
    pub undo_journal: Arc<std::sync::Mutex<crate::agent::tools::TurnUndoJournal>>,
}

/// Shared Tokio runtime for all HTTP agent runs.
#[derive(Clone)]
pub struct AgentExecutor {
    runtime: Arc<tokio::runtime::Runtime>,
}

impl AgentExecutor {
    pub fn new() -> Result<Self, String> {
        tokio::runtime::Runtime::new()
            .map(|runtime| Self {
                runtime: Arc::new(runtime),
            })
            .map_err(|e| format!("tokio: {e}"))
    }

    fn spawn<F>(&self, future: F) -> tokio::task::JoinHandle<F::Output>
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.runtime.spawn(future)
    }
}

pub fn spawn_agent_run(
    executor: &AgentExecutor,
    request: AgentRunRequest,
    tx: Sender<AgentEvent>,
) -> tokio::task::JoinHandle<()> {
    executor.spawn(async move {
        let AgentRunRequest {
            settings,
            tunnels,
            acp,
            acp_session_key,
            cwd,
            chat_for_history,
            approval_rx,
            cancel,
            wire_candidate,
            chars_per_token,
            undo_journal,
        } = request;
            let cwd_ref = cwd.as_path();
            let cfg = settings.active_config().clone();

            // ACP inverts oxi's model: Claude Code runs the agent loop in a subprocess. Handle
            // it entirely here — no system prompt, wire history, or tool definitions from oxi —
            // then return before the HTTP-provider machinery below.
            if cfg.provider == LlmProviderKind::ClaudeCodeAcp {
                undo_journal
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .mark_non_reversible(
                        "Claude Code ACP manages its own tools, so this response cannot be restored safely.",
                    );
                run_acp_turn(
                    &cfg,
                    &acp,
                    acp_session_key,
                    cwd.clone(),
                    &chat_for_history,
                    &tx,
                    approval_rx,
                    ApprovalPolicy {
                        write_edit: settings.require_write_edit_approval,
                        bash: settings.require_bash_approval,
                    },
                    &cancel,
                )
                .await;
                return;
            }

            let system =
                crate::agent::prompt::build_system_prompt_for_workspace(&settings, cwd_ref);
            let context_tokens = cfg.effective_context_window(settings.context_window_default);
            let context_budget = crate::agent::history::context_char_budget_from_tokens(
                context_tokens,
                chars_per_token,
            );
            let max_rounds = settings.max_tool_rounds;
            let mut tools =
                tool_definitions_json(&settings.tools_enabled, settings.bash_timeout_cap_secs);
            let mcp = crate::agent::mcp::McpManager::new();
            mcp.sync_servers(&settings.mcp_servers);
            tools.extend(mcp.tool_definitions());
            let wire_fingerprint = wire_fingerprint_for(&settings, &system, &tools);
            let prior_wire = wire_candidate
                .filter(|cache| cache.fingerprint == wire_fingerprint)
                .map(|cache| cache.messages);
            let mut messages = if let Some(mut wire) = prior_wire {
                if let Some(last_user) = chat_for_history.last()
                    && last_user.role == crate::model::MsgRole::User
                {
                    wire.push(serde_json::json!({
                        "role": "user",
                        "content": user_content_to_openai(&last_user.text, &last_user.attachments),
                    }));
                }
                trim_wire_history_to_budget(&mut wire, context_budget);
                wire
            } else {
                build_openai_messages(&system, &chat_for_history, context_budget)
            };
            let tool_env = ToolEnv {
                enabled: settings.tools_enabled.clone(),
                web_search_url: settings.effective_web_search_url(),
                web_search_backend: settings.web_search_backend,
                bash_timeout_cap_secs: settings.bash_timeout_cap_secs,
                mcp: Some(mcp),
                undo_journal: Some(undo_journal),
            };
            let model = cfg.model_id.clone();
            let effort_override = (!cfg.effort.trim().is_empty()).then_some(cfg.effort.trim());
            // No total request timeout: it would also cover the streamed body and kill
            // long turns mid-stream. Instead bound connect time and idle time between
            // chunks, and keep the TCP connection alive through NATs/proxies.
            let client = match reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(30))
                .read_timeout(std::time::Duration::from_secs(180))
                .tcp_keepalive(std::time::Duration::from_secs(60))
                .tls_danger_accept_invalid_certs(cfg.provider.allows_self_signed_tls())
                .build()
            {
                Ok(c) => c,
                Err(e) => {
                    finish_with_error(&tx, e.to_string());
                    return;
                }
            };
            let mut oauth = load_oauth_store();
            let mut gate = ApprovalGate::new(
                ApprovalPolicy {
                    write_edit: settings.require_write_edit_approval,
                    bash: settings.require_bash_approval,
                },
                approval_rx,
            );

            let r = match cfg.provider {
                LlmProviderKind::GptCodex => {
                    if oauth.openai_codex.is_some() {
                        let creds = match ensure_codex_access_token(&client, &mut oauth).await {
                            Ok(x) => x,
                            Err(e) => {
                                finish_with_error(&tx, e);
                                return;
                            }
                        };
                        let base = if cfg.base_url.trim().is_empty() {
                            "https://chatgpt.com/backend-api".to_string()
                        } else {
                            cfg.effective_base_url()
                        };
                        run_codex_responses_loop(
                            &mut LoopCtx {
                                client: &client,
                                base_url: &base,
                                model: &model,
                                cwd: cwd_ref,
                                env: &tool_env,
                                tx: &tx,
                                cancel: &cancel,
                                gate: &mut gate,
                                max_rounds,
                                effort_override,
                            },
                            &creds.0,
                            &creds.1,
                            &mut messages,
                            &tools,
                        )
                        .await
                    } else {
                        let key = match configured_openai_key(&cfg) {
                            Ok(k) => k,
                            Err(e) => {
                                finish_with_error(&tx, e);
                                return;
                            }
                        };
                        let base = cfg.effective_base_url();
                        run_chat_loop(
                            &mut LoopCtx {
                                client: &client,
                                base_url: &base,
                                model: &model,
                                cwd: cwd_ref,
                                env: &tool_env,
                                tx: &tx,
                                cancel: &cancel,
                                gate: &mut gate,
                                max_rounds,
                                effort_override,
                            },
                            &key,
                            &[],
                            &mut messages,
                            &tools,
                        )
                        .await
                    }
                }
                LlmProviderKind::OpenAi => {
                    let key = match configured_openai_key(&cfg) {
                        Ok(k) => k,
                        Err(e) => {
                            finish_with_error(&tx, e);
                            return;
                        }
                    };
                    let base = cfg.effective_base_url();
                    run_chat_loop(
                        &mut LoopCtx {
                            client: &client,
                            base_url: &base,
                            model: &model,
                            cwd: cwd_ref,
                            env: &tool_env,
                            tx: &tx,
                            cancel: &cancel,
                            gate: &mut gate,
                            max_rounds,
                            effort_override,
                        },
                        &key,
                        &[],
                        &mut messages,
                        &tools,
                    )
                    .await
                }
                LlmProviderKind::OpenRouter => {
                    let key = match configured_openrouter_key(&cfg) {
                        Ok(k) => k,
                        Err(e) => {
                            finish_with_error(&tx, e);
                            return;
                        }
                    };
                    let base = cfg.effective_base_url();
                    run_chat_loop(
                        &mut LoopCtx {
                            client: &client,
                            base_url: &base,
                            model: &model,
                            cwd: cwd_ref,
                            env: &tool_env,
                            tx: &tx,
                            cancel: &cancel,
                            gate: &mut gate,
                            max_rounds,
                            effort_override,
                        },
                        &key,
                        &openrouter_extra_headers(&cfg),
                        &mut messages,
                        &tools,
                    )
                    .await
                }
                LlmProviderKind::AzureOpenAi => {
                    let key = match configured_azure_openai_key(&cfg) {
                        Ok(k) => k,
                        Err(e) => {
                            finish_with_error(&tx, e);
                            return;
                        }
                    };
                    let base = cfg.effective_base_url();
                    let api_version = azure_openai_api_version();
                    run_azure_chat_loop(
                        &mut LoopCtx {
                            client: &client,
                            base_url: &base,
                            model: &model,
                            cwd: cwd_ref,
                            env: &tool_env,
                            tx: &tx,
                            cancel: &cancel,
                            gate: &mut gate,
                            max_rounds,
                            effort_override,
                        },
                        &key,
                        &api_version,
                        &mut messages,
                        &tools,
                    )
                    .await
                }
                LlmProviderKind::CustomAnthropic => {
                    let key = match configured_custom_anthropic_key(&cfg) {
                        Ok(k) => k,
                        Err(e) => {
                            finish_with_error(&tx, e);
                            return;
                        }
                    };
                    let base = cfg.effective_base_url();
                    run_anthropic_loop(
                        &mut LoopCtx {
                            client: &client,
                            base_url: &base,
                            model: &model,
                            cwd: cwd_ref,
                            env: &tool_env,
                            tx: &tx,
                            cancel: &cancel,
                            gate: &mut gate,
                            max_rounds,
                            effort_override,
                        },
                        &key,
                        &[],
                        &mut messages,
                        &tools,
                    )
                    .await
                }
                LlmProviderKind::LmStudio
                | LlmProviderKind::LocalHf
                | LlmProviderKind::RemoteHf => {
                    let key = configured_lmstudio_key(&cfg);
                    let base = match crate::compute::resolve_base_url(&cfg, &tunnels).await {
                        Ok(b) => b,
                        Err(e) => {
                            finish_with_error(&tx, e);
                            return;
                        }
                    };
                    run_chat_loop(
                        &mut LoopCtx {
                            client: &client,
                            base_url: &base,
                            model: &model,
                            cwd: cwd_ref,
                            env: &tool_env,
                            tx: &tx,
                            cancel: &cancel,
                            gate: &mut gate,
                            max_rounds,
                            effort_override,
                        },
                        &key,
                        &[],
                        &mut messages,
                        &tools,
                    )
                    .await
                }
                LlmProviderKind::Ollama => {
                    let key = configured_ollama_key(&cfg);
                    let base = match crate::compute::resolve_base_url(&cfg, &tunnels).await {
                        Ok(b) => b,
                        Err(e) => {
                            finish_with_error(&tx, e);
                            return;
                        }
                    };
                    run_chat_loop(
                        &mut LoopCtx {
                            client: &client,
                            base_url: &base,
                            model: &model,
                            cwd: cwd_ref,
                            env: &tool_env,
                            tx: &tx,
                            cancel: &cancel,
                            gate: &mut gate,
                            max_rounds,
                            effort_override,
                        },
                        &key,
                        &[],
                        &mut messages,
                        &tools,
                    )
                    .await
                }
                LlmProviderKind::OpenCodeGo => {
                    let key = match configured_opencode_go_key(&cfg) {
                        Ok(k) => k,
                        Err(e) => {
                            finish_with_error(&tx, e);
                            return;
                        }
                    };
                    let base = cfg.effective_base_url();
                    let model = model
                        .strip_prefix("opencode-go/")
                        .unwrap_or(&model)
                        .to_string();
                    if opencode_go_model_uses_anthropic(&model) {
                        // OpenCode Go exposes Anthropic-compatible models at
                        // https://opencode.ai/zen/go/v1/messages. `run_anthropic_loop`
                        // appends `/v1/messages`, so pass the base without `/v1`.
                        let anthropic_base = base.trim_end_matches("/v1").to_string();
                        run_anthropic_loop(
                            &mut LoopCtx {
                                client: &client,
                                base_url: &anthropic_base,
                                model: &model,
                                cwd: cwd_ref,
                                env: &tool_env,
                                tx: &tx,
                                cancel: &cancel,
                                gate: &mut gate,
                                max_rounds,
                                effort_override,
                            },
                            &key,
                            &[],
                            &mut messages,
                            &tools,
                        )
                        .await
                    } else {
                        // OpenCode Go exposes OpenAI-compatible models at
                        // https://opencode.ai/zen/go/v1/chat/completions. `run_chat_loop`
                        // appends `/chat/completions`, so include `/v1` in the base.
                        let chat_base = if base.trim_end_matches('/').ends_with("/v1") {
                            base
                        } else {
                            format!("{}/v1", base.trim_end_matches('/'))
                        };
                        run_chat_loop(
                            &mut LoopCtx {
                                client: &client,
                                base_url: &chat_base,
                                model: &model,
                                cwd: cwd_ref,
                                env: &tool_env,
                                tx: &tx,
                                cancel: &cancel,
                                gate: &mut gate,
                                max_rounds,
                                effort_override,
                            },
                            &key,
                            &[],
                            &mut messages,
                            &tools,
                        )
                        .await
                    }
                }
                LlmProviderKind::ClaudeCodeAcp => {
                    unreachable!("ACP is handled before the provider match")
                }
            };
            let outcome = match r {
                Err(_) if cancel.load(Ordering::SeqCst) => AgentOutcome::Cancelled,
                Err(error) => AgentOutcome::Failed { error },
                Ok(()) if cancel.load(Ordering::SeqCst) => AgentOutcome::Cancelled,
                Ok(()) => AgentOutcome::Success {
                    wire_cache: Some(WireCache {
                        fingerprint: wire_fingerprint,
                        messages,
                    }),
                },
            };
            let _ = tx.send(AgentEvent::Finished(outcome));
    })
}

/// Drive one Claude Code (ACP) turn: extract the latest user message, submit it to the ACP
/// manager, and translate the outcome into the terminal [`AgentEvent`]s the UI expects. Unlike
/// the HTTP providers there is no wire history to emit — the agent keeps session state in its
/// subprocess.
#[allow(clippy::too_many_arguments)]
async fn run_acp_turn(
    cfg: &ProviderConfig,
    acp: &crate::agent::acp::AcpManager,
    acp_session_key: String,
    cwd: PathBuf,
    chat_for_history: &[ChatMessage],
    tx: &Sender<AgentEvent>,
    approval_rx: Receiver<ApprovalDecision>,
    approval_policy: ApprovalPolicy,
    cancel: &Arc<AtomicBool>,
) {
    let last_user = chat_for_history
        .iter()
        .rev()
        .find(|m| m.role == crate::model::MsgRole::User);
    let text = last_user.map(|m| m.text.clone()).unwrap_or_default();
    let images: Vec<(String, Vec<u8>)> = last_user
        .map(|m| {
            m.attachments
                .iter()
                .map(|a| match a {
                    crate::model::UserAttachment::Image { mime, data } => {
                        (mime.clone(), data.clone())
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    let mut env = Vec::new();
    let key = cfg.api_key.trim();
    if !key.is_empty() {
        env.push(("ANTHROPIC_API_KEY".to_string(), key.to_string()));
    }

    let req = crate::agent::acp::AcpPrompt {
        session_key: acp_session_key,
        cwd,
        command_line: cfg.effective_acp_command(),
        env,
        model: cfg.model_id.clone(),
        text,
        images,
        event_tx: tx.clone(),
        approval_rx,
        approval_policy,
        cancel: cancel.clone(),
    };

    let outcome = match acp.prompt(req).await {
        Err(_) if cancel.load(Ordering::SeqCst) => AgentOutcome::Cancelled,
        Err(error) => AgentOutcome::Failed { error },
        Ok(()) if cancel.load(Ordering::SeqCst) => AgentOutcome::Cancelled,
        Ok(()) => AgentOutcome::Success { wire_cache: None },
    };
    let _ = tx.send(AgentEvent::Finished(outcome));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_fingerprint_is_stable_and_versioned() {
        let settings = AppSettings::default();
        let tools = vec![serde_json::json!({"type":"function","function":{"name":"read"}})];
        let a = wire_fingerprint_for(&settings, "system", &tools);
        let b = wire_fingerprint_for(&settings, "system", &tools);
        assert_eq!(a, b);
        assert!(a.starts_with("v1:sha256:"));
        assert_eq!(a.len(), "v1:sha256:".len() + 64);
    }

    #[test]
    fn wire_fingerprint_changes_with_prompt_or_tools() {
        let settings = AppSettings::default();
        let tools = vec![serde_json::json!({"type":"function","function":{"name":"read"}})];
        let changed_tools = vec![serde_json::json!({"type":"function","function":{"name":"edit"}})];
        assert_ne!(
            wire_fingerprint_for(&settings, "system-a", &tools),
            wire_fingerprint_for(&settings, "system-b", &tools)
        );
        assert_ne!(
            wire_fingerprint_for(&settings, "system-a", &tools),
            wire_fingerprint_for(&settings, "system-a", &changed_tools)
        );
    }

    #[test]
    fn opencode_go_anthropic_models() {
        assert!(opencode_go_model_uses_anthropic("minimax-01"));
        assert!(opencode_go_model_uses_anthropic("qwen-max"));
        assert!(opencode_go_model_uses_anthropic("qwen2.5-coder"));
    }

    #[test]
    fn opencode_go_strips_provider_prefix() {
        assert!(opencode_go_model_uses_anthropic("opencode-go/minimax-text"));
        assert!(opencode_go_model_uses_anthropic("opencode-go/qwen-max"));
        assert!(!opencode_go_model_uses_anthropic(
            "opencode-go/kimi-k2.7-code"
        ));
    }

    #[test]
    fn opencode_go_normalizes_case_and_whitespace() {
        assert!(opencode_go_model_uses_anthropic("  MiniMax-01  "));
        assert!(opencode_go_model_uses_anthropic("QWEN-MAX"));
    }

    #[test]
    fn opencode_go_chat_models_are_not_anthropic() {
        assert!(!opencode_go_model_uses_anthropic("kimi-k2.7-code"));
        assert!(!opencode_go_model_uses_anthropic("gpt-4o-mini"));
        assert!(!opencode_go_model_uses_anthropic(""));
    }

    fn cfg_with_key(key: &str) -> ProviderConfig {
        let mut c = ProviderConfig::new(LlmProviderKind::OpenAi);
        c.api_key = key.to_string();
        c
    }

    #[test]
    fn ollama_key_prefers_configured_value() {
        let mut c = ProviderConfig::new(LlmProviderKind::Ollama);
        c.api_key = "ollama-config".to_string();
        assert_eq!(configured_ollama_key(&c), "ollama-config");
    }

    #[test]
    fn ollama_key_defaults_empty_without_config_or_env() {
        let c = ProviderConfig::new(LlmProviderKind::Ollama);
        // No key configured and (in test environments) no OLLAMA_API_KEY: falls back to "".
        if std::env::var("OLLAMA_API_KEY").is_err() {
            assert_eq!(configured_ollama_key(&c), "");
        }
    }

    #[test]
    fn configured_key_prefers_configured_value() {
        // A non-empty configured key is returned regardless of environment.
        assert_eq!(
            configured_openai_key(&cfg_with_key("sk-profile")).unwrap(),
            "sk-profile"
        );
        assert_eq!(
            configured_openrouter_key(&cfg_with_key("or-profile")).unwrap(),
            "or-profile"
        );
        assert_eq!(
            configured_opencode_go_key(&cfg_with_key("og-profile")).unwrap(),
            "og-profile"
        );
    }

    #[test]
    fn configured_key_trims_whitespace() {
        assert_eq!(
            configured_openai_key(&cfg_with_key("  sk-padded  ")).unwrap(),
            "sk-padded"
        );
    }
}
