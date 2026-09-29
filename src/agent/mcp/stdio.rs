//! stdio transport: newline-delimited JSON-RPC over a child process's stdin/stdout.
//!
//! A reader thread routes responses to the waiting caller by id, answers `ping`, and flags
//! `notifications/tools/list_changed`. Requests from several threads can be in flight at once;
//! each caller waits on its own channel with a timeout.

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use serde_json::{Value, json};

use super::Transport;
use crate::agent::activity_log::{self, ActivityKind};
use crate::settings::McpServerConfig;

/// Lines larger than this are dropped instead of buffered (a runaway server can't eat memory).
const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;
const STDERR_TAIL_LINES: usize = 20;

type Pending = Arc<Mutex<HashMap<u64, mpsc::Sender<Result<Value, String>>>>>;

pub(super) struct StdioTransport {
    name: String,
    child: Mutex<Option<Child>>,
    stdin: Arc<Mutex<ChildStdin>>,
    pending: Pending,
    next_id: AtomicU64,
    alive: Arc<AtomicBool>,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl StdioTransport {
    pub(super) fn spawn(
        cfg: &McpServerConfig,
        path: Option<String>,
        tools_stale: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        let name = cfg.name.clone();
        let mut cmd = build_command(&cfg.command, &cfg.args);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(path) = path {
            cmd.env("PATH", path);
        }
        for (k, v) in cfg.env_pairs() {
            cmd.env(k, v);
        }
        crate::agent::tools::isolate_process_group(&mut cmd);
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("could not start MCP server `{name}` ({}): {e}", cfg.command))?;
        let stdin = child.stdin.take().ok_or("MCP server has no stdin")?;
        let stdout = child.stdout.take().ok_or("MCP server has no stdout")?;
        let stderr = child.stderr.take().ok_or("MCP server has no stderr")?;

        let transport = Self {
            name: name.clone(),
            child: Mutex::new(Some(child)),
            stdin: Arc::new(Mutex::new(stdin)),
            pending: Arc::default(),
            next_id: AtomicU64::new(1),
            alive: Arc::new(AtomicBool::new(true)),
            stderr_tail: Arc::default(),
        };

        let tail = transport.stderr_tail.clone();
        let stderr_name = name.clone();
        std::thread::Builder::new()
            .name(format!("mcp-{name}-stderr"))
            .spawn(move || {
                for line in BufReader::new(stderr).lines() {
                    let Ok(line) = line else { break };
                    if line.trim().is_empty() {
                        continue;
                    }
                    activity_log::log(ActivityKind::Mcp, format!("{stderr_name} stderr"), &line);
                    let mut tail = lock(&tail);
                    if tail.len() == STDERR_TAIL_LINES {
                        tail.pop_front();
                    }
                    tail.push_back(line);
                }
            })
            .map_err(|e| e.to_string())?;

        let reader = Reader {
            name: name.clone(),
            pending: transport.pending.clone(),
            stdin: transport.stdin.clone(),
            alive: transport.alive.clone(),
            stderr_tail: transport.stderr_tail.clone(),
            tools_stale,
        };
        std::thread::Builder::new()
            .name(format!("mcp-{name}-stdout"))
            .spawn(move || reader.run(stdout))
            .map_err(|e| e.to_string())?;
        Ok(transport)
    }

    fn write(&self, msg: &Value) -> Result<(), String> {
        write_message(&self.name, &self.stdin, msg)
    }
}

impl Transport for StdioTransport {
    fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        if !self.is_alive() {
            return Err(exit_message(&self.name, &self.stderr_tail));
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        lock(&self.pending).insert(id, tx);
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        if let Err(e) = self.write(&msg) {
            lock(&self.pending).remove(&id);
            return Err(e);
        }
        match rx.recv_timeout(timeout) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                lock(&self.pending).remove(&id);
                self.notify(
                    "notifications/cancelled",
                    json!({ "requestId": id, "reason": "timed out" }),
                );
                Err(format!(
                    "MCP `{}`: {method} timed out after {}s",
                    self.name,
                    timeout.as_secs()
                ))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(exit_message(&self.name, &self.stderr_tail))
            }
        }
    }

    fn notify(&self, method: &str, params: Value) {
        let _ = self.write(&json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        if let Some(mut child) = lock(&self.child).take() {
            crate::agent::tools::terminate_child_tree(&mut child);
        }
    }
}

struct Reader {
    name: String,
    pending: Pending,
    stdin: Arc<Mutex<ChildStdin>>,
    alive: Arc<AtomicBool>,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    tools_stale: Arc<AtomicBool>,
}

impl Reader {
    fn run(self, stdout: std::process::ChildStdout) {
        let mut reader = BufReader::new(stdout);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            if buf.len() > MAX_LINE_BYTES {
                activity_log::log(
                    ActivityKind::Error,
                    format!("{} ← oversized message dropped", self.name),
                    format!("{} bytes", buf.len()),
                );
                continue;
            }
            let line = String::from_utf8_lossy(&buf);
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<Value>(line) {
                Ok(msg) => self.handle(msg),
                // Servers sometimes print banners to stdout; keep them visible for debugging.
                Err(_) => activity_log::log(
                    ActivityKind::Mcp,
                    format!("{} ← (not JSON-RPC)", self.name),
                    line,
                ),
            }
        }
        self.alive.store(false, Ordering::SeqCst);
        let message = exit_message(&self.name, &self.stderr_tail);
        for (_, tx) in lock(&self.pending).drain() {
            let _ = tx.send(Err(message.clone()));
        }
    }

    fn handle(&self, msg: Value) {
        activity_log::log_json(
            ActivityKind::Mcp,
            format!("{} ← {}", self.name, activity_log::rpc_title(&msg)),
            &msg,
        );
        let method = msg.get("method").and_then(|m| m.as_str());
        let id = msg.get("id").filter(|v| !v.is_null());
        match (method, id) {
            // Response to one of our requests.
            (None, Some(id)) => {
                let Some(waiter) = id.as_u64().and_then(|id| lock(&self.pending).remove(&id))
                else {
                    return;
                };
                let result = match msg.get("error") {
                    Some(err) => Err(rpc_error_message(err)),
                    None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
                };
                let _ = waiter.send(result);
            }
            // Request from the server: answer ping, decline everything else (oxi advertises no
            // client capabilities such as sampling or roots).
            (Some(method), Some(id)) => {
                let reply = if method == "ping" {
                    json!({ "jsonrpc": "2.0", "id": id, "result": {} })
                } else {
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": -32601, "message": format!("method not supported: {method}") }
                    })
                };
                let _ = write_message(&self.name, &self.stdin, &reply);
            }
            (Some("notifications/tools/list_changed"), None) => {
                self.tools_stale.store(true, Ordering::SeqCst);
            }
            _ => {}
        }
    }
}

fn write_message(name: &str, stdin: &Mutex<ChildStdin>, msg: &Value) -> Result<(), String> {
    activity_log::log_json(
        ActivityKind::Mcp,
        format!("{name} → {}", activity_log::rpc_title(msg)),
        msg,
    );
    let mut line = serde_json::to_string(msg).map_err(|e| e.to_string())?;
    line.push('\n');
    let mut stdin = lock(stdin);
    stdin
        .write_all(line.as_bytes())
        .and_then(|_| stdin.flush())
        .map_err(|e| format!("MCP `{name}`: write failed: {e}"))
}

pub(super) fn rpc_error_message(err: &Value) -> String {
    let message = err
        .get("message")
        .and_then(|m| m.as_str())
        .unwrap_or("error");
    match err.get("code").and_then(|c| c.as_i64()) {
        Some(code) => format!("{message} (code {code})"),
        None => message.to_string(),
    }
}

fn exit_message(name: &str, tail: &Mutex<VecDeque<String>>) -> String {
    let tail = lock(tail);
    if tail.is_empty() {
        format!("MCP server `{name}` exited")
    } else {
        let lines: Vec<&str> = tail.iter().map(String::as_str).collect();
        format!("MCP server `{name}` exited:\n{}", lines.join("\n"))
    }
}

/// Command for a stdio server. On Windows it runs through `cmd` so `npx`/`uvx` (which are
/// `.cmd` shims, not executables) resolve the same way they do in a terminal.
fn build_command(command: &str, args: &[String]) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let quote = |s: &str| {
            if s.is_empty() || s.contains([' ', '\t', '&', '|', '<', '>', '^']) {
                format!("\"{s}\"")
            } else {
                s.to_string()
            }
        };
        let line = std::iter::once(quote(command))
            .chain(args.iter().map(|a| quote(a)))
            .collect::<Vec<_>>()
            .join(" ");
        let mut c = Command::new("cmd");
        c.raw_arg(format!("/D /S /C \"{line}\""))
            .creation_flags(CREATE_NO_WINDOW);
        c
    }
    #[cfg(not(windows))]
    {
        let mut c = Command::new(command);
        c.args(args);
        c
    }
}
