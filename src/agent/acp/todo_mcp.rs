//! oxi's own `todo_write` handed to ACP agents as a stdio MCP server (`oxi acp-todo-mcp`).
//! Agents do not reliably bring a checklist tool (Claude Code turns its todo tools off for some
//! models), so every session gets this one and its calls drive oxi's native checklist.

use std::io::{BufRead as _, Write as _};

use serde_json::{Value, json};

/// Command line argument that runs oxi as this MCP server instead of the app.
pub const SUBCOMMAND: &str = "acp-todo-mcp";
/// MCP server name; agents expose the tool as e.g. `mcp__oxi__todo_write`.
const SERVER_NAME: &str = "oxi";
const TOOL_NAME: &str = "todo_write";

/// The ACP `McpServer` entry that launches this server from the running oxi binary.
pub(super) fn acp_entry() -> Option<Value> {
    let exe = std::env::current_exe().ok()?;
    // Test binaries live in `target/<profile>/deps/`; the app binary sits one level up.
    #[cfg(test)]
    let exe = exe.parent()?.parent()?.join("oxi");
    Some(json!({
        "name": SERVER_NAME,
        "command": exe.to_string_lossy(),
        "args": [SUBCOMMAND],
        "env": [],
    }))
}

/// Whether an ACP tool call (by its `title` or `name`) is this server's `todo_write`, however
/// the agent spells MCP tool names.
pub(super) fn is_todo_tool(tool: &Value) -> bool {
    let named = |key: &str| tool[key].as_str().is_some_and(names_todo_tool);
    named("title") || named("name")
}

pub(super) fn names_todo_tool(s: &str) -> bool {
    let s = s.trim();
    // Claude: `mcp__oxi__todo_write`; Codex and others: `oxi.todo_write`, `oxi/todo_write`, ...
    s.strip_suffix(TOOL_NAME)
        .and_then(|rest| rest.strip_suffix(['_', '.', '/', ':']))
        .map(|rest| rest.trim_end_matches('_'))
        .and_then(|server| server.strip_suffix(SERVER_NAME))
        .is_some_and(|prefix| prefix.is_empty() || prefix.ends_with(['_', '.', '/', ':']))
}

/// Serve MCP over stdin/stdout until the agent closes the pipe.
pub fn run_stdio() -> i32 {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(reply) = handle(&msg)
            && (writeln!(stdout, "{reply}").is_err() || stdout.flush().is_err())
        {
            break;
        }
    }
    0
}

/// The reply to one JSON-RPC message; `None` for notifications.
fn handle(msg: &Value) -> Option<Value> {
    let id = msg.get("id").filter(|id| !id.is_null())?.clone();
    let result = match msg["method"].as_str().unwrap_or("") {
        "initialize" => json!({
            "protocolVersion": msg["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION") },
            "instructions": "oxi shows the todo_write checklist live above the user's composer. \
                Use it for any task with 3+ steps or when you lay out a plan.",
        }),
        "ping" => json!({}),
        "tools/list" => {
            let def = crate::agent::tools::todo_write_definition();
            json!({ "tools": [{
                "name": TOOL_NAME,
                "description": def["description"],
                "inputSchema": def["parameters"],
            }] })
        }
        "tools/call" if msg["params"]["name"] == TOOL_NAME => {
            let args = &msg["params"]["arguments"];
            let (text, is_error) = match crate::agent::tools::tool_todo_write(args) {
                Ok(text) => (text, false),
                Err(e) => (e, true),
            };
            json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
        }
        method => {
            return Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("unknown method `{method}`") },
            }));
        }
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_the_tool_under_each_agent_naming() {
        for name in [
            "mcp__oxi__todo_write",
            "oxi.todo_write",
            "oxi/todo_write",
            "oxi:todo_write",
        ] {
            assert!(names_todo_tool(name), "{name}");
        }
        for name in [
            "todo_write",
            "mcp__notoxi__todo_write",
            "mcp__oxi__read",
            "TodoWrite",
        ] {
            assert!(!names_todo_tool(name), "{name}");
        }
    }

    #[test]
    fn lists_and_calls_todo_write() {
        let init = handle(&json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-03-26"}})).unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        assert!(handle(&json!({"jsonrpc":"2.0","method":"notifications/initialized"})).is_none());
        let list = handle(&json!({"jsonrpc":"2.0","id":1,"method":"tools/list"})).unwrap();
        assert_eq!(list["result"]["tools"][0]["name"], "todo_write");
        assert_eq!(
            list["result"]["tools"][0]["inputSchema"]["required"][0],
            "todos"
        );
        let call = handle(
            &json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
                "name":"todo_write",
                "arguments":{"todos":[{"content":"Step","status":"in_progress"}]}
            }}),
        )
        .unwrap();
        assert_eq!(call["result"]["isError"], false);
        let bad = handle(
            &json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{
                "name":"todo_write","arguments":{}
            }}),
        )
        .unwrap();
        assert_eq!(bad["result"]["isError"], true);
    }
}
