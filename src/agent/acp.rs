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
//! capability, so the agent runs shell commands itself and reports them as tool-call updates;
//! their output streams through the `_meta.terminal_output_delta` extension Claude Code and
//! Codex share. oxi's MCP servers are handed to the agent's session, and plan-mode turns use
//! the agent's own plan mode when it has one (see [`modes`]).

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::mpsc::{Receiver as StdReceiver, Sender as StdSender};
use std::time::Duration;

use base64::Engine as _;
use serde_json::{Value, json};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex as AsyncMutex, mpsc, oneshot};

use super::approval::{ApprovalDecision, ApprovalPolicy, PLAN_MODE_REFUSAL};
use super::events::{AgentEvent, TokenUsage};
use crate::settings::McpServerConfig;

#[path = "acp/client_fs.rs"]
mod client_fs;

#[path = "acp/commands.rs"]
mod commands;

#[path = "acp/config_options.rs"]
pub mod config_options;
use client_fs::fs_write_text;
pub use commands::{AcpSlashCommand, available as available_commands};

#[path = "acp/install.rs"]
mod install;

#[path = "acp/mcp_servers.rs"]
mod mcp_servers;

#[path = "acp/modes.rs"]
mod modes;
#[path = "acp/terminals.rs"]
mod terminals;

#[path = "acp/todo_mcp.rs"]
pub mod todo_mcp;
use modes::{ModeState, SharedModes, set_user_option, sync_plan_mode};

#[path = "acp/permissions.rs"]
mod permissions;
use permissions::{PermReq, handle_permission};

#[path = "acp/rpc.rs"]
mod rpc;
use rpc::{CommandsKey, drain_stderr, read_loop, request, write_line};

#[path = "acp/sessions.rs"]
mod sessions;

#[path = "acp/update_events.rs"]
mod update_events;
use update_events::UpdateState;

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
    /// Configured model id, applied through the agent-advertised ACP model config option.
    pub model: String,
    /// Thinking/reasoning level, applied through the advertised `thought_level` option.
    pub effort: String,
    /// oxi's MCP servers, handed to the agent's session.
    pub mcp_servers: Vec<McpServerConfig>,
    /// The latest user message text.
    pub text: String,
    /// Transcript of the chat before the latest message. Sent ahead of the prompt only when the
    /// agent had to start a fresh session (no resumable one), so it still knows the conversation.
    pub history: String,
    /// Image attachments on the latest user message (`mime`, bytes).
    pub images: Vec<(String, Vec<u8>)>,
    /// Files and folders `@`-mentioned in the latest message, sent as ACP resources.
    pub resources: Vec<PathBuf>,
    /// Where translated agent events are delivered.
    pub event_tx: StdSender<AgentEvent>,
    /// Back-channel carrying the user's approval decisions.
    pub approval_rx: StdReceiver<ApprovalDecision>,
    /// Which permission requests should be routed through oxi's approval UI.
    pub approval_policy: ApprovalPolicy,
    /// `bash` (ACP `execute`) command prefixes that run without asking.
    pub bash_allowlist: Vec<String>,
    /// Cooperative cancellation for the turn.
    pub cancel: Arc<AtomicBool>,
    /// Plan mode: ask for a plan and refuse every permission request that could change things.
    pub plan_mode: bool,
}

/// Prepended to the user's message in plan mode. ACP agents run their own tools, so oxi can't
/// hide them; it asks for a plan and refuses the permission requests instead.
const ACP_PLAN_MODE_PREFIX: &str = "[Plan mode] Do not modify files or run commands in this turn. Investigate with read-only tools, then reply with a concrete, numbered implementation plan (files and functions to change, edge cases, how to verify). I will approve it before you implement.";

/// A request to launch (if needed) and initialize a session's agent without prompting, used to
/// warm the subprocess and discover the available model list for the UI.
pub struct AcpWarm {
    pub session_key: String,
    pub cwd: PathBuf,
    pub command_line: String,
    pub env: Vec<(String, String)>,
    /// Configured model id and thinking level, applied through ACP session config options.
    pub model: String,
    pub effort: String,
    pub mcp_servers: Vec<McpServerConfig>,
}

/// Everything a session's agent subprocess is launched and set up with. Borrowed field by field
/// from the request, so the `!Sync` approval receiver is never held across an await.
struct LaunchSpec<'a> {
    session_key: &'a str,
    command_line: &'a str,
    cwd: &'a std::path::Path,
    env: &'a [(String, String)],
    model: &'a str,
    effort: &'a str,
    mcp_servers: &'a [McpServerConfig],
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
    SetOption {
        req: AcpSetOption,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Close {
        session_key: String,
    },
}

/// The user picked a value for one of the agent's config options in the composer.
pub struct AcpSetOption {
    pub session_key: String,
    pub command_line: String,
    pub config_id: String,
    /// A string for select options, a bool for boolean ones.
    pub value: Value,
    /// When the option is the model or the effort: oxi's setting it now corresponds to, so the
    /// next prompt keeps this subprocess instead of relaunching for a "changed" model.
    pub model: Option<String>,
    pub effort: Option<String>,
}

/// Cheap to clone; every clone talks to the same background ACP-management task.
#[derive(Clone)]
pub struct AcpManager {
    tx: mpsc::UnboundedSender<AcpCommand>,
}

impl AcpManager {
    /// Spawn the manager's dedicated background thread on the shared runtime. Call once at app
    /// startup; the returned handle is safe to share and call from any thread.
    pub fn spawn() -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<AcpCommand>();
        std::thread::spawn(move || {
            let Ok(rt) = crate::runtime::runtime() else {
                return;
            };
            rt.block_on(async move {
                // Keep the managed npm adapters current without involving the user.
                tokio::spawn(async {
                    loop {
                        install::update_installed().await;
                        tokio::time::sleep(Duration::from_secs(60 * 60)).await;
                    }
                });
                let conns: Arc<AsyncMutex<HashMap<String, Conn>>> =
                    Arc::new(AsyncMutex::new(HashMap::new()));
                while let Some(cmd) = rx.recv().await {
                    match cmd {
                        AcpCommand::Prompt { req, reply } => {
                            let conns = conns.clone();
                            tokio::spawn(async move {
                                // Only Sync fields are borrowed across the await here; the
                                // `!Sync` approval Receiver stays owned by `req`.
                                let spec = LaunchSpec {
                                    session_key: &req.session_key,
                                    command_line: &req.command_line,
                                    cwd: &req.cwd,
                                    env: &req.env,
                                    model: &req.model,
                                    effort: &req.effort,
                                    mcp_servers: &req.mcp_servers,
                                };
                                let ensured = ensure_conn(&conns, &spec).await;
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
                                let spec = LaunchSpec {
                                    session_key: &req.session_key,
                                    command_line: &req.command_line,
                                    cwd: &req.cwd,
                                    env: &req.env,
                                    model: &req.model,
                                    effort: &req.effort,
                                    mcp_servers: &req.mcp_servers,
                                };
                                let ensured = ensure_conn(&conns, &spec).await;
                                let _ = reply.send(ensured.map(|h| h.available_models));
                            });
                        }
                        AcpCommand::SetOption { req, reply } => {
                            let conns = conns.clone();
                            tokio::spawn(async move {
                                let _ = reply.send(set_option(&conns, req).await);
                            });
                        }
                        AcpCommand::Close { session_key } => {
                            // Dropping the Conn kills the subprocess (kill_on_drop).
                            conns.lock().await.remove(&session_key);
                            sessions::forget(&session_key);
                            config_options::forget(&session_key);
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

    /// Set one of the agent's config options on the session's live subprocess.
    pub async fn set_option(&self, req: AcpSetOption) -> Result<(), String> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(AcpCommand::SetOption {
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
    /// Requested session configuration. A change relaunches the session so every adapter starts
    /// with a deterministic model and reasoning level.
    model: String,
    effort: String,
    /// The usable MCP servers the session was set up with; a change sets it up again.
    mcp_servers: Vec<McpServerConfig>,
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
    /// Set when the session was created blank rather than resumed; the first prompt then carries
    /// oxi's transcript of the chat so far.
    needs_history: Arc<AtomicBool>,
    /// The agent's session modes, kept current by the reader task.
    modes: SharedModes,
    /// The agent accepts embedded `resource` blocks (`promptCapabilities.embeddedContext`).
    embedded_context: bool,
    /// Held for a whole prompt turn. A prompt sent right after Stop waits here until the
    /// cancelled turn has wound down, instead of having its context cleared by it.
    turn_lock: Arc<AsyncMutex<()>>,
}

/// The event/approval context for the in-flight prompt, shared with the reader task so it can
/// route notifications and forward permission requests.
struct PromptCtx {
    session_id: String,
    cwd: PathBuf,
    plan_mode: bool,
    updates: UpdateState,
    event_tx: StdSender<AgentEvent>,
    perm_tx: mpsc::UnboundedSender<PermReq>,
}

impl PromptCtx {
    fn write_text_file(&self, params: &Value) -> Result<(), String> {
        if params["sessionId"].as_str() != Some(self.session_id.as_str()) {
            return Err("fs/write_text_file: session is not the active prompt".into());
        }
        if self.plan_mode {
            return Err(PLAN_MODE_REFUSAL.into());
        }
        fs_write_text(params)
    }

    fn emit_notification(&mut self, params: &Value) {
        if params["sessionId"].as_str() == Some(self.session_id.as_str()) {
            self.updates.emit_update(&params["update"], &self.event_tx);
        }
    }
}

/// Return handles for the session's agent, launching + initializing it if there isn't already a
/// healthy subprocess (or if the launch command changed).
async fn ensure_conn(
    conns: &Arc<AsyncMutex<HashMap<String, Conn>>>,
    spec: &LaunchSpec<'_>,
) -> Result<ConnHandles, String> {
    let mcp_servers = mcp_servers::usable(spec.mcp_servers);
    {
        let map = conns.lock().await;
        if let Some(c) = map.get(spec.session_key)
            && c.alive.load(Ordering::SeqCst)
            && c.command_line == spec.command_line
            && c.model == spec.model
            && c.effort == spec.effort
            && c.mcp_servers == mcp_servers
        {
            return Ok(c.handles.clone());
        }
    }
    // A new subprocess (or one whose launch command / model / MCP servers changed): spawn it and
    // replace any previous entry, whose Conn is dropped here and killed (kill_on_drop). The
    // agent session itself is resumed, so the conversation carries over.
    let conn = spawn_conn(spec, mcp_servers).await?;
    let handles = conn.handles.clone();
    conns
        .lock()
        .await
        .insert(spec.session_key.to_string(), conn);
    Ok(handles)
}

async fn set_option(
    conns: &Arc<AsyncMutex<HashMap<String, Conn>>>,
    req: AcpSetOption,
) -> Result<(), String> {
    let handles = {
        let mut map = conns.lock().await;
        let Some(conn) = map
            .get_mut(&req.session_key)
            .filter(|c| c.alive.load(Ordering::SeqCst) && c.command_line == req.command_line)
        else {
            return Err("the agent is not running".into());
        };
        if let Some(model) = req.model {
            conn.model = model;
        }
        if let Some(effort) = req.effort {
            conn.effort = effort;
        }
        conn.handles.clone()
    };
    set_user_option(
        &handles.stdin,
        &handles.next_id,
        &handles.pending,
        &handles.session_id,
        &handles.modes,
        &req.config_id,
        req.value,
    )
    .await
}

/// `PATH` for tool subprocesses launched from a GUI session (login-shell `PATH` merged with
/// oxi's own). Shared with MCP stdio servers, which need `npx`/`uvx` just like ACP agents.
pub(crate) async fn subprocess_path() -> Option<String> {
    install::shell_path().await
}

fn build_command(command_line: &str) -> Command {
    #[cfg(windows)]
    {
        // cmd.exe doesn't follow the MSVCRT quoting rules `Command::arg` escapes for, so a quoted
        // path (like the managed adapter's) would reach it as `\"C:\...\"` and fail to launch.
        // Pass the line verbatim; `/S` strips exactly the outer quotes added here.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut c = Command::new("cmd");
        c.raw_arg(format!("/D /S /C \"{command_line}\""))
            .creation_flags(CREATE_NO_WINDOW);
        c
    }
    #[cfg(not(windows))]
    {
        let mut c = Command::new("sh");
        c.arg("-c").arg(command_line);
        c
    }
}

fn launch_error(command_line: &str, error: &std::io::Error) -> String {
    let command = command_line.trim();
    let first = command.split_whitespace().next().unwrap_or("ACP command");
    let hint = match first {
        "agent" | "cursor-agent" => {
            " Install Cursor CLI and ensure `agent` is available in PATH (Cursor: Install CLI Command)."
        }
        "npx" => {
            " Install Node.js/npm so `npx` is available, or install the ACP adapter globally and set its command here."
        }
        "codex-acp" => " Install it with `npm install -g @agentclientprotocol/codex-acp`.",
        _ => " Check that the executable is installed and available in PATH.",
    };
    format!("Could not launch ACP agent `{command}`: {error}.{hint}")
}

async fn spawn_conn(
    spec: &LaunchSpec<'_>,
    mcp_servers: Vec<McpServerConfig>,
) -> Result<Conn, String> {
    let LaunchSpec {
        session_key,
        command_line,
        cwd,
        env,
        model,
        effort,
        ..
    } = *spec;
    let command_line = command_line.trim();
    if command_line.is_empty() {
        return Err(
            "ACP agent command is empty. Configure it in Settings → Models & providers."
                .to_string(),
        );
    }
    if !cwd.is_dir() {
        return Err(format!(
            "ACP workspace does not exist or is not a directory: {}",
            cwd.display()
        ));
    }
    // Default npx commands launch oxi's managed, auto-updated install of the adapter instead.
    let launch_line = install::resolve_command(command_line).await;
    let mut cmd = build_command(&launch_line);
    cmd.current_dir(cwd);
    if let Some(path) = install::shell_path().await {
        cmd.env("PATH", path);
    }
    // The adapter refuses to start when it detects it's nested inside another Claude Code
    // session (the `CLAUDECODE` guard). oxi is a separate app, so strip it to let ACP work
    // even when oxi itself was launched from a Claude Code terminal.
    cmd.env_remove("CLAUDECODE");
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| launch_error(command_line, &e))?;
    let stdin = child.stdin.take().ok_or("ACP: child has no stdin")?;
    let stdout = child.stdout.take().ok_or("ACP: child has no stdout")?;
    let stderr = child.stderr.take().ok_or("ACP: child has no stderr")?;

    let stdin = Arc::new(AsyncMutex::new(stdin));
    let pending: Pending = Arc::new(AsyncMutex::new(HashMap::new()));
    let prompt_ctx: Arc<AsyncMutex<Option<PromptCtx>>> = Arc::new(AsyncMutex::new(None));
    let next_id = Arc::new(AtomicI64::new(1));
    let alive = Arc::new(AtomicBool::new(true));
    let modes: SharedModes = Arc::default();

    let stderr_tail = Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
    let stderr_task = tokio::spawn(drain_stderr(stderr, stderr_tail.clone()));
    let read_task = tokio::spawn(read_loop(
        stdout,
        pending.clone(),
        prompt_ctx.clone(),
        stdin.clone(),
        alive.clone(),
        modes.clone(),
        CommandsKey {
            command_line: command_line.to_string(),
            cwd: cwd.to_path_buf(),
        },
    ));

    // initialize
    let init_params = json!({
        "protocolVersion": PROTOCOL_VERSION,
        "clientCapabilities": {
            "fs": { "readTextFile": true, "writeTextFile": true },
            "terminal": true,
            // Fast mode and similar switches arrive as `type: "boolean"` options.
            "session": { "configOptions": { "boolean": {} } },
            // Command output as appended chunks on the tool call (Claude Code and Codex). Codex
            // sends command output only this way.
            "_meta": { "terminal_output_delta": true }
        }
    });
    let init = tokio::time::timeout(
        Duration::from_secs(30),
        request(&stdin, &next_id, &pending, "initialize", init_params),
    )
    .await
    .map_err(|_| {
        format!(
            "ACP agent `{command_line}` did not initialize within 30 seconds. Check that it is installed and can start from a terminal."
        )
    })?
    .map_err(|e| format!("ACP initialize failed for `{command_line}`: {e}"));
    let init = match init {
        Ok(init) => init,
        Err(e) => {
            // The agent usually explains why it exited on stderr; give it a moment to flush.
            tokio::time::sleep(Duration::from_millis(300)).await;
            let tail = stderr_tail.lock().unwrap_or_else(|e| e.into_inner());
            if tail.is_empty() {
                return Err(e);
            }
            let tail: Vec<&str> = tail.iter().map(String::as_str).collect();
            return Err(format!("{e}\n{}", tail.join("\n")));
        }
    };

    let caps = &init["agentCapabilities"];
    let mut mcp = mcp_servers::to_acp(&mcp_servers, caps);
    if let (Some(list), Some(todo)) = (mcp.as_array_mut(), todo_mcp::acp_entry()) {
        list.push(todo);
    }
    let resumed = match sessions::lookup(session_key, command_line, cwd) {
        Some(id) => resume_session(&stdin, &next_id, &pending, caps, cwd, &id, &mcp)
            .await
            .map(|res| (id, res)),
        None => None,
    };
    let (session_id, res, needs_history) = match resumed {
        Some((id, res)) => (id, res, false),
        None => {
            let (id, res) = new_session(&stdin, &next_id, &pending, cwd, &mcp).await?;
            (id, res, true)
        }
    };
    sessions::remember(session_key, command_line, cwd, &session_id);
    let available_models = parse_available_models(&res);
    let mut config_options = res.get("configOptions").cloned().unwrap_or(Value::Null);
    config_options = set_matching_config_option(
        &stdin,
        &next_id,
        &pending,
        &session_id,
        &config_options,
        "model",
        model,
    )
    .await?;
    let config_options = set_thought_level(
        &stdin,
        &next_id,
        &pending,
        &session_id,
        &config_options,
        effort,
    )
    .await?;
    *modes.lock().unwrap_or_else(|e| e.into_inner()) = ModeState::new(
        config_options,
        res.get("modes").cloned().unwrap_or(Value::Null),
    )
    .published_as(session_key, command_line);

    let handles = ConnHandles {
        stdin,
        next_id,
        pending,
        prompt_ctx,
        session_id,
        available_models,
        needs_history: Arc::new(AtomicBool::new(needs_history)),
        modes,
        embedded_context: caps["promptCapabilities"]["embeddedContext"].as_bool() == Some(true),
        turn_lock: Arc::new(AsyncMutex::new(())),
    };
    Ok(Conn {
        command_line: command_line.to_string(),
        model: model.to_string(),
        effort: effort.to_string(),
        mcp_servers,
        alive,
        handles,
        _child: child,
        _read_task: read_task,
        _stderr_task: stderr_task,
    })
}

/// Create a blank agent session, returning its id and the raw `session/new` response.
async fn new_session(
    stdin: &Arc<AsyncMutex<ChildStdin>>,
    next_id: &Arc<AtomicI64>,
    pending: &Pending,
    cwd: &std::path::Path,
    mcp_servers: &Value,
) -> Result<(String, Value), String> {
    let params = json!({
        "cwd": cwd.to_string_lossy(),
        "mcpServers": mcp_servers
    });
    let res = tokio::time::timeout(
        Duration::from_secs(30),
        request(stdin, next_id, pending, "session/new", params),
    )
    .await
    .map_err(|_| "ACP session setup timed out after 30 seconds".to_string())?
    .map_err(|e| format!("ACP session/new failed: {e}"))?;
    let session_id = res
        .get("sessionId")
        .and_then(|v| v.as_str())
        .ok_or("ACP session/new returned no sessionId")?
        .to_string();
    Ok((session_id, res))
}

/// Reattach to a previous agent session so it keeps its conversation context: `session/resume`
/// when advertised (no history replay), else `session/load` (the agent replays the history as
/// `session/update` notifications, which are dropped because no prompt is in flight). Returns
/// `None` when the agent supports neither or the session can no longer be restored.
async fn resume_session(
    stdin: &Arc<AsyncMutex<ChildStdin>>,
    next_id: &Arc<AtomicI64>,
    pending: &Pending,
    caps: &Value,
    cwd: &std::path::Path,
    session_id: &str,
    mcp_servers: &Value,
) -> Option<Value> {
    let params = json!({
        "sessionId": session_id,
        "cwd": cwd.to_string_lossy(),
        "mcpServers": mcp_servers
    });
    let mut methods = Vec::new();
    if caps["sessionCapabilities"]["resume"].is_object() {
        methods.push("session/resume");
    }
    if caps["loadSession"].as_bool() == Some(true) {
        methods.push("session/load");
    }
    for method in methods {
        let res = tokio::time::timeout(
            Duration::from_secs(60),
            request(stdin, next_id, pending, method, params.clone()),
        )
        .await;
        match res {
            Ok(Ok(res)) => return Some(res),
            Ok(Err(e)) => eprintln!("[acp] {method} {session_id} failed: {e}"),
            Err(_) => eprintln!("[acp] {method} {session_id} timed out"),
        }
    }
    None
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

/// Set a select config option when the agent advertised it and the requested value is valid.
/// Unknown/unsupported values degrade gracefully to the adapter's current default.
async fn set_matching_config_option(
    stdin: &Arc<AsyncMutex<ChildStdin>>,
    next_id: &Arc<AtomicI64>,
    pending: &Pending,
    session_id: &str,
    config_options: &Value,
    config_id: &str,
    value: &str,
) -> Result<Value, String> {
    let value = value.trim();
    if value.is_empty() || value == "default" {
        return Ok(config_options.clone());
    }
    let Some(option) = config_options.as_array().and_then(|opts| {
        opts.iter().find(|o| {
            o.get("id").and_then(Value::as_str) == Some(config_id)
                || (config_id == "thought_level"
                    && o.get("category").and_then(Value::as_str) == Some("thought_level"))
        })
    }) else {
        return Ok(config_options.clone());
    };
    let supported = option
        .get("options")
        .and_then(Value::as_array)
        .is_some_and(|values| {
            values
                .iter()
                .any(|v| v.get("value").and_then(Value::as_str) == Some(value))
        });
    if !supported {
        return Ok(config_options.clone());
    }
    let actual_id = option
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or(config_id);
    let response = request(
        stdin,
        next_id,
        pending,
        "session/set_config_option",
        json!({"sessionId": session_id, "configId": actual_id, "value": value}),
    )
    .await
    .map_err(|e| format!("ACP could not set {actual_id} to {value}: {e}"))?;
    Ok(response
        .get("configOptions")
        .cloned()
        .unwrap_or_else(|| config_options.clone()))
}

async fn set_thought_level(
    stdin: &Arc<AsyncMutex<ChildStdin>>,
    next_id: &Arc<AtomicI64>,
    pending: &Pending,
    session_id: &str,
    config_options: &Value,
    effort: &str,
) -> Result<Value, String> {
    let Some(options) = config_options.as_array() else {
        return Ok(config_options.clone());
    };
    let config_id = options
        .iter()
        .find(|o| o.get("category").and_then(Value::as_str) == Some("thought_level"))
        .and_then(|o| o.get("id"))
        .and_then(Value::as_str)
        .or_else(|| {
            ["thought_level", "reasoning_effort", "effort"]
                .into_iter()
                .find(|id| {
                    options
                        .iter()
                        .any(|o| o.get("id").and_then(Value::as_str) == Some(*id))
                })
        });
    let Some(config_id) = config_id else {
        return Ok(config_options.clone());
    };
    set_matching_config_option(
        stdin,
        next_id,
        pending,
        session_id,
        config_options,
        config_id,
        effort,
    )
    .await
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
    let AcpPrompt {
        cwd,
        command_line,
        text,
        history,
        images,
        resources,
        event_tx,
        mut approval_rx,
        approval_policy,
        mut bash_allowlist,
        cancel,
        plan_mode,
        ..
    } = req;

    let _turn = handles.turn_lock.clone().lock_owned().await;
    if cancel.load(Ordering::SeqCst) {
        let _ = reply.send(Ok(()));
        return;
    }
    let (perm_tx, mut perm_rx) = mpsc::unbounded_channel::<PermReq>();
    *handles.prompt_ctx.lock().await = Some(PromptCtx {
        session_id: handles.session_id.clone(),
        cwd: cwd.clone(),
        plan_mode,
        updates: UpdateState::default(),
        event_tx: event_tx.clone(),
        perm_tx,
    });
    let _ = event_tx.send(AgentEvent::AgentStart);

    // Agents only run a slash command when the prompt starts with it, so a command goes out
    // verbatim: plan mode is still enforced through the permission requests, and the replayed
    // history waits for the next regular message.
    let slash_command = is_advertised_command(&text, &command_line, &cwd);
    // A slash command may itself change the mode (Codex `/plan`), so it runs in whatever mode
    // the agent is in. Otherwise the agent's own plan mode is switched to match oxi's; agents
    // without one are asked for a plan in the prompt.
    let native_plan = !slash_command
        && sync_plan_mode(
            &handles.stdin,
            &handles.next_id,
            &handles.pending,
            &handles.session_id,
            &handles.modes,
            plan_mode,
        )
        .await;
    let text = if plan_mode && !slash_command && !native_plan {
        format!("{ACP_PLAN_MODE_PREFIX}\n\n{text}")
    } else {
        text
    };
    let text = if !slash_command && handles.needs_history.swap(false, Ordering::SeqCst) {
        with_history(&history, &text)
    } else {
        text
    };
    let prompt_params = json!({
        "sessionId": handles.session_id,
        "prompt": build_prompt_blocks(&text, &images, &resource_blocks(&resources, handles.embedded_context)),
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
                        &mut bash_allowlist,
                        plan_mode,
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
    if slash_command {
        handles
            .modes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .note_user_command();
    }
    if let Ok(res) = &result
        && let Some(usage) = prompt_usage(&res["usage"])
    {
        let _ = event_tx.send(AgentEvent::ExternalUsage(usage));
    }
    let _ = reply.send(result.map(|_| ()));
}

/// Token usage from a `session/prompt` response. Agents disagree on whether `inputTokens`
/// includes the cached part (Claude Code: no, Codex: yes); when the total only adds up with
/// the cached tokens inside it, they are taken out to match [`TokenUsage::input_tokens`].
fn prompt_usage(usage: &Value) -> Option<TokenUsage> {
    let n = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
    let (input, output) = (n("inputTokens"), n("outputTokens"));
    let (read, write, total) = (
        n("cachedReadTokens"),
        n("cachedWriteTokens"),
        n("totalTokens"),
    );
    let cached = read + write;
    let input = if cached > 0 && total == input + output && input >= cached {
        input - cached
    } else {
        input
    };
    let usage = TokenUsage {
        input_tokens: input,
        output_tokens: output,
        cache_read_input_tokens: read,
        cache_creation_input_tokens: write,
        ..TokenUsage::default()
    };
    (!usage.is_zero()).then_some(usage)
}

/// Whether `text` invokes one of the agent's advertised slash commands (`/name` or
/// `/name input`), as opposed to e.g. a message that merely starts with a path.
fn is_advertised_command(text: &str, command_line: &str, cwd: &std::path::Path) -> bool {
    let Some(rest) = text.trim_start().strip_prefix('/') else {
        return false;
    };
    let name = rest.split_whitespace().next().unwrap_or("");
    !name.is_empty()
        && commands::available(command_line, cwd)
            .iter()
            .any(|c| c.name == name)
}

/// Prefix the user's message with the earlier conversation, for a blank session standing in for
/// one the agent could not resume.
fn with_history(history: &str, text: &str) -> String {
    if history.trim().is_empty() {
        return text.to_string();
    }
    format!(
        "<conversation_history>\nThis conversation started earlier; the agent session was restarted, so here is the transcript so far. Treat it as context you already have: continue from it and do not redo earlier work unless asked.\n\n{}\n</conversation_history>\n\n{text}",
        history.trim()
    )
}

/// Text files up to this size are embedded in the prompt; bigger ones are linked.
const EMBED_RESOURCE_MAX_BYTES: u64 = 64 * 1024;

/// ACP blocks for `@`-mentioned paths: small text files embedded (when the agent accepts
/// embedded context), everything else as a `resource_link` the agent can open itself.
fn resource_blocks(paths: &[PathBuf], embedded_context: bool) -> Vec<Value> {
    paths
        .iter()
        .filter_map(|path| {
            let uri = url::Url::from_file_path(path).ok()?.to_string();
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.to_string_lossy().into_owned());
            let meta = std::fs::metadata(path).ok()?;
            if embedded_context
                && meta.is_file()
                && meta.len() <= EMBED_RESOURCE_MAX_BYTES
                && let Ok(text) = std::fs::read_to_string(path)
            {
                return Some(json!({
                    "type": "resource",
                    "resource": { "uri": uri, "mimeType": "text/plain", "text": text }
                }));
            }
            let mut link = json!({ "type": "resource_link", "uri": uri, "name": name });
            if meta.is_file() {
                link["size"] = json!(meta.len());
            }
            Some(link)
        })
        .collect()
}

/// Build the ACP prompt content blocks for a user turn.
fn build_prompt_blocks(text: &str, images: &[(String, Vec<u8>)], resources: &[Value]) -> Value {
    let mut blocks = Vec::new();
    if !text.trim().is_empty() {
        blocks.push(json!({ "type": "text", "text": text }));
    }
    for (mime, data) in images {
        let b64 = base64::engine::general_purpose::STANDARD.encode(data);
        blocks.push(json!({ "type": "image", "mimeType": mime, "data": b64 }));
    }
    blocks.extend(resources.iter().cloned());
    if blocks.is_empty() {
        blocks.push(json!({ "type": "text", "text": "" }));
    }
    Value::Array(blocks)
}

#[cfg(test)]
#[path = "acp/tests.rs"]
mod tests;
