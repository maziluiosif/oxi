//! Spawn background agent run (tokio + mpsc).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread::JoinHandle;

use crate::agent::anthropic::run_copilot_loop;
use crate::agent::codex_responses::run_codex_responses_loop;
use crate::agent::events::AgentEvent;
use crate::agent::history::build_openai_messages;
use crate::agent::openai::run_chat_loop;
use crate::agent::prompt::build_system_prompt;
use crate::agent::tools::tool_definitions_json;
use crate::model::ChatMessage;
use crate::oauth::{
    ensure_codex_access_token, ensure_copilot_token, get_copilot_api_base_url, load_oauth_store,
};
use crate::settings::{AppSettings, LlmProviderKind};

fn env_openai_key() -> Result<String, String> {
    std::env::var("OPENAI_API_KEY")
        .map_err(|_| "Set OPENAI_API_KEY or sign in with ChatGPT (Codex) OAuth.".into())
}

fn env_openrouter_key() -> Result<String, String> {
    std::env::var("OPENROUTER_API_KEY")
        .map_err(|_| "Set OPENROUTER_API_KEY in the environment.".into())
}

fn env_copilot_pat() -> Result<String, String> {
    std::env::var("COPILOT_GITHUB_TOKEN")
        .or_else(|_| std::env::var("GH_TOKEN"))
        .or_else(|_| std::env::var("GITHUB_TOKEN"))
        .map_err(|_| {
            "Sign in with GitHub (Copilot) OAuth or set COPILOT_GITHUB_TOKEN / GH_TOKEN / GITHUB_TOKEN."
                .into()
        })
}

fn openrouter_extra_headers() -> Vec<(String, String)> {
    let mut h = Vec::new();
    if let Ok(r) = std::env::var("OPENROUTER_HTTP_REFERER") {
        h.push(("HTTP-Referer".to_string(), r));
    }
    if let Ok(t) = std::env::var("OPENROUTER_TITLE") {
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
    cancel: Arc<AtomicBool>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Runtime::new() {
            Ok(r) => r,
            Err(e) => {
                let _ = tx.send(AgentEvent::StreamError(format!("tokio: {e}")));
                let _ = tx.send(AgentEvent::AgentEnd);
                return;
            }
        };
        rt.block_on(async move {
            let cwd_ref = cwd.as_path();
            let system = build_system_prompt(&settings, cwd_ref.to_string_lossy().as_ref());
            let mut messages = build_openai_messages(&system, &chat_for_history);
            let tools = tool_definitions_json(&settings.tools_enabled);
            let model = settings.model_id.clone();
            let client = match reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(300))
                .build()
            {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(AgentEvent::StreamError(e.to_string()));
                    let _ = tx.send(AgentEvent::AgentEnd);
                    return;
                }
            };
            let mut oauth = load_oauth_store();

            let r = match settings.provider {
                LlmProviderKind::GitHubCopilot => {
                    let (token, base) = if oauth.github_copilot.is_some() {
                        match ensure_copilot_token(&client, &mut oauth).await {
                            Ok(t) => {
                                let b = get_copilot_api_base_url(
                                    &t,
                                    oauth
                                        .github_copilot
                                        .as_ref()
                                        .and_then(|r| r.enterprise_domain.as_deref()),
                                );
                                (t, b)
                            }
                            Err(e) => {
                                let _ = tx.send(AgentEvent::StreamError(e));
                                let _ = tx.send(AgentEvent::AgentEnd);
                                return;
                            }
                        }
                    } else {
                        match env_copilot_pat() {
                            Ok(t) => (t, settings.effective_base_url()),
                            Err(e) => {
                                let _ = tx.send(AgentEvent::StreamError(e));
                                let _ = tx.send(AgentEvent::AgentEnd);
                                return;
                            }
                        }
                    };
                    run_copilot_loop(
                        &client,
                        &base,
                        &token,
                        &model,
                        &mut messages,
                        &tools,
                        cwd_ref,
                        &settings.tools_enabled,
                        &tx,
                        &cancel,
                    )
                    .await
                }
                LlmProviderKind::GptCodex => {
                    if oauth.openai_codex.is_some() {
                        let creds = match ensure_codex_access_token(&client, &mut oauth).await {
                            Ok(x) => x,
                            Err(e) => {
                                let _ = tx.send(AgentEvent::StreamError(e));
                                let _ = tx.send(AgentEvent::AgentEnd);
                                return;
                            }
                        };
                        let base = if settings.base_url.trim().is_empty() {
                            "https://chatgpt.com/backend-api".to_string()
                        } else {
                            settings.effective_base_url()
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
                        )
                        .await
                    } else {
                        let key = match env_openai_key() {
                            Ok(k) => k,
                            Err(e) => {
                                let _ = tx.send(AgentEvent::StreamError(e));
                                let _ = tx.send(AgentEvent::AgentEnd);
                                return;
                            }
                        };
                        let base = settings.effective_base_url();
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
                        )
                        .await
                    }
                }
                LlmProviderKind::OpenAi => {
                    let key = match env_openai_key() {
                        Ok(k) => k,
                        Err(e) => {
                            let _ = tx.send(AgentEvent::StreamError(e));
                            let _ = tx.send(AgentEvent::AgentEnd);
                            return;
                        }
                    };
                    let base = settings.effective_base_url();
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
                    )
                    .await
                }
                LlmProviderKind::OpenRouter => {
                    let key = match env_openrouter_key() {
                        Ok(k) => k,
                        Err(e) => {
                            let _ = tx.send(AgentEvent::StreamError(e));
                            let _ = tx.send(AgentEvent::AgentEnd);
                            return;
                        }
                    };
                    let base = settings.effective_base_url();
                    run_chat_loop(
                        &client,
                        &base,
                        &key,
                        &model,
                        &openrouter_extra_headers(),
                        &mut messages,
                        &tools,
                        cwd_ref,
                        &settings.tools_enabled,
                        &tx,
                        &cancel,
                    )
                    .await
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
