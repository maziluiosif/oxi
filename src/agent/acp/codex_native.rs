//! Official native Codex CLI client over `codex app-server --stdio` (JSON-RPC).
//!
//! Speaks Codex's first-class app-server protocol directly instead of wrapping it through an
//! npm ACP adapter: native `thread/start` & `thread/resume`, `turn/start`, mid-turn `turn/steer`,
//! `turn/interrupt`, `model/list`, `skills/list`, structured `commandExecution`/`fileChange`/
//! `mcpToolCall` items, native approval & `requestUserInput` prompts, exact token usage /
//! context window tracking, and external `thread/list` + `thread/read` session import.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::mpsc::{Receiver as StdReceiver, Sender as StdSender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex as AsyncMutex, mpsc, oneshot};

use super::backend::{resolve_cli_binary, split_shell_words};
use super::commands::AcpSlashCommand;
use super::config_options::{self, AcpConfigKind, AcpConfigOption};
use super::import::{AcpImport, ExternalSession, without_oxi_preamble};
use super::permissions::wait_decision;
use super::rpc::{drain_stderr, write_line};
use super::{
    ACP_PLAN_MODE_PREFIX, AcpPrompt, AcpSetOption, CANCEL_WAIT_LIMIT, LaunchSpec, Pending,
    autonomous, chat_archive, commands, is_advertised_command, learned_rules, mcp_servers, oxi_mcp,
    reaper, session_info, sessions, with_history, with_missed,
};
use crate::agent::activity_log::{self, ActivityKind};
use crate::agent::approval::{ApprovalDecision, ApprovalPolicy};
use crate::agent::events::{AgentEvent, TokenUsage};
use crate::model::{
    ChatMessage, ToolLocation, ToolMetadata, ToolStatus, ToolUpdate, apply_tool_update,
};
use crate::settings::{McpServerConfig, McpTransport, ProviderConfig};

/// Default Codex app-server mode values exposed in the composer.
const MODE_DEFAULT: &str = "default";
const MODE_PLAN: &str = "plan";
const MODE_FULL_ACCESS: &str = "full_access";

#[derive(Clone, Debug)]
struct CodexModel {
    id: String,
    display_name: String,
    description: String,
    efforts: Vec<(String, String)>,
    default_effort: String,
    supports_fast: bool,
    supports_images: bool,
    is_default: bool,
    hidden: bool,
}

struct CodexState {
    models: Vec<CodexModel>,
    model: String,
    effort: String,
    mode: String,
    fast: bool,
    session_key: String,
    command_line: String,
}

impl CodexState {
    fn active_model(&self) -> Option<&CodexModel> {
        self.models
            .iter()
            .find(|m| m.id == self.model)
            .or_else(|| self.models.iter().find(|m| m.is_default))
            .or_else(|| self.models.first())
    }

    fn supports_images(&self) -> bool {
        self.active_model().is_none_or(|m| m.supports_images)
    }

    fn supports_fast(&self) -> bool {
        self.active_model().is_some_and(|m| m.supports_fast)
    }

    fn available_model_ids(&self) -> Vec<String> {
        self.models
            .iter()
            .filter(|m| !m.hidden || m.id == self.model)
            .map(|m| m.id.clone())
            .collect()
    }

    fn to_config_options(&self) -> Value {
        let mut options = Vec::new();
        options.push(json!({
            "id": "mode",
            "name": "Mode",
            "description": "How Codex runs commands and edits files",
            "category": "mode",
            "type": "select",
            "currentValue": self.mode,
            "options": [
                { "value": MODE_DEFAULT, "name": "Agent", "description": "Workspace write access with approval prompts" },
                { "value": MODE_PLAN, "name": "Plan", "description": "Read-only exploration and planning" },
                { "value": MODE_FULL_ACCESS, "name": "Full access", "description": "Unrestricted workspace and shell access" }
            ]
        }));

        let visible_models: Vec<Value> = self
            .models
            .iter()
            .filter(|m| !m.hidden || m.id == self.model)
            .map(|m| {
                json!({
                    "value": m.id,
                    "name": m.display_name,
                    "description": m.description,
                })
            })
            .collect();
        if !visible_models.is_empty() {
            options.push(json!({
                "id": "model",
                "name": "Model",
                "description": "Model Codex uses for the session",
                "category": "model",
                "type": "select",
                "currentValue": self.model,
                "options": visible_models,
            }));
        }

        if let Some(m) = self.active_model()
            && !m.efforts.is_empty()
        {
            let current = if m.efforts.iter().any(|(e, _)| e == &self.effort) {
                self.effort.clone()
            } else {
                m.default_effort.clone()
            };
            let effort_opts: Vec<Value> = m
                .efforts
                .iter()
                .map(|(val, desc)| {
                    json!({
                        "value": val,
                        "name": title_case_effort(val),
                        "description": desc,
                    })
                })
                .collect();
            options.push(json!({
                "id": "reasoning_effort",
                "name": "Reasoning effort",
                "description": "How much reasoning effort the model should use",
                "category": "thought_level",
                "type": "select",
                "currentValue": current,
                "options": effort_opts,
            }));
        }

        if self.supports_fast() {
            options.push(json!({
                "id": "fast",
                "name": "Fast mode",
                "description": "Priority speed tier when supported by the model",
                "category": "model_config",
                "type": "boolean",
                "currentValue": self.fast,
            }));
        }

        Value::Array(options)
    }

    fn publish(&self) {
        if !self.session_key.is_empty() {
            config_options::publish(
                &self.session_key,
                &self.command_line,
                &self.to_config_options(),
            );
        }
    }

    fn apply_model(&mut self, model: &str) {
        let model = model.trim();
        if model.is_empty() || model == "default" {
            return;
        }
        if self.models.is_empty() || self.models.iter().any(|m| m.id == model) {
            self.model = model.to_owned();
            if let Some(m) = self.active_model()
                && !m.efforts.is_empty()
                && !m.efforts.iter().any(|(e, _)| e == &self.effort)
            {
                self.effort = m.default_effort.clone();
            }
        }
    }

    fn apply_effort(&mut self, effort: &str) {
        let effort = effort.trim();
        if effort.is_empty() || effort == "default" {
            return;
        }
        if self
            .active_model()
            .is_none_or(|m| m.efforts.is_empty() || m.efforts.iter().any(|(e, _)| e == effort))
        {
            self.effort = effort.to_owned();
        }
    }

    fn restore_previous(&mut self, previous: &[AcpConfigOption]) {
        for opt in previous {
            if opt.id == "mode"
                && let AcpConfigKind::Select { current, .. } = &opt.kind
                && matches!(current.as_str(), MODE_DEFAULT | MODE_FULL_ACCESS)
            {
                self.mode = current.clone();
            } else if opt.id == "fast"
                && let AcpConfigKind::Boolean(on) = opt.kind
            {
                self.fast = on;
            }
        }
    }
}

fn title_case_effort(effort: &str) -> String {
    let mut chars = effort.chars();
    match chars.next() {
        Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
        None => String::new(),
    }
}

/// Incoming server→client JSON-RPC request needing a response.
struct CodexServerReq {
    id: Value,
    method: String,
    params: Value,
}

/// Completion payload delivered when `turn/completed` arrives.
struct TurnCompletion {
    status: String,
    error: Option<String>,
}

/// Active turn state shared with the stdout reader loop.
struct CodexTurnCtx {
    thread_id: String,
    event_tx: StdSender<AgentEvent>,
    req_tx: mpsc::UnboundedSender<CodexServerReq>,
    done_tx: Option<oneshot::Sender<TurnCompletion>>,
    /// Per-item accumulated command output & metadata.
    tools: HashMap<String, CodexToolState>,
    /// Tracks whether `item/agentMessage/delta` already streamed text for an item.
    streamed_messages: HashSet<String>,
    /// Tracks whether `item/reasoning/*Delta` already streamed thinking for an item.
    streamed_reasoning: HashSet<String>,
    /// Last emitted checklist JSON to suppress duplicate updates.
    last_checklist: Option<Value>,
    /// Last reported fatal error notification (when `willRetry == false`).
    last_error: Option<String>,
    /// Cumulative thread token usage before this turn started, so we emit per-turn deltas.
    baseline_usage: TokenUsage,
    latest_total_usage: TokenUsage,
    /// Last non-zero model context window observed.
    context_window: u64,
}

#[derive(Default)]
struct CodexToolState {
    name: String,
    kind: String,
    title: String,
    args: Option<Value>,
    output: String,
    diff: Option<String>,
    locations: Vec<ToolLocation>,
    status: ToolStatus,
}

impl CodexTurnCtx {
    fn emit_tool(&self, id: &str, tool: &CodexToolState) {
        let limit = crate::agent::tools::MAX_TOOL_OUTPUT_CHARS;
        let output_truncated = tool.output.chars().count() > limit;
        let _ = self
            .event_tx
            .send(AgentEvent::ToolUpdate(Box::new(ToolUpdate {
                tool_call_id: id.to_owned(),
                name: tool.name.clone(),
                args: tool.args.clone(),
                output: tool.output.chars().take(limit).collect(),
                diff: tool.diff.clone(),
                output_truncated,
                metadata: ToolMetadata {
                    title: tool.title.clone(),
                    kind: tool.kind.clone(),
                    name: Some(tool.name.clone()),
                    locations: tool.locations.clone(),
                    status: tool.status,
                },
            })));
    }

    fn emit_checklist(&mut self, entries: &[Value]) {
        let todos: Vec<Value> = entries
            .iter()
            .filter_map(|e| {
                let content = e
                    .get("content")
                    .or_else(|| e.get("step"))
                    .and_then(Value::as_str)?
                    .trim();
                let status = match e.get("status").and_then(Value::as_str).unwrap_or("pending") {
                    "inProgress" | "in_progress" => "in_progress",
                    "completed" => "completed",
                    _ => "pending",
                };
                (!content.is_empty()).then(|| json!({ "content": content, "status": status }))
            })
            .collect();
        let args = json!({ "todos": todos });
        if self.last_checklist.as_ref() == Some(&args) {
            return;
        }
        self.last_checklist = Some(args.clone());
        let _ = self
            .event_tx
            .send(AgentEvent::ToolUpdate(Box::new(ToolUpdate {
                tool_call_id: "codex-plan".to_owned(),
                name: "todo_write".to_owned(),
                output: crate::agent::tools::tool_todo_write(&args).unwrap_or_default(),
                args: Some(args),
                diff: None,
                output_truncated: false,
                metadata: ToolMetadata {
                    title: String::new(),
                    kind: "think".to_owned(),
                    name: Some("todo_write".to_owned()),
                    locations: Vec::new(),
                    status: ToolStatus::Completed,
                },
            })));
    }

    fn handle_notification(&mut self, method: &str, params: &Value) {
        if let Some(tid) = params["threadId"].as_str()
            && tid != self.thread_id
        {
            return;
        }
        match method {
            "item/agentMessage/delta" => {
                if let Some(delta) = params["delta"].as_str()
                    && !delta.is_empty()
                {
                    if let Some(id) = params["itemId"].as_str() {
                        self.streamed_messages.insert(id.to_owned());
                    }
                    let _ = self.event_tx.send(AgentEvent::TextDelta(delta.to_owned()));
                }
            }
            "item/plan/delta" => {
                if let Some(delta) = params["delta"].as_str()
                    && !delta.is_empty()
                {
                    if let Some(id) = params["itemId"].as_str() {
                        self.streamed_messages.insert(id.to_owned());
                    }
                    let _ = self.event_tx.send(AgentEvent::TextDelta(delta.to_owned()));
                }
            }
            "item/reasoning/summaryTextDelta" | "item/reasoning/textDelta" => {
                if let Some(delta) = params["delta"].as_str()
                    && !delta.is_empty()
                {
                    if let Some(id) = params["itemId"].as_str() {
                        self.streamed_reasoning.insert(id.to_owned());
                    }
                    let _ = self
                        .event_tx
                        .send(AgentEvent::ThinkingDelta(delta.to_owned()));
                }
            }
            "item/reasoning/summaryPartAdded" => {
                if let Some(id) = params["itemId"].as_str()
                    && self.streamed_reasoning.contains(id)
                {
                    let _ = self.event_tx.send(AgentEvent::ThinkingDelta("\n\n".into()));
                }
            }
            "item/commandExecution/outputDelta" => {
                if let (Some(id), Some(delta)) =
                    (params["itemId"].as_str(), params["delta"].as_str())
                {
                    let limit = crate::agent::tools::MAX_TOOL_OUTPUT_CHARS;
                    let tool = self
                        .tools
                        .entry(id.to_owned())
                        .or_insert_with(|| CodexToolState {
                            name: "bash".into(),
                            kind: "execute".into(),
                            status: ToolStatus::InProgress,
                            ..Default::default()
                        });
                    if tool.output.len() <= limit * 4 {
                        tool.output.push_str(delta);
                    }
                    let snapshot = CodexToolState {
                        name: tool.name.clone(),
                        kind: tool.kind.clone(),
                        title: tool.title.clone(),
                        args: tool.args.clone(),
                        output: tool.output.clone(),
                        diff: tool.diff.clone(),
                        locations: tool.locations.clone(),
                        status: tool.status,
                    };
                    self.emit_tool(id, &snapshot);
                }
            }
            "item/started" => {
                self.handle_item(&params["item"], false);
            }
            "item/completed" => {
                self.handle_item(&params["item"], true);
            }
            "turn/plan/updated" => {
                if let Some(plan) = params["plan"].as_array() {
                    self.emit_checklist(plan);
                }
            }
            "thread/tokenUsage/updated" => {
                let usage = &params["tokenUsage"];
                if let Some(window) = usage["modelContextWindow"].as_u64()
                    && window > 0
                {
                    self.context_window = window;
                }
                let last = &usage["last"];
                let used = last["totalTokens"].as_u64().unwrap_or(0);
                if used > 0 && self.context_window > 0 {
                    let _ = self.event_tx.send(AgentEvent::ContextUsage {
                        used,
                        size: self.context_window,
                    });
                }
                let total = parse_codex_usage(&usage["total"]);
                if !total.is_zero() {
                    self.latest_total_usage = total;
                }
            }
            "error" => {
                if params["willRetry"].as_bool() != Some(true)
                    && let Some(msg) = format_turn_error(&params["error"])
                {
                    self.last_error = Some(msg);
                }
            }
            "turn/completed" => {
                let turn = &params["turn"];
                let status = turn["status"].as_str().unwrap_or("completed").to_owned();
                let error = format_turn_error(&turn["error"]).or_else(|| self.last_error.take());
                if let Some(done) = self.done_tx.take() {
                    let _ = done.send(TurnCompletion { status, error });
                }
            }
            _ => {}
        }
    }

    fn handle_item(&mut self, item: &Value, completed: bool) {
        let Some(kind) = item["type"].as_str() else {
            return;
        };
        let id = item["id"].as_str().unwrap_or("").to_owned();
        match kind {
            "agentMessage" | "plan" => {
                if completed
                    && !self.streamed_messages.contains(&id)
                    && let Some(text) = item["text"].as_str()
                    && !text.is_empty()
                {
                    let _ = self.event_tx.send(AgentEvent::TextDelta(text.to_owned()));
                }
            }
            "reasoning" => {
                if completed && !self.streamed_reasoning.contains(&id) {
                    let text = reasoning_item_text(item);
                    if !text.is_empty() {
                        let _ = self.event_tx.send(AgentEvent::ThinkingDelta(text));
                    }
                }
            }
            "commandExecution" => {
                if id.is_empty() {
                    return;
                }
                let command = item["command"].as_str().unwrap_or("").to_owned();
                let cwd = item["cwd"].as_str().unwrap_or("").to_owned();
                let status = map_codex_status(item["status"].as_str(), completed);
                let tool = self.tools.entry(id.clone()).or_default();
                tool.name = "bash".into();
                tool.kind = "execute".into();
                tool.title = if command.is_empty() {
                    "Run command".into()
                } else {
                    command.clone()
                };
                tool.args = Some(json!({ "command": command, "cwd": cwd }));
                tool.status = status;
                if let Some(out) = item["aggregatedOutput"].as_str()
                    && !out.is_empty()
                {
                    tool.output = out.to_owned();
                } else if let Some(code) = item["exitCode"].as_i64()
                    && completed
                    && tool.output.is_empty()
                    && code != 0
                {
                    tool.output = format!("Exit code {code}");
                }
                let snapshot = CodexToolState {
                    name: tool.name.clone(),
                    kind: tool.kind.clone(),
                    title: tool.title.clone(),
                    args: tool.args.clone(),
                    output: tool.output.clone(),
                    diff: tool.diff.clone(),
                    locations: tool.locations.clone(),
                    status: tool.status,
                };
                self.emit_tool(&id, &snapshot);
            }
            "fileChange" => {
                if id.is_empty() {
                    return;
                }
                let status = map_codex_status(item["status"].as_str(), completed);
                let (title, diff, locations) = extract_file_changes(&item["changes"]);
                let tool = self.tools.entry(id.clone()).or_default();
                tool.name = "edit".into();
                tool.kind = "edit".into();
                tool.title = title;
                tool.diff = diff;
                tool.locations = locations;
                tool.status = status;
                let snapshot = CodexToolState {
                    name: tool.name.clone(),
                    kind: tool.kind.clone(),
                    title: tool.title.clone(),
                    args: tool.args.clone(),
                    output: tool.output.clone(),
                    diff: tool.diff.clone(),
                    locations: tool.locations.clone(),
                    status: tool.status,
                };
                self.emit_tool(&id, &snapshot);
            }
            "mcpToolCall" => {
                if id.is_empty() {
                    return;
                }
                let server = item["server"].as_str().unwrap_or("");
                let tool_name = item["tool"].as_str().unwrap_or("");
                let full_name = if server.is_empty() {
                    tool_name.to_owned()
                } else {
                    format!("mcp__{server}__{tool_name}")
                };
                // oxi's own checklist tool drives the native checklist rather than a tool row.
                if oxi_mcp::names_todo_tool(&full_name) {
                    if let Some(todos) = item["arguments"]["todos"].as_array() {
                        self.emit_checklist(todos);
                    }
                    return;
                }
                let display_name = oxi_mcp::oxi_tool_name(&full_name)
                    .map(str::to_owned)
                    .unwrap_or_else(|| full_name.clone());
                let status = map_codex_status(item["status"].as_str(), completed);
                let output = extract_mcp_result(item);
                let tool = self.tools.entry(id.clone()).or_default();
                tool.name = display_name;
                tool.kind = "other".into();
                tool.title = full_name;
                tool.args = Some(item["arguments"].clone()).filter(|v| !v.is_null());
                tool.status = status;
                if !output.is_empty() {
                    tool.output = output;
                }
                let snapshot = CodexToolState {
                    name: tool.name.clone(),
                    kind: tool.kind.clone(),
                    title: tool.title.clone(),
                    args: tool.args.clone(),
                    output: tool.output.clone(),
                    diff: tool.diff.clone(),
                    locations: tool.locations.clone(),
                    status: tool.status,
                };
                self.emit_tool(&id, &snapshot);
            }
            "webSearch" => {
                if id.is_empty() {
                    return;
                }
                let query = item["query"].as_str().unwrap_or("").to_owned();
                let status = if completed {
                    ToolStatus::Completed
                } else {
                    ToolStatus::InProgress
                };
                let tool = CodexToolState {
                    name: "web_search".into(),
                    kind: "search".into(),
                    title: if query.is_empty() {
                        "Web search".into()
                    } else {
                        query.clone()
                    },
                    args: (!query.is_empty()).then(|| json!({ "query": query })),
                    output: String::new(),
                    diff: None,
                    locations: Vec::new(),
                    status,
                };
                self.emit_tool(&id, &tool);
            }
            "imageView" => {
                if id.is_empty() {
                    return;
                }
                let path = item["path"].as_str().unwrap_or("").to_owned();
                let tool = CodexToolState {
                    name: "read".into(),
                    kind: "read".into(),
                    title: if path.is_empty() {
                        "View image".into()
                    } else {
                        path.clone()
                    },
                    args: (!path.is_empty()).then(|| json!({ "path": path })),
                    output: String::new(),
                    diff: None,
                    locations: if path.is_empty() {
                        Vec::new()
                    } else {
                        vec![ToolLocation { path, line: None }]
                    },
                    status: if completed {
                        ToolStatus::Completed
                    } else {
                        ToolStatus::InProgress
                    },
                };
                self.emit_tool(&id, &tool);
            }
            "collabAgentToolCall" => {
                if id.is_empty() {
                    return;
                }
                let tool_kind = item["tool"].as_str().unwrap_or("subagent");
                let prompt = item["prompt"].as_str().unwrap_or("");
                let tool = CodexToolState {
                    name: "subagent".into(),
                    kind: "think".into(),
                    title: format!("Subagent ({tool_kind})"),
                    args: (!prompt.is_empty()).then(|| json!({ "prompt": prompt })),
                    output: String::new(),
                    diff: None,
                    locations: Vec::new(),
                    status: map_codex_status(item["status"].as_str(), completed),
                };
                self.emit_tool(&id, &tool);
            }
            _ => {}
        }
    }
}

/// Live native Codex app-server connection for one oxi session.
pub(super) struct CodexConn {
    pub(super) command_line: String,
    pub(super) mcp_servers: Vec<McpServerConfig>,
    pub(super) alive: Arc<AtomicBool>,
    pub(super) handles: CodexHandles,
    _child: Child,
    _read_task: tokio::task::JoinHandle<()>,
    _stderr_task: tokio::task::JoinHandle<()>,
}

#[derive(Clone)]
pub(super) struct CodexHandles {
    stdin: Arc<AsyncMutex<ChildStdin>>,
    next_id: Arc<AtomicI64>,
    pending: Pending,
    turn_ctx: Arc<AsyncMutex<Option<CodexTurnCtx>>>,
    thread_id: String,
    state: Arc<Mutex<CodexState>>,
    needs_history: Arc<AtomicBool>,
    needs_rules: Arc<AtomicBool>,
    turn_lock: Arc<AsyncMutex<()>>,
    pub(super) alive: Arc<AtomicBool>,
    pub(super) activity: Arc<reaper::Activity>,
    evidence_dir: PathBuf,
    last_total_usage: Arc<Mutex<TokenUsage>>,
    /// The thread is in Codex's own Plan collaboration mode, which sticks to later turns.
    native_plan: Arc<AtomicBool>,
}

impl CodexHandles {
    pub(super) fn busy(&self) -> bool {
        self.turn_lock.try_lock().is_err()
            || self.pending.try_lock().map_or(true, |p| !p.is_empty())
    }

    pub(super) fn available_models(&self) -> Vec<String> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .available_model_ids()
    }

    pub(super) fn sync_model_effort(&self, model: &str, effort: &str) {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let before = (st.model.clone(), st.effort.clone());
        st.apply_model(model);
        st.apply_effort(effort);
        if (st.model.clone(), st.effort.clone()) != before {
            st.publish();
        }
    }

    pub(super) fn apply_option(&self, req: &AcpSetOption) -> Result<(), String> {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        match req.config_id.as_str() {
            "model" => {
                if let Some(v) = req.value.as_str() {
                    st.apply_model(v);
                }
            }
            "reasoning_effort" | "thought_level" | "effort" => {
                if let Some(v) = req.value.as_str() {
                    st.apply_effort(v);
                }
            }
            "mode" => {
                if let Some(v) = req.value.as_str()
                    && matches!(v, MODE_DEFAULT | MODE_PLAN | MODE_FULL_ACCESS)
                {
                    st.mode = v.to_owned();
                }
            }
            "fast" => {
                if let Some(b) = req.value.as_bool() {
                    st.fast = b;
                }
            }
            _ => {}
        }
        st.publish();
        Ok(())
    }
}

/// Spawn and initialize a native `codex app-server --stdio` connection.
pub(super) async fn spawn_conn(
    spec: &LaunchSpec<'_>,
    mcp_servers: Vec<McpServerConfig>,
) -> Result<CodexConn, String> {
    if !spec.cwd.is_dir() {
        return Err(format!(
            "Codex workspace does not exist or is not a directory: {}",
            spec.cwd.display()
        ));
    }
    let evidence_dir = crate::evidence::new_conn_dir();
    let delegation_dir =
        crate::delegation::available().then(|| crate::delegation::dir_for(spec.session_key));

    let (mut child, stdin, stdout, stderr) = spawn_codex_process(
        spec.command_line,
        spec.cwd,
        spec.env,
        &mcp_servers,
        Some(&evidence_dir),
        delegation_dir.as_deref(),
    )
    .await?;

    let stdin = Arc::new(AsyncMutex::new(stdin));
    let pending: Pending = Arc::new(AsyncMutex::new(HashMap::new()));
    let turn_ctx: Arc<AsyncMutex<Option<CodexTurnCtx>>> = Arc::new(AsyncMutex::new(None));
    let next_id = Arc::new(AtomicI64::new(1));
    let alive = Arc::new(AtomicBool::new(true));
    let activity = Arc::new(reaper::Activity::new());
    let stderr_tail = Arc::new(Mutex::new(VecDeque::new()));
    let stderr_task = tokio::spawn(drain_stderr(stderr, stderr_tail.clone()));
    let read_task = tokio::spawn(read_loop(
        stdout,
        pending.clone(),
        turn_ctx.clone(),
        alive.clone(),
        activity.clone(),
        spec.session_key.to_owned(),
    ));

    // Handshake: initialize -> initialized
    let init_res = tokio::time::timeout(
        Duration::from_secs(30),
        codex_request(
            &stdin,
            &next_id,
            &pending,
            "initialize",
            json!({
                "clientInfo": {
                    "name": "oxi",
                    "title": "oxi",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "capabilities": {
                    "experimentalApi": true,
                    "requestAttestation": false,
                }
            }),
        ),
    )
    .await
    .map_err(|_| "Codex app-server did not initialize within 30 seconds.".to_string())?;
    if let Err(e) = init_res {
        let _ = child.kill().await;
        return Err(with_stderr_tail(
            format!("Codex initialize failed: {e}"),
            &stderr_tail,
        ));
    }
    write_line(
        &stdin,
        &json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }),
    )
    .await?;

    // Discover models (non-fatal if empty, e.g. mock or offline).
    let models = match tokio::time::timeout(
        Duration::from_secs(15),
        codex_request(&stdin, &next_id, &pending, "model/list", json!({})),
    )
    .await
    {
        Ok(Ok(res)) => parse_models(&res),
        Ok(Err(e)) => {
            log::warn!("codex model/list failed: {e}");
            Vec::new()
        }
        Err(_) => {
            log::warn!("codex model/list timed out");
            Vec::new()
        }
    };

    // Discover skills for slash menu (best effort).
    if let Ok(Ok(res)) = tokio::time::timeout(
        Duration::from_secs(8),
        codex_request(
            &stdin,
            &next_id,
            &pending,
            "skills/list",
            json!({ "cwds": [spec.cwd.to_string_lossy()] }),
        ),
    )
    .await
    {
        let cmds = parse_skills_as_commands(&res);
        commands::store(spec.command_line, spec.cwd, cmds);
    }

    let mut state = CodexState {
        model: models
            .iter()
            .find(|m| m.is_default)
            .or_else(|| models.first())
            .map(|m| m.id.clone())
            .unwrap_or_default(),
        effort: models
            .iter()
            .find(|m| m.is_default)
            .or_else(|| models.first())
            .map(|m| m.default_effort.clone())
            .unwrap_or_else(|| "high".into()),
        models,
        mode: MODE_DEFAULT.into(),
        fast: false,
        session_key: spec.session_key.to_owned(),
        command_line: spec.command_line.to_owned(),
    };
    state.apply_model(spec.model);
    state.apply_effort(spec.effort);

    let previous = config_options::options(spec.session_key, spec.command_line).or_else(|| {
        let picked = config_options::picked_before_launch(spec.session_key, spec.command_line);
        (!picked.is_empty()).then(|| Arc::new(picked))
    });
    if let Some(prev) = previous.as_deref() {
        state.restore_previous(prev);
    }

    // Resume existing thread (including sessions created under legacy codex-acp!) or start fresh.
    let existing_id = sessions::lookup_with_fallbacks(
        spec.session_key,
        spec.command_line,
        spec.cwd,
        &[ProviderConfig::LEGACY_CODEX_ACP_COMMAND],
    );
    let mut thread_res: Option<(String, Value, bool)> = None;
    if let Some(tid) = existing_id {
        let mut params = json!({
            "threadId": tid,
            "cwd": spec.cwd.to_string_lossy(),
            "approvalPolicy": "on-request",
            "approvalsReviewer": "user",
            "sandbox": "workspace-write",
        });
        if !state.model.is_empty() {
            params["model"] = json!(state.model);
        }
        match tokio::time::timeout(
            Duration::from_secs(30),
            codex_request(&stdin, &next_id, &pending, "thread/resume", params),
        )
        .await
        {
            Ok(Ok(res)) => {
                let id = res["thread"]["id"].as_str().unwrap_or(&tid).to_owned();
                thread_res = Some((id, res, false));
            }
            Ok(Err(e)) => log::warn!("codex thread/resume {tid} failed, starting fresh: {e}"),
            Err(_) => log::warn!("codex thread/resume {tid} timed out, starting fresh"),
        }
    }
    let (thread_id, res, needs_history) = match thread_res {
        Some(v) => v,
        None => {
            let mut params = json!({
                "cwd": spec.cwd.to_string_lossy(),
                "approvalPolicy": "on-request",
                "approvalsReviewer": "user",
                "sandbox": "workspace-write",
            });
            if !state.model.is_empty() {
                params["model"] = json!(state.model);
            }
            let res = tokio::time::timeout(
                Duration::from_secs(30),
                codex_request(&stdin, &next_id, &pending, "thread/start", params),
            )
            .await
            .map_err(|_| "Codex thread/start timed out after 30 seconds.".to_string())?
            .map_err(|e| format!("Codex thread/start failed: {e}"))?;
            let id = res["thread"]["id"]
                .as_str()
                .ok_or("Codex thread/start returned no thread id")?
                .to_owned();
            (id, res, true)
        }
    };

    if state.model.is_empty()
        && let Some(m) = res["model"].as_str()
    {
        state.model = m.to_owned();
    }
    if let Some(e) = res["reasoningEffort"].as_str()
        && spec.effort.trim().is_empty()
    {
        state.apply_effort(e);
    }

    sessions::remember(spec.session_key, spec.command_line, spec.cwd, &thread_id);
    let opts_json = state.to_config_options();
    if needs_history {
        config_options::remember_defaults(spec.command_line, &opts_json);
    }
    state.publish();

    let handles = CodexHandles {
        stdin,
        next_id,
        pending,
        turn_ctx,
        thread_id,
        state: Arc::new(Mutex::new(state)),
        needs_history: Arc::new(AtomicBool::new(needs_history)),
        needs_rules: Arc::new(AtomicBool::new(true)),
        turn_lock: Arc::new(AsyncMutex::new(())),
        alive: alive.clone(),
        activity,
        evidence_dir,
        last_total_usage: Arc::new(Mutex::new(TokenUsage::default())),
        native_plan: Arc::new(AtomicBool::new(false)),
    };
    Ok(CodexConn {
        command_line: spec.command_line.to_owned(),
        mcp_servers,
        alive,
        handles,
        _child: child,
        _read_task: read_task,
        _stderr_task: stderr_task,
    })
}

/// Drive one prompt turn on a live native Codex connection.
pub(super) async fn run_prompt(
    handles: CodexHandles,
    req: AcpPrompt,
    reply: oneshot::Sender<Result<(), String>>,
) {
    let AcpPrompt {
        session_key,
        cwd,
        command_line,
        text,
        history,
        missed,
        archive,
        images,
        resources,
        event_tx,
        mut approval_rx,
        approval_policy,
        mut bash_allowlist,
        mut allowed_tools,
        cancel,
        plan_mode,
        evidence_policy,
        evidence_base,
        steer,
        ..
    } = req;

    let _turn = handles.turn_lock.clone().lock_owned().await;
    handles.activity.touch();
    if cancel.load(Ordering::SeqCst) {
        let _ = reply.send(Ok(()));
        return;
    }

    if let Err(e) = chat_archive::write(&handles.evidence_dir, &archive, &session_key, &cwd) {
        log::warn!("chat_read: could not save the conversation: {e}");
    }
    let evidence_turn = format!("t{}", crate::evidence::now_ms());
    let evidence_ready = crate::evidence::begin_turn(
        &handles.evidence_dir,
        &crate::evidence::TurnMarker {
            baseline: evidence_base
                .as_ref()
                .map(|(repo, tree)| crate::evidence::WorkspaceState {
                    root: repo.to_string_lossy().into_owned(),
                    tree: tree.clone(),
                })
                .or_else(|| crate::evidence::WorkspaceState::capture(&cwd).ok()),
            turn: evidence_turn.clone(),
            started_ms: crate::evidence::now_ms(),
            cwd: cwd.to_string_lossy().into_owned(),
            repo: evidence_base
                .as_ref()
                .map(|(repo, _)| repo.to_string_lossy().into_owned()),
            base_tree: evidence_base.map(|(_, tree)| tree),
        },
    )
    .inspect_err(|e| log::warn!("evidence: could not start the turn: {e}"))
    .is_ok();
    let evidence_enabled = || evidence_policy.mode.enabled() && evidence_ready && !plan_mode;

    let (model, model_id, effort, mode, fast, supports_images) = {
        let st = handles.state.lock().unwrap_or_else(|e| e.into_inner());
        (
            st.model.clone(),
            st.active_model().map(|m| m.id.clone()).unwrap_or_default(),
            st.effort.clone(),
            st.mode.clone(),
            st.fast && st.supports_fast(),
            st.supports_images(),
        )
    };
    if !images.is_empty() && !supports_images {
        let _ = reply.send(Err(format!(
            "This Codex model does not accept images, so the message was not sent. Remove the {} or pick a vision-capable model.",
            if images.len() == 1 { "image" } else { "images" }
        )));
        return;
    }

    let effective_plan_mode = plan_mode || mode == MODE_PLAN;
    let text = learned_rules::expand(&text, &cwd).unwrap_or(text);
    let slash_command = is_advertised_command(&text, &command_line, &cwd);

    // Native `/compact` maps directly to `thread/compact/start`.
    if text.trim() == "/compact" {
        let _ = event_tx.send(AgentEvent::AgentStart);
        let res = tokio::time::timeout(
            Duration::from_secs(60),
            codex_request(
                &handles.stdin,
                &handles.next_id,
                &handles.pending,
                "thread/compact/start",
                json!({ "threadId": handles.thread_id }),
            ),
        )
        .await
        .map_err(|_| "Codex compaction timed out.".to_string())
        .and_then(|r| r.map(|_| ()));
        if res.is_ok() {
            let _ = event_tx.send(AgentEvent::TextDelta(
                "*Compacted the Codex thread context.*\n".into(),
            ));
        }
        let _ = reply.send(res);
        return;
    }

    let text = if effective_plan_mode && !slash_command {
        format!("{ACP_PLAN_MODE_PREFIX}\n\n{text}")
    } else if evidence_enabled() && !slash_command {
        format!("{}\n\n{text}", crate::evidence::PROMPT_PREFIX)
    } else {
        text
    };
    let text = if slash_command {
        text
    } else if handles.needs_history.swap(false, Ordering::SeqCst) {
        with_history(&history, &text)
    } else {
        with_missed(&missed, &text)
    };
    let text = if !slash_command && handles.needs_rules.swap(false, Ordering::SeqCst) {
        learned_rules::with_rules(&cwd, &text)
    } else {
        text
    };

    let mut input_items = match build_user_inputs(&text, &images, &resources, &handles.evidence_dir)
    {
        Ok(items) => items,
        Err(e) => {
            let _ = reply.send(Err(e));
            return;
        }
    };
    let (approval_policy_str, sandbox_policy) =
        turn_policies(effective_plan_mode, &mode, &cwd, approval_policy);

    let _ = event_tx.send(AgentEvent::AgentStart);
    let _ = event_tx.send(AgentEvent::SteerAvailable);

    let mut auto_approve = false;
    let mut followups = 0;

    let final_result: Result<(), String> = loop {
        let (req_tx, mut req_rx) = mpsc::unbounded_channel::<CodexServerReq>();
        let (done_tx, mut done_rx) = oneshot::channel::<TurnCompletion>();
        let baseline = *handles
            .last_total_usage
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        {
            *handles.turn_ctx.lock().await = Some(CodexTurnCtx {
                thread_id: handles.thread_id.clone(),
                event_tx: event_tx.clone(),
                req_tx,
                done_tx: Some(done_tx),
                tools: HashMap::new(),
                streamed_messages: HashSet::new(),
                streamed_reasoning: HashSet::new(),
                last_checklist: None,
                last_error: None,
                baseline_usage: baseline,
                latest_total_usage: baseline,
                context_window: 0,
            });
        }

        let mut start_params = json!({
            "threadId": handles.thread_id,
            "cwd": cwd.to_string_lossy(),
            "input": input_items,
            "approvalPolicy": approval_policy_str,
            "approvalsReviewer": "user",
            "sandboxPolicy": sandbox_policy,
        });
        if !model.is_empty() && model != "default" {
            start_params["model"] = json!(model);
        }
        if !effort.is_empty() && effort != "default" {
            start_params["effort"] = json!(effort);
        }
        if fast {
            start_params["serviceTier"] = json!("priority");
        } else {
            start_params["serviceTier"] = json!("default");
        }
        let was_plan = handles.native_plan.load(Ordering::SeqCst);
        let collab = collaboration_mode(effective_plan_mode, was_plan, &model_id, &effort);
        let sets_mode = collab.is_some();
        if let Some(collab) = collab {
            start_params["collaborationMode"] = collab;
        }

        let started = tokio::time::timeout(
            Duration::from_secs(30),
            codex_request(
                &handles.stdin,
                &handles.next_id,
                &handles.pending,
                "turn/start",
                start_params,
            ),
        )
        .await;
        let turn_id = match started {
            Ok(Ok(res)) => {
                if sets_mode {
                    handles
                        .native_plan
                        .store(effective_plan_mode, Ordering::SeqCst);
                }
                res["turn"]["id"].as_str().unwrap_or("").to_owned()
            }
            Ok(Err(e)) => {
                *handles.turn_ctx.lock().await = None;
                break Err(format!("Codex turn/start failed: {e}"));
            }
            Err(_) => {
                *handles.turn_ctx.lock().await = None;
                break Err("Codex turn/start timed out.".to_string());
            }
        };

        let mut cancel_sent_at: Option<std::time::Instant> = None;
        let mut steer_task: Option<tokio::task::JoinHandle<()>> = None;
        let turn_outcome: Result<TurnCompletion, String> = loop {
            tokio::select! {
                biased;
                done = &mut done_rx => {
                    break done.map_err(|_| "Codex app-server closed the connection.".to_string());
                }
                maybe_req = req_rx.recv() => {
                    if let Some(sreq) = maybe_req {
                        handle_server_request(
                            &handles.stdin,
                            &event_tx,
                            &mut approval_rx,
                            approval_policy,
                            &mut bash_allowlist,
                            &mut allowed_tools,
                            effective_plan_mode,
                            &cancel,
                            &mut auto_approve,
                            sreq,
                        )
                        .await;
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(150)) => {
                    if !handles.alive.load(Ordering::SeqCst) {
                        break Err("Codex app-server exited unexpectedly.".to_string());
                    }
                    if let Some(sent_at) = cancel_sent_at {
                        if sent_at.elapsed() >= CANCEL_WAIT_LIMIT {
                            handles.alive.store(false, Ordering::SeqCst);
                            break Err("Codex did not respond to interruption.".to_string());
                        }
                    } else if cancel.load(Ordering::SeqCst) {
                        cancel_sent_at = Some(std::time::Instant::now());
                        // Fire and forget: awaiting the ack here would stop this loop from
                        // seeing the turn end or the CANCEL_WAIT_LIMIT deadline.
                        let h = handles.clone();
                        let params = json!({ "threadId": handles.thread_id, "turnId": turn_id });
                        tokio::spawn(async move {
                            let _ = codex_request_within(
                                &h.stdin,
                                &h.next_id,
                                &h.pending,
                                "turn/interrupt",
                                params,
                                CANCEL_WAIT_LIMIT,
                            )
                            .await;
                        });
                    } else if steer_task.as_ref().is_none_or(|t| t.is_finished())
                        && let Some(inbox) = steer.as_ref()
                    {
                        // One task at a time keeps steers in order; the loop stays free to
                        // answer approvals and notice the turn ending meanwhile.
                        let msgs = inbox.take();
                        if !msgs.is_empty() {
                            let h = handles.clone();
                            let turn = turn_id.clone();
                            let tx = event_tx.clone();
                            steer_task = Some(tokio::spawn(async move {
                                for msg in msgs {
                                    steer_turn(&h, &turn, &msg, &tx).await;
                                }
                            }));
                        }
                    }
                }
            }
        };
        // A steer still in flight reports Steered/SteerReturned before the turn is reported
        // finished, so a message sent at the very end is handed back instead of lost.
        if let Some(task) = steer_task.take() {
            let _ = task.await;
        }

        // Record per-turn token usage delta and clear turn context.
        {
            let mut slot = handles.turn_ctx.lock().await;
            if let Some(ctx) = slot.take() {
                let delta = usage_delta(&ctx.latest_total_usage, &ctx.baseline_usage);
                if !delta.is_zero() {
                    let _ = event_tx.send(AgentEvent::ExternalUsage(delta));
                }
                if !ctx.latest_total_usage.is_zero() {
                    *handles
                        .last_total_usage
                        .lock()
                        .unwrap_or_else(|e| e.into_inner()) = ctx.latest_total_usage;
                }
            }
        }

        let completion = match turn_outcome {
            Ok(c) => c,
            Err(e) => break Err(e),
        };
        if completion.status == "failed" {
            break Err(completion
                .error
                .unwrap_or_else(|| "Codex turn failed.".to_string()));
        }
        if completion.status == "interrupted" || cancel.load(Ordering::SeqCst) {
            break Ok(());
        }

        // Evidence follow-up check inside the same reply.
        if evidence_enabled()
            && followups < crate::evidence::MAX_FOLLOWUPS
            && !cancel.load(Ordering::SeqCst)
        {
            let decision = evidence_policy.inspect(&handles.evidence_dir, &evidence_turn);
            let problems = decision.problems;
            if !problems.is_empty() {
                followups += 1;
                let _ = event_tx.send(AgentEvent::TextDelta(format!(
                    "\n\n*oxi asked the agent for the missing evidence ({}).*\n\n",
                    problems.len()
                )));
                let followup_text = format!(
                    "{}\n\n{}",
                    crate::evidence::PROMPT_PREFIX,
                    crate::evidence::followup_prompt(&problems)
                );
                input_items = vec![json!({
                    "type": "text",
                    "text": followup_text,
                    "text_elements": [],
                })];
                continue;
            }
        }
        break Ok(());
    };

    handles.activity.touch();
    if evidence_ready {
        let mut policy = evidence_policy.clone();
        if effective_plan_mode {
            policy.mode = false.into();
        }
        let report = policy.inspect(&handles.evidence_dir, &evidence_turn).report;
        if report.requested || !report.is_empty() {
            let _ = event_tx.send(AgentEvent::Evidence(Box::new(report)));
        }
    }
    let _ = reply.send(final_result);
}

async fn steer_turn(
    handles: &CodexHandles,
    turn_id: &str,
    text: &str,
    event_tx: &StdSender<AgentEvent>,
) {
    let params = json!({
        "threadId": handles.thread_id,
        "expectedTurnId": turn_id,
        "input": [{ "type": "text", "text": text, "text_elements": [] }],
    });
    let sent = codex_request_within(
        &handles.stdin,
        &handles.next_id,
        &handles.pending,
        "turn/steer",
        params,
        Duration::from_secs(15),
    )
    .await;
    let event = match sent {
        Ok(_) => AgentEvent::Steered(text.to_owned()),
        other => {
            log::info!("codex turn/steer not taken: {other:?}");
            AgentEvent::SteerReturned
        }
    };
    let _ = event_tx.send(event);
}

/// `turn/start.collaborationMode`: Codex's own Plan mode while oxi's Plan mode is on, and an
/// explicit switch back to the default mode on the first turn after it (the mode sticks to the
/// thread). `None` leaves the thread's mode alone. The mode takes precedence over the turn's
/// model and effort, so both are repeated in its settings.
pub(super) fn collaboration_mode(
    plan: bool,
    was_plan: bool,
    model_id: &str,
    effort: &str,
) -> Option<Value> {
    if (!plan && !was_plan) || model_id.is_empty() {
        return None;
    }
    let effort = Some(effort).filter(|e| !e.is_empty() && *e != "default");
    Some(json!({
        "mode": if plan { "plan" } else { "default" },
        "settings": {
            "model": model_id,
            "reasoning_effort": effort,
            "developer_instructions": null,
        },
    }))
}

fn turn_policies(
    plan_mode: bool,
    mode: &str,
    cwd: &Path,
    approval_policy: ApprovalPolicy,
) -> (&'static str, Value) {
    if plan_mode {
        return (
            "untrusted",
            json!({ "type": "readOnly", "networkAccess": false }),
        );
    }
    let ask = if approval_policy.bash || approval_policy.write_edit {
        "untrusted"
    } else {
        "never"
    };
    let sandbox = if mode == MODE_FULL_ACCESS {
        json!({ "type": "dangerFullAccess" })
    } else {
        json!({
            "type": "workspaceWrite",
            "writableRoots": [cwd.to_string_lossy()],
            "networkAccess": true,
            "excludeTmpdirEnvVar": false,
            "excludeSlashTmp": false,
        })
    };
    (ask, sandbox)
}

/// What oxi answers to a Codex approval request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Grant {
    Accept,
    Decline,
    Cancel,
}

impl Grant {
    /// Command / file-change decision. Never `acceptForSession`: oxi's "Allow for this turn"
    /// is enforced by `auto_approve`, which resets when the turn ends.
    pub(super) fn wire(self) -> &'static str {
        match self {
            Grant::Accept => "accept",
            Grant::Decline => "decline",
            Grant::Cancel => "cancel",
        }
    }
}

/// Apply the user's choice to the turn's approval state and reduce it to a [`Grant`].
pub(super) fn apply_decision(
    decision: Option<ApprovalDecision>,
    auto_approve: &mut bool,
    bash_allowlist: &mut Vec<String>,
    allowed_tools: &mut Vec<String>,
) -> Grant {
    match decision {
        Some(ApprovalDecision::Approve) => Grant::Accept,
        Some(ApprovalDecision::ApproveRest) => {
            *auto_approve = true;
            Grant::Accept
        }
        Some(ApprovalDecision::AllowPrefix(prefix)) => {
            bash_allowlist.push(prefix);
            Grant::Accept
        }
        Some(ApprovalDecision::AllowTool(tool)) => {
            allowed_tools.push(tool);
            Grant::Accept
        }
        Some(ApprovalDecision::Deny | ApprovalDecision::Elicitation(_)) => Grant::Decline,
        None => Grant::Cancel,
    }
}

/// `item/permissions/requestApproval` result: always turn-scoped, empty grant when refused.
pub(super) fn permissions_result(grant: Grant, requested: &Value) -> Value {
    match grant {
        Grant::Accept => json!({ "permissions": requested, "scope": "turn" }),
        Grant::Decline | Grant::Cancel => json!({ "permissions": {}, "scope": "turn" }),
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_server_request(
    stdin: &Arc<AsyncMutex<ChildStdin>>,
    event_tx: &StdSender<AgentEvent>,
    approval_rx: &mut StdReceiver<ApprovalDecision>,
    approval_policy: ApprovalPolicy,
    bash_allowlist: &mut Vec<String>,
    allowed_tools: &mut Vec<String>,
    plan_mode: bool,
    cancel: &Arc<AtomicBool>,
    auto_approve: &mut bool,
    req: CodexServerReq,
) {
    match req.method.as_str() {
        "item/commandExecution/requestApproval" | "execCommandApproval" => {
            let command = req.params["command"]
                .as_str()
                .map(str::to_owned)
                .or_else(|| {
                    req.params["command"].as_array().map(|arr| {
                        arr.iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                })
                .unwrap_or_default();
            let args = json!({
                "command": command,
                "cwd": req.params["cwd"],
                "reason": req.params["reason"],
            });
            let decision =
                if plan_mode && crate::agent::approval::bash_command_is_read_only(&command) {
                    Some(ApprovalDecision::Approve)
                } else if plan_mode {
                    Some(ApprovalDecision::Deny)
                } else if *auto_approve
                    || !approval_policy.requires_approval("bash")
                    || crate::agent::approval::tool_allowed("bash", allowed_tools)
                    || crate::agent::approval::bash_command_allowlisted(&command, bash_allowlist)
                {
                    Some(ApprovalDecision::Approve)
                } else {
                    let _ = event_tx.send(AgentEvent::ApprovalRequest {
                        name: "bash".into(),
                        args: Some(args),
                    });
                    let d = wait_decision(approval_rx, cancel, None, None).await;
                    let _ = event_tx.send(AgentEvent::ApprovalResolved);
                    d
                };
            let wire_decision =
                apply_decision(decision, auto_approve, bash_allowlist, allowed_tools).wire();
            let _ = write_line(
                stdin,
                &json!({ "jsonrpc": "2.0", "id": req.id, "result": { "decision": wire_decision } }),
            )
            .await;
        }
        "item/fileChange/requestApproval" | "applyPatchApproval" => {
            let args = json!({
                "reason": req.params["reason"],
                "grantRoot": req.params["grantRoot"],
            });
            let decision = if plan_mode {
                Some(ApprovalDecision::Deny)
            } else if *auto_approve
                || !approval_policy.requires_approval("edit")
                || crate::agent::approval::tool_allowed("edit", allowed_tools)
            {
                Some(ApprovalDecision::Approve)
            } else {
                let _ = event_tx.send(AgentEvent::ApprovalRequest {
                    name: "edit".into(),
                    args: Some(args),
                });
                let d = wait_decision(approval_rx, cancel, None, None).await;
                let _ = event_tx.send(AgentEvent::ApprovalResolved);
                d
            };
            let wire_decision =
                apply_decision(decision, auto_approve, bash_allowlist, allowed_tools).wire();
            let _ = write_line(
                stdin,
                &json!({ "jsonrpc": "2.0", "id": req.id, "result": { "decision": wire_decision } }),
            )
            .await;
        }
        "item/permissions/requestApproval" => {
            let decision = if plan_mode {
                Some(ApprovalDecision::Deny)
            } else if *auto_approve || (!approval_policy.bash && !approval_policy.write_edit) {
                Some(ApprovalDecision::Approve)
            } else {
                let _ = event_tx.send(AgentEvent::ApprovalRequest {
                    name: "permissions".into(),
                    args: Some(req.params.clone()),
                });
                let d = wait_decision(approval_rx, cancel, None, None).await;
                let _ = event_tx.send(AgentEvent::ApprovalResolved);
                d
            };
            // Allow-prefix / allow-tool grants don't map onto a permissions profile, so they
            // only grant this request.
            let grant = match decision {
                Some(ApprovalDecision::AllowPrefix(_) | ApprovalDecision::AllowTool(_)) => {
                    Grant::Accept
                }
                other => apply_decision(other, auto_approve, bash_allowlist, allowed_tools),
            };
            let result = permissions_result(grant, &req.params["permissions"]);
            let _ = write_line(
                stdin,
                &json!({ "jsonrpc": "2.0", "id": req.id, "result": result }),
            )
            .await;
        }
        "item/tool/requestUserInput" => {
            let questions = req.params["questions"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let elicit_params = codex_questions_to_elicitation(&questions);
            let _ = event_tx.send(AgentEvent::ElicitationRequest {
                params: elicit_params,
            });
            let decision = wait_decision(approval_rx, cancel, None, None).await;
            let _ = event_tx.send(AgentEvent::ApprovalResolved);
            let answers = elicitation_to_codex_answers(&questions, decision);
            let _ = write_line(
                stdin,
                &json!({ "jsonrpc": "2.0", "id": req.id, "result": { "answers": answers } }),
            )
            .await;
        }
        "mcpServer/elicitation/request" => {
            let mut elicit_params = req.params.clone();
            if elicit_params.get("mode").is_none() {
                elicit_params["mode"] = json!("form");
            }
            let _ = event_tx.send(AgentEvent::ElicitationRequest {
                params: elicit_params,
            });
            let decision = wait_decision(approval_rx, cancel, None, None).await;
            let _ = event_tx.send(AgentEvent::ApprovalResolved);
            let response = crate::agent::elicitation::response(decision);
            let _ = write_line(
                stdin,
                &json!({ "jsonrpc": "2.0", "id": req.id, "result": response }),
            )
            .await;
        }
        other => {
            let _ = write_line(
                stdin,
                &json!({
                    "jsonrpc": "2.0",
                    "id": req.id,
                    "error": { "code": -32601, "message": format!("Unsupported client method: {other}") }
                }),
            )
            .await;
        }
    }
}

/// Convert Codex `ToolRequestUserInputQuestion[]` to oxi's `elicitation/create` form shape.
pub(super) fn codex_questions_to_elicitation(questions: &[Value]) -> Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    for (idx, q) in questions.iter().enumerate() {
        let key = format!("question_{idx}");
        let header = q["header"].as_str().unwrap_or("");
        let prompt = q["question"].as_str().unwrap_or("");
        let title = if header.is_empty() {
            prompt.to_owned()
        } else {
            format!("{header}: {prompt}")
        };
        if let Some(opts) = q["options"].as_array().filter(|a| !a.is_empty()) {
            let one_of: Vec<Value> = opts
                .iter()
                .filter_map(|o| {
                    let label = o["label"].as_str()?;
                    Some(json!({
                        "const": label,
                        "title": label,
                        "description": o["description"].as_str().unwrap_or(""),
                    }))
                })
                .collect();
            properties.insert(
                key.clone(),
                json!({
                    "type": "string",
                    "title": title,
                    "oneOf": one_of,
                }),
            );
            if q["isOther"].as_bool() == Some(true) {
                properties.insert(
                    format!("{key}_custom"),
                    json!({
                        "type": "string",
                        "title": "Other (optional)",
                    }),
                );
            }
        } else {
            properties.insert(
                key.clone(),
                json!({
                    "type": "string",
                    "title": title,
                }),
            );
            required.push(json!(key));
        }
    }
    json!({
        "mode": "form",
        "message": "Codex has a question for you:",
        "requestedSchema": {
            "type": "object",
            "properties": properties,
            "required": required,
        }
    })
}

/// Convert oxi's `ApprovalDecision::Elicitation` back to Codex's `ToolRequestUserInputResponse.answers`.
pub(super) fn elicitation_to_codex_answers(
    questions: &[Value],
    decision: Option<ApprovalDecision>,
) -> Value {
    let mut out = serde_json::Map::new();
    let resp = crate::agent::elicitation::response(decision);
    if resp["action"].as_str() != Some("accept") {
        return Value::Object(out);
    }
    let Some(content) = resp["content"].as_object() else {
        return Value::Object(out);
    };
    for (idx, q) in questions.iter().enumerate() {
        let Some(qid) = q["id"].as_str() else {
            continue;
        };
        let key = format!("question_{idx}");
        let custom_key = format!("{key}_custom");
        let answer = content
            .get(&custom_key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .or_else(|| content.get(&key).and_then(Value::as_str).map(str::trim))
            .filter(|s| !s.is_empty());
        if let Some(val) = answer {
            out.insert(qid.to_owned(), json!({ "answers": [val] }));
        }
    }
    Value::Object(out)
}

async fn read_loop(
    stdout: tokio::process::ChildStdout,
    pending: Pending,
    turn_ctx: Arc<AsyncMutex<Option<CodexTurnCtx>>>,
    alive: Arc<AtomicBool>,
    activity: Arc<reaper::Activity>,
    session_key: String,
) {
    let mut lines = BufReader::new(stdout).lines();
    let mut chunks = String::new();
    let mut chunk_count = 0usize;
    let flush_chunks = |chunks: &mut String, count: &mut usize| {
        if *count > 0 {
            activity_log::log(
                ActivityKind::Acp,
                format!("← codex · {count} streamed deltas"),
                &*chunks,
            );
            chunks.clear();
            *count = 0;
        }
    };

    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if activity_log::is_enabled() {
            let method = msg["method"].as_str().unwrap_or("");
            if method.ends_with("Delta") || method.ends_with("/delta") {
                chunks.push_str(&line);
                chunks.push('\n');
                chunk_count += 1;
            } else {
                flush_chunks(&mut chunks, &mut chunk_count);
                activity_log::log_json(
                    ActivityKind::Acp,
                    format!("← codex {}", activity_log::rpc_title(&msg)),
                    &msg,
                );
            }
        }
        activity.touch();

        // Response to a client→server request (has id, no method).
        if msg.get("method").is_none()
            && let Some(id) = msg.get("id").and_then(Value::as_i64)
        {
            let res = if let Some(err) = msg.get("error") {
                Err(err["message"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| err.to_string()))
            } else {
                Ok(msg.get("result").cloned().unwrap_or(Value::Null))
            };
            if let Some(tx) = pending.lock().await.remove(&id) {
                let _ = tx.send(res);
            }
            continue;
        }

        let Some(method) = msg["method"].as_str() else {
            continue;
        };
        let params = msg.get("params").cloned().unwrap_or(Value::Null);

        // Server→client request (has id AND method).
        if let Some(id) = msg.get("id").filter(|id| !id.is_null()).cloned() {
            let ctx = turn_ctx.lock().await;
            if let Some(ctx) = ctx.as_ref() {
                let _ = ctx.req_tx.send(CodexServerReq {
                    id,
                    method: method.to_owned(),
                    params,
                });
            }
            continue;
        }

        // Server→client notification.
        if method == "thread/name/updated"
            && let Some(name) = params["threadName"].as_str()
            && session_info::note_title(&session_key, name)
        {
            autonomous::wake();
        }
        let mut ctx = turn_ctx.lock().await;
        if let Some(ctx) = ctx.as_mut() {
            ctx.handle_notification(method, &params);
        }
    }

    flush_chunks(&mut chunks, &mut chunk_count);
    alive.store(false, Ordering::SeqCst);
    let mut p = pending.lock().await;
    for (_, tx) in p.drain() {
        let _ = tx.send(Err("Codex app-server closed the connection.".to_string()));
    }
    drop(p);
    if let Some(ctx) = turn_ctx.lock().await.as_mut()
        && let Some(done) = ctx.done_tx.take()
    {
        let _ = done.send(TurnCompletion {
            status: "failed".into(),
            error: Some("Codex app-server closed the connection.".into()),
        });
    }
}

async fn codex_request(
    stdin: &Arc<AsyncMutex<ChildStdin>>,
    next_id: &Arc<AtomicI64>,
    pending: &Pending,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    let (_, rx) = send_codex_request(stdin, next_id, pending, method, params).await?;
    match rx.await {
        Ok(r) => r,
        Err(_) => Err("Codex app-server closed the connection.".to_string()),
    }
}

/// [`codex_request`] bounded by `limit`; on timeout the pending slot is dropped so an
/// unanswered request does not keep the connection looking busy to the idle reaper.
async fn codex_request_within(
    stdin: &Arc<AsyncMutex<ChildStdin>>,
    next_id: &Arc<AtomicI64>,
    pending: &Pending,
    method: &str,
    params: Value,
    limit: Duration,
) -> Result<Value, String> {
    let (id, rx) = send_codex_request(stdin, next_id, pending, method, params).await?;
    match tokio::time::timeout(limit, rx).await {
        Ok(Ok(r)) => r,
        Ok(Err(_)) => Err("Codex app-server closed the connection.".to_string()),
        Err(_) => {
            pending.lock().await.remove(&id);
            Err(format!("Codex app-server did not answer {method} in time."))
        }
    }
}

async fn send_codex_request(
    stdin: &Arc<AsyncMutex<ChildStdin>>,
    next_id: &Arc<AtomicI64>,
    pending: &Pending,
    method: &str,
    params: Value,
) -> Result<(i64, oneshot::Receiver<Result<Value, String>>), String> {
    let id = next_id.fetch_add(1, Ordering::SeqCst);
    let (tx, rx) = oneshot::channel();
    pending.lock().await.insert(id, tx);
    if let Err(e) = write_line(
        stdin,
        &json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }),
    )
    .await
    {
        pending.lock().await.remove(&id);
        return Err(e);
    }
    Ok((id, rx))
}

async fn spawn_codex_process(
    command_line: &str,
    cwd: &Path,
    env: &[(String, String)],
    mcp_servers: &[McpServerConfig],
    evidence_dir: Option<&Path>,
    delegation_dir: Option<&Path>,
) -> Result<
    (
        Child,
        ChildStdin,
        tokio::process::ChildStdout,
        tokio::process::ChildStderr,
    ),
    String,
> {
    let words = split_shell_words(command_line);
    let program_word = words.first().map(String::as_str).unwrap_or("codex");
    let bin = resolve_cli_binary(program_word, &[".local/bin", ".codex/bin"], None)
        .await
        .ok_or_else(|| {
            format!(
                "Could not find `{program_word}` in PATH. Install OpenAI Codex CLI (`npm i -g @openai/codex` or `brew install codex`) and sign in with `codex login`."
            )
        })?;
    #[cfg(windows)]
    let npm_native = npm_native_codex_exe(&bin);
    #[cfg(windows)]
    let bin = npm_native.clone().unwrap_or(bin);
    let extra_args: &[String] = words.get(1..).unwrap_or(&[]);
    let mut args = build_codex_args(extra_args, mcp_servers, evidence_dir, delegation_dir);
    let mut cmd = Command::new(&bin);
    cmd.args(&mut args);
    cmd.current_dir(cwd);
    crate::shell_env::apply_async(&mut cmd).await;
    // Mirror what npm's `codex.js` wrapper does: put the bundled `rg` on PATH.
    #[cfg(windows)]
    if let Some(exe) = &npm_native {
        if let Some(tools) = exe.parent().and_then(Path::parent).map(|d| d.join("path")) {
            let current = cmd
                .as_std()
                .get_envs()
                .find(|(k, _)| k.eq_ignore_ascii_case("PATH"))
                .and_then(|(_, v)| v.map(ToOwned::to_owned))
                .or_else(|| std::env::var_os("PATH"))
                .unwrap_or_default();
            let joined =
                std::env::join_paths(std::iter::once(tools).chain(std::env::split_paths(&current)));
            if let Ok(joined) = joined {
                cmd.env("PATH", joined);
            }
        }
        cmd.env("CODEX_MANAGED_BY_NPM", "1");
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Could not launch Codex app-server `{}`: {e}", bin.display()))?;
    let stdin = child.stdin.take().ok_or("Codex: child has no stdin")?;
    let stdout = child.stdout.take().ok_or("Codex: child has no stdout")?;
    let stderr = child.stderr.take().ok_or("Codex: child has no stderr")?;
    Ok((child, stdin, stdout, stderr))
}

/// For an npm-installed Codex (`%APPDATA%\npm\codex.cmd`), the native `codex.exe` shipped
/// inside the package. Spawning it directly avoids the `cmd.exe` → `node` → `codex.exe` chain,
/// whose grandchild would survive `kill_on_drop` and whose batch quoting mangles `-c` overrides.
#[cfg(windows)]
fn npm_native_codex_exe(shim: &Path) -> Option<std::path::PathBuf> {
    let is_shim = shim
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"));
    if !is_shim {
        return None;
    }
    let pkg = shim
        .parent()?
        .join("node_modules")
        .join("@openai")
        .join("codex");
    find_file_named(&pkg, "codex.exe", 7)
}

#[cfg(windows)]
fn find_file_named(dir: &Path, name: &str, depth: usize) -> Option<std::path::PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut subdirs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_file()
            && entry
                .file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case(name)
        {
            return Some(path);
        }
        if ft.is_dir() && depth > 0 {
            subdirs.push(path);
        }
    }
    subdirs
        .into_iter()
        .find_map(|d| find_file_named(&d, name, depth - 1))
}

/// Build CLI arguments for `codex app-server --stdio`, injecting oxi's MCP server and any
/// user-configured MCP servers via `-c mcp_servers.<name>.*` overrides.
pub(super) fn build_codex_args(
    extra_args: &[String],
    mcp_servers: &[McpServerConfig],
    evidence_dir: Option<&Path>,
    delegation_dir: Option<&Path>,
) -> Vec<String> {
    let mut args = Vec::new();
    let has_app_server = extra_args.iter().any(|a| a == "app-server");
    let has_stdio = extra_args
        .iter()
        .any(|a| a == "--stdio" || a.starts_with("--listen"));
    if !has_app_server {
        args.push("app-server".into());
    }
    args.extend(extra_args.iter().cloned());
    if !has_stdio {
        args.push("--stdio".into());
    }
    if let Some(server) = oxi_mcp::server_spec(evidence_dir, delegation_dir) {
        push_codex_stdio_mcp(&mut args, "oxi", &server.command, &server.args, &server.env);
    }
    for s in mcp_servers::usable(mcp_servers) {
        let name = s.name.trim();
        if name.is_empty() || name == "oxi" {
            continue;
        }
        match s.transport {
            McpTransport::Stdio => {
                let (command, server_args) = if cfg!(windows) {
                    let mut wrapped = vec!["/D".into(), "/S".into(), "/C".into()];
                    wrapped.push(s.command.trim().to_owned());
                    wrapped.extend(s.args.iter().cloned());
                    ("cmd".to_owned(), wrapped)
                } else {
                    (s.command.trim().to_owned(), s.args.clone())
                };
                push_codex_stdio_mcp(&mut args, name, &command, &server_args, &s.env_pairs());
            }
            McpTransport::Http => {
                let key = toml_key_segment(name);
                args.push("-c".into());
                args.push(format!(
                    "mcp_servers.{key}.url={}",
                    toml_string(s.url.trim())
                ));
                let token = s.bearer_token.trim();
                if !token.is_empty() {
                    args.push("-c".into());
                    args.push(format!(
                        "mcp_servers.{key}.http_headers={{Authorization={}}}",
                        toml_string(&format!("Bearer {token}"))
                    ));
                }
            }
        }
    }
    args
}

fn push_codex_stdio_mcp(
    out: &mut Vec<String>,
    name: &str,
    command: &str,
    server_args: &[String],
    env_pairs: &[(String, String)],
) {
    let key = toml_key_segment(name);
    out.push("-c".into());
    out.push(format!(
        "mcp_servers.{key}.command={}",
        toml_string(command)
    ));
    let arr = server_args
        .iter()
        .map(|a| toml_string(a))
        .collect::<Vec<_>>()
        .join(",");
    out.push("-c".into());
    out.push(format!("mcp_servers.{key}.args=[{arr}]"));
    if !env_pairs.is_empty() {
        let map = env_pairs
            .iter()
            .map(|(k, v)| format!("{}={}", toml_key_segment(k), toml_string(v)))
            .collect::<Vec<_>>()
            .join(",");
        out.push("-c".into());
        out.push(format!("mcp_servers.{key}.env={{{map}}}"));
    }
}

fn toml_key_segment(s: &str) -> String {
    if !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        s.to_owned()
    } else {
        toml_string(s)
    }
}

fn toml_string(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

fn build_user_inputs(
    text: &str,
    images: &[(String, Vec<u8>)],
    resources: &[PathBuf],
    evidence_dir: &Path,
) -> Result<Vec<Value>, String> {
    let mut items = Vec::new();
    if !text.trim().is_empty() {
        items.push(json!({ "type": "text", "text": text, "text_elements": [] }));
    }
    for path in resources {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        items.push(json!({
            "type": "mention",
            "name": name,
            "path": path.to_string_lossy(),
        }));
    }
    for (idx, (mime, data)) in images.iter().enumerate() {
        let ext = match mime.as_str() {
            "image/jpeg" | "image/jpg" => "jpg",
            "image/gif" => "gif",
            "image/webp" => "webp",
            _ => "png",
        };
        let path = evidence_dir.join(format!(
            "input-image-{}-{idx}.{ext}",
            crate::evidence::now_ms()
        ));
        crate::fsutil::write_atomic(&path, data)
            .map_err(|e| format!("Could not write image attachment for Codex: {e}"))?;
        items.push(json!({
            "type": "localImage",
            "path": path.to_string_lossy(),
        }));
    }
    if items.is_empty() {
        items.push(json!({ "type": "text", "text": "", "text_elements": [] }));
    }
    Ok(items)
}

fn parse_models(res: &Value) -> Vec<CodexModel> {
    res["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let id = m["id"].as_str().or_else(|| m["model"].as_str())?.to_owned();
            let display_name = m["displayName"]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or(&id)
                .to_owned();
            let description = m["description"].as_str().unwrap_or("").to_owned();
            let efforts: Vec<(String, String)> = m["supportedReasoningEfforts"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|e| {
                    let level = e["reasoningEffort"].as_str()?.to_owned();
                    let desc = e["description"].as_str().unwrap_or("").to_owned();
                    Some((level, desc))
                })
                .collect();
            let default_effort = m["defaultReasoningEffort"]
                .as_str()
                .map(str::to_owned)
                .or_else(|| efforts.first().map(|(e, _)| e.clone()))
                .unwrap_or_else(|| "high".into());
            let supports_fast = m["serviceTiers"]
                .as_array()
                .is_some_and(|arr| arr.iter().any(|t| t["id"].as_str() == Some("priority")))
                || m["additionalSpeedTiers"]
                    .as_array()
                    .is_some_and(|arr| arr.iter().any(|t| t.as_str() == Some("fast")));
            let supports_images = m["inputModalities"]
                .as_array()
                .is_none_or(|arr| arr.iter().any(|v| v.as_str() == Some("image")));
            Some(CodexModel {
                id,
                display_name,
                description,
                efforts,
                default_effort,
                supports_fast,
                supports_images,
                is_default: m["isDefault"].as_bool() == Some(true),
                hidden: m["hidden"].as_bool() == Some(true),
            })
        })
        .collect()
}

fn parse_skills_as_commands(res: &Value) -> Vec<AcpSlashCommand> {
    let mut out = vec![
        AcpSlashCommand {
            name: "compact".into(),
            description: "Compact the current thread context".into(),
            hint: None,
        },
        AcpSlashCommand {
            name: "review".into(),
            description: "Review local changes".into(),
            hint: Some("optional instructions".into()),
        },
    ];
    for group in res["data"].as_array().into_iter().flatten() {
        for skill in group["skills"].as_array().into_iter().flatten() {
            if let Some(name) = skill["name"].as_str().map(str::trim)
                && !name.is_empty()
                && !name.contains(char::is_whitespace)
            {
                let description = skill["interface"]["shortDescription"]
                    .as_str()
                    .or_else(|| skill["description"].as_str())
                    .unwrap_or("")
                    .trim()
                    .to_owned();
                out.push(AcpSlashCommand {
                    name: name.to_owned(),
                    description,
                    hint: None,
                });
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out.dedup_by(|a, b| a.name == b.name);
    out
}

fn parse_codex_usage(v: &Value) -> TokenUsage {
    let total_input = v["inputTokens"].as_u64().unwrap_or(0);
    let cached = v["cachedInputTokens"].as_u64().unwrap_or(0);
    let uncached_input = total_input.saturating_sub(cached);
    let output = v["outputTokens"].as_u64().unwrap_or(0);
    TokenUsage {
        input_tokens: uncached_input,
        output_tokens: output,
        cache_read_input_tokens: cached,
        ..TokenUsage::default()
    }
}

fn usage_delta(now: &TokenUsage, before: &TokenUsage) -> TokenUsage {
    TokenUsage {
        input_tokens: now.input_tokens.saturating_sub(before.input_tokens),
        output_tokens: now.output_tokens.saturating_sub(before.output_tokens),
        cache_read_input_tokens: now
            .cache_read_input_tokens
            .saturating_sub(before.cache_read_input_tokens),
        cache_creation_input_tokens: now
            .cache_creation_input_tokens
            .saturating_sub(before.cache_creation_input_tokens),
        ..TokenUsage::default()
    }
}

fn map_codex_status(status: Option<&str>, completed: bool) -> ToolStatus {
    match status {
        Some("completed") => ToolStatus::Completed,
        Some("failed" | "declined") => ToolStatus::Failed,
        Some("inProgress" | "in_progress") => ToolStatus::InProgress,
        _ if completed => ToolStatus::Completed,
        _ => ToolStatus::InProgress,
    }
}

fn reasoning_item_text(item: &Value) -> String {
    let summary: Vec<&str> = item["summary"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .collect();
    if !summary.is_empty() {
        return summary.join("\n\n");
    }
    item["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join("\n")
}

fn extract_file_changes(changes: &Value) -> (String, Option<String>, Vec<ToolLocation>) {
    let mut paths = Vec::new();
    let mut locations = Vec::new();
    let mut diff_out = String::new();
    for ch in changes.as_array().into_iter().flatten() {
        let path = ch["path"].as_str().unwrap_or("");
        if !path.is_empty() {
            paths.push(path.to_owned());
            locations.push(ToolLocation {
                path: path.to_owned(),
                line: None,
            });
        }
        if let Some(d) = ch["diff"].as_str()
            && !d.is_empty()
        {
            if !diff_out.is_empty() && !diff_out.ends_with('\n') {
                diff_out.push('\n');
            }
            diff_out.push_str(d);
        }
    }
    let title = match paths.len() {
        0 => "Edit files".into(),
        1 => paths[0].clone(),
        n => format!("{} (+{} more)", paths[0], n - 1),
    };
    (title, (!diff_out.is_empty()).then_some(diff_out), locations)
}

fn extract_mcp_result(item: &Value) -> String {
    if let Some(err) = item["error"]["message"].as_str() {
        return err.to_owned();
    }
    let result = &item["result"];
    if result.is_null() {
        return String::new();
    }
    let texts: Vec<&str> = result["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c["text"].as_str())
        .collect();
    if !texts.is_empty() {
        return texts.join("\n");
    }
    if !result["structuredContent"].is_null() {
        return serde_json::to_string_pretty(&result["structuredContent"]).unwrap_or_default();
    }
    String::new()
}

fn format_turn_error(err: &Value) -> Option<String> {
    if err.is_null() {
        return None;
    }
    let msg = err["message"].as_str().unwrap_or("").trim();
    let extra = err["additionalDetails"].as_str().unwrap_or("").trim();
    let info = err["codexErrorInfo"].as_str().unwrap_or("");
    let base = if !msg.is_empty() && !extra.is_empty() && !msg.contains(extra) {
        format!("{msg} ({extra})")
    } else if !msg.is_empty() {
        msg.to_owned()
    } else if !extra.is_empty() {
        extra.to_owned()
    } else {
        return None;
    };
    if info == "usageLimitExceeded" && !crate::router::quota::is_quota_error(&base) {
        Some(format!("Usage limit exceeded: {base}"))
    } else {
        Some(base)
    }
}

fn with_stderr_tail(base: String, stderr_tail: &Mutex<VecDeque<String>>) -> String {
    let tail = stderr_tail.lock().unwrap_or_else(|e| e.into_inner());
    if tail.is_empty() {
        return base;
    }
    let lines: Vec<&str> = tail.iter().map(String::as_str).collect();
    format!("{base}\n{}", lines.join("\n"))
}

// ── External session listing & import (`thread/list` + `thread/read`) ────────

const MAX_IMPORT_PAGES: usize = 5;

pub(super) async fn list_sessions(req: &AcpImport) -> Result<Vec<ExternalSession>, String> {
    let (mut child, stdin, stdout, stderr) =
        spawn_codex_process(&req.command_line, &req.cwd, &req.env, &[], None, None).await?;
    let stdin = Arc::new(AsyncMutex::new(stdin));
    let pending: Pending = Arc::new(AsyncMutex::new(HashMap::new()));
    let turn_ctx: Arc<AsyncMutex<Option<CodexTurnCtx>>> = Arc::new(AsyncMutex::new(None));
    let next_id = Arc::new(AtomicI64::new(1));
    let alive = Arc::new(AtomicBool::new(true));
    let activity = Arc::new(reaper::Activity::new());
    let stderr_tail = Arc::new(Mutex::new(VecDeque::new()));
    let _stderr = tokio::spawn(drain_stderr(stderr, stderr_tail));
    let _reader = tokio::spawn(read_loop(
        stdout,
        pending.clone(),
        turn_ctx,
        alive,
        activity,
        String::new(),
    ));

    let res = async {
        codex_request(
            &stdin,
            &next_id,
            &pending,
            "initialize",
            json!({
                "clientInfo": { "name": "oxi", "title": "oxi", "version": env!("CARGO_PKG_VERSION") },
                "capabilities": { "experimentalApi": true, "requestAttestation": false }
            }),
        )
        .await?;
        write_line(
            &stdin,
            &json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }),
        )
        .await?;

        let known = sessions::known_ids();
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_IMPORT_PAGES {
            let mut params = json!({
                "cwd": req.cwd.to_string_lossy(),
                "limit": 50,
            });
            if let Some(c) = &cursor {
                params["cursor"] = json!(c);
            }
            let page = tokio::time::timeout(
                Duration::from_secs(30),
                codex_request(&stdin, &next_id, &pending, "thread/list", params),
            )
            .await
            .map_err(|_| "Codex took too long to list sessions.".to_string())??;
            for t in page["data"].as_array().into_iter().flatten() {
                let Some(id) = t["id"].as_str() else { continue };
                if known.contains(id) {
                    continue;
                }
                let title = t["name"]
                    .as_str()
                    .or_else(|| t["preview"].as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .unwrap_or("Untitled session")
                    .to_owned();
                let updated_at = t["updatedAt"]
                    .as_i64()
                    .and_then(|ts| chrono::DateTime::from_timestamp(ts, 0))
                    .map(|dt| dt.to_rfc3339());
                out.push(ExternalSession {
                    session_id: id.to_owned(),
                    title,
                    updated_at,
                });
            }
            cursor = page["nextCursor"].as_str().map(str::to_owned);
            if cursor.is_none() {
                break;
            }
        }
        Ok(out)
    }
    .await;
    let _ = child.kill().await;
    res
}

pub(super) async fn load_session(
    req: &AcpImport,
    session_id: &str,
) -> Result<Vec<ChatMessage>, String> {
    let (mut child, stdin, stdout, stderr) =
        spawn_codex_process(&req.command_line, &req.cwd, &req.env, &[], None, None).await?;
    let stdin = Arc::new(AsyncMutex::new(stdin));
    let pending: Pending = Arc::new(AsyncMutex::new(HashMap::new()));
    let turn_ctx: Arc<AsyncMutex<Option<CodexTurnCtx>>> = Arc::new(AsyncMutex::new(None));
    let next_id = Arc::new(AtomicI64::new(1));
    let alive = Arc::new(AtomicBool::new(true));
    let activity = Arc::new(reaper::Activity::new());
    let stderr_tail = Arc::new(Mutex::new(VecDeque::new()));
    let _stderr = tokio::spawn(drain_stderr(stderr, stderr_tail));
    let _reader = tokio::spawn(read_loop(
        stdout,
        pending.clone(),
        turn_ctx,
        alive,
        activity,
        String::new(),
    ));

    let res = async {
        codex_request(
            &stdin,
            &next_id,
            &pending,
            "initialize",
            json!({
                "clientInfo": { "name": "oxi", "title": "oxi", "version": env!("CARGO_PKG_VERSION") },
                "capabilities": { "experimentalApi": true, "requestAttestation": false }
            }),
        )
        .await?;
        write_line(
            &stdin,
            &json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }),
        )
        .await?;
        let data = tokio::time::timeout(
            Duration::from_secs(60),
            codex_request(
                &stdin,
                &next_id,
                &pending,
                "thread/read",
                json!({ "threadId": session_id, "includeTurns": true }),
            ),
        )
        .await
        .map_err(|_| "Codex took too long to read the session.".to_string())??;
        let messages = thread_to_messages(&data["thread"]);
        if messages.is_empty() {
            return Err("The Codex thread has no messages to import.".into());
        }
        Ok(messages)
    }
    .await;
    let _ = child.kill().await;
    res
}

/// Convert a Codex `Thread` (with `turns`) into oxi `ChatMessage`s.
pub(super) fn thread_to_messages(thread: &Value) -> Vec<ChatMessage> {
    let mut messages = Vec::new();
    for turn in thread["turns"].as_array().into_iter().flatten() {
        let mut assistant: Option<ChatMessage> = None;
        for item in turn["items"].as_array().into_iter().flatten() {
            match item["type"].as_str() {
                Some("userMessage") => {
                    if let Some(mut a) = assistant.take() {
                        a.finish_streaming();
                        messages.push(a);
                    }
                    let text: String = item["content"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|c| match c["type"].as_str() {
                            Some("text") => c["text"].as_str().map(str::to_owned),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    let clean = without_oxi_preamble(&text).trim().to_owned();
                    if !clean.is_empty() {
                        messages.push(ChatMessage::user(&clean, &[]));
                    }
                }
                Some("agentMessage" | "plan") => {
                    if let Some(text) = item["text"].as_str()
                        && !text.is_empty()
                    {
                        let msg =
                            assistant.get_or_insert_with(|| ChatMessage::streaming_assistant(None));
                        msg.append_answer(text);
                    }
                }
                Some("reasoning") => {
                    let text = reasoning_item_text(item);
                    if !text.is_empty() {
                        let msg =
                            assistant.get_or_insert_with(|| ChatMessage::streaming_assistant(None));
                        msg.append_thinking(&text);
                    }
                }
                Some("commandExecution") => {
                    let msg =
                        assistant.get_or_insert_with(|| ChatMessage::streaming_assistant(None));
                    let command = item["command"].as_str().unwrap_or("");
                    let update = ToolUpdate {
                        tool_call_id: item["id"].as_str().unwrap_or("cmd").to_owned(),
                        name: "bash".into(),
                        args: Some(json!({ "command": command })),
                        output: item["aggregatedOutput"].as_str().unwrap_or("").to_owned(),
                        diff: None,
                        output_truncated: false,
                        metadata: ToolMetadata {
                            title: command.to_owned(),
                            kind: "execute".into(),
                            name: Some("bash".into()),
                            locations: Vec::new(),
                            status: map_codex_status(item["status"].as_str(), true),
                        },
                    };
                    apply_tool_update(&mut msg.blocks, update);
                }
                Some("fileChange") => {
                    let msg =
                        assistant.get_or_insert_with(|| ChatMessage::streaming_assistant(None));
                    let (title, diff, locations) = extract_file_changes(&item["changes"]);
                    let update = ToolUpdate {
                        tool_call_id: item["id"].as_str().unwrap_or("edit").to_owned(),
                        name: "edit".into(),
                        args: None,
                        output: String::new(),
                        diff,
                        output_truncated: false,
                        metadata: ToolMetadata {
                            title,
                            kind: "edit".into(),
                            name: Some("edit".into()),
                            locations,
                            status: map_codex_status(item["status"].as_str(), true),
                        },
                    };
                    apply_tool_update(&mut msg.blocks, update);
                }
                Some("mcpToolCall") => {
                    let msg =
                        assistant.get_or_insert_with(|| ChatMessage::streaming_assistant(None));
                    let server = item["server"].as_str().unwrap_or("");
                    let tool = item["tool"].as_str().unwrap_or("");
                    let full = format!("mcp__{server}__{tool}");
                    let update = ToolUpdate {
                        tool_call_id: item["id"].as_str().unwrap_or("mcp").to_owned(),
                        name: oxi_mcp::oxi_tool_name(&full)
                            .map(str::to_owned)
                            .unwrap_or_else(|| full.clone()),
                        args: Some(item["arguments"].clone()).filter(|v| !v.is_null()),
                        output: extract_mcp_result(item),
                        diff: None,
                        output_truncated: false,
                        metadata: ToolMetadata {
                            title: full,
                            kind: "other".into(),
                            name: Some(tool.to_owned()),
                            locations: Vec::new(),
                            status: map_codex_status(item["status"].as_str(), true),
                        },
                    };
                    apply_tool_update(&mut msg.blocks, update);
                }
                _ => {}
            }
        }
        if let Some(mut a) = assistant {
            a.finish_streaming();
            messages.push(a);
        }
    }
    messages
}
