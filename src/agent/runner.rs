//! Spawn background agent run (tokio + mpsc).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;

use crate::agent::anthropic::run_anthropic_loop;
use crate::agent::approval::{ApprovalDecision, ApprovalGate};
use crate::agent::codex_responses::run_codex_responses_loop;
use crate::agent::events::AgentEvent;
use crate::agent::history::build_openai_messages;
use crate::agent::openai::run_chat_loop;
use crate::agent::prompt::build_system_prompt;
use crate::agent::tools::tool_definitions_json;
use crate::model::ChatMessage;
use crate::oauth::{ensure_codex_access_token, load_oauth_store};
use crate::settings::{AppSettings, LlmProviderKind, ProviderProfile};

fn finish_with_error(tx: &Sender<AgentEvent>, msg: impl Into<String>) {
    let _ = tx.send(AgentEvent::StreamError(msg.into()));
    let _ = tx.send(AgentEvent::AgentEnd);
}

fn configured_openai_key(profile: &ProviderProfile) -> Result<String, String> {
    let key = profile.api_key.trim();
    if !key.is_empty() {
        return Ok(key.to_string());
    }
    std::env::var("OPENAI_API_KEY")
        .map_err(|_| "Set OpenAI API key in profile or OPENAI_API_KEY, or sign in with ChatGPT (Codex) OAuth.".into())
}

fn configured_openrouter_key(profile: &ProviderProfile) -> Result<String, String> {
    let key = profile.api_key.trim();
    if !key.is_empty() {
        return Ok(key.to_string());
    }
    std::env::var("OPENROUTER_API_KEY").map_err(|_| {
        "Set OpenRouter API key in profile or OPENROUTER_API_KEY in the environment.".into()
    })
}

fn configured_opencode_go_key(profile: &ProviderProfile) -> Result<String, String> {
    let key = profile.api_key.trim();
    if !key.is_empty() {
        return Ok(key.to_string());
    }
    std::env::var("OPENCODE_GO_API_KEY").or_else(|_| std::env::var("OPENCODE_API_KEY")).map_err(|_| {
        "Set OpenCode Go API key in profile or OPENCODE_GO_API_KEY / OPENCODE_API_KEY in the environment."
            .into()
    })
}

fn opencode_go_model_uses_anthropic(model: &str) -> bool {
    let m = model
        .trim()
        .strip_prefix("opencode-go/")
        .unwrap_or(model.trim())
        .to_ascii_lowercase();
    m.starts_with("minimax-") || m.starts_with("qwen")
}

fn openrouter_extra_headers(profile: &ProviderProfile) -> Vec<(String, String)> {
    let mut h = Vec::new();
    let referer = if profile.openrouter_http_referer.trim().is_empty() {
        std::env::var("OPENROUTER_HTTP_REFERER").ok()
    } else {
        Some(profile.openrouter_http_referer.trim().to_string())
    };
    if let Some(r) = referer.filter(|s| !s.is_empty()) {
        h.push(("HTTP-Referer".to_string(), r));
    }
    let title = if profile.openrouter_title.trim().is_empty() {
        std::env::var("OPENROUTER_TITLE").ok()
    } else {
        Some(profile.openrouter_title.trim().to_string())
    };
    if let Some(t) = title.filter(|s| !s.is_empty()) {
        h.push(("X-Title".to_string(), t));
    }
    h
}

/// `chat_for_history`: messages including the latest user turn; excludes the trailing empty assistant placeholder.
pub fn spawn_agent_run(
    settings: AppSettings,
    cwd: PathBuf,
    chat_for_history: Vec<ChatMessage>,
    tx: Sender<AgentEvent>,
    approval_rx: Receiver<ApprovalDecision>,
    cancel: Arc<AtomicBool>,
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
            let profile = match settings.active_profile().cloned() {
                Some(p) => p,
                None => {
                    finish_with_error(&tx, "No active profile configured.");
                    return;
                }
            };
            let system = build_system_prompt(&settings, cwd_ref.to_string_lossy().as_ref());
            let mut messages = build_openai_messages(&system, &chat_for_history);
            let tools = tool_definitions_json(&settings.tools_enabled);
            let model = profile.model_id.clone();
            let client = match reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(300))
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

            let r = match profile.provider {
                LlmProviderKind::GptCodex => {
                    if oauth.openai_codex.is_some() {
                        let creds = match ensure_codex_access_token(&client, &mut oauth).await {
                            Ok(x) => x,
                            Err(e) => {
                                finish_with_error(&tx, e);
                                return;
                            }
                        };
                        let base = if profile.base_url.trim().is_empty() {
                            "https://chatgpt.com/backend-api".to_string()
                        } else {
                            profile.effective_base_url()
                        };
                        run_codex_responses_loop(
                            &client,
                            &base,
                            &creds.0,
                            &creds.1,
                            &model,
                            &mut messages,
                            &tools,
                            cwd_ref,
                            &settings.tools_enabled,
                            &tx,
                            &cancel,
                            &mut gate,
                        )
                        .await
                    } else {
                        let key = match configured_openai_key(&profile) {
                            Ok(k) => k,
                            Err(e) => {
                                finish_with_error(&tx, e);
                                return;
                            }
                        };
                        let base = profile.effective_base_url();
                        run_chat_loop(
                            &client,
                            &base,
                            &key,
                            &model,
                            &[],
                            &mut messages,
                            &tools,
                            cwd_ref,
                            &settings.tools_enabled,
                            &tx,
                            &cancel,
                            &mut gate,
                        )
                        .await
                    }
                }
                LlmProviderKind::OpenAi => {
                    let key = match configured_openai_key(&profile) {
                        Ok(k) => k,
                        Err(e) => {
                            finish_with_error(&tx, e);
                            return;
                        }
                    };
                    let base = profile.effective_base_url();
                    run_chat_loop(
                        &client,
                        &base,
                        &key,
                        &model,
                        &[],
                        &mut messages,
                        &tools,
                        cwd_ref,
                        &settings.tools_enabled,
                        &tx,
                        &cancel,
                        &mut gate,
                    )
                    .await
                }
                LlmProviderKind::OpenRouter => {
                    let key = match configured_openrouter_key(&profile) {
                        Ok(k) => k,
                        Err(e) => {
                            finish_with_error(&tx, e);
                            return;
                        }
                    };
                    let base = profile.effective_base_url();
                    run_chat_loop(
                        &client,
                        &base,
                        &key,
                        &model,
                        &openrouter_extra_headers(&profile),
                        &mut messages,
                        &tools,
                        cwd_ref,
                        &settings.tools_enabled,
                        &tx,
                        &cancel,
                        &mut gate,
                    )
                    .await
                }
                LlmProviderKind::OpenCodeGo => {
                    let key = match configured_opencode_go_key(&profile) {
                        Ok(k) => k,
                        Err(e) => {
                            finish_with_error(&tx, e);
                            return;
                        }
                    };
                    let base = profile.effective_base_url();
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
                            &client,
                            &anthropic_base,
                            &key,
                            &model,
                            &[],
                            &mut messages,
                            &tools,
                            cwd_ref,
                            &settings.tools_enabled,
                            &tx,
                            &cancel,
                            &mut gate,
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
                            &client,
                            &chat_base,
                            &key,
                            &model,
                            &[],
                            &mut messages,
                            &tools,
                            cwd_ref,
                            &settings.tools_enabled,
                            &tx,
                            &cancel,
                            &mut gate,
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

    fn profile_with_key(key: &str) -> ProviderProfile {
        let mut p = ProviderProfile::new("t", LlmProviderKind::OpenAi, "t");
        p.api_key = key.to_string();
        p
    }

    #[test]
    fn configured_key_prefers_profile_value() {
        // A non-empty profile key is returned regardless of environment.
        assert_eq!(
            configured_openai_key(&profile_with_key("sk-profile")).unwrap(),
            "sk-profile"
        );
        assert_eq!(
            configured_openrouter_key(&profile_with_key("or-profile")).unwrap(),
            "or-profile"
        );
        assert_eq!(
            configured_opencode_go_key(&profile_with_key("og-profile")).unwrap(),
            "og-profile"
        );
    }

    #[test]
    fn configured_key_trims_whitespace() {
        assert_eq!(
            configured_openai_key(&profile_with_key("  sk-padded  ")).unwrap(),
            "sk-padded"
        );
    }
}
