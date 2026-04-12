//! Anthropic Messages API streaming (GitHub Copilot uses the same wire format at a custom `base_url`).

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;

use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Value};

use super::copilot::copilot_x_initiator_from_openai_messages;
use super::events::AgentEvent;
use super::tools::run_tool;

#[derive(Default, Clone)]
struct ToolUseAccum {
    id: String,
    name: String,
    input_json: String,
}

/// Convert OpenAI-style tool defs to Anthropic `tools` array.
fn to_anthropic_tools(openai_tools: &[Value]) -> Vec<Value> {
    let mut out = Vec::new();
    for t in openai_tools {
        if let Some(f) = t.get("function") {
            let name = f.get("name").and_then(|x| x.as_str()).unwrap_or("");
            let desc = f.get("description").and_then(|x| x.as_str()).unwrap_or("");
            let params = f.get("parameters").cloned().unwrap_or(json!({}));
            out.push(json!({
                "name": name,
                "description": desc,
                "input_schema": params
            }));
        }
    }
    out
}

/// Convert OpenAI-format `messages` to Anthropic `messages` (system stripped — pass separately).
fn to_anthropic_messages(openai: &[Value], cache_control: Option<Value>) -> (String, Vec<Value>) {
    let mut system = String::new();
    let mut msgs = Vec::new();
    for m in openai {
        let role = m.get("role").and_then(|x| x.as_str()).unwrap_or("");
        if role == "system" {
            if let Some(c) = m.get("content").and_then(|x| x.as_str()) {
                system = c.to_string();
            }
            continue;
        }
        if role == "tool" {
            let id = m.get("tool_call_id").and_then(|x| x.as_str()).unwrap_or("");
            let content = m.get("content").and_then(|x| x.as_str()).unwrap_or("");
            let is_error = m.get("is_error").and_then(|x| x.as_bool()).unwrap_or(false);
            msgs.push(json!({
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": id,
                    "content": content,
                    "is_error": is_error
                }]
            }));
            continue;
        }
        if role == "assistant" {
            if let Some(tcs) = m.get("tool_calls").and_then(|x| x.as_array()) {
                let mut blocks = Vec::new();
                if let Some(tx) = m.get("content").and_then(|x| x.as_str()) {
                    if !tx.is_empty() {
                        blocks.push(json!({"type": "text", "text": tx}));
                    }
                }
                for tc in tcs {
                    let id = tc.get("id").and_then(|x| x.as_str()).unwrap_or("");
                    let name = tc
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|x| x.as_str())
                        .unwrap_or("");
                    let args = tc
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(|x| x.as_str())
                        .unwrap_or("{}");
                    let input: Value = serde_json::from_str(args).unwrap_or(json!({}));
                    blocks.push(json!({
                        "type": "tool_use",
                        "id": id,
                        "name": name,
                        "input": input
                    }));
                }
                msgs.push(json!({ "role": "assistant", "content": blocks }));
            } else {
                let c = m.get("content").and_then(|x| x.as_str()).unwrap_or("");
                msgs.push(json!({
                    "role": "assistant",
                    "content": [{ "type": "text", "text": c }]
                }));
            }
            continue;
        }
        let content = openai_user_to_anthropic_content(m.get("content"));
        msgs.push(json!({
            "role": "user",
            "content": content
        }));
    }
    if let Some(cache_control) = cache_control {
        if let Some(last_msg) = msgs.last_mut() {
            if last_msg.get("role").and_then(|x| x.as_str()) == Some("user") {
                if let Some(content) = last_msg.get_mut("content").and_then(|x| x.as_array_mut()) {
                    if let Some(last_block) = content.last_mut() {
                        if let Some(obj) = last_block.as_object_mut() {
                            obj.insert("cache_control".to_string(), cache_control);
                        }
                    }
                }
            }
        }
    }
    (system, msgs)
}

fn openai_user_to_anthropic_content(content: Option<&Value>) -> Vec<Value> {
    match content {
        Some(Value::Array(items)) => {
            let mut out = Vec::new();
            for item in items {
                let typ = item.get("type").and_then(|x| x.as_str()).unwrap_or("");
                match typ {
                    "text" => {
                        if let Some(text) = item.get("text").and_then(|x| x.as_str()) {
                            out.push(json!({ "type": "text", "text": text }));
                        }
                    }
                    "image_url" => {
                        if let Some(url) = item
                            .get("image_url")
                            .and_then(|x| x.get("url"))
                            .and_then(|x| x.as_str())
                        {
                            if let Some((media_type, data)) = parse_data_url(url) {
                                out.push(json!({
                                    "type": "image",
                                    "source": {
                                        "type": "base64",
                                        "media_type": media_type,
                                        "data": data,
                                    }
                                }));
                            }
                        }
                    }
                    _ => {}
                }
            }
            if out.is_empty() {
                vec![json!({ "type": "text", "text": "" })]
            } else {
                out
            }
        }
        Some(Value::String(s)) => vec![json!({ "type": "text", "text": s })],
        _ => vec![json!({ "type": "text", "text": "" })],
    }
}

fn parse_data_url(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("data:")?;
    let (header, data) = rest.split_once(',')?;
    if !header.contains(";base64") {
        return None;
    }
    let media_type = header.split(';').next()?.trim();
    if media_type.is_empty() {
        return None;
    }
    Some((media_type.to_string(), data.to_string()))
}


/// Check if a Claude model supports extended thinking.
fn supports_extended_thinking(model: &str) -> bool {
    let m = model.trim().to_ascii_lowercase();
    m.starts_with("claude-sonnet-4")
        || m.starts_with("claude-opus-4")
        || m.starts_with("claude-4")
        || m.starts_with("claude-3.7-sonnet")
        || m.starts_with("claude-3-7-sonnet")
        || m.starts_with("claude-3.5-sonnet")
        || m.starts_with("claude-3-5-sonnet")
}

pub async fn run_copilot_loop(
    client: &reqwest::Client,
    base_url: &str,
    bearer_token: &str,
    model: &str,
    openai_messages: &mut Vec<Value>,
    tools_openai: &[Value],
    cwd: &Path,
    enabled: &[bool; 7],
    tx: &Sender<AgentEvent>,
    cancel: &Arc<AtomicBool>,
) -> Result<(), String> {
    let url = format!("{}/v1/messages", base_url.trim_end_matches('/'));
    let anthropic_tools = to_anthropic_tools(tools_openai);
    let cache_control = Some(json!({ "type": "ephemeral" }));
    // Enable extended thinking for models that support it.
    let supports_thinking = supports_extended_thinking(model);
    let mut round = 0u32;
    loop {
        if cancel.load(Ordering::SeqCst) {
            let _ = tx.send(AgentEvent::StreamError("Cancelled".into()));
            break;
        }
        round += 1;
        if round > 64 {
            return Err("Too many tool rounds".into());
        }
        let (system, anth_msgs) = to_anthropic_messages(openai_messages, cache_control.clone());
        let _ = tx.send(AgentEvent::AgentStart);
        let mut body = json!({
            "model": model,
            "max_tokens": if supports_thinking { 16384 } else { 8192 },
            "stream": true,
            "system": system,
            "messages": anth_msgs,
            "tools": anthropic_tools,
        });
        if supports_thinking {
            body["thinking"] = json!({
                "type": "enabled",
                "budget_tokens": 10240
            });
        }
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {bearer_token}")).map_err(|e| e.to_string())?,
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            reqwest::header::HeaderName::from_static("anthropic-version"),
            HeaderValue::from_static("2025-04-14"),
        );
        headers.insert(
            reqwest::header::HeaderName::from_static("anthropic-dangerous-direct-browser-access"),
            HeaderValue::from_static("true"),
        );
        headers.insert(reqwest::header::ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(
            reqwest::header::USER_AGENT,
            HeaderValue::from_static("GitHubCopilotChat/0.35.0"),
        );
        headers.insert(
            reqwest::header::HeaderName::from_static("editor-version"),
            HeaderValue::from_static("vscode/1.107.0"),
        );
        headers.insert(
            reqwest::header::HeaderName::from_static("editor-plugin-version"),
            HeaderValue::from_static("copilot-chat/0.35.0"),
        );
        headers.insert(
            reqwest::header::HeaderName::from_static("copilot-integration-id"),
            HeaderValue::from_static("vscode-chat"),
        );
        let initiator = copilot_x_initiator_from_openai_messages(openai_messages);
        headers.insert(
            reqwest::header::HeaderName::from_static("x-initiator"),
            HeaderValue::from_static(initiator),
        );
        headers.insert(
            reqwest::header::HeaderName::from_static("openai-intent"),
            HeaderValue::from_static("conversation-edits"),
        );
        let res = client
            .post(&url)
            .headers(headers)
            .json(&body)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !res.status().is_success() {
            let status = res.status();
            let t = res.text().await.unwrap_or_default();
            return Err(format!("HTTP {}: {}", status, t));
        }
        let mut stream = res.bytes_stream();
        let mut buf = String::new();
        let mut text_out = String::new();
        let mut tool_uses: HashMap<u64, ToolUseAccum> = HashMap::new();
        let mut stop_reason: Option<String> = None;
        let _ = tx.send(AgentEvent::TextStart);
        while let Some(chunk) = stream.next().await {
            if cancel.load(Ordering::SeqCst) {
                break;
            }
            let chunk = chunk.map_err(|e| e.to_string())?;
            buf.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(pos) = buf.find("\n\n") {
                let event_block = buf[..pos].to_string();
                buf.drain(..=pos + 1);
                parse_anthropic_event(
                    &event_block,
                    &mut text_out,
                    &mut tool_uses,
                    &mut stop_reason,
                    tx,
                )?;
            }
        }
        for block in buf.split("\n\n") {
            if !block.trim().is_empty() {
                parse_anthropic_event(block, &mut text_out, &mut tool_uses, &mut stop_reason, tx)?;
            }
        }
        let _ = tx.send(AgentEvent::AssistantMessageDone);
        let mut tus: Vec<(u64, ToolUseAccum)> = tool_uses.into_iter().collect();
        tus.sort_by_key(|(i, _)| *i);
        let tool_list: Vec<ToolUseAccum> = tus.into_iter().map(|(_, v)| v).collect();
        let sr = stop_reason.as_deref().unwrap_or("");
        if sr == "tool_use" || !tool_list.is_empty() {
            let mut asst = json!({ "role": "assistant", "content": text_out });
            if !tool_list.is_empty() {
                let arr: Vec<Value> = tool_list
                    .iter()
                    .filter(|t| !t.id.is_empty())
                    .map(|t| {
                        json!({
                            "id": &t.id,
                            "type": "function",
                            "function": {
                                "name": &t.name,
                                "arguments": &t.input_json
                            }
                        })
                    })
                    .collect();
                if let Some(obj) = asst.as_object_mut() {
                    obj.insert("tool_calls".into(), Value::Array(arr));
                } else {
                    return Err("internal: assistant message is not a JSON object".into());
                }
            }
            openai_messages.push(asst);
            // Execute tool calls in parallel where safe.
            let is_readonly = |name: &str| matches!(name, "read" | "grep" | "find" | "ls");
            struct ToolCall { id: String, name: String, args: Value }
            let parsed: Vec<ToolCall> = tool_list.into_iter().map(|tu| {
                let args: Value = serde_json::from_str(&tu.input_json).unwrap_or(json!({}));
                ToolCall { id: tu.id, name: tu.name, args }
            }).collect();

            let mut i = 0;
            while i < parsed.len() {
                if cancel.load(Ordering::SeqCst) { break; }
                if is_readonly(&parsed[i].name) {
                    let batch_start = i;
                    while i < parsed.len() && is_readonly(&parsed[i].name) { i += 1; }
                    let batch = &parsed[batch_start..i];
                    for tc in batch {
                        let _ = tx.send(AgentEvent::ToolStart {
                            name: tc.name.clone(),
                            tool_call_id: tc.id.clone(),
                            args: Some(tc.args.clone()),
                        });
                    }
                    let mut handles = Vec::new();
                    for tc in batch {
                        let cwd_owned = cwd.to_path_buf();
                        let name = tc.name.clone();
                        let args = tc.args.clone();
                        let enabled_copy = *enabled;
                        handles.push(tokio::task::spawn_blocking(move || {
                            run_tool(&cwd_owned, &name, &args, &enabled_copy)
                        }));
                    }
                    for (j, handle) in handles.into_iter().enumerate() {
                        let tc = &batch[j];
                        let result = handle.await.map_err(|e| e.to_string())?;
                        let text = result.output.clone();
                        let is_err = result.is_error;
                        let _ = tx.send(AgentEvent::ToolOutput {
                            tool_call_id: tc.id.clone(),
                            text: text.clone(),
                            truncated: text.len() >= 120_000,
                        });
                        let _ = tx.send(AgentEvent::ToolEnd {
                            tool_call_id: tc.id.clone(),
                            is_error: Some(is_err),
                            full_output_path: None,
                            diff: result.diff,
                        });
                        openai_messages.push(json!({
                            "role": "tool",
                            "tool_call_id": tc.id,
                            "content": text,
                            "is_error": is_err,
                        }));
                    }
                } else {
                    let tc = &parsed[i];
                    let _ = tx.send(AgentEvent::ToolStart {
                        name: tc.name.clone(),
                        tool_call_id: tc.id.clone(),
                        args: Some(tc.args.clone()),
                    });
                    let result = run_tool(cwd, &tc.name, &tc.args, enabled);
                    let text = result.output.clone();
                    let is_err = result.is_error;
                    let _ = tx.send(AgentEvent::ToolOutput {
                        tool_call_id: tc.id.clone(),
                        text: text.clone(),
                        truncated: text.len() >= 120_000,
                    });
                    let _ = tx.send(AgentEvent::ToolEnd {
                        tool_call_id: tc.id.clone(),
                        is_error: Some(is_err),
                        full_output_path: None,
                        diff: result.diff,
                    });
                    openai_messages.push(json!({
                        "role": "tool",
                        "tool_call_id": tc.id,
                        "content": text,
                        "is_error": is_err,
                    }));
                    i += 1;
                }
            }
            continue;
        }
        openai_messages.push(json!({
            "role": "assistant",
            "content": text_out,
        }));
        let _ = tx.send(AgentEvent::AgentEnd);
        break;
    }
    Ok(())
}

fn parse_anthropic_event(
    block: &str,
    text_out: &mut String,
    tool_uses: &mut HashMap<u64, ToolUseAccum>,
    stop_reason: &mut Option<String>,
    tx: &Sender<AgentEvent>,
) -> Result<(), String> {
    let mut event_type = "";
    let mut data_lines: Vec<&str> = Vec::new();
    for line in block.lines() {
        if let Some(rest) = line.strip_prefix("event:") {
            event_type = rest.trim();
        } else if let Some(rest) = line.strip_prefix("data:") {
            data_lines.push(rest.trim());
        }
    }
    if data_lines.is_empty() {
        return Ok(());
    }
    let data_line = data_lines.join("\n");
    if data_line == "[DONE]" {
        return Ok(());
    }
    let v: Value = serde_json::from_str(&data_line)
        .map_err(|e| format!("Anthropic SSE JSON: {e}: {data_line}"))?;
    match event_type {
        "content_block_delta" => {
            if let Some(delta) = v.get("delta") {
                let delta_type = delta.get("type").and_then(|x| x.as_str()).unwrap_or("");
                if delta_type == "thinking_delta" {
                    if let Some(t) = delta.get("thinking").and_then(|x| x.as_str()) {
                        let _ = tx.send(AgentEvent::ThinkingDelta(t.to_string()));
                    }
                } else if let Some(t) = delta.get("text").and_then(|x| x.as_str()) {
                    text_out.push_str(t);
                    let _ = tx.send(AgentEvent::TextDelta(t.to_string()));
                }
                if let Some(p) = delta.get("partial_json").and_then(|x| x.as_str()) {
                    let idx = v.get("index").and_then(|x| x.as_u64()).unwrap_or(0);
                    tool_uses.entry(idx).or_default().input_json.push_str(p);
                }
            }
        }
        "content_block_start" => {
            if let Some(cb) = v.get("content_block") {
                if cb.get("type").and_then(|x| x.as_str()) == Some("tool_use") {
                    let idx = v.get("index").and_then(|x| x.as_u64()).unwrap_or(0);
                    let entry = tool_uses.entry(idx).or_default();
                    entry.id = cb
                        .get("id")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    entry.name = cb
                        .get("name")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                }
            }
        }
        "message_delta" => {
            if let Some(sr) = v.get("delta").and_then(|d| d.get("stop_reason")) {
                if let Some(s) = sr.as_str() {
                    *stop_reason = Some(s.to_string());
                }
            }
        }
        _ => {}
    }
    Ok(())
}


#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn thinking_supported_for_known_models() {
        assert!(supports_extended_thinking("claude-sonnet-4"));
        assert!(supports_extended_thinking("claude-opus-4"));
        assert!(supports_extended_thinking("claude-3.7-sonnet"));
        assert!(supports_extended_thinking("claude-3-5-sonnet-20241022"));
    }

    #[test]
    fn thinking_not_supported_for_old_models() {
        assert!(!supports_extended_thinking("claude-3-opus-20240229"));
        assert!(!supports_extended_thinking("claude-3-haiku-20240307"));
        assert!(!supports_extended_thinking("claude-3.5-haiku-20241022"));
    }

    #[test]
    fn anthropic_tools_conversion() {
        let openai = vec![json!({
            "type": "function",
            "function": {
                "name": "read",
                "description": "Read a file",
                "parameters": { "type": "object" }
            }
        })];
        let anth = to_anthropic_tools(&openai);
        assert_eq!(anth.len(), 1);
        assert_eq!(anth[0]["name"], "read");
        assert_eq!(anth[0]["description"], "Read a file");
        assert_eq!(anth[0]["input_schema"]["type"], "object");
    }
}
