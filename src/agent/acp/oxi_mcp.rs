//! oxi's own tools handed to ACP agents as a stdio MCP server (`oxi acp-todo-mcp`).
//! Agents do not reliably bring a checklist tool (Claude Code turns its todo tools off for some
//! models), so every session gets `todo_write`, whose calls drive oxi's native checklist. The
//! global scratchpad comes along (`scratchpad_read`, `scratchpad`) unless the user turned the
//! scratchpad tool off in Settings > Agent. `chat_read` hands back earlier turns of the chat that
//! a handoff transcript trimmed (see [`super::chat_archive`]).

use std::io::{BufRead as _, Write as _};

use serde_json::{Value, json};

/// Command line argument that runs oxi as this MCP server instead of the app. The name predates
/// the scratchpad tools; agents that saved it in their session config keep working.
pub const SUBCOMMAND: &str = "acp-todo-mcp";
/// MCP server name; agents expose the tools as e.g. `mcp__oxi__todo_write`.
const SERVER_NAME: &str = "oxi";
const TOOL_NAME: &str = "todo_write";
pub(super) const SCRATCHPAD_READ: &str = "scratchpad_read";
pub(super) const SCRATCHPAD: &str = "scratchpad";
const TOOL_NAMES: [&str; 3] = [TOOL_NAME, SCRATCHPAD_READ, SCRATCHPAD];

/// The ACP `McpServer` entry that launches this server from the running oxi binary.
/// `evidence_dir`: the connection's evidence folder, which turns on the evidence tools.
pub(super) fn acp_entry(evidence_dir: Option<&std::path::Path>) -> Option<Value> {
    let exe = std::env::current_exe().ok()?;
    // Test binaries live in `target/<profile>/deps/`; the app binary sits one level up.
    #[cfg(test)]
    let exe = exe.parent()?.parent()?.join("oxi");
    let env: Vec<Value> = evidence_dir
        .map(|dir| json!({ "name": crate::evidence::DIR_ENV, "value": dir.to_string_lossy() }))
        .into_iter()
        .collect();
    Some(json!({
        "name": SERVER_NAME,
        "command": exe.to_string_lossy(),
        "args": [SUBCOMMAND],
        "env": env,
    }))
}

/// Which of this server's tools an ACP tool call is, if any. Claude Code puts the MCP name in
/// `_meta` when its title is a friendlier label.
pub(super) fn oxi_tool(tool: &Value) -> Option<&'static str> {
    [
        &tool["title"],
        &tool["name"],
        &tool["_meta"]["claudeCode"]["toolName"],
    ]
    .into_iter()
    .find_map(|v| v.as_str().and_then(oxi_tool_name))
}

pub(super) fn names_todo_tool(s: &str) -> bool {
    oxi_tool_name(s) == Some(TOOL_NAME)
}

/// The oxi tool `s` names, however the agent spells MCP tool names.
pub(super) fn oxi_tool_name(s: &str) -> Option<&'static str> {
    let s = s.trim();
    // Claude: `mcp__oxi__todo_write`; Codex and others: `oxi.todo_write`, `oxi/todo_write`, ...
    TOOL_NAMES
        .into_iter()
        .chain([super::chat_archive::TOOL])
        .chain(crate::evidence::tools::NAMES)
        .find(|tool| {
            s.strip_suffix(tool)
                .and_then(|rest| rest.strip_suffix(['_', '.', '/', ':']))
                .map(|rest| rest.trim_end_matches('_'))
                .and_then(|server| server.strip_suffix(SERVER_NAME))
                .is_some_and(|prefix| prefix.is_empty() || prefix.ends_with(['_', '.', '/', ':']))
        })
}

/// The user's Settings > Agent switch for the scratchpad tool. Read straight from
/// `settings.json`: loading settings properly would touch the keychain from this subprocess.
fn scratchpad_enabled() -> bool {
    let index = crate::settings::ALL_TOOL_NAMES
        .iter()
        .position(|name| *name == SCRATCHPAD);
    let settings = std::fs::read(crate::settings::AppSettings::config_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    match (index, settings) {
        (Some(index), Some(settings)) => settings["tools_enabled"][index].as_bool().unwrap_or(true),
        _ => true,
    }
}

fn scratchpad_tools() -> [Value; 2] {
    [
        json!({
            "name": SCRATCHPAD_READ,
            "description": "Read the user's global oxi scratchpad: their own notes, autosaved and shared across workspaces. Read it when the user refers to their notes or scratchpad.",
            "inputSchema": { "type": "object", "properties": {} },
        }),
        json!({
            "name": SCRATCHPAD,
            "description": "Update the user's global oxi scratchpad (shared across workspaces, open in the oxi editor). Use only when the user asks you to note something there. `append` concatenates content exactly (include needed newlines); `rewrite` replaces all text (empty clears it), so read it first. Edits the user is typing at the same time are merged, not lost. Do not touch its file through other tools.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "mode": { "type": "string", "enum": ["append", "rewrite"] },
                    "content": { "type": "string" }
                },
                "required": ["mode", "content"]
            },
        }),
    ]
}

fn call_scratchpad(name: &str, args: &Value) -> Result<String, String> {
    if !scratchpad_enabled() {
        return Err("The scratchpad tool is turned off in oxi's Settings > Agent.".into());
    }
    let path = crate::scratchpad::path();
    if name == SCRATCHPAD_READ {
        let text = crate::scratchpad::read_locked(&path)?;
        return Ok(if text.is_empty() {
            "(The scratchpad is empty.)".into()
        } else {
            text
        });
    }
    let field = |key: &str| {
        args[key]
            .as_str()
            .ok_or_else(|| format!("Missing string `{key}`."))
    };
    let mode = field("mode")?;
    let (_, after) = crate::scratchpad::update(&path, mode, field("content")?)?;
    Ok(format!(
        "Scratchpad updated ({mode}, {} bytes).",
        after.len()
    ))
}

fn instructions() -> String {
    let mut text = String::from(
        "oxi shows the todo_write checklist live above the user's composer. \
        Use it for any task with 3+ steps or when you lay out a plan.",
    );
    if scratchpad_enabled() {
        text.push_str(" scratchpad_read and scratchpad reach the user's global notes in oxi.");
    }
    if crate::evidence::tools::conn_dir().is_some() {
        text.push_str(
            " chat_read returns earlier turns of this conversation in full when a transcript oxi sent you left them out.",
        );
        text.push(' ');
        text.push_str(crate::evidence::tools::INSTRUCTIONS);
    }
    text
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
            "instructions": instructions(),
        }),
        "ping" => json!({}),
        "tools/list" => {
            let def = crate::agent::tools::todo_write_definition();
            let mut tools = vec![json!({
                "name": TOOL_NAME,
                "description": def["description"],
                "inputSchema": def["parameters"],
            })];
            if scratchpad_enabled() {
                tools.extend(scratchpad_tools());
            }
            if crate::evidence::tools::conn_dir().is_some() {
                tools.push(super::chat_archive::definition());
                tools.extend(crate::evidence::tools::definitions());
            }
            json!({ "tools": tools })
        }
        "tools/call" if msg["params"]["name"] == TOOL_NAME => {
            let args = &msg["params"]["arguments"];
            let (text, is_error) = match crate::agent::tools::tool_todo_write(args) {
                Ok(text) => (text, false),
                Err(e) => (e, true),
            };
            json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
        }
        "tools/call" if msg["params"]["name"] == super::chat_archive::TOOL => {
            let (text, is_error) = match super::chat_archive::call(&msg["params"]["arguments"]) {
                Ok(text) => (text, false),
                Err(e) => (e, true),
            };
            json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
        }
        "tools/call"
            if matches!(
                msg["params"]["name"].as_str(),
                Some(SCRATCHPAD_READ | SCRATCHPAD)
            ) =>
        {
            let name = msg["params"]["name"].as_str().unwrap_or_default();
            let (text, is_error) = match call_scratchpad(name, &msg["params"]["arguments"]) {
                Ok(text) => (text, false),
                Err(e) => (e, true),
            };
            json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
        }
        "tools/call"
            if msg["params"]["name"]
                .as_str()
                .is_some_and(|name| crate::evidence::tools::NAMES.contains(&name)) =>
        {
            let name = msg["params"]["name"].as_str().unwrap_or_default();
            let (text, is_error) =
                match crate::evidence::tools::call(name, &msg["params"]["arguments"]) {
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
        assert_eq!(oxi_tool_name("mcp__oxi__scratchpad"), Some(SCRATCHPAD));
        assert_eq!(oxi_tool_name("oxi.scratchpad_read"), Some(SCRATCHPAD_READ));
        assert_eq!(oxi_tool_name("scratchpad"), None);
        assert_eq!(oxi_tool_name("mcp__oxi__chat_read"), Some("chat_read"));
        assert_eq!(
            oxi_tool(
                &json!({"title": "Scratchpad", "_meta": {"claudeCode": {"toolName": "mcp__oxi__scratchpad"}}})
            ),
            Some(SCRATCHPAD)
        );
    }

    #[test]
    fn reads_and_updates_the_scratchpad() {
        // Tests keep the config directory, and so the scratchpad, in a scratch folder.
        let call = |id: u32, name: &str, arguments: Value| {
            handle(
                &json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{
                    "name": name, "arguments": arguments
                }}),
            )
            .unwrap()["result"]
                .clone()
        };
        let list = handle(&json!({"jsonrpc":"2.0","id":1,"method":"tools/list"})).unwrap();
        let names: Vec<&str> = list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        assert_eq!(names, TOOL_NAMES);
        let wrote = call(
            2,
            SCRATCHPAD,
            json!({"mode": "rewrite", "content": "note\n"}),
        );
        assert_eq!(wrote["isError"], false);
        assert_eq!(
            call(
                3,
                SCRATCHPAD,
                json!({"mode": "append", "content": "more\n"})
            )["isError"],
            false
        );
        assert_eq!(
            call(4, SCRATCHPAD, json!({"mode": "bad", "content": ""}))["isError"],
            true
        );
        assert_eq!(
            call(5, SCRATCHPAD, json!({"mode": "append"}))["isError"],
            true
        );
        let read = call(6, SCRATCHPAD_READ, json!({}));
        assert_eq!(read["content"][0]["text"], "note\nmore\n");
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
