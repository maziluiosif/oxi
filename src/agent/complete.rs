//! One-shot LLM text completion (no tools) used for the "generate commit message" button.
//!
//! Reuses the provider dispatch of normal agent runs, but with no tool definitions and a single
//! round, so the model just returns plain text. Deltas are
//! streamed back over the channel, followed by a terminal [`CompleteEvent::Done`].

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;

use serde_json::{Value, json};

use crate::agent::approval::ApprovalGate;
use crate::agent::events::AgentEvent;
use crate::settings::{ProviderConfig, WebSearchBackend};

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
    let rt = match crate::runtime::runtime() {
        Ok(r) => r,
        Err(e) => {
            let _ = tx.send(CompleteEvent::Done(Err(e)));
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

    let client = crate::agent::dispatch::streaming_client(&cfg, 60)?;

    // No tools, no approval, single round.
    let tools: Vec<Value> = Vec::new();
    let mut messages: Vec<Value> = vec![
        json!({ "role": "system", "content": system_prompt }),
        json!({ "role": "user", "content": user_prompt }),
    ];
    let cancel = Arc::new(AtomicBool::new(false));
    let (_approval_tx, approval_rx) = mpsc::channel();
    let mut gate = ApprovalGate::new(
        crate::agent::approval::ApprovalPolicy::disabled(),
        approval_rx,
    );
    let max_rounds = 1;

    // Bridge agent events into completion deltas.
    let (agent_tx, agent_rx) = mpsc::channel::<AgentEvent>();
    // `collect_deltas` blocks on a std channel, so it must not occupy a runtime worker.
    let delta_tx = tx.clone();
    let collector =
        tokio::task::spawn_blocking(move || collect_deltas(agent_rx, delta_tx, max_chars));

    let cwd = std::path::Path::new(".");
    let tool_env = crate::agent::tools::ToolEnv {
        enabled: Vec::new(),
        web_search_url: String::new(),
        web_search_backend: WebSearchBackend::default(),
        // Inert: completion runs have no tools enabled.
        bash_timeout_cap_secs: 300,
        mcp: None,
        undo_journal: None,
        subagent: None,
    };

    let r = if cfg.is_acp() {
        // ACP drives a full interactive agent session; it has no cheap one-shot
        // text-completion path for helpers like commit-message generation.
        Err("ACP agents do not support one-shot completion. \
             Pick another provider for commit-message generation."
            .to_string())
    } else {
        crate::agent::dispatch::run_provider_loop(
            crate::agent::dispatch::DispatchParams {
                cfg: &cfg,
                client: &client,
                tunnels: None,
                cwd,
                env: &tool_env,
                tx: &agent_tx,
                cancel: &cancel,
                gate: &mut gate,
                max_rounds,
                effort_override: effort_override.as_deref(),
                context_char_budget: usize::MAX,
                tools_chars: 0,
            },
            &mut messages,
            &tools,
        )
        .await
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
fn collect_deltas(
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
            AgentEvent::Finished(crate::agent::AgentOutcome::Failed { error }) => {
                return Err(error);
            }
            AgentEvent::Finished(crate::agent::AgentOutcome::Cancelled) => {
                return Err("Cancelled".to_string());
            }
            // The round is being re-sent after a dropped stream: discard the partial
            // text so the retried generation does not get appended to it.
            AgentEvent::StreamRetry { .. } => out.clear(),
            _ => {}
        }
    }
    Ok(out)
}
