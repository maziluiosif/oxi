//! Official native Claude Code CLI client over `claude --output-format stream-json --input-format stream-json`.
//!
//! Talks directly to the user's installed `claude` CLI instead of an npm ACP bridge:
//! - Control handshake (`control_request` / `control_response` `initialize`) for live models,
//!   effort levels, slash commands, fast mode, and permission mode.
//! - Live `set_model`, `set_permission_mode`, `apply_flag_settings`, and `interrupt` without
//!   restarting the subprocess.
//! - Real-time `stream_event` deltas (text, thinking, tool calls), `assistant`/`user` tool
//!   results with live diffs, exact `usage`/`modelUsage`/cost reporting, and rate-limit events.
//! - Bidirectional `can_use_tool` permission prompts, `AskUserQuestion` -> oxi elicitation UI,
//!   `ExitPlanMode`, MCP `elicitation`, and `control_cancel_request`.
//! - Native mid-turn steering by writing follow-up `user` frames to stdin.
//! - Instant external session listing and import from `~/.claude/projects/<slug>/*.jsonl`.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::mpsc::{Receiver as StdReceiver, Sender as StdSender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
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
    ACP_PLAN_MODE_PREFIX, AcpPrompt, AcpSetOption, CANCEL_WAIT_LIMIT, LaunchSpec, autonomous,
    chat_archive, commands, install, is_advertised_command, learned_rules, mcp_servers, oxi_mcp,
    reaper, session_info, sessions, with_history, with_missed, workflows,
};
use crate::agent::activity_log::{self, ActivityKind};
use crate::agent::approval::{ApprovalDecision, ApprovalPolicy, PLAN_MODE_REFUSAL};
use crate::agent::events::{AgentEvent, TokenUsage};
use crate::model::{
    ChatMessage, ToolLocation, ToolMetadata, ToolStatus, ToolUpdate, apply_tool_update,
    merge_tool_update,
};
use crate::settings::{McpServerConfig, McpTransport, ProviderConfig};

type PendingControl = Arc<AsyncMutex<HashMap<String, oneshot::Sender<Result<Value, String>>>>>;

#[derive(Clone, Debug)]
struct ClaudeModel {
    value: String,
    display_name: String,
    description: String,
    efforts: Vec<String>,
    supports_fast: bool,
    supports_auto: bool,
}

struct ClaudeState {
    models: Vec<ClaudeModel>,
    model: String,
    effort: String,
    mode: String,
    restore_mode: Option<String>,
    fast: bool,
    session_key: String,
    command_line: String,
}

impl ClaudeState {
    fn active_model(&self) -> Option<&ClaudeModel> {
        self.models
            .iter()
            .find(|m| m.value == self.model)
            .or_else(|| self.models.first())
    }

    fn supports_fast(&self) -> bool {
        self.active_model().is_some_and(|m| m.supports_fast)
    }

    fn supports_auto_mode(&self) -> bool {
        self.models.iter().any(|m| m.supports_auto)
    }

    fn available_model_ids(&self) -> Vec<String> {
        self.models.iter().map(|m| m.value.clone()).collect()
    }

    fn to_config_options(&self) -> Value {
        let mut options = Vec::new();
        let mut mode_opts = vec![
            json!({ "value": "default", "name": "Ask permissions", "description": "Prompt before edits and commands" }),
            json!({ "value": "acceptEdits", "name": "Accept edits", "description": "Automatically accept file edits" }),
            json!({ "value": "plan", "name": "Plan mode", "description": "Read-only exploration and planning" }),
        ];
        if self.supports_auto_mode() {
            mode_opts.push(json!({
                "value": "auto",
                "name": "Auto mode",
                "description": "Classifier-assisted permission approvals"
            }));
        }
        mode_opts.push(json!({
            "value": "bypassPermissions",
            "name": "Bypass permissions",
            "description": "Skip permission prompts"
        }));
        options.push(json!({
            "id": "mode",
            "name": "Mode",
            "description": "Claude Code permission mode",
            "category": "mode",
            "type": "select",
            "currentValue": self.mode,
            "options": mode_opts,
        }));

        let mut model_opts: Vec<Value> = self
            .models
            .iter()
            .map(|m| {
                json!({
                    "value": m.value,
                    "name": m.display_name,
                    "description": m.description,
                })
            })
            .collect();
        if !self.model.is_empty()
            && self.model != "default"
            && !self.models.iter().any(|m| m.value == self.model)
        {
            model_opts.push(json!({
                "value": self.model,
                "name": self.model,
                "description": "Custom model",
            }));
        }
        if !model_opts.is_empty() {
            options.push(json!({
                "id": "model",
                "name": "Model",
                "description": "AI model to use",
                "category": "model",
                "type": "select",
                "currentValue": if self.model.is_empty() { "default" } else { &self.model },
                "options": model_opts,
            }));
        }

        let efforts = self
            .active_model()
            .map(|m| m.efforts.as_slice())
            .unwrap_or(&[]);
        if !efforts.is_empty() {
            let mut effort_opts = vec![json!({ "value": "default", "name": "Default" })];
            for e in efforts {
                effort_opts.push(json!({
                    "value": e,
                    "name": title_case(e),
                }));
            }
            let current = if self.effort.is_empty() || !efforts.iter().any(|e| e == &self.effort) {
                "default"
            } else {
                self.effort.as_str()
            };
            options.push(json!({
                "id": "effort",
                "name": "Effort",
                "description": "Available effort levels for this model",
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
                "description": "Faster output generation when supported",
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

    fn restore_previous(&mut self, previous: &[AcpConfigOption]) {
        for opt in previous {
            if opt.id == "mode"
                && let AcpConfigKind::Select { current, .. } = &opt.kind
                && !current.is_empty()
                && current != "plan"
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

fn title_case(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
        None => String::new(),
    }
}

/// Incoming `control_request` from Claude CLI requiring a `control_response`.
struct ClaudeControlReq {
    request_id: String,
    request: Value,
    withdrawn: Arc<AtomicBool>,
}

/// Completion delivered when Claude emits a `type: "result"` frame.
struct ClaudeTurnResult {
    ok: bool,
    error: Option<String>,
    stop_notice: Option<&'static str>,
}

#[derive(Default, Clone)]
struct ClaudeToolState {
    name: String,
    raw_name: String,
    kind: String,
    title: String,
    input_buf: String,
    input: Option<Value>,
    output: String,
    diff: Option<String>,
    locations: Vec<ToolLocation>,
    status: ToolStatus,
    plan_emitted: bool,
}

struct ClaudeTurnCtx {
    event_tx: StdSender<AgentEvent>,
    ctrl_tx: mpsc::UnboundedSender<ClaudeControlReq>,
    done_tx: Option<oneshot::Sender<ClaudeTurnResult>>,
    withdrawn_map: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    /// Map content block index -> tool_use_id for the current streaming assistant message.
    block_tools: HashMap<usize, String>,
    tools: HashMap<String, ClaudeToolState>,
    /// True once `stream_event` emitted text/thinking deltas for the current assistant message.
    streamed_text: bool,
    streamed_thinking: bool,
    /// Cumulative answer text so far (to avoid duplicating `ExitPlanMode` plan text).
    answer_text: String,
    last_checklist: Option<Value>,
    last_error: Option<String>,
    context_window: u64,
    session_key: String,
    command_line: String,
    cwd: PathBuf,
    session_id: Arc<Mutex<String>>,
    /// Tools whose work moved to a background task (`task_started` with `is_backgrounded`).
    backgrounded: std::collections::HashSet<String>,
    /// Background tools of this connection, kept across turns so a later turn can finish them.
    bg_tools: BgTools,
    /// `result` frames that close a cycle the agent started itself before this turn's prompt
    /// was read, rather than this turn.
    skip_results: usize,
}

/// Background tools by `tool_use_id`, with what their pill showed when the turn moved on.
type BgTools = Arc<Mutex<HashMap<String, ClaudeToolState>>>;

/// What Claude Code streamed with no turn in flight, oldest first: a turn it started itself
/// (a background command finished, a wake-up fired), held until the UI attaches a turn.
type IdleQueue = Arc<Mutex<VecDeque<Value>>>;

/// More held frames than this drops the oldest, so a cycle nobody attaches to stays bounded.
const MAX_IDLE_FRAMES: usize = 20_000;

/// An attached turn that hears nothing from Claude Code for this long is taken as over.
const ATTACH_QUIET_LIMIT: Duration = Duration::from_secs(10 * 60);

/// Whether a frame received between turns means Claude Code is running a turn of its own.
fn starts_cycle(msg: &Value) -> bool {
    match msg["type"].as_str().unwrap_or("") {
        "assistant" | "stream_event" | "user" | "control_request" => true,
        "system" => msg["subtype"].as_str() == Some("task_notification"),
        _ => false,
    }
}

/// Keep a frame that arrived with no turn in flight. Only one that starts a cycle opens the
/// queue; anything else (rate limits, task bookkeeping) is kept only behind it.
fn hold_idle(idle: &IdleQueue, session_key: &str, msg: &Value) {
    let starts = starts_cycle(msg);
    {
        let mut queue = idle.lock().unwrap_or_else(|e| e.into_inner());
        if !starts && queue.is_empty() {
            return;
        }
        if queue.len() >= MAX_IDLE_FRAMES {
            queue.pop_front();
        }
        queue.push_back(msg.clone());
    }
    if starts {
        autonomous::announce(session_key);
    }
}

/// Deliver one frame to the turn in flight.
fn route(ctx: &mut ClaudeTurnCtx, msg: &Value) {
    match msg["type"].as_str().unwrap_or("") {
        "control_request" => {
            let rid = msg["request_id"].as_str().unwrap_or("").to_owned();
            let request = msg.get("request").cloned().unwrap_or(Value::Null);
            let withdrawn = Arc::new(AtomicBool::new(false));
            ctx.withdrawn_map
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(rid.clone(), withdrawn.clone());
            let _ = ctx.ctrl_tx.send(ClaudeControlReq {
                request_id: rid,
                request,
                withdrawn,
            });
        }
        "control_cancel_request" => {
            if let Some(rid) = msg["request_id"].as_str()
                && let Some(flag) = ctx
                    .withdrawn_map
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(rid)
            {
                flag.store(true, Ordering::SeqCst);
            }
        }
        _ => ctx.handle_message(msg),
    }
}

/// Replay frames held between turns into `ctx`. Their `result` frames close cycles that are
/// already over and do not end `ctx`'s turn. Returns whether the last held cycle is still open.
fn replay_idle(ctx: &mut ClaudeTurnCtx, held: Vec<Value>) -> bool {
    let mut open = false;
    for msg in held {
        if msg["type"].as_str() == Some("result") {
            open = false;
            if let Some(usage) = parse_claude_usage(&msg["usage"]) {
                let _ = ctx.event_tx.send(AgentEvent::ExternalUsage(usage));
            }
            continue;
        }
        if starts_cycle(&msg) {
            open = true;
        }
        route(ctx, &msg);
    }
    open
}

impl ClaudeTurnCtx {
    fn new(
        handles: &ClaudeHandles,
        event_tx: StdSender<AgentEvent>,
        ctrl_tx: mpsc::UnboundedSender<ClaudeControlReq>,
        done_tx: oneshot::Sender<ClaudeTurnResult>,
        session_key: String,
        command_line: String,
        cwd: PathBuf,
    ) -> Self {
        Self {
            event_tx,
            ctrl_tx,
            done_tx: Some(done_tx),
            withdrawn_map: Arc::new(Mutex::new(HashMap::new())),
            block_tools: HashMap::new(),
            tools: HashMap::new(),
            streamed_text: false,
            streamed_thinking: false,
            answer_text: String::new(),
            last_checklist: None,
            last_error: None,
            context_window: 0,
            session_key,
            command_line,
            cwd,
            session_id: handles.session_id.clone(),
            backgrounded: Default::default(),
            bg_tools: handles.bg_tools.clone(),
            skip_results: 0,
        }
    }

    /// A tool's background task ended: finish its pill, wherever it was shown.
    fn finish_background(&mut self, msg: &Value) {
        let Some(id) = msg["tool_use_id"].as_str().filter(|s| !s.is_empty()) else {
            return;
        };
        let stored = self
            .bg_tools
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        let Some(mut tool) = self.tools.get(id).cloned().or(stored) else {
            return;
        };
        tool.status = match msg["status"].as_str() {
            Some("failed") => ToolStatus::Failed,
            Some("killed" | "stopped" | "cancelled") => ToolStatus::Interrupted,
            _ => ToolStatus::Completed,
        };
        if let Some(summary) = msg["summary"].as_str().filter(|s| !s.trim().is_empty()) {
            if !tool.output.is_empty() {
                tool.output.push_str("\n\n");
            }
            tool.output.push_str(summary.trim());
        }
        if let Some(t) = self.tools.get_mut(id) {
            *t = tool.clone();
        }
        self.emit_tool(id, &tool);
    }

    fn emit_tool(&self, id: &str, tool: &ClaudeToolState) {
        let limit = crate::agent::tools::MAX_TOOL_OUTPUT_CHARS;
        let output_truncated = tool.output.chars().count() > limit;
        let _ = self
            .event_tx
            .send(AgentEvent::ToolUpdate(Box::new(ToolUpdate {
                tool_call_id: id.to_owned(),
                name: tool.name.clone(),
                args: tool.input.clone(),
                output: tool.output.chars().take(limit).collect(),
                diff: tool.diff.clone(),
                output_truncated,
                metadata: ToolMetadata {
                    title: tool.title.clone(),
                    kind: tool.kind.clone(),
                    name: Some(tool.raw_name.clone()),
                    locations: tool.locations.clone(),
                    status: tool.status,
                },
            })));
    }

    fn emit_checklist(&mut self, entries: &[Value]) {
        let todos: Vec<Value> = entries
            .iter()
            .filter_map(|e| {
                let content = e.get("content")?.as_str()?.trim();
                let status = e
                    .get("status")
                    .and_then(Value::as_str)
                    .filter(|s| matches!(*s, "in_progress" | "completed"))
                    .unwrap_or("pending");
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
                tool_call_id: "claude-plan".to_owned(),
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

    fn record_tool_use(&mut self, id: &str, raw_name: &str, input: Option<Value>, emit: bool) {
        if raw_name == "ToolSearch" {
            return;
        }
        if (raw_name == "TodoWrite" || oxi_mcp::names_todo_tool(raw_name))
            && let Some(todos) = input.as_ref().and_then(|v| v["todos"].as_array())
        {
            self.emit_checklist(todos);
            return;
        }
        let (name, kind, _) = map_claude_tool_name(raw_name);
        let (title, diff, locations) = summarize_claude_tool_input(raw_name, input.as_ref());
        let mut plan_to_emit: Option<String> = None;
        let snapshot = {
            let tool = self.tools.entry(id.to_owned()).or_default();
            tool.name = name;
            tool.raw_name = raw_name.to_owned();
            tool.kind = kind.to_owned();
            if !title.is_empty() {
                tool.title = title;
            } else if tool.title.is_empty() {
                tool.title = raw_name.to_owned();
            }
            if let Some(inp) = input {
                tool.input = Some(inp);
            }
            if diff.is_some() {
                tool.diff = diff;
            }
            if !locations.is_empty() {
                tool.locations = locations;
            }
            if tool.status == ToolStatus::Pending {
                tool.status = ToolStatus::InProgress;
            }
            if raw_name == "ExitPlanMode"
                && !tool.plan_emitted
                && let Some(plan) = tool
                    .input
                    .as_ref()
                    .and_then(|i| i["plan"].as_str())
                    .map(str::trim)
                    .filter(|p| !p.is_empty() && !self.answer_text.contains(p))
            {
                tool.plan_emitted = true;
                plan_to_emit = Some(format!("\n\n{plan}\n\n"));
            }
            ClaudeToolState {
                name: tool.name.clone(),
                raw_name: tool.raw_name.clone(),
                kind: tool.kind.clone(),
                title: tool.title.clone(),
                input_buf: String::new(),
                input: tool.input.clone(),
                output: tool.output.clone(),
                diff: tool.diff.clone(),
                locations: tool.locations.clone(),
                status: tool.status,
                plan_emitted: tool.plan_emitted,
            }
        };
        if emit {
            self.emit_tool(id, &snapshot);
        }
        if let Some(plan) = plan_to_emit {
            self.answer_text.push_str(&plan);
            let _ = self.event_tx.send(AgentEvent::TextDelta(plan));
        }
    }

    fn handle_message(&mut self, msg: &Value) {
        if let Some(sid) = msg["session_id"].as_str().filter(|s| !s.is_empty()) {
            let mut slot = self.session_id.lock().unwrap_or_else(|e| e.into_inner());
            if *slot != sid {
                *slot = sid.to_owned();
                sessions::remember(&self.session_key, &self.command_line, &self.cwd, sid);
            }
        }
        let is_subagent = msg
            .get("parent_tool_use_id")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty());

        match msg["type"].as_str() {
            Some("stream_event") if !is_subagent => {
                let ev = &msg["event"];
                match ev["type"].as_str() {
                    Some("message_start") => {
                        self.block_tools.clear();
                        self.streamed_text = false;
                        self.streamed_thinking = false;
                        if let Some(usage) = ev["message"].get("usage") {
                            self.emit_context_from_message_usage(usage);
                        }
                    }
                    Some("content_block_start") => {
                        let idx = ev["index"].as_u64().unwrap_or(0) as usize;
                        let block = &ev["content_block"];
                        if block["type"].as_str() == Some("tool_use")
                            && let (Some(id), Some(name)) =
                                (block["id"].as_str(), block["name"].as_str())
                        {
                            self.block_tools.insert(idx, id.to_owned());
                            let initial_input = block
                                .get("input")
                                .filter(|v| {
                                    v.is_object()
                                        && !v.as_object().is_none_or(serde_json::Map::is_empty)
                                })
                                .cloned();
                            self.record_tool_use(id, name, initial_input, true);
                        }
                    }
                    Some("content_block_delta") => {
                        let idx = ev["index"].as_u64().unwrap_or(0) as usize;
                        let delta = &ev["delta"];
                        match delta["type"].as_str() {
                            Some("text_delta") => {
                                if let Some(text) = delta["text"].as_str()
                                    && !text.is_empty()
                                {
                                    self.streamed_text = true;
                                    self.answer_text.push_str(text);
                                    let _ =
                                        self.event_tx.send(AgentEvent::TextDelta(text.to_owned()));
                                }
                            }
                            Some("thinking_delta") => {
                                if let Some(thinking) = delta["thinking"].as_str()
                                    && !thinking.is_empty()
                                {
                                    self.streamed_thinking = true;
                                    let _ = self
                                        .event_tx
                                        .send(AgentEvent::ThinkingDelta(thinking.to_owned()));
                                }
                            }
                            Some("input_json_delta") => {
                                if let (Some(id), Some(part)) = (
                                    self.block_tools.get(&idx).cloned(),
                                    delta["partial_json"].as_str(),
                                ) && let Some(tool) = self.tools.get_mut(&id)
                                {
                                    tool.input_buf.push_str(part);
                                }
                            }
                            _ => {}
                        }
                    }
                    Some("content_block_stop") => {
                        let idx = ev["index"].as_u64().unwrap_or(0) as usize;
                        if let Some(id) = self.block_tools.remove(&idx) {
                            let parsed = self.tools.get_mut(&id).and_then(|t| {
                                let raw = std::mem::take(&mut t.input_buf);
                                let name = t.raw_name.clone();
                                (!raw.is_empty())
                                    .then(|| serde_json::from_str::<Value>(&raw).ok())
                                    .flatten()
                                    .map(|v| (name, v))
                            });
                            if let Some((raw_name, val)) = parsed {
                                self.record_tool_use(&id, &raw_name, Some(val), true);
                            }
                        }
                    }
                    _ => {}
                }
            }
            Some("assistant") if !is_subagent => {
                if let Some(err) = msg["error"].as_str() {
                    self.last_error = Some(claude_assistant_error_text(err).to_owned());
                }
                let message = &msg["message"];
                if let Some(usage) = message.get("usage") {
                    self.emit_context_from_message_usage(usage);
                }
                for block in message["content"].as_array().into_iter().flatten() {
                    match block["type"].as_str() {
                        Some("text") if !self.streamed_text => {
                            if let Some(text) = block["text"].as_str()
                                && !text.is_empty()
                            {
                                self.answer_text.push_str(text);
                                let _ = self.event_tx.send(AgentEvent::TextDelta(text.to_owned()));
                            }
                        }
                        Some("thinking") if !self.streamed_thinking => {
                            if let Some(thinking) = block["thinking"].as_str()
                                && !thinking.is_empty()
                            {
                                let _ = self
                                    .event_tx
                                    .send(AgentEvent::ThinkingDelta(thinking.to_owned()));
                            }
                        }
                        Some("tool_use") => {
                            if let (Some(id), Some(name)) =
                                (block["id"].as_str(), block["name"].as_str())
                            {
                                let input = block.get("input").cloned();
                                self.record_tool_use(id, name, input, true);
                            }
                        }
                        _ => {}
                    }
                }
                // Reset stream flags in case the next assistant block arrives without stream_event.
                self.streamed_text = false;
                self.streamed_thinking = false;
            }
            Some("user") if !is_subagent => {
                for block in msg["message"]["content"].as_array().into_iter().flatten() {
                    if block["type"].as_str() != Some("tool_result") {
                        continue;
                    }
                    let Some(id) = block["tool_use_id"].as_str() else {
                        continue;
                    };
                    let is_err = block["is_error"].as_bool() == Some(true);
                    let output = extract_tool_result_text(&block["content"]);
                    // A command moved to a background task: it reports back after the turn.
                    let backgrounded = !is_err
                        && (self.backgrounded.contains(id)
                            || msg["tool_use_result"]["backgroundTaskId"]
                                .as_str()
                                .is_some_and(|s| !s.is_empty()));
                    if let Some(tool) = self.tools.get_mut(id) {
                        if tool.raw_name == "ToolSearch"
                            || tool.raw_name == "TodoWrite"
                            || oxi_mcp::names_todo_tool(&tool.raw_name)
                        {
                            continue;
                        }
                        tool.status = if is_err {
                            ToolStatus::Failed
                        } else if backgrounded {
                            ToolStatus::InProgress
                        } else {
                            ToolStatus::Completed
                        };
                        if !output.is_empty() {
                            tool.output = output;
                        }
                        if backgrounded {
                            self.bg_tools
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .insert(id.to_owned(), tool.clone());
                        }
                        let snapshot = ClaudeToolState {
                            name: tool.name.clone(),
                            raw_name: tool.raw_name.clone(),
                            kind: tool.kind.clone(),
                            title: tool.title.clone(),
                            input_buf: String::new(),
                            input: tool.input.clone(),
                            output: tool.output.clone(),
                            diff: tool.diff.clone(),
                            locations: tool.locations.clone(),
                            status: tool.status,
                            plan_emitted: tool.plan_emitted,
                        };
                        self.emit_tool(id, &snapshot);
                    }
                }
            }
            Some("system") => match msg["subtype"].as_str() {
                Some("task_started") if msg["is_backgrounded"].as_bool() == Some(true) => {
                    if let Some(id) = msg["tool_use_id"].as_str().filter(|s| !s.is_empty()) {
                        self.backgrounded.insert(id.to_owned());
                        // Moved to the background after its result arrived: keep it running.
                        if let Some(tool) = self.tools.get_mut(id)
                            && tool.status != ToolStatus::InProgress
                        {
                            tool.status = ToolStatus::InProgress;
                            let snapshot = tool.clone();
                            self.bg_tools
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .insert(id.to_owned(), snapshot.clone());
                            self.emit_tool(id, &snapshot);
                        }
                    }
                }
                Some("task_notification") => self.finish_background(msg),
                _ => {}
            },
            Some("rate_limit_event") => {
                let info = &msg["rate_limit_info"];
                if info["status"].as_str() == Some("rejected") {
                    self.last_error = Some("Claude Code usage rate limit reached.".into());
                }
            }
            Some("result") => {
                if self.skip_results > 0 {
                    // Closes the agent's own cycle that ran ahead of this turn's prompt.
                    self.skip_results -= 1;
                    if let Some(usage) = parse_claude_usage(&msg["usage"]) {
                        let _ = self.event_tx.send(AgentEvent::ExternalUsage(usage));
                    }
                    return;
                }
                // Report exact per-turn usage & context window & cost.
                if let Some(usage) = parse_claude_usage(&msg["usage"]) {
                    let _ = self.event_tx.send(AgentEvent::ExternalUsage(usage));
                }
                if let Some((used, size)) = extract_model_usage_context(&msg["modelUsage"]) {
                    self.context_window = size;
                    let _ = self.event_tx.send(AgentEvent::ContextUsage { used, size });
                }
                if let Some(cost) = msg["total_cost_usd"].as_f64() {
                    let sid = self
                        .session_id
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone();
                    session_info::note_cost(&self.session_key, &sid, cost, "USD");
                }

                let subtype = msg["subtype"].as_str().unwrap_or("success");
                let is_error = msg["is_error"].as_bool() == Some(true);
                let stop_notice = match subtype {
                    "error_max_turns" => Some(
                        "\n\n*The agent stopped: it reached its limit of turns for this request. Say \"continue\" to let it go on.*\n\n",
                    ),
                    "error_max_budget_usd" => {
                        Some("\n\n*The agent stopped: it reached its configured spend limit.*\n\n")
                    }
                    _ => match msg["terminal_reason"].as_str() {
                        Some("max_tokens") => Some(
                            "\n\n*The agent stopped: the reply reached its output token limit. Say \"continue\" to pick up where it left off.*\n\n",
                        ),
                        Some("refusal") => {
                            Some("\n\n*The agent declined to continue with this request.*\n\n")
                        }
                        _ => None,
                    },
                };
                let ok = !is_error && subtype == "success";
                let error = if ok {
                    None
                } else {
                    extract_result_error(msg).or_else(|| self.last_error.take())
                };
                if let Some(done) = self.done_tx.take() {
                    let _ = done.send(ClaudeTurnResult {
                        ok,
                        error,
                        stop_notice,
                    });
                }
            }
            _ => {}
        }
    }

    fn emit_context_from_message_usage(&self, usage: &Value) {
        if self.context_window == 0 {
            return;
        }
        let used = usage["input_tokens"].as_u64().unwrap_or(0)
            + usage["cache_read_input_tokens"].as_u64().unwrap_or(0)
            + usage["cache_creation_input_tokens"].as_u64().unwrap_or(0);
        if used > 0 {
            let _ = self.event_tx.send(AgentEvent::ContextUsage {
                used,
                size: self.context_window,
            });
        }
    }
}

/// Live native Claude Code CLI connection for one oxi session.
pub(super) struct ClaudeConn {
    pub(super) command_line: String,
    pub(super) mcp_servers: Vec<McpServerConfig>,
    pub(super) alive: Arc<AtomicBool>,
    pub(super) handles: ClaudeHandles,
    _child: Child,
    _read_task: tokio::task::JoinHandle<()>,
    _stderr_task: tokio::task::JoinHandle<()>,
}

#[derive(Clone)]
pub(super) struct ClaudeHandles {
    stdin: Arc<AsyncMutex<ChildStdin>>,
    next_id: Arc<AtomicI64>,
    pending_ctrl: PendingControl,
    turn_ctx: Arc<AsyncMutex<Option<ClaudeTurnCtx>>>,
    workflow_tx: Arc<AsyncMutex<Option<StdSender<AgentEvent>>>>,
    held: Arc<autonomous::Held>,
    /// Frames of a turn Claude Code started itself, until a turn is attached to show them.
    idle: IdleQueue,
    bg_tools: BgTools,
    session_id: Arc<Mutex<String>>,
    state: Arc<Mutex<ClaudeState>>,
    needs_history: Arc<AtomicBool>,
    needs_rules: Arc<AtomicBool>,
    turn_lock: Arc<AsyncMutex<()>>,
    pub(super) alive: Arc<AtomicBool>,
    pub(super) activity: Arc<reaper::Activity>,
    evidence_dir: PathBuf,
}

impl ClaudeHandles {
    /// Stop a native workflow (`stop_task`) started by this connection's session. Workflow
    /// agents have no stop of their own in Claude Code; only the whole workflow does.
    pub(super) async fn stop_child(
        &self,
        root: &str,
        child: &str,
        task: bool,
    ) -> Result<(), String> {
        let sid = self
            .session_id
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if !self.alive.load(Ordering::SeqCst)
            || sid != root
            || !task
            || !self.held.can_cancel(child)
        {
            return Err("This subagent is disconnected or does not allow stopping".into());
        }
        self.held.cancel_permissions(child);
        control_request_within(
            &self.stdin,
            &self.next_id,
            &self.pending_ctrl,
            json!({ "subtype": "stop_task", "task_id": child }),
            Duration::from_secs(15),
        )
        .await
        .map(|_| ())
    }

    pub(super) fn busy(&self) -> bool {
        self.turn_lock.try_lock().is_err()
            || !self.held.is_empty()
            || !self
                .idle
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_empty()
            || self.pending_ctrl.try_lock().map_or(true, |p| !p.is_empty())
    }

    pub(super) fn available_models(&self) -> Vec<String> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .available_model_ids()
    }

    pub(super) async fn sync_model_effort(&self, model: &str, effort: &str) {
        let (model_changed, effort_changed) = {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let mc = {
                let m = model.trim();
                if !m.is_empty() && m != st.model {
                    st.model = m.to_owned();
                    true
                } else {
                    false
                }
            };
            let ec = {
                let e = effort.trim();
                let normalized = if e.is_empty() { "default" } else { e };
                if normalized != st.effort {
                    st.effort = normalized.to_owned();
                    true
                } else {
                    false
                }
            };
            if mc || ec {
                st.publish();
            }
            (mc, ec)
        };
        if model_changed {
            let target = if model.trim() == "default" {
                Value::Null
            } else {
                json!(model.trim())
            };
            let _ = control_request(
                &self.stdin,
                &self.next_id,
                &self.pending_ctrl,
                json!({ "subtype": "set_model", "model": target }),
            )
            .await;
        }
        if effort_changed {
            let val = effort.trim();
            let setting = if val.is_empty() || val == "default" {
                Value::Null
            } else {
                json!(val)
            };
            let _ = control_request(
                &self.stdin,
                &self.next_id,
                &self.pending_ctrl,
                json!({ "subtype": "apply_flag_settings", "settings": { "effortLevel": setting } }),
            )
            .await;
        }
    }

    pub(super) async fn apply_option(&self, req: &AcpSetOption) -> Result<(), String> {
        match req.config_id.as_str() {
            "model" => {
                let val = req.value.as_str().unwrap_or("default");
                let wire = if val == "default" {
                    Value::Null
                } else {
                    json!(val)
                };
                control_request(
                    &self.stdin,
                    &self.next_id,
                    &self.pending_ctrl,
                    json!({ "subtype": "set_model", "model": wire }),
                )
                .await?;
                let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
                st.model = val.to_owned();
                st.publish();
            }
            "mode" => {
                let val = req.value.as_str().unwrap_or("default");
                control_request(
                    &self.stdin,
                    &self.next_id,
                    &self.pending_ctrl,
                    json!({ "subtype": "set_permission_mode", "mode": val }),
                )
                .await?;
                let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
                st.mode = val.to_owned();
                st.restore_mode = None;
                st.publish();
            }
            "effort" | "thought_level" => {
                let val = req.value.as_str().unwrap_or("default");
                let setting = if val.is_empty() || val == "default" {
                    Value::Null
                } else {
                    json!(val)
                };
                control_request(
                    &self.stdin,
                    &self.next_id,
                    &self.pending_ctrl,
                    json!({ "subtype": "apply_flag_settings", "settings": { "effortLevel": setting } }),
                )
                .await?;
                let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
                st.effort = val.to_owned();
                st.publish();
            }
            "fast" => {
                let on = req.value.as_bool().unwrap_or(false);
                control_request(
                    &self.stdin,
                    &self.next_id,
                    &self.pending_ctrl,
                    json!({ "subtype": "apply_flag_settings", "settings": { "fastMode": on } }),
                )
                .await?;
                let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
                st.fast = on;
                st.publish();
            }
            _ => {}
        }
        Ok(())
    }
}

/// Spawn and initialize a native Claude Code CLI connection.
pub(super) async fn spawn_conn(
    spec: &LaunchSpec<'_>,
    mcp_servers: Vec<McpServerConfig>,
) -> Result<ClaudeConn, String> {
    if !spec.cwd.is_dir() {
        return Err(format!(
            "Claude Code workspace does not exist or is not a directory: {}",
            spec.cwd.display()
        ));
    }
    let existing_id = sessions::lookup_with_fallbacks(
        spec.session_key,
        spec.command_line,
        spec.cwd,
        &[ProviderConfig::LEGACY_CLAUDE_ACP_COMMAND],
    );
    if let Some(resume_id) = existing_id.as_deref() {
        match spawn_attempt(spec, &mcp_servers, Some(resume_id)).await {
            Ok(conn) => return Ok(conn),
            Err(e) => {
                log::warn!("claude --resume {resume_id} failed, starting fresh session: {e}");
            }
        }
    }
    spawn_attempt(spec, &mcp_servers, None).await
}

async fn spawn_attempt(
    spec: &LaunchSpec<'_>,
    mcp_servers: &[McpServerConfig],
    resume_id: Option<&str>,
) -> Result<ClaudeConn, String> {
    let evidence_dir = crate::evidence::new_conn_dir();
    let delegation_dir =
        crate::delegation::available().then(|| crate::delegation::dir_for(spec.session_key));
    let mcp_config_path = write_claude_mcp_config(
        &evidence_dir,
        mcp_servers,
        Some(&evidence_dir),
        delegation_dir.as_deref(),
    )?;

    let words = split_shell_words(spec.command_line);
    let program_word = words.first().map(String::as_str).unwrap_or("claude");
    let bin = resolve_cli_binary(
        program_word,
        &[".local/bin", ".claude/local"],
        install::bundled_claude_cli(),
    )
    .await
    .ok_or_else(|| {
        format!(
            "Could not find `{program_word}` in PATH. Install Claude Code (`curl -fsSL https://claude.ai/install.sh | bash` or `npm i -g @anthropic-ai/claude-code`) and sign in."
        )
    })?;

    let extra_args: &[String] = words.get(1..).unwrap_or(&[]);
    let args = build_claude_args(
        extra_args,
        &mcp_config_path,
        resume_id,
        spec.model,
        spec.effort,
    );

    let mut cmd = Command::new(&bin);
    cmd.args(&args);
    cmd.current_dir(spec.cwd);
    crate::shell_env::apply_async(&mut cmd).await;
    // Strip nested-session guard so oxi works even if launched from a Claude Code terminal.
    cmd.env_remove("CLAUDECODE");
    for (k, v) in spec.env {
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
        .map_err(|e| format!("Could not launch Claude Code `{}`: {e}", bin.display()))?;
    let stdin = child.stdin.take().ok_or("Claude: child has no stdin")?;
    let stdout = child.stdout.take().ok_or("Claude: child has no stdout")?;
    let stderr = child.stderr.take().ok_or("Claude: child has no stderr")?;

    let stdin = Arc::new(AsyncMutex::new(stdin));
    let pending_ctrl: PendingControl = Arc::new(AsyncMutex::new(HashMap::new()));
    let turn_ctx: Arc<AsyncMutex<Option<ClaudeTurnCtx>>> = Arc::new(AsyncMutex::new(None));
    let workflow_tx: Arc<AsyncMutex<Option<StdSender<AgentEvent>>>> =
        Arc::new(AsyncMutex::new(None));
    let next_id = Arc::new(AtomicI64::new(1));
    let alive = Arc::new(AtomicBool::new(true));
    let activity = Arc::new(reaper::Activity::new());
    let held = Arc::new(autonomous::Held::new(spec.session_key));
    let idle: IdleQueue = Arc::default();
    let session_id = Arc::new(Mutex::new(resume_id.unwrap_or("").to_owned()));
    let stderr_tail = Arc::new(Mutex::new(VecDeque::new()));

    let stderr_task = tokio::spawn(drain_stderr(stderr, stderr_tail.clone()));
    let read_task = tokio::spawn(read_loop(
        stdout,
        pending_ctrl.clone(),
        turn_ctx.clone(),
        alive.clone(),
        activity.clone(),
        session_id.clone(),
        (idle.clone(), spec.session_key.to_owned()),
    ));

    let init = tokio::time::timeout(
        Duration::from_secs(30),
        control_request(
            &stdin,
            &next_id,
            &pending_ctrl,
            json!({ "subtype": "initialize" }),
        ),
    )
    .await
    .map_err(|_| "Claude Code did not initialize within 30 seconds.".to_string())?;
    let init = match init {
        Ok(v) => v,
        Err(e) => {
            let _ = child.kill().await;
            tokio::time::sleep(Duration::from_millis(200)).await;
            let tail = stderr_tail.lock().unwrap_or_else(|e| e.into_inner());
            if tail.is_empty() {
                return Err(format!("Claude Code initialize failed: {e}"));
            }
            let lines: Vec<&str> = tail.iter().map(String::as_str).collect();
            return Err(format!(
                "Claude Code initialize failed: {e}\n{}",
                lines.join("\n")
            ));
        }
    };

    let models = parse_claude_models(&init["models"]);
    let cmds = parse_claude_commands(&init["commands"]);
    if !cmds.is_empty() {
        commands::store(spec.command_line, spec.cwd, cmds);
    }

    let initial_mode = init["current_permission_mode"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or("default")
        .to_owned();
    let initial_fast = init["fast_mode_state"].as_str() == Some("on");
    let requested_model = spec.model.trim();
    let initial_model = if requested_model.is_empty() {
        "default".to_owned()
    } else {
        requested_model.to_owned()
    };
    let requested_effort = spec.effort.trim();
    let initial_effort = if requested_effort.is_empty() {
        "default".to_owned()
    } else {
        requested_effort.to_owned()
    };

    let mut state = ClaudeState {
        models,
        model: initial_model,
        effort: initial_effort,
        mode: initial_mode,
        restore_mode: None,
        fast: initial_fast,
        session_key: spec.session_key.to_owned(),
        command_line: spec.command_line.to_owned(),
    };

    let previous = config_options::options(spec.session_key, spec.command_line).or_else(|| {
        let picked = config_options::picked_before_launch(spec.session_key, spec.command_line);
        (!picked.is_empty()).then(|| Arc::new(picked))
    });
    let prev_mode = state.mode.clone();
    let prev_fast = state.fast;
    if let Some(prev) = previous.as_deref() {
        state.restore_previous(prev);
    }
    if state.mode != prev_mode {
        let _ = control_request(
            &stdin,
            &next_id,
            &pending_ctrl,
            json!({ "subtype": "set_permission_mode", "mode": state.mode }),
        )
        .await;
    }
    if state.fast != prev_fast {
        let _ = control_request(
            &stdin,
            &next_id,
            &pending_ctrl,
            json!({ "subtype": "apply_flag_settings", "settings": { "fastMode": state.fast } }),
        )
        .await;
    }

    let sid = session_id.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if !sid.is_empty() {
        sessions::remember(spec.session_key, spec.command_line, spec.cwd, &sid);
        held.set_root(&sid);
        workflows::watch_native(
            alive.clone(),
            sid,
            spec.cwd,
            held.clone(),
            workflow_tx.clone(),
        );
    }
    held.arm();

    let needs_history = resume_id.is_none();
    let opts_json = state.to_config_options();
    if needs_history {
        config_options::remember_defaults(spec.command_line, &opts_json);
    }
    state.publish();

    let handles = ClaudeHandles {
        stdin,
        next_id,
        pending_ctrl,
        turn_ctx,
        workflow_tx,
        held,
        idle,
        bg_tools: Arc::default(),
        session_id,
        state: Arc::new(Mutex::new(state)),
        needs_history: Arc::new(AtomicBool::new(needs_history)),
        needs_rules: Arc::new(AtomicBool::new(true)),
        turn_lock: Arc::new(AsyncMutex::new(())),
        alive: alive.clone(),
        activity,
        evidence_dir,
    };
    Ok(ClaudeConn {
        command_line: spec.command_line.to_owned(),
        mcp_servers: mcp_servers.to_vec(),
        alive,
        handles,
        _child: child,
        _read_task: read_task,
        _stderr_task: stderr_task,
    })
}

/// Drive one prompt turn on a live native Claude Code connection.
pub(super) async fn run_prompt(
    handles: ClaudeHandles,
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

    // Sync native Claude plan mode.
    let text = learned_rules::expand(&text, &cwd).unwrap_or(text);
    let slash_command = is_advertised_command(&text, &command_line, &cwd);
    let native_plan = if !slash_command {
        sync_claude_plan_mode(&handles, plan_mode).await
    } else {
        false
    };
    let effective_plan_mode =
        plan_mode || handles.state.lock().unwrap_or_else(|e| e.into_inner()).mode == "plan";

    let text = if plan_mode && !slash_command && !native_plan {
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

    let mut content_blocks = build_claude_user_content(&text, &images, &resources);

    let _ = event_tx.send(AgentEvent::AgentStart);
    let _ = event_tx.send(AgentEvent::SteerAvailable);
    *handles.workflow_tx.lock().await = Some(event_tx.clone());
    // Replay any workflow updates held between turns.
    for params in handles.held.drain() {
        let root = handles
            .session_id
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let _ = event_tx.send(AgentEvent::Delegated { root, params });
    }

    let mut auto_approve = false;
    let mut followups = 0;

    let final_result: Result<(), String> = loop {
        let (ctrl_tx, mut ctrl_rx) = mpsc::unbounded_channel::<ClaudeControlReq>();
        let (done_tx, mut done_rx) = oneshot::channel::<ClaudeTurnResult>();
        {
            let mut slot = handles.turn_ctx.lock().await;
            let mut ctx = ClaudeTurnCtx::new(
                &handles,
                event_tx.clone(),
                ctrl_tx,
                done_tx,
                session_key.clone(),
                command_line.clone(),
                cwd.clone(),
            );
            // A turn the agent started itself shows here when this prompt overtook it; a cycle
            // still running ends with a `result` of its own before this prompt's.
            if replay_idle(&mut ctx, take_idle(&handles)) {
                ctx.skip_results = 1;
            }
            *slot = Some(ctx);
        }

        let sid = handles
            .session_id
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let mut user_frame = json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": content_blocks,
            }
        });
        if !sid.is_empty() {
            user_frame["session_id"] = json!(sid);
        }
        if let Err(e) = write_line(&handles.stdin, &user_frame).await {
            *handles.turn_ctx.lock().await = None;
            break Err(e);
        }

        let mut cancel_sent_at: Option<std::time::Instant> = None;
        let turn_outcome: Result<ClaudeTurnResult, String> = loop {
            tokio::select! {
                biased;
                done = &mut done_rx => {
                    break done.map_err(|_| "Claude Code closed the connection.".to_string());
                }
                maybe_req = ctrl_rx.recv() => {
                    if let Some(creq) = maybe_req {
                        handle_control_request(
                            &handles.stdin,
                            &event_tx,
                            &mut approval_rx,
                            approval_policy,
                            &mut bash_allowlist,
                            &mut allowed_tools,
                            effective_plan_mode,
                            &cancel,
                            &mut auto_approve,
                            creq,
                        )
                        .await;
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(150)) => {
                    if !handles.alive.load(Ordering::SeqCst) {
                        break Err("Claude Code exited unexpectedly.".to_string());
                    }
                    if let Some(sent_at) = cancel_sent_at {
                        if sent_at.elapsed() >= CANCEL_WAIT_LIMIT {
                            handles.alive.store(false, Ordering::SeqCst);
                            break Err("Claude Code did not respond to interruption.".to_string());
                        }
                    } else if cancel.load(Ordering::SeqCst) {
                        cancel_sent_at = Some(std::time::Instant::now());
                        // Fire and forget: awaiting the ack here would stop this loop from
                        // seeing the turn end or the CANCEL_WAIT_LIMIT deadline.
                        let h = handles.clone();
                        tokio::spawn(async move {
                            let _ = control_request_within(
                                &h.stdin,
                                &h.next_id,
                                &h.pending_ctrl,
                                json!({ "subtype": "interrupt" }),
                                CANCEL_WAIT_LIMIT,
                            )
                            .await;
                        });
                    } else if let Some(inbox) = steer.as_ref() {
                        for msg in inbox.take() {
                            let sid = handles
                                .session_id
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .clone();
                            let mut frame = json!({
                                "type": "user",
                                "message": {
                                    "role": "user",
                                    "content": [{ "type": "text", "text": msg }],
                                }
                            });
                            if !sid.is_empty() {
                                frame["session_id"] = json!(sid);
                            }
                            if write_line(&handles.stdin, &frame).await.is_ok() {
                                let _ = event_tx.send(AgentEvent::Steered(msg));
                            } else {
                                let _ = event_tx.send(AgentEvent::SteerReturned);
                            }
                        }
                    }
                }
            }
        };

        *handles.turn_ctx.lock().await = None;
        let result = match turn_outcome {
            Ok(r) => r,
            Err(e) => break Err(e),
        };
        if let Some(notice) = result.stop_notice {
            let _ = event_tx.send(AgentEvent::TextDelta(notice.to_owned()));
        }
        if !result.ok {
            if cancel.load(Ordering::SeqCst) {
                break Ok(());
            }
            break Err(result
                .error
                .unwrap_or_else(|| "Claude Code turn failed.".to_string()));
        }

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
                content_blocks = vec![json!({
                    "type": "text",
                    "text": format!(
                        "{}\n\n{}",
                        crate::evidence::PROMPT_PREFIX,
                        crate::evidence::followup_prompt(&problems)
                    ),
                })];
                continue;
            }
        }
        break Ok(());
    };

    *handles.workflow_tx.lock().await = None;
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

/// Show a turn Claude Code started on its own (a background command finished, a wake-up fired)
/// the way a prompted one is shown, without sending anything: ends with that cycle's `result`.
pub(super) async fn run_attach(
    handles: ClaudeHandles,
    req: super::AcpAttach,
    reply: oneshot::Sender<Result<(), String>>,
) {
    let super::AcpAttach {
        session_key,
        cwd,
        event_tx,
        mut approval_rx,
        approval_policy,
        mut bash_allowlist,
        mut allowed_tools,
        cancel,
        plan_mode,
    } = req;
    let _turn = handles.turn_lock.clone().lock_owned().await;
    handles.activity.touch();
    let command_line = handles
        .state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .command_line
        .clone();
    let effective_plan_mode =
        plan_mode || handles.state.lock().unwrap_or_else(|e| e.into_inner()).mode == "plan";

    let _ = event_tx.send(AgentEvent::AgentStart);
    *handles.workflow_tx.lock().await = Some(event_tx.clone());
    for params in handles.held.drain() {
        let root = handles
            .session_id
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let _ = event_tx.send(AgentEvent::Delegated { root, params });
    }

    let (ctrl_tx, mut ctrl_rx) = mpsc::unbounded_channel::<ClaudeControlReq>();
    let (done_tx, mut done_rx) = oneshot::channel::<ClaudeTurnResult>();
    let open = {
        let mut slot = handles.turn_ctx.lock().await;
        let mut ctx = ClaudeTurnCtx::new(
            &handles,
            event_tx.clone(),
            ctrl_tx,
            done_tx,
            session_key,
            command_line,
            cwd,
        );
        let open = replay_idle(&mut ctx, take_idle(&handles));
        // Nothing left running (the cycle was over, or a prompt already showed it): no turn.
        if open {
            *slot = Some(ctx);
        }
        open
    };

    if open {
        let mut auto_approve = false;
        let mut cancel_sent_at: Option<std::time::Instant> = None;
        let outcome = loop {
            tokio::select! {
                biased;
                done = &mut done_rx => break done.ok(),
                maybe_req = ctrl_rx.recv() => {
                    if let Some(creq) = maybe_req {
                        handle_control_request(
                            &handles.stdin,
                            &event_tx,
                            &mut approval_rx,
                            approval_policy,
                            &mut bash_allowlist,
                            &mut allowed_tools,
                            effective_plan_mode,
                            &cancel,
                            &mut auto_approve,
                            creq,
                        )
                        .await;
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(150)) => {
                    if !handles.alive.load(Ordering::SeqCst) {
                        break None;
                    }
                    if let Some(sent_at) = cancel_sent_at {
                        if sent_at.elapsed() >= CANCEL_WAIT_LIMIT {
                            handles.alive.store(false, Ordering::SeqCst);
                            break None;
                        }
                    } else if cancel.load(Ordering::SeqCst) {
                        cancel_sent_at = Some(std::time::Instant::now());
                        let h = handles.clone();
                        tokio::spawn(async move {
                            let _ = control_request_within(
                                &h.stdin,
                                &h.next_id,
                                &h.pending_ctrl,
                                json!({ "subtype": "interrupt" }),
                                CANCEL_WAIT_LIMIT,
                            )
                            .await;
                        });
                    } else if handles.activity.quiet_for() >= ATTACH_QUIET_LIMIT {
                        break None;
                    }
                }
            }
        };
        *handles.turn_ctx.lock().await = None;
        if let Some(notice) = outcome.and_then(|r| r.stop_notice) {
            let _ = event_tx.send(AgentEvent::TextDelta(notice.to_owned()));
        }
    }
    *handles.workflow_tx.lock().await = None;
    handles.activity.touch();
    let _ = reply.send(Ok(()));
}

async fn sync_claude_plan_mode(handles: &ClaudeHandles, plan_mode: bool) -> bool {
    let target = {
        let mut st = handles.state.lock().unwrap_or_else(|e| e.into_inner());
        if plan_mode {
            if st.mode == "plan" {
                return true;
            }
            st.restore_mode = Some(st.mode.clone());
            Some("plan".to_owned())
        } else if st.mode == "plan" {
            st.restore_mode.take()
        } else {
            None
        }
    };
    let Some(target) = target else {
        return false;
    };
    let ok = control_request(
        &handles.stdin,
        &handles.next_id,
        &handles.pending_ctrl,
        json!({ "subtype": "set_permission_mode", "mode": target }),
    )
    .await
    .is_ok();
    if ok {
        let mut st = handles.state.lock().unwrap_or_else(|e| e.into_inner());
        st.mode = target;
        st.publish();
    }
    ok
}

#[allow(clippy::too_many_arguments)]
async fn handle_control_request(
    stdin: &Arc<AsyncMutex<ChildStdin>>,
    event_tx: &StdSender<AgentEvent>,
    approval_rx: &mut StdReceiver<ApprovalDecision>,
    approval_policy: ApprovalPolicy,
    bash_allowlist: &mut Vec<String>,
    allowed_tools: &mut Vec<String>,
    plan_mode: bool,
    cancel: &Arc<AtomicBool>,
    auto_approve: &mut bool,
    req: ClaudeControlReq,
) {
    let subtype = req.request["subtype"].as_str().unwrap_or("");
    match subtype {
        "can_use_tool" => {
            let tool_name = req.request["tool_name"].as_str().unwrap_or("");
            let input = req
                .request
                .get("input")
                .cloned()
                .unwrap_or_else(|| json!({}));

            // 1) AskUserQuestion -> oxi elicitation form UI.
            if tool_name == "AskUserQuestion" {
                let questions = input["questions"].as_array().cloned().unwrap_or_default();
                if !questions.is_empty() {
                    let elicit_params = claude_questions_to_elicitation(&questions);
                    let _ = event_tx.send(AgentEvent::ElicitationRequest {
                        params: elicit_params,
                    });
                    let decision =
                        wait_decision(approval_rx, cancel, None, Some(&req.withdrawn)).await;
                    let _ = event_tx.send(AgentEvent::ApprovalResolved);
                    let resp = match elicitation_to_claude_answers(&questions, decision) {
                        Some(answers) => {
                            let mut updated = input.clone();
                            if let Some(obj) = updated.as_object_mut() {
                                obj.insert("answers".into(), answers);
                            }
                            json!({ "behavior": "allow", "updatedInput": updated })
                        }
                        None => {
                            json!({ "behavior": "deny", "message": "User cancelled the question." })
                        }
                    };
                    send_control_response(stdin, &req.request_id, resp).await;
                    return;
                }
            }

            // 2) Standard tool permission check.
            let (mapped_name, _kind, is_read_only) = map_claude_tool_name(tool_name);
            let oxi_name = oxi_mcp::oxi_tool_name(tool_name);
            let (effective_name, effective_args) = match oxi_name {
                Some(name) => {
                    if let Some(cmd_args) = crate::evidence::tools::command_args(name, &input) {
                        ("bash".to_owned(), cmd_args)
                    } else {
                        (name.to_owned(), input.clone())
                    }
                }
                None => (mapped_name, input.clone()),
            };

            let decision = if matches!(
                oxi_name,
                Some(
                    "todo_write"
                        | oxi_mcp::SCRATCHPAD_READ
                        | super::chat_archive::TOOL
                        | super::chat_history::TOOL
                        | crate::delegation::STATUS
                )
            ) || crate::evidence::tools::NAMES.contains(&effective_name.as_str())
                || (plan_mode && is_claude_plan_file_edit(tool_name, &input))
            {
                Some(ApprovalDecision::Approve)
            } else if plan_mode && !is_read_only && tool_name != "ExitPlanMode" {
                Some(ApprovalDecision::Deny)
            } else if *auto_approve
                || is_computer_use_tool(tool_name)
                || !approval_policy.requires_approval(&effective_name)
                || crate::agent::approval::tool_allowed(&effective_name, allowed_tools)
                || (effective_name == "bash"
                    && crate::agent::approval::bash_command_arg(&effective_args).is_some_and(|c| {
                        crate::agent::approval::bash_command_allowlisted(c, bash_allowlist)
                    }))
            {
                Some(ApprovalDecision::Approve)
            } else {
                let _ = event_tx.send(AgentEvent::ApprovalRequest {
                    name: effective_name.clone(),
                    args: Some(effective_args.clone()),
                });
                let d = wait_decision(approval_rx, cancel, None, Some(&req.withdrawn)).await;
                let _ = event_tx.send(AgentEvent::ApprovalResolved);
                d
            };

            let resp = match decision {
                Some(ApprovalDecision::Approve) => {
                    json!({ "behavior": "allow", "updatedInput": input })
                }
                Some(ApprovalDecision::ApproveRest) => {
                    *auto_approve = true;
                    json!({ "behavior": "allow", "updatedInput": input })
                }
                Some(ApprovalDecision::AllowPrefix(prefix)) => {
                    bash_allowlist.push(prefix);
                    json!({ "behavior": "allow", "updatedInput": input })
                }
                Some(ApprovalDecision::AllowTool(tool)) => {
                    allowed_tools.push(tool);
                    json!({ "behavior": "allow", "updatedInput": input })
                }
                Some(ApprovalDecision::Deny | ApprovalDecision::Elicitation(_)) => {
                    let msg = if plan_mode {
                        PLAN_MODE_REFUSAL
                    } else {
                        "Denied by user."
                    };
                    json!({ "behavior": "deny", "message": msg })
                }
                None => {
                    json!({ "behavior": "deny", "message": "Cancelled by user.", "interrupt": true })
                }
            };
            send_control_response(stdin, &req.request_id, resp).await;
        }
        "elicitation" => {
            let mut params = req.request.clone();
            if params.get("mode").is_none() {
                params["mode"] = json!("form");
            }
            let _ = event_tx.send(AgentEvent::ElicitationRequest { params });
            let decision = wait_decision(approval_rx, cancel, None, Some(&req.withdrawn)).await;
            let _ = event_tx.send(AgentEvent::ApprovalResolved);
            let resp = crate::agent::elicitation::response(decision);
            send_control_response(stdin, &req.request_id, resp).await;
        }
        other => {
            let _ = write_line(
                stdin,
                &json!({
                    "type": "control_response",
                    "response": {
                        "subtype": "error",
                        "request_id": req.request_id,
                        "error": format!("Unsupported control request subtype: {other}"),
                    }
                }),
            )
            .await;
        }
    }
}

async fn send_control_response(
    stdin: &Arc<AsyncMutex<ChildStdin>>,
    request_id: &str,
    response: Value,
) {
    let _ = write_line(
        stdin,
        &json!({
            "type": "control_response",
            "response": {
                "subtype": "success",
                "request_id": request_id,
                "response": response,
            }
        }),
    )
    .await;
}

async fn control_request(
    stdin: &Arc<AsyncMutex<ChildStdin>>,
    next_id: &Arc<AtomicI64>,
    pending: &PendingControl,
    request: Value,
) -> Result<Value, String> {
    let (_, rx) = send_control(stdin, next_id, pending, request).await?;
    match rx.await {
        Ok(r) => r,
        Err(_) => Err("Claude Code closed the connection.".to_string()),
    }
}

/// [`control_request`] bounded by `limit`; on timeout the pending slot is dropped so an
/// unanswered request does not keep the connection looking busy to the idle reaper.
async fn control_request_within(
    stdin: &Arc<AsyncMutex<ChildStdin>>,
    next_id: &Arc<AtomicI64>,
    pending: &PendingControl,
    request: Value,
    limit: Duration,
) -> Result<Value, String> {
    let (id, rx) = send_control(stdin, next_id, pending, request).await?;
    match tokio::time::timeout(limit, rx).await {
        Ok(Ok(r)) => r,
        Ok(Err(_)) => Err("Claude Code closed the connection.".to_string()),
        Err(_) => {
            pending.lock().await.remove(&id);
            Err("Claude Code did not answer in time.".to_string())
        }
    }
}

async fn send_control(
    stdin: &Arc<AsyncMutex<ChildStdin>>,
    next_id: &Arc<AtomicI64>,
    pending: &PendingControl,
    request: Value,
) -> Result<(String, oneshot::Receiver<Result<Value, String>>), String> {
    let id = format!("oxi_ctrl_{}", next_id.fetch_add(1, Ordering::SeqCst));
    let (tx, rx) = oneshot::channel();
    pending.lock().await.insert(id.clone(), tx);
    if let Err(e) = write_line(
        stdin,
        &json!({
            "type": "control_request",
            "request_id": id,
            "request": request,
        }),
    )
    .await
    {
        pending.lock().await.remove(&id);
        return Err(e);
    }
    Ok((id, rx))
}

/// Take the frames held between turns, which also settles their announcement.
fn take_idle(handles: &ClaudeHandles) -> Vec<Value> {
    let held: Vec<Value> = handles
        .idle
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .drain(..)
        .collect();
    autonomous::claim(handles.held.session_key());
    held
}

/// Keep the connection's running background commands current, so the chat shows it waits on
/// them after the turn ended. Returns `true` when one finished.
fn track_background_task(
    msg: &Value,
    activity: &reaper::Activity,
    tasks: &mut HashMap<String, String>,
) -> bool {
    match msg["subtype"].as_str() {
        Some("task_started") if msg["is_backgrounded"].as_bool() == Some(true) => {
            if let (Some(task), Some(tool)) = (
                msg["task_id"].as_str().filter(|s| !s.is_empty()),
                msg["tool_use_id"].as_str().filter(|s| !s.is_empty()),
            ) {
                tasks.insert(task.to_owned(), tool.to_owned());
                activity.add_background_tools(vec![reaper::BackgroundTool {
                    tool_call_id: tool.to_owned(),
                    label: msg["description"].as_str().unwrap_or("").trim().to_owned(),
                    started_at: std::time::Instant::now(),
                }]);
            }
            false
        }
        Some("task_notification") => {
            let tool = msg["task_id"]
                .as_str()
                .and_then(|task| tasks.remove(task))
                .or_else(|| msg["tool_use_id"].as_str().map(str::to_owned));
            tool.is_some_and(|tool| activity.finish_background_tool(&tool))
        }
        // The full list of running tasks: anything missing ended (e.g. stopped with TaskStop).
        Some("background_tasks_changed") => {
            let running: std::collections::HashSet<&str> = msg["tasks"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|t| t["task_id"].as_str())
                .collect();
            let mut finished = false;
            tasks.retain(|task, tool| {
                let keep = running.contains(task.as_str());
                if !keep {
                    finished |= activity.finish_background_tool(tool);
                }
                keep
            });
            finished
        }
        _ => false,
    }
}

async fn read_loop(
    stdout: tokio::process::ChildStdout,
    pending_ctrl: PendingControl,
    turn_ctx: Arc<AsyncMutex<Option<ClaudeTurnCtx>>>,
    alive: Arc<AtomicBool>,
    activity: Arc<reaper::Activity>,
    session_id: Arc<Mutex<String>>,
    idle: (IdleQueue, String),
) {
    let (idle, session_key) = idle;
    // Background task id -> the tool call that started it.
    let mut tasks: HashMap<String, String> = HashMap::new();
    let mut lines = BufReader::new(stdout).lines();
    let mut chunks = String::new();
    let mut chunk_count = 0usize;
    let flush_chunks = |chunks: &mut String, count: &mut usize| {
        if *count > 0 {
            activity_log::log(
                ActivityKind::Acp,
                format!("← claude · {count} stream events"),
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
        let msg_type = msg["type"].as_str().unwrap_or("");
        if activity_log::is_enabled() {
            if msg_type == "stream_event" {
                chunks.push_str(&line);
                chunks.push('\n');
                chunk_count += 1;
            } else {
                flush_chunks(&mut chunks, &mut chunk_count);
                let title = match msg["subtype"].as_str() {
                    Some(sub) => format!("← claude {msg_type}/{sub}"),
                    None => format!("← claude {msg_type}"),
                };
                activity_log::log_json(ActivityKind::Acp, title, &msg);
            }
        }
        activity.touch();

        if let Some(sid) = msg["session_id"].as_str().filter(|s| !s.is_empty()) {
            let mut slot = session_id.lock().unwrap_or_else(|e| e.into_inner());
            if slot.is_empty() {
                *slot = sid.to_owned();
            }
        }

        match msg_type {
            "control_response" => {
                let resp = &msg["response"];
                let rid = resp["request_id"].as_str().unwrap_or("");
                if let Some(tx) = pending_ctrl.lock().await.remove(rid) {
                    if resp["subtype"].as_str() == Some("error") {
                        let err = resp["error"]
                            .as_str()
                            .unwrap_or("Claude control request failed")
                            .to_owned();
                        let _ = tx.send(Err(err));
                    } else {
                        let _ = tx.send(Ok(resp.get("response").cloned().unwrap_or(Value::Null)));
                    }
                }
            }
            "keep_alive" => {}
            _ => {
                let mut slot = turn_ctx.lock().await;
                match slot.as_mut() {
                    Some(ctx) => route(ctx, &msg),
                    None => hold_idle(&idle, &session_key, &msg),
                }
            }
        }
        // After routing: a finished task is held for the turn that shows it before the UI
        // stops showing the command as running.
        if msg_type == "system" && track_background_task(&msg, &activity, &mut tasks) {
            autonomous::wake();
        }
    }

    flush_chunks(&mut chunks, &mut chunk_count);
    alive.store(false, Ordering::SeqCst);
    let mut p = pending_ctrl.lock().await;
    for (_, tx) in p.drain() {
        let _ = tx.send(Err("Claude Code closed the connection.".to_string()));
    }
    drop(p);
    if let Some(ctx) = turn_ctx.lock().await.as_mut()
        && let Some(done) = ctx.done_tx.take()
    {
        let _ = done.send(ClaudeTurnResult {
            ok: false,
            error: Some("Claude Code closed the connection.".into()),
            stop_notice: None,
        });
    }
}

/// Build CLI flags for `claude --output-format stream-json --input-format stream-json`.
pub(super) fn build_claude_args(
    extra_args: &[String],
    mcp_config_path: &Path,
    resume_id: Option<&str>,
    model: &str,
    effort: &str,
) -> Vec<String> {
    let mut args = vec![
        "--output-format".into(),
        "stream-json".into(),
        "--input-format".into(),
        "stream-json".into(),
        "--verbose".into(),
        "--include-partial-messages".into(),
        "--permission-prompt-tool".into(),
        "stdio".into(),
        "--mcp-config".into(),
        mcp_config_path.to_string_lossy().into_owned(),
    ];
    if let Some(id) = resume_id.filter(|s| !s.is_empty()) {
        args.push("--resume".into());
        args.push(id.to_owned());
    }
    let m = model.trim();
    if !m.is_empty() && m != "default" {
        args.push("--model".into());
        args.push(m.to_owned());
    }
    let e = effort.trim();
    if !e.is_empty() && e != "default" {
        args.push("--effort".into());
        args.push(e.to_owned());
    }
    // Preserve custom user flags (e.g. --chrome, --add-dir) without duplicating managed flags.
    let mut skip_next = false;
    for arg in extra_args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if matches!(
            arg.as_str(),
            "--output-format" | "--input-format" | "--permission-prompt-tool"
        ) {
            skip_next = true;
            continue;
        }
        if arg == "--verbose" || arg == "--include-partial-messages" {
            continue;
        }
        args.push(arg.clone());
    }
    args
}

/// Write a temporary `--mcp-config` JSON file for Claude Code containing `oxi` + user servers.
pub(super) fn write_claude_mcp_config(
    dir: &Path,
    servers: &[McpServerConfig],
    evidence_dir: Option<&Path>,
    delegation_dir: Option<&Path>,
) -> Result<PathBuf, String> {
    let mut map = serde_json::Map::new();
    if let Some(oxi_mcp::ServerSpec {
        command,
        args,
        env: env_pairs,
    }) = oxi_mcp::server_spec(evidence_dir, delegation_dir)
    {
        let env: serde_json::Map<String, Value> = env_pairs
            .into_iter()
            .map(|(k, v)| (k, Value::String(v)))
            .collect();
        map.insert(
            "oxi".into(),
            json!({
                "type": "stdio",
                "command": command,
                "args": args,
                "env": env,
            }),
        );
    }
    for s in mcp_servers::usable(servers) {
        let name = s.name.trim();
        if name.is_empty() || name == "oxi" {
            continue;
        }
        match s.transport {
            McpTransport::Stdio => {
                let (command, args) = if cfg!(windows) {
                    let mut wrapped = vec!["/D".into(), "/S".into(), "/C".into()];
                    wrapped.push(s.command.trim().to_owned());
                    wrapped.extend(s.args.iter().cloned());
                    ("cmd".to_owned(), wrapped)
                } else {
                    (s.command.trim().to_owned(), s.args.clone())
                };
                let env: serde_json::Map<String, Value> = s
                    .env_pairs()
                    .into_iter()
                    .map(|(k, v)| (k, Value::String(v)))
                    .collect();
                map.insert(
                    name.to_owned(),
                    json!({
                        "type": "stdio",
                        "command": command,
                        "args": args,
                        "env": env,
                    }),
                );
            }
            McpTransport::Http => {
                let mut headers = serde_json::Map::new();
                let token = s.bearer_token.trim();
                if !token.is_empty() {
                    headers.insert("Authorization".into(), json!(format!("Bearer {token}")));
                }
                map.insert(
                    name.to_owned(),
                    json!({
                        "type": "http",
                        "url": s.url.trim(),
                        "headers": headers,
                    }),
                );
            }
        }
    }
    let path = dir.join("claude-mcp-config.json");
    let bytes = serde_json::to_vec_pretty(&json!({ "mcpServers": map }))
        .map_err(|e| format!("Could not serialize Claude MCP config: {e}"))?;
    crate::fsutil::write_atomic(&path, &bytes)
        .map_err(|e| format!("Could not write Claude MCP config: {e}"))?;
    Ok(path)
}

fn build_claude_user_content(
    text: &str,
    images: &[(String, Vec<u8>)],
    resources: &[PathBuf],
) -> Vec<Value> {
    let mut blocks = Vec::new();
    let mut full_text = text.to_owned();
    for path in resources {
        if let Ok(meta) = std::fs::metadata(path)
            && meta.is_file()
            && meta.len() <= super::EMBED_RESOURCE_MAX_BYTES
            && let Ok(content) = std::fs::read_to_string(path)
        {
            full_text.push_str(&format!(
                "\n\n<attached_file path=\"{}\">\n{}\n</attached_file>",
                path.display(),
                content
            ));
            continue;
        }
        full_text.push_str(&format!(
            "\n\n<attached_path>{}</attached_path>",
            path.display()
        ));
    }
    if !full_text.trim().is_empty() {
        blocks.push(json!({ "type": "text", "text": full_text }));
    }
    for (mime, data) in images {
        let b64 = base64::engine::general_purpose::STANDARD.encode(data);
        blocks.push(json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": mime,
                "data": b64,
            }
        }));
    }
    if blocks.is_empty() {
        blocks.push(json!({ "type": "text", "text": "" }));
    }
    blocks
}

fn parse_claude_models(v: &Value) -> Vec<ClaudeModel> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let value = m["value"].as_str()?.to_owned();
            let display_name = m["displayName"]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or(&value)
                .to_owned();
            let description = m["description"].as_str().unwrap_or("").to_owned();
            let efforts: Vec<String> = m["supportedEffortLevels"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
            Some(ClaudeModel {
                value,
                display_name,
                description,
                efforts,
                supports_fast: m["supportsFastMode"].as_bool() == Some(true),
                supports_auto: m["supportsAutoMode"].as_bool() == Some(true),
            })
        })
        .collect()
}

fn parse_claude_commands(v: &Value) -> Vec<AcpSlashCommand> {
    let mut cmds: Vec<AcpSlashCommand> = v
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| {
            let name = c["name"].as_str()?.trim().trim_start_matches('/');
            if name.is_empty() || name.contains(char::is_whitespace) {
                return None;
            }
            let description = c["description"].as_str().unwrap_or("").trim().to_owned();
            let hint = c["argumentHint"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned);
            Some(AcpSlashCommand {
                name: name.to_owned(),
                description,
                hint,
            })
        })
        .collect();
    cmds.sort_by(|a, b| a.name.cmp(&b.name));
    cmds.dedup_by(|a, b| a.name == b.name);
    cmds
}

/// Map a Claude tool name to `(oxi_name, acp_kind, is_read_only)`.
fn map_claude_tool_name(raw: &str) -> (String, &'static str, bool) {
    if let Some(oxi) = oxi_mcp::oxi_tool_name(raw) {
        let ro = matches!(
            oxi,
            "todo_write"
                | oxi_mcp::SCRATCHPAD_READ
                | super::chat_archive::TOOL
                | super::chat_history::TOOL
                | crate::delegation::STATUS
        );
        return (oxi.to_owned(), "other", ro);
    }
    match raw {
        "Bash" => ("bash".into(), "execute", false),
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => ("edit".into(), "edit", false),
        "Read" => ("read".into(), "read", true),
        "Grep" | "Glob" | "LS" => ("grep".into(), "search", true),
        "WebFetch" | "WebSearch" => ("web_fetch".into(), "fetch", true),
        "Agent" | "Task" | "TodoWrite" | "ExitPlanMode" => ("task".into(), "think", true),
        other => (other.to_owned(), "other", false),
    }
}

fn summarize_claude_tool_input(
    raw_name: &str,
    input: Option<&Value>,
) -> (String, Option<String>, Vec<ToolLocation>) {
    let Some(inp) = input else {
        return (String::new(), None, Vec::new());
    };
    let mut locations = Vec::new();
    let mut diff = None;
    let file_path = inp["file_path"]
        .as_str()
        .or_else(|| inp["path"].as_str())
        .or_else(|| inp["notebook_path"].as_str());
    if let Some(p) = file_path
        && !p.is_empty()
    {
        let line = inp["offset"].as_u64();
        locations.push(ToolLocation {
            path: p.to_owned(),
            line,
        });
    }
    match raw_name {
        "Bash" => {
            let title = inp["description"]
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .or_else(|| inp["command"].as_str())
                .unwrap_or("Bash")
                .to_owned();
            return (title, None, locations);
        }
        "Edit" => {
            if let (Some(path), Some(old), Some(new)) = (
                file_path,
                inp["old_string"].as_str(),
                inp["new_string"].as_str(),
            ) {
                let d = crate::agent::tools::make_unified_diff(path, old, new);
                if !d.is_empty() {
                    diff = Some(d);
                }
            }
            let title = file_path
                .map(|p| format!("Edit {p}"))
                .unwrap_or_else(|| "Edit".into());
            return (title, diff, locations);
        }
        "Write" => {
            if let (Some(path), Some(content)) = (file_path, inp["content"].as_str()) {
                let d = crate::agent::tools::make_unified_diff(path, "", content);
                if !d.is_empty() {
                    diff = Some(d);
                }
            }
            let title = file_path
                .map(|p| format!("Write {p}"))
                .unwrap_or_else(|| "Write".into());
            return (title, diff, locations);
        }
        "Read" => {
            let title = file_path
                .map(|p| format!("Read {p}"))
                .unwrap_or_else(|| "Read".into());
            return (title, None, locations);
        }
        "Grep" | "Glob" => {
            let pat = inp["pattern"].as_str().unwrap_or("");
            let title = if pat.is_empty() {
                raw_name.to_owned()
            } else {
                format!("{raw_name} {pat}")
            };
            return (title, None, locations);
        }
        _ => {}
    }
    (raw_name.to_owned(), diff, locations)
}

fn is_claude_plan_file_edit(tool_name: &str, input: &Value) -> bool {
    if !matches!(tool_name, "Edit" | "Write" | "MultiEdit") {
        return false;
    }
    let Some(dir) = dirs::home_dir().map(|h| h.join(".claude").join("plans")) else {
        return false;
    };
    input["file_path"]
        .as_str()
        .is_some_and(|p| Path::new(p).starts_with(&dir))
}

fn is_computer_use_tool(tool_name: &str) -> bool {
    let synthetic = json!({
        "name": tool_name,
        "_meta": { "claudeCode": { "toolName": tool_name } }
    });
    crate::computer_use::is_acp_tool(&synthetic)
}

/// Convert Claude `AskUserQuestion` `questions` array into oxi's `elicitation/create` form schema.
pub(super) fn claude_questions_to_elicitation(questions: &[Value]) -> Value {
    let mut properties = serde_json::Map::new();
    for (idx, q) in questions.iter().enumerate() {
        let key = format!("question_{idx}");
        let header = q["header"].as_str().unwrap_or("");
        let prompt = q["question"].as_str().unwrap_or("");
        let title = if header.is_empty() {
            prompt.to_owned()
        } else {
            format!("{header}: {prompt}")
        };
        let multi = q["multiSelect"].as_bool() == Some(true);
        let opts: Vec<Value> = q["options"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|o| {
                let label = o["label"].as_str()?;
                let mut entry = json!({
                    "const": label,
                    "title": label,
                    "description": o["description"].as_str().unwrap_or(""),
                });
                if let Some(preview) = o["preview"].as_str().filter(|p| !p.is_empty()) {
                    entry["_meta"] = json!({
                        "_claude/askUserQuestionOption": { "preview": preview }
                    });
                }
                Some(entry)
            })
            .collect();
        if multi {
            properties.insert(
                key.clone(),
                json!({
                    "type": "array",
                    "title": title,
                    "items": { "anyOf": opts },
                }),
            );
        } else {
            properties.insert(
                key.clone(),
                json!({
                    "type": "string",
                    "title": title,
                    "oneOf": opts,
                }),
            );
        }
        properties.insert(
            format!("{key}_custom"),
            json!({
                "type": "string",
                "title": "Other (optional)",
            }),
        );
    }
    json!({
        "mode": "form",
        "message": "Claude has a question for you:",
        "requestedSchema": {
            "type": "object",
            "properties": properties,
            "required": [],
        }
    })
}

/// Convert oxi's `ApprovalDecision::Elicitation` back into Claude's `AskUserQuestion` `answers` map.
pub(super) fn elicitation_to_claude_answers(
    questions: &[Value],
    decision: Option<ApprovalDecision>,
) -> Option<Value> {
    let resp = crate::agent::elicitation::response(decision);
    if resp["action"].as_str() != Some("accept") {
        return None;
    }
    let content = resp["content"].as_object()?;
    let mut answers = serde_json::Map::new();
    for (idx, q) in questions.iter().enumerate() {
        let Some(qtext) = q["question"].as_str() else {
            continue;
        };
        let key = format!("question_{idx}");
        let custom = content
            .get(&format!("{key}_custom"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty());
        if let Some(c) = custom {
            answers.insert(qtext.to_owned(), json!(c));
            continue;
        }
        match content.get(&key) {
            Some(Value::String(s)) if !s.trim().is_empty() => {
                answers.insert(qtext.to_owned(), json!(s.trim()));
            }
            Some(Value::Array(arr)) => {
                let joined: Vec<&str> = arr.iter().filter_map(Value::as_str).collect();
                if !joined.is_empty() {
                    answers.insert(qtext.to_owned(), json!(joined.join(", ")));
                }
            }
            _ => {}
        }
    }
    Some(Value::Object(answers))
}

fn extract_tool_result_text(content: &Value) -> String {
    if let Some(s) = content.as_str() {
        return s.to_owned();
    }
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|b| b["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_claude_usage(v: &Value) -> Option<TokenUsage> {
    let usage = TokenUsage {
        input_tokens: v["input_tokens"].as_u64().unwrap_or(0),
        output_tokens: v["output_tokens"].as_u64().unwrap_or(0),
        cache_read_input_tokens: v["cache_read_input_tokens"].as_u64().unwrap_or(0),
        cache_creation_input_tokens: v["cache_creation_input_tokens"].as_u64().unwrap_or(0),
        ..TokenUsage::default()
    };
    (!usage.is_zero()).then_some(usage)
}

fn extract_model_usage_context(model_usage: &Value) -> Option<(u64, u64)> {
    let obj = model_usage.as_object()?;
    let mut best_window = 0u64;
    let mut best_used = 0u64;
    for entry in obj.values() {
        let window = entry["contextWindow"].as_u64().unwrap_or(0);
        let used = entry["inputTokens"].as_u64().unwrap_or(0)
            + entry["cacheReadInputTokens"].as_u64().unwrap_or(0)
            + entry["cacheCreationInputTokens"].as_u64().unwrap_or(0);
        if window > best_window {
            best_window = window;
            best_used = used;
        }
    }
    (best_window > 0 && best_used > 0).then_some((best_used, best_window))
}

fn claude_assistant_error_text(code: &str) -> &'static str {
    match code {
        "authentication_failed" => {
            "Claude Code authentication failed. Sign in again with `claude auth login`."
        }
        "billing_error" => "Claude Code billing error. Check your account or API credits.",
        "rate_limit" => "Claude Code rate limit exceeded. Try again later.",
        "overloaded" => "Claude API is temporarily overloaded. Try again shortly.",
        "model_not_found" => "Selected Claude model was not found.",
        "max_output_tokens" => "Claude reached its maximum output tokens.",
        _ => "Claude Code encountered an API error.",
    }
}

fn extract_result_error(msg: &Value) -> Option<String> {
    if let Some(arr) = msg["errors"].as_array() {
        let joined: Vec<&str> = arr
            .iter()
            .filter_map(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .collect();
        if !joined.is_empty() {
            return Some(joined.join("\n"));
        }
    }
    msg["result"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

// ── External session listing & import from `~/.claude/projects/<slug>/*.jsonl` ──

pub(super) async fn list_sessions(req: &AcpImport) -> Result<Vec<ExternalSession>, String> {
    let cwd = req.cwd.clone();
    tokio::task::spawn_blocking(move || list_sessions_in_dir(&cwd))
        .await
        .map_err(|e| format!("Could not list Claude sessions: {e}"))?
}

fn list_sessions_in_dir(cwd: &Path) -> Result<Vec<ExternalSession>, String> {
    let Some(dir) = workflows::claude_project_dir(cwd) else {
        return Ok(Vec::new());
    };
    list_sessions_from_project_dir(&dir, &sessions::known_ids())
}

pub(super) fn list_sessions_from_project_dir(
    dir: &Path,
    known: &std::collections::HashSet<String>,
) -> Result<Vec<ExternalSession>, String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(Vec::new());
    };
    let mut rows: Vec<(std::time::SystemTime, ExternalSession)> = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if stem.starts_with("agent-") || known.contains(stem) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if meta.len() == 0 {
            continue;
        }
        let modified = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        let Some(title) = extract_jsonl_title(&path) else {
            continue;
        };
        let updated_at = chrono::DateTime::<chrono::Utc>::from(modified).to_rfc3339();
        rows.push((
            modified,
            ExternalSession {
                session_id: stem.to_owned(),
                title,
                updated_at: Some(updated_at),
            },
        ));
    }
    rows.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    Ok(rows.into_iter().take(100).map(|(_, s)| s).collect())
}

fn extract_jsonl_title(path: &Path) -> Option<String> {
    use std::io::BufRead;
    let file = std::fs::File::open(path).ok()?;
    let reader = std::io::BufReader::new(file);
    let mut first_user: Option<String> = None;
    let mut custom_title: Option<String> = None;
    for line in reader.lines().take(200).flatten() {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if v["type"].as_str() == Some("custom-title")
            && let Some(t) = v["customTitle"].as_str().map(str::trim)
            && !t.is_empty()
        {
            custom_title = Some(t.to_owned());
        }
        if first_user.is_none()
            && v["type"].as_str() == Some("user")
            && v["isMeta"].as_bool() != Some(true)
            && v["isSidechain"].as_bool() != Some(true)
        {
            let text = claude_message_text(&v["message"]["content"]);
            let clean = without_oxi_preamble(&text).trim().to_owned();
            if !clean.is_empty() {
                first_user = Some(crate::model::make_session_title(&clean));
            }
        }
    }
    custom_title.or(first_user)
}

pub(super) async fn load_session(
    req: &AcpImport,
    session_id: &str,
) -> Result<Vec<ChatMessage>, String> {
    let cwd = req.cwd.clone();
    let sid = session_id.to_owned();
    tokio::task::spawn_blocking(move || {
        let dir = workflows::claude_project_dir(&cwd)
            .ok_or_else(|| "Could not locate Claude Code sessions directory.".to_string())?;
        let path = dir.join(format!("{sid}.jsonl"));
        load_session_from_jsonl(&path)
    })
    .await
    .map_err(|e| format!("Could not load Claude session: {e}"))?
}

/// Parse a Claude Code `.jsonl` session transcript into oxi `ChatMessage`s.
pub(super) fn load_session_from_jsonl(path: &Path) -> Result<Vec<ChatMessage>, String> {
    use std::io::BufRead;
    let file = std::fs::File::open(path)
        .map_err(|e| format!("Could not open Claude session transcript: {e}"))?;
    let reader = std::io::BufReader::new(file);
    let mut messages: Vec<ChatMessage> = Vec::new();
    let mut tool_names: HashMap<String, String> = HashMap::new();
    let mut hidden_tools: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Split on raw bytes so one malformed line (invalid UTF-8, or a half-written last line of a
    // live session) is skipped like any other unparsable line; only real read errors abort.
    for line in reader.split(b'\n') {
        let line = line.map_err(|e| format!("Could not read Claude session transcript: {e}"))?;
        let Ok(v) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        if v["isMeta"].as_bool() == Some(true) || v["isSidechain"].as_bool() == Some(true) {
            continue;
        }
        match v["type"].as_str() {
            Some("user") => {
                let content = &v["message"]["content"];
                // Tool results attach to the preceding assistant message.
                let mut had_tool_result = false;
                if let Some(arr) = content.as_array() {
                    for block in arr {
                        if block["type"].as_str() == Some("tool_result")
                            && let Some(id) = block["tool_use_id"].as_str()
                        {
                            had_tool_result = true;
                            // Results of hidden tools (ToolSearch) stay hidden, like live.
                            if hidden_tools.contains(id) {
                                continue;
                            }
                            if let Some(last) = messages
                                .last_mut()
                                .filter(|m| m.role == crate::model::MsgRole::Assistant)
                            {
                                let raw_name =
                                    tool_names.get(id).map(String::as_str).unwrap_or("tool");
                                let (name, kind, _) = map_claude_tool_name(raw_name);
                                let status = if block["is_error"].as_bool() == Some(true) {
                                    ToolStatus::Failed
                                } else {
                                    ToolStatus::Completed
                                };
                                merge_tool_update(
                                    &mut last.blocks,
                                    ToolUpdate {
                                        tool_call_id: id.to_owned(),
                                        name,
                                        args: None,
                                        output: extract_tool_result_text(&block["content"]),
                                        diff: None,
                                        output_truncated: false,
                                        metadata: ToolMetadata {
                                            kind: kind.to_owned(),
                                            name: Some(raw_name.to_owned()),
                                            status,
                                            ..ToolMetadata::default()
                                        },
                                    },
                                );
                            }
                        }
                    }
                }
                let text = claude_message_text(content);
                let clean = without_oxi_preamble(&text).trim().to_owned();
                if !clean.is_empty() && !had_tool_result {
                    if let Some(last) = messages.last_mut()
                        && last.role == crate::model::MsgRole::Assistant
                    {
                        last.finish_streaming();
                    }
                    messages.push(ChatMessage::user(&clean, &[]));
                }
            }
            Some("assistant") => {
                let content = &v["message"]["content"];
                let Some(arr) = content.as_array() else {
                    continue;
                };
                if messages
                    .last()
                    .is_none_or(|m| m.role != crate::model::MsgRole::Assistant)
                {
                    messages.push(ChatMessage::streaming_assistant(None));
                }
                let Some(msg) = messages.last_mut() else {
                    continue;
                };
                for block in arr {
                    match block["type"].as_str() {
                        Some("text") => {
                            if let Some(t) = block["text"].as_str()
                                && !t.is_empty()
                            {
                                msg.append_answer(t);
                            }
                        }
                        Some("thinking") => {
                            if let Some(t) = block["thinking"].as_str()
                                && !t.is_empty()
                            {
                                msg.append_thinking(t);
                            }
                        }
                        Some("tool_use") => {
                            if let (Some(id), Some(raw_name)) =
                                (block["id"].as_str(), block["name"].as_str())
                            {
                                if raw_name == "ToolSearch" {
                                    hidden_tools.insert(id.to_owned());
                                    continue;
                                }
                                tool_names.insert(id.to_owned(), raw_name.to_owned());
                                let (name, kind, _) = map_claude_tool_name(raw_name);
                                let input = block.get("input").cloned();
                                let (title, diff, locations) =
                                    summarize_claude_tool_input(raw_name, input.as_ref());
                                apply_tool_update(
                                    &mut msg.blocks,
                                    ToolUpdate {
                                        tool_call_id: id.to_owned(),
                                        name,
                                        args: input,
                                        output: String::new(),
                                        diff,
                                        output_truncated: false,
                                        metadata: ToolMetadata {
                                            title,
                                            kind: kind.to_owned(),
                                            name: Some(raw_name.to_owned()),
                                            locations,
                                            status: ToolStatus::Completed,
                                        },
                                    },
                                );
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    for msg in &mut messages {
        if msg.role == crate::model::MsgRole::Assistant {
            msg.finish_streaming();
        }
    }
    if messages.is_empty() {
        return Err("The Claude Code session has no messages to import.".into());
    }
    Ok(messages)
}

fn claude_message_text(content: &Value) -> String {
    if let Some(s) = content.as_str() {
        return s.to_owned();
    }
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|b| (b["type"].as_str() == Some("text")).then(|| b["text"].as_str())?)
        .collect::<Vec<_>>()
        .join("\n")
}
