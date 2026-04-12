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
use crate::settings::{AppSettings, LlmProviderKind, ProviderProfile};

fn finish_with_error(tx: &Sender<AgentEvent>, msg: impl Into<String>) {
    let _ = tx.send(AgentEvent::StreamError(msg.into()));
    let _ = tx.send(AgentEvent::AgentEnd);
}

fn copilot_chat_extra_headers() -> Vec<(String, String)> {
    vec![
        (
            "User-Agent".to_string(),
            "GitHubCopilotChat/0.35.0".to_string(),
        ),
        ("Editor-Version".to_string(), "vscode/1.107.0".to_string()),
        (
            "Editor-Plugin-Version".to_string(),
            "copilot-chat/0.35.0".to_string(),
        ),
        (
            "Copilot-Integration-Id".to_string(),
            "vscode-chat".to_string(),
        ),
        (
            "Openai-Intent".to_string(),
            "conversation-edits".to_string(),
        ),
    ]
}

/// Which Copilot backend API to use for a given model id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CopilotApi {
    /// Anthropic Messages API (Claude family).
    Anthropic,
    /// OpenAI Responses API (o-series, gpt-5+).
    Responses,
    /// Standard OpenAI Chat Completions streaming.
    Chat,
}

fn copilot_model_api(model: &str) -> CopilotApi {
    let m = model.trim().to_ascii_lowercase();
    // Anthropic: any claude-* model
    if m.starts_with("claude-") {
        return CopilotApi::Anthropic;
    }
    // OpenAI Responses API: o-series reasoning models and gpt-5+
    // Covers: o1, o1-mini, o1-preview, o3, o3-mini, o4-mini, gpt-5, gpt-5-turbo, …
    let responses_prefixes = ["o1", "o2", "o3", "o4", "o5", "gpt-5"];
    if responses_prefixes
        .iter()
        .any(|p| m.starts_with(p) && m[p.len()..].starts_with(|c: char| !c.is_alphabetic()))
        || m.starts_with("gpt-5")
    {
        return CopilotApi::Responses;
    }
    CopilotApi::Chat
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

fn configured_copilot_pat(profile: &ProviderProfile) -> Result<String, String> {
    let key = profile.api_key.trim();
    if !key.is_empty() {
        return Ok(key.to_string());
    }
    std::env::var("COPILOT_GITHUB_TOKEN")
        .or_else(|_| std::env::var("GH_TOKEN"))
        .or_else(|_| std::env::var("GITHUB_TOKEN"))
        .map_err(|_| {
            "Sign in with GitHub (Copilot) OAuth or set a Copilot token in profile / COPILOT_GITHUB_TOKEN / GH_TOKEN / GITHUB_TOKEN."
                .into()
        })
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

            let r = match profile.provider {
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
                                finish_with_error(&tx, e);
                                return;
                            }
                        }
                    } else {
                        match configured_copilot_pat(&profile) {
                            Ok(t) => (t, profile.effective_base_url()),
                            Err(e) => {
                                finish_with_error(&tx, e);
                                return;
                            }
                        }
                    };

                    match copilot_model_api(&model) {
                        CopilotApi::Anthropic => {
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
                        CopilotApi::Chat => {
                            run_chat_loop(
                                &client,
                                &base,
                                &token,
                                &model,
                                &copilot_chat_extra_headers(),
                                &mut messages,
                                &tools,
                                cwd_ref,
                                &settings.tools_enabled,
                                &tx,
                                &cancel,
                                true,
                            )
                            .await
                        }
                        CopilotApi::Responses => Err(format!(
                            "GitHub Copilot model `{model}` requires the Responses API, \
                                 which is not yet implemented in oxi. \
                                 Try a Claude / GPT-4o / Gemini Copilot model instead."
                        )),
                    }
                }
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
                            false,
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
                        false,
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
                        false,
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
