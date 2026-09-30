//! MCP (Model Context Protocol) client.
//!
//! [`McpManager`] is long-lived (owned by the app, shared with every agent run): servers are
//! connected once and reused across runs, reconnected when their process dies or their HTTP
//! session expires, and restarted when their settings change. Two transports:
//! - stdio: spawn a command and exchange newline-delimited JSON-RPC ([`stdio`]);
//! - Streamable HTTP: POST JSON-RPC, answers as JSON or an SSE stream ([`http`]).
//!
//! Tools are exposed to the agent as `mcp_<server>_<tool>` (sanitized). Servers that offer
//! resources also get `mcp_<server>_list_resources` / `mcp_<server>_read_resource`. Every call
//! has a timeout, and one slow server never blocks calls to another.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use crate::settings::{McpServerConfig, McpTransport};

mod http;
mod stdio;

/// Protocol revision oxi speaks. Servers answer with the revision they support; older servers
/// that only know 2024-11-05 still work since oxi uses nothing newer than tools/resources.
const PROTOCOL_VERSION: &str = "2025-06-18";
/// `initialize` may have to wait for `npx`/`uvx` to download a server on first use.
const INIT_TIMEOUT: Duration = Duration::from_secs(90);
const LIST_TIMEOUT: Duration = Duration::from_secs(30);
/// Guard against servers that keep returning a `nextCursor`.
const MAX_LIST_PAGES: usize = 50;

#[derive(Debug, Clone)]
pub struct McpToolInfo {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Connection summary for the settings page.
#[derive(Debug, Clone, PartialEq)]
pub struct McpServerStatus {
    pub name: String,
    pub connected: bool,
    pub connecting: bool,
    pub tools: usize,
    pub resources: bool,
    pub error: Option<String>,
}

/// One live JSON-RPC connection to a server.
trait Transport: Send + Sync {
    fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, String>;
    fn notify(&self, method: &str, params: Value);
    fn is_alive(&self) -> bool;
    /// Negotiated protocol revision (HTTP sends it as a header on every later request).
    fn set_protocol_version(&self, _version: &str) {}
}

#[derive(Default)]
struct ServerState {
    conn: Option<Arc<dyn Transport>>,
    tools: Vec<McpToolInfo>,
    resources: bool,
    error: Option<String>,
}

struct Server {
    cfg: McpServerConfig,
    state: Mutex<ServerState>,
    /// Serializes (re)connects so parallel tool calls don't each spawn a process.
    connect_lock: Mutex<()>,
    connecting: AtomicBool,
    /// Set by the transport on `notifications/tools/list_changed`.
    tools_stale: Arc<AtomicBool>,
}

impl Server {
    fn new(cfg: McpServerConfig) -> Self {
        Self {
            cfg,
            state: Mutex::default(),
            connect_lock: Mutex::new(()),
            connecting: AtomicBool::new(false),
            tools_stale: Arc::new(AtomicBool::new(false)),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, ServerState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn live_conn(&self) -> Option<Arc<dyn Transport>> {
        self.state().conn.clone().filter(|c| c.is_alive())
    }

    /// Connect (or reconnect) if needed and refresh the tool list when the server said it
    /// changed. Returns the live connection.
    fn ensure_ready(&self) -> Result<Arc<dyn Transport>, String> {
        let _guard = self.connect_lock.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(conn) = self.live_conn() {
            if self.tools_stale.swap(false, Ordering::SeqCst) {
                match list_tools(conn.as_ref()) {
                    Ok(tools) => self.state().tools = tools,
                    Err(e) => self.state().error = Some(e),
                }
            }
            return Ok(conn);
        }
        self.connecting.store(true, Ordering::SeqCst);
        let result = self.connect();
        self.connecting.store(false, Ordering::SeqCst);
        let mut state = self.state();
        match result {
            Ok(fresh) => {
                *state = fresh;
                state
                    .conn
                    .clone()
                    .ok_or_else(|| "MCP connection missing".to_string())
            }
            Err(e) => {
                state.conn = None;
                state.error = Some(e.clone());
                Err(e)
            }
        }
    }

    /// Start a fresh connection: spawn/handshake, then discover tools and capabilities.
    fn connect(&self) -> Result<ServerState, String> {
        let name = &self.cfg.name;
        self.tools_stale.store(false, Ordering::SeqCst);
        let conn: Arc<dyn Transport> = match self.cfg.transport {
            McpTransport::Stdio => {
                let path = run_io(crate::agent::acp::subprocess_path(), LIST_TIMEOUT)
                    .ok()
                    .flatten();
                Arc::new(stdio::StdioTransport::spawn(
                    &self.cfg,
                    path,
                    self.tools_stale.clone(),
                )?)
            }
            McpTransport::Http => Arc::new(http::HttpTransport::new(&self.cfg)?),
        };
        let init = conn
            .request(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": { "name": "oxi", "version": env!("CARGO_PKG_VERSION") }
                }),
                INIT_TIMEOUT,
            )
            .map_err(|e| format!("MCP `{name}` initialize failed: {e}"))?;
        if let Some(version) = init.get("protocolVersion").and_then(|v| v.as_str()) {
            conn.set_protocol_version(version);
        }
        conn.notify("notifications/initialized", json!({}));
        let caps = init.get("capabilities").cloned().unwrap_or(Value::Null);
        let tools = if caps.get("tools").is_some() || caps.is_null() {
            list_tools(conn.as_ref()).map_err(|e| format!("MCP `{name}` tools/list: {e}"))?
        } else {
            Vec::new()
        };
        Ok(ServerState {
            conn: Some(conn),
            tools,
            resources: caps.get("resources").is_some(),
            error: None,
        })
    }
}

/// Shared registry of MCP servers. Cheap to clone.
#[derive(Clone, Default)]
pub struct McpManager {
    servers: Arc<Mutex<Vec<Arc<Server>>>>,
}

impl std::fmt::Debug for McpManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let n = self.servers.lock().map(|s| s.len()).unwrap_or(0);
        f.debug_struct("McpManager").field("servers", &n).finish()
    }
}

/// What an exposed tool name resolves to.
enum Target {
    Tool(String),
    ListResources,
    ReadResource,
}

struct Exposed {
    name: String,
    server: Arc<Server>,
    target: Target,
    description: String,
    schema: Value,
}

impl McpManager {
    pub fn new() -> Self {
        Self::default()
    }

    fn snapshot(&self) -> Vec<Arc<Server>> {
        self.servers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Match the registry to `servers`: unchanged entries keep their live connection, changed or
    /// removed ones are dropped (which stops their process), and every usable server is
    /// connected (in parallel) if it isn't already. Blocking; call off the UI thread.
    pub fn sync_servers(&self, servers: &[McpServerConfig]) {
        let current = {
            let mut list = self.servers.lock().unwrap_or_else(|e| e.into_inner());
            let next: Vec<Arc<Server>> = servers
                .iter()
                .filter(|c| c.is_usable())
                .map(|cfg| {
                    list.iter()
                        .find(|s| s.cfg == *cfg)
                        .cloned()
                        .unwrap_or_else(|| Arc::new(Server::new(cfg.clone())))
                })
                .collect();
            *list = next;
            list.clone()
        };
        std::thread::scope(|scope| {
            for server in &current {
                scope.spawn(move || {
                    let _ = server.ensure_ready();
                });
            }
        });
    }

    /// Reconnect every server from scratch (settings "Reconnect" button).
    pub fn reconnect_all(&self, servers: &[McpServerConfig]) {
        self.servers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.sync_servers(servers);
    }

    pub fn statuses(&self) -> Vec<McpServerStatus> {
        self.snapshot()
            .iter()
            .map(|s| {
                let st = s.state();
                McpServerStatus {
                    name: s.cfg.name.clone(),
                    connected: st.conn.as_ref().is_some_and(|c| c.is_alive()),
                    connecting: s.connecting.load(Ordering::SeqCst),
                    tools: st.tools.len(),
                    resources: st.resources,
                    error: st.error.clone(),
                }
            })
            .collect()
    }

    fn exposed(&self) -> Vec<Exposed> {
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for server in self.snapshot() {
            let st = server.state();
            let mut add = |name: String, target: Target, description: String, schema: Value| {
                // Sanitization can collapse distinct names to one id; keep the first so a call
                // can never resolve ambiguously.
                if seen.insert(name.clone()) {
                    out.push(Exposed {
                        name,
                        server: server.clone(),
                        target,
                        description,
                        schema,
                    });
                }
            };
            for t in &st.tools {
                add(
                    mcp_tool_name(&server.cfg.name, &t.name),
                    Target::Tool(t.name.clone()),
                    format!("[MCP:{}] {}", server.cfg.name, t.description),
                    t.input_schema.clone(),
                );
            }
            if st.resources {
                add(
                    mcp_tool_name(&server.cfg.name, "list_resources"),
                    Target::ListResources,
                    format!(
                        "[MCP:{}] List the resources (documents, files, records) this server exposes, with their URIs.",
                        server.cfg.name
                    ),
                    json!({"type": "object", "properties": {}}),
                );
                add(
                    mcp_tool_name(&server.cfg.name, "read_resource"),
                    Target::ReadResource,
                    format!(
                        "[MCP:{}] Read one resource by URI (from list_resources).",
                        server.cfg.name
                    ),
                    json!({
                        "type": "object",
                        "properties": { "uri": { "type": "string", "description": "Resource URI" } },
                        "required": ["uri"]
                    }),
                );
            }
        }
        out
    }

    pub fn tool_definitions(&self) -> Vec<Value> {
        self.exposed()
            .into_iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.schema,
                    }
                })
            })
            .collect()
    }

    /// Run an exposed MCP tool. Blocking (bounded by the server's call timeout).
    pub fn call_tool(&self, full_name: &str, args: &Value) -> Result<String, String> {
        let exposed = self
            .exposed()
            .into_iter()
            .find(|t| t.name == full_name)
            .ok_or_else(|| format!("unknown MCP tool: {full_name}"))?;
        let server = exposed.server;
        let conn = server.ensure_ready()?;
        let timeout = server.cfg.call_timeout();
        match exposed.target {
            Target::Tool(name) => {
                let res = conn.request(
                    "tools/call",
                    json!({ "name": name, "arguments": args }),
                    timeout,
                )?;
                format_tool_result(&res)
            }
            Target::ListResources => {
                let items = paginate(conn.as_ref(), "resources/list", "resources", timeout)?;
                if items.is_empty() {
                    return Ok("No resources.".into());
                }
                Ok(items
                    .iter()
                    .map(|r| {
                        let uri = r.get("uri").and_then(|v| v.as_str()).unwrap_or("?");
                        let name = r.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        let desc = r.get("description").and_then(|v| v.as_str()).unwrap_or("");
                        let mime = r.get("mimeType").and_then(|v| v.as_str()).unwrap_or("");
                        let mut line = format!("- {uri}");
                        for extra in [name, mime, desc] {
                            if !extra.is_empty() {
                                line.push_str(" · ");
                                line.push_str(extra);
                            }
                        }
                        line
                    })
                    .collect::<Vec<_>>()
                    .join("\n"))
            }
            Target::ReadResource => {
                let uri = args
                    .get("uri")
                    .and_then(|v| v.as_str())
                    .ok_or("missing uri")?;
                let res = conn.request("resources/read", json!({ "uri": uri }), timeout)?;
                Ok(format_resource_contents(&res))
            }
        }
    }

    pub fn is_mcp_tool(name: &str) -> bool {
        name.starts_with("mcp_")
    }
}

fn mcp_tool_name(server: &str, tool: &str) -> String {
    let sanitize = |s: &str| {
        s.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect::<String>()
    };
    format!("mcp_{}_{}", sanitize(server), sanitize(tool))
}

/// Collect every page of a cursor-paginated list method.
fn paginate(
    conn: &dyn Transport,
    method: &str,
    key: &str,
    timeout: Duration,
) -> Result<Vec<Value>, String> {
    let mut items = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_LIST_PAGES {
        let params = match &cursor {
            Some(c) => json!({ "cursor": c }),
            None => json!({}),
        };
        let res = conn.request(method, params, timeout)?;
        if let Some(page) = res.get(key).and_then(|v| v.as_array()) {
            items.extend(page.iter().cloned());
        }
        cursor = res
            .get("nextCursor")
            .and_then(|v| v.as_str())
            .filter(|c| !c.is_empty())
            .map(str::to_string);
        if cursor.is_none() {
            break;
        }
    }
    Ok(items)
}

fn list_tools(conn: &dyn Transport) -> Result<Vec<McpToolInfo>, String> {
    Ok(paginate(conn, "tools/list", "tools", LIST_TIMEOUT)?
        .into_iter()
        .filter_map(|t| {
            Some(McpToolInfo {
                name: t.get("name")?.as_str()?.to_string(),
                description: t
                    .get("description")
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
                    .to_string(),
                input_schema: t
                    .get("inputSchema")
                    .cloned()
                    .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
            })
        })
        .collect())
}

/// Flatten a `tools/call` result into text for the model. `isError` results become `Err` so the
/// agent sees them as failed calls.
fn format_tool_result(res: &Value) -> Result<String, String> {
    let mut parts = Vec::new();
    for part in res
        .get("content")
        .and_then(|c| c.as_array())
        .into_iter()
        .flatten()
    {
        let kind = part.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let text = match kind {
            "text" => part
                .get("text")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string(),
            "image" | "audio" => format!(
                "[{kind}: {}, {} base64 chars]",
                part.get("mimeType")
                    .and_then(|m| m.as_str())
                    .unwrap_or("unknown type"),
                part.get("data")
                    .and_then(|d| d.as_str())
                    .map_or(0, str::len)
            ),
            "resource" => format_resource_contents(&json!({
                "contents": [part.get("resource").cloned().unwrap_or(Value::Null)]
            })),
            "resource_link" => format!(
                "[resource link: {}] {}",
                part.get("uri").and_then(|u| u.as_str()).unwrap_or("?"),
                part.get("name").and_then(|u| u.as_str()).unwrap_or("")
            ),
            _ => part.to_string(),
        };
        if !text.is_empty() {
            parts.push(text);
        }
    }
    let mut out = parts.join("\n");
    if out.is_empty() {
        out = res
            .get("structuredContent")
            .map(Value::to_string)
            .unwrap_or_else(|| res.to_string());
    }
    if res.get("isError").and_then(|v| v.as_bool()) == Some(true) {
        Err(out)
    } else {
        Ok(out)
    }
}

fn format_resource_contents(res: &Value) -> String {
    let mut out = Vec::new();
    for c in res
        .get("contents")
        .and_then(|c| c.as_array())
        .into_iter()
        .flatten()
    {
        if let Some(text) = c.get("text").and_then(|t| t.as_str()) {
            out.push(text.to_string());
        } else if let Some(blob) = c.get("blob").and_then(|b| b.as_str()) {
            out.push(format!(
                "[binary resource {}: {}, {} base64 chars]",
                c.get("uri").and_then(|u| u.as_str()).unwrap_or("?"),
                c.get("mimeType")
                    .and_then(|m| m.as_str())
                    .unwrap_or("unknown type"),
                blob.len()
            ));
        }
    }
    if out.is_empty() {
        "(empty resource)".into()
    } else {
        out.join("\n")
    }
}

/// Small runtime for MCP HTTP I/O and PATH discovery, so blocking callers (tool threads) can
/// wait on async work without being inside a runtime themselves.
static IO_RT: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("oxi-mcp-io")
        .enable_all()
        .build()
        .expect("failed to start the MCP I/O runtime")
});

/// Run `fut` on the MCP runtime and wait for it, at most `timeout`. Safe to call from any
/// thread, including from inside another tokio runtime.
fn run_io<T: Send + 'static>(
    fut: impl std::future::Future<Output = T> + Send + 'static,
    timeout: Duration,
) -> Result<T, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    IO_RT.spawn(async move {
        let _ = tx.send(fut.await);
    });
    rx.recv_timeout(timeout)
        .map_err(|_| format!("timed out after {}s", timeout.as_secs()))
}

#[cfg(test)]
#[path = "mcp/tests.rs"]
mod tests;
