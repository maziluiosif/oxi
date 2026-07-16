//! Translation of ACP session updates into oxi's provider-neutral event stream.

use std::fmt::Write as _;
use std::sync::mpsc::Sender;

use serde_json::Value;

use crate::agent::events::AgentEvent;

/// Translate one ACP `session/update` payload into oxi [`AgentEvent`]s.
pub(super) fn emit_update(update: &Value, tx: &Sender<AgentEvent>) {
    let kind = update.get("sessionUpdate").and_then(|v| v.as_str());
    match kind {
        Some("agent_message_chunk") => {
            if let Some(text) = update.get("content").and_then(content_block_text) {
                let _ = tx.send(AgentEvent::TextDelta(text));
            }
        }
        Some("agent_thought_chunk") => {
            if let Some(text) = update.get("content").and_then(content_block_text) {
                let _ = tx.send(AgentEvent::ThinkingDelta(text));
            }
        }
        Some("tool_call") => {
            let tool_call_id = update
                .get("toolCallId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let name = update
                .get("kind")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .or_else(|| update.get("title").and_then(|v| v.as_str()))
                .unwrap_or("tool")
                .to_string();
            let _ = tx.send(AgentEvent::ToolStart {
                name,
                tool_call_id: tool_call_id.clone(),
                args: update.get("rawInput").cloned(),
            });
            emit_tool_content_and_status(update, &tool_call_id, tx);
        }
        Some("tool_call_update") => {
            let tool_call_id = update
                .get("toolCallId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            emit_tool_content_and_status(update, &tool_call_id, tx);
        }
        _ => {}
    }
}

fn emit_tool_content_and_status(update: &Value, tool_call_id: &str, tx: &Sender<AgentEvent>) {
    let (text, diff) = update
        .get("content")
        .map(extract_tool_content)
        .unwrap_or_default();
    if !text.is_empty() {
        let _ = tx.send(AgentEvent::ToolOutput {
            tool_call_id: tool_call_id.to_string(),
            text,
            truncated: false,
        });
    }
    match update.get("status").and_then(|v| v.as_str()) {
        Some("completed") | Some("failed") => {
            let is_error = update.get("status").and_then(|v| v.as_str()) == Some("failed");
            let _ = tx.send(AgentEvent::ToolEnd {
                tool_call_id: tool_call_id.to_string(),
                is_error: Some(is_error),
                full_output_path: None,
                diff,
            });
        }
        _ => {}
    }
}

fn content_block_text(block: &Value) -> Option<String> {
    match block.get("type").and_then(|t| t.as_str()) {
        Some("text") => block
            .get("text")
            .and_then(|t| t.as_str())
            .map(|s| s.to_string()),
        _ => None,
    }
}

fn extract_tool_content(content: &Value) -> (String, Option<String>) {
    let mut text = String::new();
    let mut diff: Option<String> = None;
    let Some(items) = content.as_array() else {
        return (text, diff);
    };
    for item in items {
        match item.get("type").and_then(|t| t.as_str()) {
            Some("content") => {
                if let Some(t) = item.get("content").and_then(content_block_text) {
                    text.push_str(&t);
                }
            }
            Some("diff") => {
                let path = item.get("path").and_then(|v| v.as_str()).unwrap_or("");
                let old = item.get("oldText").and_then(|v| v.as_str()).unwrap_or("");
                let new = item.get("newText").and_then(|v| v.as_str()).unwrap_or("");
                diff = Some(build_unified_diff(path, old, new));
            }
            _ => {}
        }
    }
    (text, diff)
}

fn build_unified_diff(path: &str, old: &str, new: &str) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "--- {path}");
    let _ = writeln!(s, "+++ {path}");
    for line in old.lines() {
        let _ = writeln!(s, "-{line}");
    }
    for line in new.lines() {
        let _ = writeln!(s, "+{line}");
    }
    s
}
