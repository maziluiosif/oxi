//! Spawn background agent run (tokio + mpsc).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::thread::JoinHandle;

use crate::agent::anthropic::run_anthropic_loop;
use crate::agent::approval::{ApprovalDecision, ApprovalGate};
use crate::agent::codex_responses::run_codex_responses_loop;
use crate::agent::events::AgentEvent;
use crate::agent::history::{
    build_openai_messages, trim_wire_history_to_budget, user_content_to_openai,
};
use crate::agent::loop_ctx::LoopCtx;
use crate::agent::openai::run_chat_loop;
use crate::agent::prompt::build_system_prompt;
use crate::agent::tools::{ToolEnv, tool_definitions_json};
use crate::model::ChatMessage;
use crate::oauth::{ensure_codex_access_token, load_oauth_store};
use crate::settings::{AppSettings, LlmProviderKind, ProviderConfig};

pub fn wire_fingerprint_for(
    settings: &AppSettings,
    system: &str,
    tools: &[serde_json::Value],
) -> u64 {
    let cfg = settings.active_config();
    let mut h = DefaultHasher::new();
    cfg.provider.hash(&mut h);
    cfg.model_id.hash(&mut h);
    system.hash(&mut h);
    serde_json::to_string(tools)
        .unwrap_or_default()
        .hash(&mut h);
    h.finish()
}

fn finish_with_error(tx: &Sender<AgentEvent>, msg: impl Into<String>) {
    let _ = tx.send(AgentEvent::StreamError(msg.into()));
    let _ = tx.send(AgentEvent::AgentEnd);
}

pub(super) fn configured_openai_key(cfg: &ProviderConfig) -> Result<String, String> {
    let key = cfg.api_key.trim();
    if !key.is_empty() {
        return Ok(key.to_string());
    }
    std::env::var("OPENAI_API_KEY")
        .map_err(|_| "Set OpenAI API key in Settings or OPENAI_API_KEY, or sign in with ChatGPT (Codex) OAuth.".into())
}

pub(super) fn configured_custom_openai_key(cfg: &ProviderConfig) -> String {
    let key = cfg.api_key.trim();
    if !key.is_empty() {
        return key.to_string();
    }
    std::env::var("CUSTOM_OPENAI_API_KEY").unwrap_or_default()
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

/// `chat_for_history`: messages including the latest user turn; excludes the trailing empty assistant placeholder.
#[allow(clippy::too_many_arguments)]
pub fn spawn_agent_run(
    settings: AppSettings,
    tunnels: crate::compute::TunnelManager,
    cwd: PathBuf,
    chat_for_history: Vec<ChatMessage>,
    tx: Sender<AgentEvent>,
    approval_rx: Receiver<ApprovalDecision>,
    cancel: Arc<AtomicBool>,
    prior_wire: Option<Vec<serde_json::Value>>,
    chars_per_token: f32,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Runtime::new() {
            Ok(r) => r,
            Err(e) => {
                finish_with_error(&tx, format!("tokio: {e}"));
                return;
            }
        };
        rt.block_on(async move {
            let cwd_ref = cwd.as_path();
            let cfg = settings.active_config().clone();
            let system = build_system_prompt(&settings, cwd_ref.to_string_lossy().as_ref());
            let context_tokens = cfg.effective_context_window(settings.context_window_default);
            let context_budget = crate::agent::history::context_char_budget_from_tokens(
                context_tokens,
                chars_per_token,
            );
            let max_rounds = settings.max_tool_rounds;
            let tools =
                tool_definitions_json(&settings.tools_enabled, settings.bash_timeout_cap_secs);
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
            let mut gate = ApprovalGate::new(settings.require_approval, approval_rx);

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
                LlmProviderKind::CustomOpenAi => {
                    let key = configured_custom_openai_key(&cfg);
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
                LlmProviderKind::LmStudio => {
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
            };
            if let Err(e) = r {
                if !cancel.load(Ordering::SeqCst) {
                    let _ = tx.send(AgentEvent::StreamError(e));
                }
                let _ = tx.send(AgentEvent::AgentEnd);
            } else if !cancel.load(Ordering::SeqCst) {
                let _ = tx.send(AgentEvent::WireHistory(messages));
                let _ = tx.send(AgentEvent::AgentEnd);
            }
        });
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
