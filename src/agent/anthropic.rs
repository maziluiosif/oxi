//! Anthropic Messages API streaming (GitHub Copilot uses the same wire format at a custom `base_url`).

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;

use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Value};

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
fn to_anthropic_messages(openai: &[Value]) -> (String, Vec<Value>) {
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
            msgs.push(json!({
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": id,
                    "content": content
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
        let (system, anth_msgs) = to_anthropic_messages(openai_messages);
        let _ = tx.send(AgentEvent::AgentStart);
        let body = json!({
            "model": model,
            "max_tokens": 8192,
            "stream": true,
            "system": system,
            "messages": anth_msgs,
            "tools": anthropic_tools,
        });
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {bearer_token}")).map_err(|e| e.to_string())?,
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            reqwest::header::HeaderName::from_static("anthropic-version"),
            HeaderValue::from_static("2023-06-01"),
        );
        headers.insert(
            reqwest::header::HeaderName::from_static("anthropic-dangerous-direct-browser-access"),
            HeaderValue::from_static("true"),
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
            for tu in tool_list {
                if cancel.load(Ordering::SeqCst) {
                    break;
                }
                let args: Value = serde_json::from_str(&tu.input_json).unwrap_or(json!({}));
                let tid = tu.id.clone();
                let _ = tx.send(AgentEvent::ToolStart {
                    name: tu.name.clone(),
                    tool_call_id: tid.clone(),
                    args: Some(args.clone()),
                });
                let result = run_tool(cwd, &tu.name, &args, enabled);
                let (text, is_err) = match result {
                    Ok(s) => (s, false),
                    Err(e) => (e, true),
                };
                let _ = tx.send(AgentEvent::ToolOutput {
                    tool_call_id: tid.clone(),
                    text: text.clone(),
                    truncated: text.len() >= 120_000,
                });
                let _ = tx.send(AgentEvent::ToolEnd {
                    tool_call_id: tid.clone(),
                    is_error: Some(is_err),
                    full_output_path: None,
                    diff: None,
                });
                openai_messages.push(json!({
                    "role": "tool",
                    "tool_call_id": tid,
                    "content": text,
                }));
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
    let mut data_line = "";
    for line in block.lines() {
        if let Some(rest) = line.strip_prefix("event:") {
            event_type = rest.trim();
        } else if let Some(rest) = line.strip_prefix("data:") {
            data_line = rest.trim();
        }
    }
    if data_line.is_empty() {
        return Ok(());
    }
    let v: Value = serde_json::from_str(data_line).map_err(|e| e.to_string())?;
    match event_type {
        "content_block_delta" => {
            if let Some(delta) = v.get("delta") {
                if let Some(t) = delta.get("text").and_then(|x| x.as_str()) {
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
