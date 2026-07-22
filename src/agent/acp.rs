//! Agent Client Protocol (ACP) client for driving Claude Code as an external agent.
//!
//! Every other provider in oxi is an HTTP LLM API where *oxi* runs the agent loop (streaming,
//! tools, approval). ACP inverts that: the agent (Claude Code, via the
//! `@zed-industries/claude-code-acp` adapter) runs as a **subprocess** and owns the loop, while
//! oxi is the ACP *client*. Communication is newline-delimited JSON-RPC 2.0 over the child's
//! stdin/stdout.
//!
//! [`AcpManager`] mirrors [`crate::compute::TunnelManager`]: a dedicated background thread with
//! its own Tokio runtime that keeps **one long-lived subprocess per oxi session** so multi-turn
//! context lives in the agent. A per-turn caller submits a prompt via [`AcpManager::prompt`] and
//! blocks until the turn finishes; the agent's `session/update` notifications are translated into
//! the same [`AgentEvent`] stream every other provider produces, so the UI is unchanged.
//!
//! Client responsibilities we implement: `fs/read_text_file`, `fs/write_text_file`, and
//! `session/request_permission` (routed through oxi's approval gate). We advertise no terminal
//! capability, so Claude Code runs shell commands itself and reports them as tool-call updates.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::mpsc::{Receiver as StdReceiver, Sender as StdSender, TryRecvError};
use std::time::Duration;

use base64::Engine as _;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex as AsyncMutex, mpsc, oneshot};

use super::approval::{ApprovalDecision, ApprovalPolicy};
use super::events::AgentEvent;

#[path = "acp/client_fs.rs"]
mod client_fs;
use client_fs::{fs_read_text, fs_write_text};

#[path = "acp/update_events.rs"]
mod update_events;
use update_events::emit_update;

/// ACP protocol major version we speak.
const PROTOCOL_VERSION: i64 = 1;

/// Outstanding client→agent requests, keyed by JSON-RPC id, awaiting a response.
type Pending = Arc<AsyncMutex<HashMap<i64, oneshot::Sender<Result<Value, String>>>>>;

/// One prompt turn submitted to the manager. Carries the channels the background runtime uses
/// to stream events back and to ask the UI for approval, plus everything needed to (re)launch
/// and address the agent subprocess.
pub struct AcpPrompt {
    /// Stable key identifying the oxi session (usually its session-file path). One agent
    /// subprocess is kept alive per key.
    pub session_key: String,
    /// Working directory the agent session operates in.
    pub cwd: PathBuf,
    /// Shell command line launching the ACP agent (e.g. `npx @zed-industries/claude-code-acp`).
    pub command_line: String,
    /// Extra environment variables for the subprocess (e.g. `ANTHROPIC_API_KEY`).
    pub env: Vec<(String, String)>,
    /// Configured model id (e.g. `sonnet`/`haiku`/`default`). Applied via `session/set_model`
    /// before the prompt when it matches one of the agent's available models. Empty = leave the
    /// agent on its current model.
    pub model: String,
    /// The latest user message text.
    pub text: String,
    /// Image attachments on the latest user message (`mime`, bytes).
    pub images: Vec<(String, Vec<u8>)>,
    /// Where translated agent events are delivered.
    pub event_tx: StdSender<AgentEvent>,
    /// Back-channel carrying the user's approval decisions.
    pub approval_rx: StdReceiver<ApprovalDecision>,
    /// Which permission requests should be routed through oxi's approval UI.
    pub approval_policy: ApprovalPolicy,
    /// Cooperative cancellation for the turn.
    pub cancel: Arc<AtomicBool>,
}

/// A request to launch (if needed) and initialize a session's agent without prompting, used to
/// warm the subprocess and discover the available model list for the UI.
pub struct AcpWarm {
    pub session_key: String,
    pub cwd: PathBuf,
    pub command_line: String,
    pub env: Vec<(String, String)>,
    /// Configured model id; applied to the session at creation via `session/set_model` so the
    /// warmed subprocess already runs on the selected model before the first prompt.
    pub model: String,
}

enum AcpCommand {
    Prompt {
        req: AcpPrompt,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Warm {
        req: AcpWarm,
        reply: oneshot::Sender<Result<Vec<String>, String>>,
    },
    Close {
        session_key: String,
    },
}

/// Cheap to clone; every clone talks to the same background ACP-management task.
#[derive(Clone)]
pub struct AcpManager {
    tx: mpsc::UnboundedSender<AcpCommand>,
}

impl AcpManager {
    /// Spawn the manager's dedicated background thread + Tokio runtime. Call once at app
    /// startup; the returned handle is safe to share and call from any thread.
    pub fn spawn() -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<AcpCommand>();
        std::thread::spawn(move || {
            let rt = match tokio::runtime::Runtime::new() {
                Ok(rt) => rt,
                Err(_) => return,
            };
            rt.block_on(async move {
                let conns: Arc<AsyncMutex<HashMap<String, Conn>>> =
                    Arc::new(AsyncMutex::new(HashMap::new()));
                while let Some(cmd) = rx.recv().await {
                    match cmd {
                        AcpCommand::Prompt { req, reply } => {
                            let conns = conns.clone();
                            tokio::spawn(async move {
                                // Only Sync fields are borrowed across the await here; the
                                // `!Sync` approval Receiver stays owned by `req`.
                                let ensured = ensure_conn(
                                    &conns,
                                    &req.session_key,
                                    &req.command_line,
                                    &req.cwd,
                                    &req.env,
                                    &req.model,
                                )
                                .await;
                                match ensured {
                                    Ok(handles) => run_prompt(handles, req, reply).await,
                                    Err(e) => {
                                        let _ = reply.send(Err(e));
                                    }
                                }
                            });
                        }
                        AcpCommand::Warm { req, reply } => {
                            let conns = conns.clone();
                            tokio::spawn(async move {
                                let ensured = ensure_conn(
                                    &conns,
                                    &req.session_key,
                                    &req.command_line,
                                    &req.cwd,
                                    &req.env,
                                    &req.model,
                                )
                                .await;
                                let _ = reply.send(ensured.map(|h| h.available_models));
                            });
                        }
                        AcpCommand::Close { session_key } => {
                            // Dropping the Conn kills the subprocess (kill_on_drop).
                            conns.lock().await.remove(&session_key);
                        }
                    }
                }
            });
        });
        Self { tx }
    }

    /// Run one prompt turn against the session's agent, launching the subprocess on first use.
    /// Returns when the turn finishes (or errors). Events stream over `req.event_tx` while this
    /// is in flight.
    pub async fn prompt(&self, req: AcpPrompt) -> Result<(), String> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(AcpCommand::Prompt {
                req,
                reply: reply_tx,
            })
            .map_err(|_| "ACP manager is not running".to_string())?;
        reply_rx
            .await
            .map_err(|_| "ACP manager dropped the request".to_string())?
    }

    /// Launch + initialize the session's agent without prompting and return its available model
    /// ids. Reuses an already-warm subprocess. Used to populate the model dropdown and to spin
    /// the agent up in the background when the Claude Code provider is selected.
    pub async fn warm(&self, req: AcpWarm) -> Result<Vec<String>, String> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(AcpCommand::Warm {
                req,
                reply: reply_tx,
            })
            .map_err(|_| "ACP manager is not running".to_string())?;
        reply_rx
            .await
            .map_err(|_| "ACP manager dropped the request".to_string())?
    }

    /// Tear down the subprocess for a session (e.g. when its tab is closed). No-op if none.
    pub fn close(&self, session_key: &str) {
        let _ = self.tx.send(AcpCommand::Close {
            session_key: session_key.to_string(),
        });
    }
}

/// A live agent subprocess plus the shared plumbing to talk to it. Dropping this kills the
/// child (via `kill_on_drop`) and ends the reader tasks.
struct Conn {
    command_line: String,
    /// Model this subprocess was launched with (via `ANTHROPIC_MODEL`). A change means the
    /// subprocess must be relaunched, since the model is fixed at process start.
    model: String,
    alive: Arc<AtomicBool>,
    handles: ConnHandles,
    // Kept alive for their side effects; never read directly.
    _child: Child,
    _read_task: tokio::task::JoinHandle<()>,
    _stderr_task: tokio::task::JoinHandle<()>,
}

/// Cloneable handles for addressing an existing [`Conn`] from a prompt task.
#[derive(Clone)]
struct ConnHandles {
    stdin: Arc<AsyncMutex<ChildStdin>>,
    next_id: Arc<AtomicI64>,
    pending: Pending,
    prompt_ctx: Arc<AsyncMutex<Option<PromptCtx>>>,
    session_id: String,
    /// Model ids the agent advertised for this session (from the `session/new` response).
    available_models: Vec<String>,
}

/// The event/approval context for the in-flight prompt, shared with the reader task so it can
/// route notifications and forward permission requests.
struct PromptCtx {
    event_tx: StdSender<AgentEvent>,
    perm_tx: mpsc::UnboundedSender<PermReq>,
}

/// A `session/request_permission` request forwarded from the reader task to the prompt task.
struct PermReq {
    id: Value,
    params: Value,
}

/// Return handles for the session's agent, launching + initializing it if there isn't already a
/// healthy subprocess (or if the launch command changed).
async fn ensure_conn(
    conns: &Arc<AsyncMutex<HashMap<String, Conn>>>,
    session_key: &str,
    command_line: &str,
    cwd: &std::path::Path,
    env: &[(String, String)],
    model: &str,
) -> Result<ConnHandles, String> {
    {
        let map = conns.lock().await;
        if let Some(c) = map.get(session_key)
            && c.alive.load(Ordering::SeqCst)
            && c.command_line == command_line
            && c.model == model
        {
            return Ok(c.handles.clone());
        }
    }
    // A new subprocess (or one whose launch command / model changed): spawn it and replace any
    // previous entry, whose Conn is dropped here and killed (kill_on_drop).
    let conn = spawn_conn(command_line, cwd, env, model).await?;
    let handles = conn.handles.clone();
    conns.lock().await.insert(session_key.to_string(), conn);
    Ok(handles)
}

fn build_command(command_line: &str) -> Command {
    #[cfg(windows)]
    {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(command_line);
        c
    }
    #[cfg(not(windows))]
    {
        let mut c = Command::new("sh");
        c.arg("-c").arg(command_line);
        c
    }
}

async fn spawn_conn(
    command_line: &str,
    cwd: &std::path::Path,
    env: &[(String, String)],
    model: &str,
) -> Result<Conn, String> {
    let mut cmd = build_command(command_line);
    cmd.current_dir(cwd);
    // The adapter refuses to start when it detects it's nested inside another Claude Code
    // session (the `CLAUDECODE` guard). oxi is a separate app, so strip it to let ACP work
    // even when oxi itself was launched from a Claude Code terminal.
    cmd.env_remove("CLAUDECODE");
    for (k, v) in env {
        cmd.env(k, v);
    }
    // The model is fixed at process start via Claude Code's `ANTHROPIC_MODEL` (the current
    // adapter has no runtime model-switch method). Accepts an alias (`sonnet`, `opus`) or a full
    // id (`claude-sonnet-5`).
    if !model.trim().is_empty() {
        cmd.env("ANTHROPIC_MODEL", model.trim());
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to launch ACP agent `{}`: {e}", command_line.trim()))?;
    let stdin = child.stdin.take().ok_or("ACP: child has no stdin")?;
    let stdout = child.stdout.take().ok_or("ACP: child has no stdout")?;
    let stderr = child.stderr.take().ok_or("ACP: child has no stderr")?;

    let stdin = Arc::new(AsyncMutex::new(stdin));
    let pending: Pending = Arc::new(AsyncMutex::new(HashMap::new()));
    let prompt_ctx: Arc<AsyncMutex<Option<PromptCtx>>> = Arc::new(AsyncMutex::new(None));
    let next_id = Arc::new(AtomicI64::new(1));
    let alive = Arc::new(AtomicBool::new(true));

    let stderr_task = tokio::spawn(drain_stderr(stderr));
    let read_task = tokio::spawn(read_loop(
        stdout,
        pending.clone(),
        prompt_ctx.clone(),
        stdin.clone(),
        alive.clone(),
    ));

    // initialize
    let init_params = json!({
        "protocolVersion": PROTOCOL_VERSION,
        "clientCapabilities": {
            "fs": { "readTextFile": true, "writeTextFile": true },
            "terminal": false
        }
    });
    request(&stdin, &next_id, &pending, "initialize", init_params)
        .await
        .map_err(|e| format!("ACP initialize failed: {e}"))?;

    // session/new
    let new_params = json!({
        "cwd": cwd.to_string_lossy(),
        "mcpServers": []
    });
    let res = request(&stdin, &next_id, &pending, "session/new", new_params)
        .await
        .map_err(|e| format!("ACP session/new failed: {e}"))?;
    let session_id = res
        .get("sessionId")
        .and_then(|v| v.as_str())
        .ok_or("ACP session/new returned no sessionId")?
        .to_string();
    let available_models = parse_available_models(&res);

    let handles = ConnHandles {
        stdin,
        next_id,
        pending,
        prompt_ctx,
        session_id,
        available_models,
    };
    Ok(Conn {
        command_line: command_line.to_string(),
        model: model.to_string(),
        alive,
        handles,
        _child: child,
        _read_task: read_task,
        _stderr_task: stderr_task,
    })
}

/// Extract the selectable model ids from a `session/new` response, supporting both adapter
/// shapes: the current adapter exposes them under `configOptions` (a `model` select option),
/// while the older `@zed-industries/claude-code-acp` used `models.availableModels`.
fn parse_available_models(res: &Value) -> Vec<String> {
    if let Some(opts) = res.get("configOptions").and_then(|c| c.as_array())
        && let Some(model_opt) = opts
            .iter()
            .find(|o| o.get("id").and_then(|v| v.as_str()) == Some("model"))
        && let Some(values) = model_opt.get("options").and_then(|o| o.as_array())
    {
        let ids: Vec<String> = values
            .iter()
            .filter_map(|o| o.get("value").and_then(|v| v.as_str()).map(String::from))
            .collect();
        if !ids.is_empty() {
            return ids;
        }
    }
    res.get("models")
        .and_then(|m| m.get("availableModels"))
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("modelId").and_then(|v| v.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Drive one `session/prompt` turn: set the prompt context, send the prompt, and pump agent
/// permission requests + cancellation until the agent reports a stop reason.
async fn run_prompt(
    handles: ConnHandles,
    req: AcpPrompt,
    reply: oneshot::Sender<Result<(), String>>,
) {
    // Destructure into owned locals so nothing borrows the `!Sync` approval Receiver across an
    // await — an async fn holds all its params for the whole future, so `&Receiver`/`&Sender`
    // params would make this future `!Send` and unspawnable.
    // `model` is applied at subprocess launch via `ANTHROPIC_MODEL` (see `spawn_conn`); a model
    // change relaunches the subprocess through `ensure_conn`, so there's nothing to do per turn.
    let AcpPrompt {
        text,
        images,
        event_tx,
        mut approval_rx,
        approval_policy,
        cancel,
        ..
    } = req;

    let (perm_tx, mut perm_rx) = mpsc::unbounded_channel::<PermReq>();
    *handles.prompt_ctx.lock().await = Some(PromptCtx {
        event_tx: event_tx.clone(),
        perm_tx,
    });
    let _ = event_tx.send(AgentEvent::AgentStart);

    let prompt_params = json!({
        "sessionId": handles.session_id,
        "prompt": build_prompt_blocks(&text, &images),
    });
    let id = handles.next_id.fetch_add(1, Ordering::SeqCst);
    let (rtx, mut rrx) = oneshot::channel::<Result<Value, String>>();
    handles.pending.lock().await.insert(id, rtx);
    let send = write_line(
        &handles.stdin,
        &json!({"jsonrpc":"2.0","id":id,"method":"session/prompt","params":prompt_params}),
    )
    .await;
    if let Err(e) = send {
        *handles.prompt_ctx.lock().await = None;
        let _ = reply.send(Err(e));
        return;
    }

    let mut auto_approve = false;
    let mut cancel_sent = false;
    let result: Result<Value, String> = loop {
        tokio::select! {
            biased;
            r = &mut rrx => {
                break r.unwrap_or_else(|_| Err("ACP connection closed".to_string()));
            }
            maybe = perm_rx.recv() => {
                if let Some(pr) = maybe {
                    handle_permission(
                        &handles.stdin,
                        event_tx.clone(),
                        &mut approval_rx,
                        approval_policy,
                        &cancel,
                        &mut auto_approve,
                        pr,
                    )
                    .await;
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(150)) => {
                if !cancel_sent && cancel.load(Ordering::SeqCst) {
                    cancel_sent = true;
                    let _ = write_line(
                        &handles.stdin,
                        &json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId": handles.session_id}}),
                    ).await;
                }
            }
        }
    };
    *handles.prompt_ctx.lock().await = None;
    let _ = reply.send(result.map(|_| ()));
}

/// Resolve one forwarded `session/request_permission` request: ask the UI (unless approval is
/// disabled or already auto-approved), then answer the agent with the selected option.
#[allow(clippy::too_many_arguments)]
async fn handle_permission(
    stdin: &Arc<AsyncMutex<ChildStdin>>,
    event_tx: StdSender<AgentEvent>,
    approval_rx: &mut StdReceiver<ApprovalDecision>,
    approval_policy: ApprovalPolicy,
    cancel: &Arc<AtomicBool>,
    auto_approve: &mut bool,
    pr: PermReq,
) {
    let (name, args) = permission_name_args(&pr.params["toolCall"]);
    let options = pr.params["options"].as_array().cloned().unwrap_or_default();

    let decision = if *auto_approve || !approval_policy.requires_approval(&name) {
        Some(ApprovalDecision::Approve)
    } else {
        let _ = event_tx.send(AgentEvent::ApprovalRequest { name, args });
        wait_decision(approval_rx, cancel).await
    };

    let outcome = match decision {
        None => json!({ "outcome": "cancelled" }),
        Some(d) => {
            let wanted: &[&str] = match d {
                ApprovalDecision::Approve => &["allow_once", "allow"],
                ApprovalDecision::ApproveRest => {
                    *auto_approve = true;
                    &["allow_always", "allow_once", "allow"]
                }
                ApprovalDecision::Deny => &["reject_once", "reject"],
            };
            let option_id = pick_option(&options, wanted);
            json!({ "outcome": "selected", "optionId": option_id })
        }
    };
    let _ = write_line(
        stdin,
        &json!({"jsonrpc":"2.0","id": pr.id, "result": { "outcome": outcome }}),
    )
    .await;
}

/// Poll the approval back-channel until a decision arrives or the turn is cancelled.
async fn wait_decision(
    rx: &mut StdReceiver<ApprovalDecision>,
    cancel: &Arc<AtomicBool>,
) -> Option<ApprovalDecision> {
    loop {
        if cancel.load(Ordering::SeqCst) {
            return None;
        }
        match rx.try_recv() {
            Ok(d) => return Some(d),
            Err(TryRecvError::Empty) => tokio::time::sleep(Duration::from_millis(80)).await,
            Err(TryRecvError::Disconnected) => return None,
        }
    }
}

/// Choose a permission `optionId` from the offered options, preferring the given option `kind`s
/// in order, then any allow/reject match, then the first option.
fn pick_option(options: &[Value], wanted_kinds: &[&str]) -> String {
    for want in wanted_kinds {
        if let Some(id) = options
            .iter()
            .find(|o| o.get("kind").and_then(|k| k.as_str()) == Some(*want))
            .and_then(|o| o.get("optionId").and_then(|v| v.as_str()))
        {
            return id.to_string();
        }
    }
    // Fall back to any option whose kind starts with the same allow/reject prefix.
    let prefix = if wanted_kinds.iter().any(|k| k.starts_with("allow")) {
        "allow"
    } else {
        "reject"
    };
    options
        .iter()
        .find(|o| {
            o.get("kind")
                .and_then(|k| k.as_str())
                .is_some_and(|k| k.starts_with(prefix))
        })
        .or_else(|| options.first())
        .and_then(|o| o.get("optionId").and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_string()
}

/// Map an ACP `toolCall` object to the (name, args) shape oxi's approval UI expects.
fn permission_name_args(tool: &Value) -> (String, Option<Value>) {
    let kind = tool.get("kind").and_then(|k| k.as_str()).unwrap_or("");
    let title = tool.get("title").and_then(|t| t.as_str()).unwrap_or("");
    let name = match kind {
        "execute" => "bash".to_string(),
        "edit" | "delete" | "move" => "edit".to_string(),
        "" => {
            if title.is_empty() {
                "tool".to_string()
            } else {
                title.to_string()
            }
        }
        other => other.to_string(),
    };
    (name, tool.get("rawInput").cloned())
}

/// Build the ACP prompt content blocks for a user turn.
fn build_prompt_blocks(text: &str, images: &[(String, Vec<u8>)]) -> Value {
    let mut blocks = Vec::new();
    if !text.trim().is_empty() {
        blocks.push(json!({ "type": "text", "text": text }));
    }
    for (mime, data) in images {
        let b64 = base64::engine::general_purpose::STANDARD.encode(data);
        blocks.push(json!({ "type": "image", "mimeType": mime, "data": b64 }));
    }
    if blocks.is_empty() {
        blocks.push(json!({ "type": "text", "text": "" }));
    }
    Value::Array(blocks)
}

/// Send a client→agent request and await its response.
async fn request(
    stdin: &Arc<AsyncMutex<ChildStdin>>,
    next_id: &Arc<AtomicI64>,
    pending: &Pending,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    let id = next_id.fetch_add(1, Ordering::SeqCst);
    let (tx, rx) = oneshot::channel();
    pending.lock().await.insert(id, tx);
    write_line(
        stdin,
        &json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}),
    )
    .await?;
    match rx.await {
        Ok(r) => r,
        Err(_) => Err("ACP connection closed".to_string()),
    }
}

async fn write_line(stdin: &Arc<AsyncMutex<ChildStdin>>, msg: &Value) -> Result<(), String> {
    let mut line = serde_json::to_string(msg).map_err(|e| e.to_string())?;
    line.push('\n');
    let mut guard = stdin.lock().await;
    guard
        .write_all(line.as_bytes())
        .await
        .map_err(|e| format!("ACP write failed: {e}"))?;
    guard
        .flush()
        .await
        .map_err(|e| format!("ACP flush failed: {e}"))?;
    Ok(())
}

/// Read newline-delimited JSON-RPC messages from the agent until stdout closes, dispatching
/// each. On close, fail every outstanding request so callers don't hang.
async fn read_loop(
    stdout: tokio::process::ChildStdout,
    pending: Pending,
    prompt_ctx: Arc<AsyncMutex<Option<PromptCtx>>>,
    stdin: Arc<AsyncMutex<ChildStdin>>,
    alive: Arc<AtomicBool>,
) {
    let mut lines = BufReader::new(stdout).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        dispatch(&line, &pending, &prompt_ctx, &stdin).await;
    }
    alive.store(false, Ordering::SeqCst);
    let mut p = pending.lock().await;
    for (_, tx) in p.drain() {
        let _ = tx.send(Err("ACP agent closed the connection".to_string()));
    }
}

async fn dispatch(
    line: &str,
    pending: &Pending,
    prompt_ctx: &Arc<AsyncMutex<Option<PromptCtx>>>,
    stdin: &Arc<AsyncMutex<ChildStdin>>,
) {
    let v: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return,
    };
    if let Some(method) = v.get("method").and_then(|m| m.as_str()) {
        let id = v.get("id").cloned().filter(|x| !x.is_null());
        if let Some(id) = id {
            let params = v.get("params").cloned().unwrap_or(Value::Null);
            handle_agent_request(method, id, params, stdin, prompt_ctx).await;
        } else if method == "session/update"
            && let Some(ctx) = prompt_ctx.lock().await.as_ref()
        {
            emit_update(&v["params"]["update"], &ctx.event_tx);
        }
        // Other notifications (available_commands_update, current_mode_update, …) are ignored.
    } else if let Some(id) = v.get("id").and_then(|x| x.as_i64()) {
        let waiter = pending.lock().await.remove(&id);
        if let Some(w) = waiter {
            if let Some(err) = v.get("error") {
                let msg = err
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("ACP error")
                    .to_string();
                let _ = w.send(Err(msg));
            } else {
                let _ = w.send(Ok(v.get("result").cloned().unwrap_or(Value::Null)));
            }
        }
    }
}

/// Handle an agent→client request (`fs/*`, `session/request_permission`).
async fn handle_agent_request(
    method: &str,
    id: Value,
    params: Value,
    stdin: &Arc<AsyncMutex<ChildStdin>>,
    prompt_ctx: &Arc<AsyncMutex<Option<PromptCtx>>>,
) {
    match method {
        "fs/read_text_file" => match fs_read_text(&params) {
            Ok(content) => reply_ok(stdin, id, json!({ "content": content })).await,
            Err(e) => reply_err(stdin, id, -32000, &e).await,
        },
        "fs/write_text_file" => match fs_write_text(&params) {
            Ok(()) => reply_ok(stdin, id, Value::Null).await,
            Err(e) => reply_err(stdin, id, -32000, &e).await,
        },
        "session/request_permission" => {
            let forwarded = {
                let ctx = prompt_ctx.lock().await;
                ctx.as_ref().map(|c| {
                    c.perm_tx.send(PermReq {
                        id: id.clone(),
                        params,
                    })
                })
            };
            // No active prompt (or the prompt task is gone): cancel the request so the agent
            // doesn't block forever.
            if !matches!(forwarded, Some(Ok(()))) {
                reply_ok(stdin, id, json!({ "outcome": { "outcome": "cancelled" } })).await;
            }
        }
        _ => {
            reply_err(
                stdin,
                id,
                -32601,
                &format!("method not supported: {method}"),
            )
            .await
        }
    }
}

async fn reply_ok(stdin: &Arc<AsyncMutex<ChildStdin>>, id: Value, result: Value) {
    let _ = write_line(stdin, &json!({"jsonrpc":"2.0","id":id,"result":result})).await;
}

async fn reply_err(stdin: &Arc<AsyncMutex<ChildStdin>>, id: Value, code: i64, message: &str) {
    let _ = write_line(
        stdin,
        &json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}}),
    )
    .await;
}

async fn drain_stderr(stderr: tokio::process::ChildStderr) {
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if !line.trim().is_empty() {
            eprintln!("[acp] {line}");
        }
    }
}

#[cfg(test)]
#[path = "acp/tests.rs"]
mod tests;
