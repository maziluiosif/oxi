//! Minimal MCP (Model Context Protocol) stdio client.
//!
//! Spawns configured MCP servers, lists their tools, and forwards tool calls.
//! Tool names are exposed to the agent as `mcp_<server>_<tool>` (sanitized).

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use crate::settings::McpServerConfig;

const MAX_MCP_MESSAGE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct McpToolInfo {
    pub server: String,
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

struct McpProcess {
    _child: Child,
    stdin: ChildStdin,
    reader: BufReader<std::process::ChildStdout>,
    next_id: u64,
}

/// Shared registry of live MCP connections and discovered tools.
#[derive(Clone, Default)]
pub struct McpManager {
    inner: Arc<Mutex<McpState>>,
}

impl std::fmt::Debug for McpManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let n = self.inner.lock().map(|s| s.tools.len()).unwrap_or(0);
        f.debug_struct("McpManager").field("tools", &n).finish()
    }
}

#[derive(Default)]
struct McpState {
    processes: HashMap<String, McpProcess>,
    tools: Vec<McpToolInfo>,
}

impl McpManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reconnect to all enabled servers from settings. Best-effort; failures are logged.
    pub fn sync_servers(&self, servers: &[McpServerConfig]) {
        let mut state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        // Drop processes for removed/disabled servers.
        let keep: std::collections::HashSet<&str> = servers
            .iter()
            .filter(|s| s.enabled && !s.name.trim().is_empty() && !s.command.trim().is_empty())
            .map(|s| s.name.as_str())
            .collect();
        state
            .processes
            .retain(|name, _| keep.contains(name.as_str()));

        let mut tools = Vec::new();
        for cfg in servers
            .iter()
            .filter(|s| s.enabled && !s.name.trim().is_empty() && !s.command.trim().is_empty())
        {
            if !state.processes.contains_key(&cfg.name)
                && let Ok(proc) = spawn_mcp(cfg)
            {
                state.processes.insert(cfg.name.clone(), proc);
            }
            if let Some(proc) = state.processes.get_mut(&cfg.name)
                && let Ok(listed) = list_tools(proc)
            {
                for t in listed {
                    tools.push(McpToolInfo {
                        server: cfg.name.clone(),
                        name: t.name,
                        description: t.description,
                        input_schema: t.input_schema,
                    });
                }
            }
        }
        // Sanitization can collapse distinct punctuation-heavy names to the same exposed tool id.
        // Keep a single deterministic definition so provider calls can never resolve ambiguously.
        dedupe_mcp_tool_names(&mut tools);
        state.tools = tools;
    }

    pub fn tool_definitions(&self) -> Vec<Value> {
        let state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        state
            .tools
            .iter()
            .map(|t| {
                let tool_name = mcp_tool_name(&t.server, &t.name);
                json!({
                    "type": "function",
                    "function": {
                        "name": tool_name,
                        "description": format!("[MCP:{}] {}", t.server, t.description),
                        "parameters": t.input_schema,
                    }
                })
            })
            .collect()
    }

    pub fn call_tool(&self, full_name: &str, args: &Value) -> Result<String, String> {
        let mut state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        // Resolve against the discovered registry instead of splitting the exposed name. Both
        // server and tool names may contain underscores, so `split_once('_')` is ambiguous.
        let info = state
            .tools
            .iter()
            .find(|t| mcp_tool_name(&t.server, &t.name) == full_name)
            .cloned()
            .ok_or_else(|| format!("unknown MCP tool: {full_name}"))?;
        let proc = state
            .processes
            .get_mut(&info.server)
            .ok_or_else(|| format!("MCP server `{}` is not connected", info.server))?;
        call_tool(proc, &info.name, args)
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

fn dedupe_mcp_tool_names(tools: &mut Vec<McpToolInfo>) {
    let mut seen = std::collections::HashSet::new();
    tools.retain(|tool| seen.insert(mcp_tool_name(&tool.server, &tool.name)));
}

struct ListedTool {
    name: String,
    description: String,
    input_schema: Value,
}

fn spawn_mcp(cfg: &McpServerConfig) -> Result<McpProcess, String> {
    let mut child = Command::new(&cfg.command)
        .args(&cfg.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn MCP `{}`: {e}", cfg.name))?;
    let stdin = child.stdin.take().ok_or("no stdin")?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let mut proc = McpProcess {
        _child: child,
        stdin,
        reader: BufReader::new(stdout),
        next_id: 1,
    };
    // initialize
    let _ = request(
        &mut proc,
        "initialize",
        json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "oxi", "version": env!("CARGO_PKG_VERSION") }
        }),
    )?;
    let _ = notify(&mut proc, "notifications/initialized", json!({}));
    Ok(proc)
}

fn list_tools(proc: &mut McpProcess) -> Result<Vec<ListedTool>, String> {
    let res = request(proc, "tools/list", json!({}))?;
    let tools = res
        .get("tools")
        .and_then(|t| t.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(tools
        .into_iter()
        .filter_map(|t| {
            Some(ListedTool {
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

fn call_tool(proc: &mut McpProcess, name: &str, args: &Value) -> Result<String, String> {
    let res = request(
        proc,
        "tools/call",
        json!({ "name": name, "arguments": args }),
    )?;
    if let Some(content) = res.get("content").and_then(|c| c.as_array()) {
        let mut out = String::new();
        for part in content {
            if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(text);
            }
        }
        if !out.is_empty() {
            return Ok(out);
        }
    }
    Ok(res.to_string())
}

fn request(proc: &mut McpProcess, method: &str, params: Value) -> Result<Value, String> {
    let id = proc.next_id;
    proc.next_id += 1;
    let msg = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    });
    write_message(proc, &msg)?;
    loop {
        let line = read_line(proc)?;
        let value: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
        if value.get("id").and_then(|i| i.as_u64()) == Some(id) {
            if let Some(err) = value.get("error") {
                return Err(err.to_string());
            }
            return Ok(value.get("result").cloned().unwrap_or(Value::Null));
        }
        // Skip unrelated notifications / responses.
    }
}

fn notify(proc: &mut McpProcess, method: &str, params: Value) -> Result<(), String> {
    let msg = json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
    });
    write_message(proc, &msg)
}

fn write_message(proc: &mut McpProcess, msg: &Value) -> Result<(), String> {
    let body = serde_json::to_string(msg).map_err(|e| e.to_string())?;
    // Prefer Content-Length framing; also works with newline-delimited servers.
    write!(proc.stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body)
        .map_err(|e| e.to_string())?;
    proc.stdin.flush().map_err(|e| e.to_string())
}

fn read_line(proc: &mut McpProcess) -> Result<String, String> {
    // Support both Content-Length framed and newline-delimited JSON.
    let mut header = String::new();
    loop {
        header.clear();
        proc.reader
            .read_line(&mut header)
            .map_err(|e| e.to_string())?;
        if header.is_empty() {
            return Err("MCP server closed stdout".into());
        }
        let trimmed = header.trim();
        if trimmed.is_empty() {
            // End of headers — next is body with Content-Length (handled below if we saw it).
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("Content-Length:") {
            let len: usize = rest.trim().parse().map_err(|e| format!("{e}"))?;
            if len > MAX_MCP_MESSAGE_BYTES {
                return Err(format!(
                    "MCP message is too large ({len} bytes; limit is {MAX_MCP_MESSAGE_BYTES})"
                ));
            }
            // Consume remaining headers until blank line.
            loop {
                let mut line = String::new();
                proc.reader
                    .read_line(&mut line)
                    .map_err(|e| e.to_string())?;
                if line.trim().is_empty() {
                    break;
                }
            }
            let mut buf = vec![0u8; len];
            use std::io::Read;
            proc.reader
                .read_exact(&mut buf)
                .map_err(|e| e.to_string())?;
            return String::from_utf8(buf).map_err(|e| e.to_string());
        }
        // Newline-delimited JSON message.
        if trimmed.starts_with('{') {
            return Ok(trimmed.to_string());
        }
    }
}
