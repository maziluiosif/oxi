//! Spawn background agent run (tokio + mpsc).

use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};

use crate::agent::approval::{ApprovalDecision, ApprovalGate, ApprovalPolicy};
use crate::agent::events::{AgentEvent, AgentOutcome};
use crate::agent::history::{
    build_openai_messages, trim_wire_history_to_budget, user_content_to_openai,
};
use crate::agent::tools::{ToolEnv, tool_definitions_json};
use crate::model::{ChatMessage, WireCache};
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
        LlmProviderKind::ClaudeCodeAcp | LlmProviderKind::CursorAcp | LlmProviderKind::CodexAcp => {
            "acp"
        }
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

mod provider_config;
pub use provider_config::openrouter_extra_headers;
pub(crate) use provider_config::{
    azure_openai_api_version, configured_azure_openai_key, configured_custom_anthropic_key,
    configured_lmstudio_key, configured_ollama_key, configured_openai_key,
    configured_opencode_go_key, configured_openrouter_key, opencode_go_model_uses_anthropic,
};

/// Immutable snapshot for one agent run.
pub struct AgentRunRequest {
    pub settings: AppSettings,
    pub tunnels: crate::compute::TunnelManager,
    pub acp: crate::agent::acp::AcpManager,
    /// App-wide MCP connections, reused across runs.
    pub mcp: crate::agent::mcp::McpManager,
    pub acp_session_key: String,
    pub cwd: PathBuf,
    pub chat_for_history: Vec<ChatMessage>,
    pub approval_rx: Receiver<ApprovalDecision>,
    pub cancel: Arc<AtomicBool>,
    pub wire_candidate: Option<WireCache>,
    pub chars_per_token: f32,
    /// Plan mode: read-only tools only, and the model is asked for a plan instead of changes.
    pub plan_mode: bool,
    pub undo_journal: Arc<std::sync::Mutex<crate::agent::tools::TurnUndoJournal>>,
}

/// Spawns agent runs on the app-wide runtime ([`crate::runtime`]).
#[derive(Clone)]
pub struct AgentExecutor {
    runtime: &'static tokio::runtime::Runtime,
}

impl AgentExecutor {
    pub fn new() -> Result<Self, String> {
        crate::runtime::runtime().map(|runtime| Self { runtime })
    }

    fn spawn<F>(&self, future: F) -> tokio::task::JoinHandle<F::Output>
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.runtime.spawn(future)
    }
}

mod routing;
pub use routing::spawn_agent_run;

fn spawn_agent_attempt(
    executor: &AgentExecutor,
    request: AgentRunRequest,
    tx: Sender<AgentEvent>,
) -> tokio::task::JoinHandle<()> {
    executor.spawn(async move {
        let AgentRunRequest {
            settings,
            tunnels,
            acp,
            mcp,
            acp_session_key,
            cwd,
            chat_for_history,
            approval_rx,
            cancel,
            wire_candidate,
            chars_per_token,
            plan_mode,
            undo_journal,
        } = request;

        let cwd_ref = cwd.as_path();
        let cfg = settings.active_config().clone();

        // ACP inverts oxi's model: Claude Code runs the agent loop in a subprocess. Handle
        // it entirely here — no system prompt, wire history, or tool definitions from oxi —
        // then return before the HTTP-provider machinery below.
        if cfg.is_acp() {
            // The agent edits files with its own tools, so the turn is made reversible by
            // snapshotting the work tree around it (git repositories only).
            let checkpoint_cwd = cwd.clone();
            let before = tokio::task::spawn_blocking(move || {
                crate::git::checkpoint::snapshot(&checkpoint_cwd)
            })
            .await
            .map_err(|e| e.to_string())
            .and_then(|r| r);
            {
                let mut journal = undo_journal.lock().unwrap_or_else(|e| e.into_inner());
                match before {
                    Ok(snapshot) => journal.set_checkpoint_before(snapshot),
                    Err(_) => journal.mark_non_reversible(
                        "The ACP agent edits files itself; responses can only be restored in a git repository.",
                    ),
                }
            }
            let outcome = run_acp_turn(
                &cfg,
                settings.mcp_servers.clone(),
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
                settings.bash_allowlist.clone(),
                &cancel,
                plan_mode,
            )
            .await;
            if let AgentOutcome::Failed { error } = &outcome
                && crate::router::quota::is_quota_error(error)
            {
                crate::router::quota::start_cooldown(cfg.provider, QUOTA_COOLDOWN);
            }
            let checkpoint_cwd = cwd.clone();
            let after = tokio::task::spawn_blocking(move || {
                crate::git::checkpoint::snapshot(&checkpoint_cwd)
            })
            .await
            .map_err(|e| e.to_string())
            .and_then(|r| r);
            let changes = {
                let mut journal = undo_journal.lock().unwrap_or_else(|e| e.into_inner());
                match after {
                    Ok(snapshot) => journal.set_checkpoint_after(snapshot.tree),
                    Err(e) => {
                        journal.mark_non_reversible(format!("Could not snapshot the workspace: {e}"));
                        None
                    }
                }
            };
            if let Some(changes) = changes {
                let _ = tx.send(AgentEvent::TurnChanges(Box::new(changes)));
            }
            let _ = tx.send(AgentEvent::Finished(outcome));
            return;
        }

        let mut system =
            crate::agent::prompt::build_system_prompt_for_workspace(&settings, cwd_ref);
        let mut enabled = settings.tools_enabled.clone();
        if plan_mode {
            system.push_str(crate::agent::prompt::PLAN_MODE_PROMPT);
            for (on, name) in enabled.iter_mut().zip(crate::settings::ALL_TOOL_NAMES) {
                *on &= crate::agent::approval::allowed_in_plan_mode(name);
            }
        }
        let max_rounds = settings.max_tool_rounds;
        let mut tools = tool_definitions_json(&enabled, settings.bash_timeout_cap_secs);
        // Connects new/changed servers and restarts dead ones; a no-op when everything is up.
        // Blocking (process spawns, HTTP handshakes), so keep it off the async workers.
        {
            let mcp = mcp.clone();
            let servers = settings.mcp_servers.clone();
            let _ = tokio::task::spawn_blocking(move || mcp.sync_servers(&servers)).await;
        }
        // MCP tools have unknown side effects, so plan mode leaves them out.
        if !plan_mode {
            tools.extend(mcp.tool_definitions());
        }
        // The tool definitions ride along in every request, so count them as fixed overhead when
        // deciding how much history fits under the trim ceiling.
        let tools_chars: usize = tools.iter().map(|v| v.to_string().len()).sum();
        let default_window = settings.context_window_default;
        let context_budget_for = |cfg: &ProviderConfig| {
            crate::agent::history::context_char_budget_from_tokens(
                cfg.effective_context_window(default_window),
                chars_per_token,
            )
        };
        let context_budget = context_budget_for(&cfg);
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
            trim_wire_history_to_budget(&mut wire, tools_chars, context_budget);
            wire
        } else {
            build_openai_messages(&system, &chat_for_history, tools_chars, context_budget)
        };
        let mut tool_env = ToolEnv {
            enabled,
            web_search_url: settings.effective_web_search_url(),
            web_search_backend: settings.web_search_backend,
            bash_timeout_cap_secs: settings.bash_timeout_cap_secs,
            mcp: (!plan_mode).then_some(mcp),
            undo_journal: Some(undo_journal),
            subagent: None,
        };
        let mut gate = ApprovalGate::new(
            ApprovalPolicy {
                write_edit: settings.require_write_edit_approval,
                bash: settings.require_bash_approval,
            },
            approval_rx,
        )
        .with_plan_mode(plan_mode)
        .with_bash_allowlist(settings.bash_allowlist.clone());

        let outcome = {
            tool_env.subagent = Some(Arc::new(
                crate::agent::subagent::SubagentRunner::new(
                    cfg.clone(),
                    tunnels.clone(),
                    cwd.clone(),
                    &tool_env,
                    tokio::runtime::Handle::current(),
                    cancel.clone(),
                    context_budget,
                )
                .with_usage_sender(tx.clone()),
            ));

            let effort_override = (!cfg.effort.trim().is_empty()).then_some(cfg.effort.trim());
            // No total request timeout: it would also cover the streamed body and kill long
            // turns mid-stream (see `streaming_client`).
            let client = match crate::agent::dispatch::streaming_client(&cfg, 180) {
                Ok(c) => c,
                Err(e) => {
                    finish_with_error(&tx, e);
                    return;
                }
            };

            let r = crate::agent::dispatch::run_provider_loop(
                crate::agent::dispatch::DispatchParams {
                    cfg: &cfg,
                    client: &client,
                    tunnels: Some(&tunnels),
                    cwd: cwd_ref,
                    env: &tool_env,
                    tx: &tx,
                    cancel: &cancel,
                    gate: &mut gate,
                    max_rounds,
                    effort_override,
                    context_char_budget: context_budget,
                    tools_chars,
                },
                &mut messages,
                &tools,
            )
            .await;
            match r {
                _ if cancel.load(Ordering::SeqCst) => AgentOutcome::Cancelled,
                Err(error) => {
                    if crate::router::quota::is_quota_error(&error) {
                        crate::router::quota::start_cooldown(cfg.provider, QUOTA_COOLDOWN);
                    }
                    AgentOutcome::Failed { error }
                }
                Ok(()) => AgentOutcome::Success {
                    wire_cache: Some(WireCache { fingerprint: wire_fingerprint, messages }),
                },
            }
        };
        let _ = tx.send(AgentEvent::Finished(outcome));
    })
}

/// How long the router avoids a provider after a rate-limit / quota error when the provider
/// did not report when its window resets.
const QUOTA_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(15 * 60);

fn first_line(s: &str) -> &str {
    let line = s.lines().next().unwrap_or(s);
    match line.char_indices().nth(140) {
        Some((i, _)) => &line[..i],
        None => line,
    }
}

/// Drive one Claude Code (ACP) turn: extract the latest user message, submit it to the ACP
/// manager, and return the outcome for the caller's terminal [`AgentEvent::Finished`]. Unlike
/// the HTTP providers there is no wire history to emit — the agent keeps session state in its
/// subprocess.
#[allow(clippy::too_many_arguments)]
async fn run_acp_turn(
    cfg: &ProviderConfig,
    mcp_servers: Vec<crate::settings::McpServerConfig>,
    acp: &crate::agent::acp::AcpManager,
    acp_session_key: String,
    cwd: PathBuf,
    chat_for_history: &[ChatMessage],
    tx: &Sender<AgentEvent>,
    approval_rx: Receiver<ApprovalDecision>,
    approval_policy: ApprovalPolicy,
    bash_allowlist: Vec<String>,
    cancel: &Arc<AtomicBool>,
    plan_mode: bool,
) -> AgentOutcome {
    let last_user_idx = chat_for_history
        .iter()
        .rposition(|m| m.role == crate::model::MsgRole::User);
    let last_user = last_user_idx.map(|i| &chat_for_history[i]);
    let history = acp_history_transcript(&chat_for_history[..last_user_idx.unwrap_or(0)]);
    // `@`-mentions travel as ACP resources instead of the context oxi inlines for other
    // providers, so the agent sees real files (and opens big ones itself).
    let (text, resources) = last_user
        .map(|m| {
            let mut raw = m.clone();
            raw.text = crate::app::mentions::strip_mention_context(&m.text).to_string();
            let resources = crate::app::mentions::mentioned_paths(&raw.text, &cwd);
            (raw.user_prompt_text(), resources)
        })
        .unwrap_or_default();
    let images: Vec<(String, Vec<u8>)> = last_user
        .map(|m| {
            m.attachments
                .iter()
                .filter_map(|a| match a {
                    crate::model::UserAttachment::Text { .. } => None,
                    crate::model::UserAttachment::Image { mime, data } => {
                        Some((mime.clone(), data.clone()))
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    let env = cfg.acp_env();

    let req = crate::agent::acp::AcpPrompt {
        session_key: acp_session_key,
        cwd,
        command_line: cfg.effective_acp_command(),
        env,
        model: cfg.model_id.clone(),
        effort: cfg.effort.clone(),
        mcp_servers,
        text,
        history,
        images,
        resources,
        event_tx: tx.clone(),
        approval_rx,
        approval_policy,
        bash_allowlist,
        cancel: cancel.clone(),
        plan_mode,
    };

    match acp.prompt(req).await {
        Err(_) if cancel.load(Ordering::SeqCst) => AgentOutcome::Cancelled,
        Err(error) => AgentOutcome::Failed { error },
        Ok(()) if cancel.load(Ordering::SeqCst) => AgentOutcome::Cancelled,
        Ok(()) => AgentOutcome::Success { wire_cache: None },
    }
}

/// Upper bound on the replayed transcript; older turns are dropped first.
const ACP_HISTORY_MAX_CHARS: usize = 120_000;

/// Plain-text transcript of earlier turns, handed to an ACP agent that had to start a blank
/// session (it could not resume its own) so it still knows the conversation. Keeps user text,
/// compaction summaries, answers, and one line per tool call; drops thinking and tool output.
fn acp_history_transcript(chat: &[ChatMessage]) -> String {
    use crate::model::{AssistantBlock, MsgRole};
    let mut turns: Vec<String> = Vec::new();
    for m in chat {
        match m.role {
            MsgRole::User if m.is_summary => {
                turns.push(format!(
                    "[Summary of earlier conversation]\n{}",
                    m.text.trim()
                ));
            }
            MsgRole::User => {
                let mut t = format!("User: {}", m.user_prompt_text().trim());
                if m.attachments
                    .iter()
                    .any(|a| matches!(a, crate::model::UserAttachment::Image { .. }))
                {
                    t.push_str(&format!(
                        "\n[{} image(s) attached]",
                        m.attachments
                            .iter()
                            .filter(|a| matches!(a, crate::model::UserAttachment::Image { .. }))
                            .count()
                    ));
                }
                turns.push(t);
            }
            MsgRole::Assistant => {
                let mut parts = Vec::new();
                for b in &m.blocks {
                    match b {
                        AssistantBlock::Answer(a) if !a.trim().is_empty() => {
                            parts.push(a.trim().to_string())
                        }
                        AssistantBlock::Tool {
                            name, args_summary, ..
                        } => parts.push(match args_summary {
                            Some(args) => format!("[tool {name}: {args}]"),
                            None => format!("[tool {name}]"),
                        }),
                        _ => {}
                    }
                }
                if !parts.is_empty() {
                    turns.push(format!("Assistant: {}", parts.join("\n")));
                }
            }
        }
    }
    let mut total = 0;
    let mut start = turns.len();
    while start > 0 && total + turns[start - 1].len() <= ACP_HISTORY_MAX_CHARS {
        start -= 1;
        total += turns[start].len() + 2;
    }
    turns[start..].join("\n\n")
}

/// Cancel routing immediately even while a classifier request or quota probe is pending.
async fn wait_for_router_cancel(cancel: &AtomicBool) {
    while !cancel.load(Ordering::SeqCst) {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

#[cfg(test)]
#[path = "runner/tests.rs"]
mod tests;
