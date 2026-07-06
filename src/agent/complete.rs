//! One-shot LLM text completion (no tools) used for the "generate commit message" button.
//!
//! Reuses the streaming chat/Anthropic/Codex loop implementations, but with no tool
//! definitions and a single round, so the model just returns plain text. Deltas are
//! streamed back over the channel, followed by a terminal [`CompleteEvent::Done`].

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;

use serde_json::{Value, json};

use crate::agent::anthropic::run_anthropic_loop;
use crate::agent::approval::ApprovalGate;
use crate::agent::codex_responses::run_codex_responses_loop;
use crate::agent::events::AgentEvent;
use crate::agent::loop_ctx::LoopCtx;
use crate::agent::openai::run_chat_loop;
use crate::agent::runner::{
    configured_lmstudio_key, configured_ollama_key, configured_openai_key,
    configured_opencode_go_key, configured_openrouter_key, opencode_go_model_uses_anthropic,
    openrouter_extra_headers,
};
use crate::oauth::{ensure_codex_access_token, load_oauth_store};
use crate::settings::{LlmProviderKind, ProviderConfig, WebSearchBackend};

/// One streaming event from a completion run.
#[derive(Debug)]
pub enum CompleteEvent {
    /// Incremental generated text.
    Delta(String),
    /// Terminal event: `Ok` carries the full accumulated text, `Err` carries a message.
    Done(Result<String, String>),
}

/// Request payload for a one-shot completion.
pub struct CompleteRequest {
    pub config: ProviderConfig,
    pub system_prompt: String,
    pub user_prompt: String,
    /// Optional max output characters before we stop early (used to keep commit
    /// messages short). `None` = no cap.
    pub max_chars: Option<usize>,
    pub effort_override: Option<String>,
}

/// Spawn a background completion. The returned [`Receiver`] yields deltas and a final
/// [`CompleteEvent::Done`]. Cancelling is not exposed (the run finishes in one round);
/// the handle stays alive until the worker thread exits.
pub fn spawn_completion(req: CompleteRequest) -> (Receiver<CompleteEvent>, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel::<CompleteEvent>();
    let handle = std::thread::spawn(move || run(req, tx));
    (rx, handle)
}

fn run(req: CompleteRequest, tx: Sender<CompleteEvent>) {
    let rt = match tokio::runtime::Runtime::new() {
        Ok(r) => r,
        Err(e) => {
            let _ = tx.send(CompleteEvent::Done(Err(format!("tokio: {e}"))));
            return;
        }
    };
    rt.block_on(async move {
        let result = run_async(req, &tx).await;
        let _ = tx.send(CompleteEvent::Done(result));
    });
}

async fn run_async(req: CompleteRequest, tx: &Sender<CompleteEvent>) -> Result<String, String> {
    let CompleteRequest {
        config: cfg,
        system_prompt,
        user_prompt,
        max_chars,
        effort_override,
    } = req;

    let model = cfg.model_id.clone();
    // Bound connect + idle-between-chunks time rather than the whole request, so a
    // slow but progressing stream is not cut off.
    let client = match reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(30))
        .read_timeout(std::time::Duration::from_secs(60))
        .tcp_keepalive(std::time::Duration::from_secs(60))
        .tls_danger_accept_invalid_certs(cfg.provider.allows_self_signed_tls())
        .build()
    {
        Ok(c) => c,
        Err(e) => return Err(e.to_string()),
    };

    // No tools, no approval, single round.
    let tools: Vec<Value> = Vec::new();
    let mut messages: Vec<Value> = vec![
        json!({ "role": "system", "content": system_prompt }),
        json!({ "role": "user", "content": user_prompt }),
    ];
    let cancel = Arc::new(AtomicBool::new(false));
    let (_approval_tx, approval_rx) = mpsc::channel();
    let mut gate = ApprovalGate::new(false, approval_rx);
    let max_rounds = 1;

    // Bridge agent events into completion deltas.
    let (agent_tx, agent_rx) = mpsc::channel::<AgentEvent>();
    let collector = tokio::spawn(collect_deltas(agent_rx, tx.clone(), max_chars));

    let cwd = std::path::Path::new(".");
    let tool_env = crate::agent::tools::ToolEnv {
        enabled: Vec::new(),
        web_search_url: String::new(),
        web_search_backend: WebSearchBackend::default(),
    };

    let r = match cfg.provider {
        LlmProviderKind::OpenAi => {
            let key = configured_openai_key(&cfg)?;
            let base = cfg.effective_base_url();
            run_chat_loop(
                &mut LoopCtx {
                    client: &client,
                    base_url: &base,
                    model: &model,
                    cwd,
                    env: &tool_env,
                    tx: &agent_tx,
                    cancel: &cancel,
                    gate: &mut gate,
                    max_rounds,
                    effort_override: effort_override.as_deref(),
                },
                &key,
                &[],
                &mut messages,
                &tools,
            )
            .await
        }
        LlmProviderKind::OpenRouter => {
            let key = configured_openrouter_key(&cfg)?;
            let base = cfg.effective_base_url();
            run_chat_loop(
                &mut LoopCtx {
                    client: &client,
                    base_url: &base,
                    model: &model,
                    cwd,
                    env: &tool_env,
                    tx: &agent_tx,
                    cancel: &cancel,
                    gate: &mut gate,
                    max_rounds,
                    effort_override: effort_override.as_deref(),
                },
                &key,
                &openrouter_extra_headers(&cfg),
                &mut messages,
                &tools,
            )
            .await
        }
        LlmProviderKind::LmStudio => {
            let key = configured_lmstudio_key(&cfg);
            let base = cfg.effective_base_url();
            run_chat_loop(
                &mut LoopCtx {
                    client: &client,
                    base_url: &base,
                    model: &model,
                    cwd,
                    env: &tool_env,
                    tx: &agent_tx,
                    cancel: &cancel,
                    gate: &mut gate,
                    max_rounds,
                    effort_override: effort_override.as_deref(),
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
            let base = cfg.effective_base_url();
            run_chat_loop(
                &mut LoopCtx {
                    client: &client,
                    base_url: &base,
                    model: &model,
                    cwd,
                    env: &tool_env,
                    tx: &agent_tx,
                    cancel: &cancel,
                    gate: &mut gate,
                    max_rounds,
                    effort_override: effort_override.as_deref(),
                },
                &key,
                &[],
                &mut messages,
                &tools,
            )
            .await
        }
        LlmProviderKind::GptCodex => {
            let mut oauth = load_oauth_store();
            if oauth.openai_codex.is_some() {
                let creds = ensure_codex_access_token(&client, &mut oauth).await?;
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
                        cwd,
                        env: &tool_env,
                        tx: &agent_tx,
                        cancel: &cancel,
                        gate: &mut gate,
                        max_rounds,
                        effort_override: effort_override.as_deref(),
                    },
                    &creds.0,
                    &creds.1,
                    &mut messages,
                    &tools,
                )
                .await
            } else {
                let key = configured_openai_key(&cfg)?;
                let base = cfg.effective_base_url();
                run_chat_loop(
                    &mut LoopCtx {
                        client: &client,
                        base_url: &base,
                        model: &model,
                        cwd,
                        env: &tool_env,
                        tx: &agent_tx,
                        cancel: &cancel,
                        gate: &mut gate,
                        max_rounds,
                        effort_override: effort_override.as_deref(),
                    },
                    &key,
                    &[],
                    &mut messages,
                    &tools,
                )
                .await
            }
        }
        LlmProviderKind::OpenCodeGo => {
            let key = configured_opencode_go_key(&cfg)?;
            let base = cfg.effective_base_url();
            let model = model
                .strip_prefix("opencode-go/")
                .unwrap_or(&model)
                .to_string();
            if opencode_go_model_uses_anthropic(&model) {
                let anthropic_base = base.trim_end_matches("/v1").to_string();
                run_anthropic_loop(
                    &mut LoopCtx {
                        client: &client,
                        base_url: &anthropic_base,
                        model: &model,
                        cwd,
                        env: &tool_env,
                        tx: &agent_tx,
                        cancel: &cancel,
                        gate: &mut gate,
                        max_rounds,
                        effort_override: effort_override.as_deref(),
                    },
                    &key,
                    &[],
                    &mut messages,
                    &tools,
                )
                .await
            } else {
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
                        cwd,
                        env: &tool_env,
                        tx: &agent_tx,
                        cancel: &cancel,
                        gate: &mut gate,
                        max_rounds,
                        effort_override: effort_override.as_deref(),
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

    // The agent producer side is done; drop the sender so the collector finishes.
    drop(agent_tx);
    let collected = collector
        .await
        .map_err(|e| format!("collector join: {e}"))??;

    if let Err(e) = r {
        if cancel.load(Ordering::SeqCst) {
            return Err("Cancelled".to_string());
        }
        return Err(e);
    }
    Ok(collected)
}

/// Consume [`AgentEvent`]s and forward text deltas to the completion channel,
/// accumulating the full text. Honors an optional character cap by stopping early.
async fn collect_deltas(
    rx: mpsc::Receiver<AgentEvent>,
    tx: Sender<CompleteEvent>,
    max_chars: Option<usize>,
) -> Result<String, String> {
    let mut out = String::new();
    while let Ok(ev) = rx.recv() {
        match ev {
            AgentEvent::TextDelta(d) => {
                out.push_str(&d);
                let _ = tx.send(CompleteEvent::Delta(d));
                if let Some(cap) = max_chars
                    && out.chars().count() >= cap
                {
                    break;
                }
            }
            AgentEvent::StreamError(e) => return Err(e),
            // The round is being re-sent after a dropped stream: discard the partial
            // text so the retried generation does not get appended to it.
            AgentEvent::StreamRetry { .. } => out.clear(),
            _ => {}
        }
    }
    Ok(out)
}
