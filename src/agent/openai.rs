//! OpenAI Chat Completions streaming (used for OpenAI, OpenRouter, GPT Codex via same API).

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;

use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Value};

use super::events::AgentEvent;
use super::tools::run_tool;

#[derive(Default, Clone)]
struct ToolCallAccum {
    id: String,
    name: String,
    arguments: String,
}

pub async fn run_chat_loop(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    model: &str,
    extra_headers: &[(String, String)],
    messages: &mut Vec<Value>,
    tools: &[Value],
    cwd: &Path,
    enabled: &[bool; 7],
    tx: &Sender<AgentEvent>,
    cancel: &Arc<AtomicBool>,
) -> Result<(), String> {
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
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
        let _ = tx.send(AgentEvent::AgentStart);
        let body = json!({
            "model": model,
            "messages": messages,
            "tools": tools,
            "tool_choice": "auto",
            "stream": true,
            "parallel_tool_calls": true,
        });
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {api_key}")).map_err(|e| e.to_string())?,
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        for (k, v) in extra_headers {
            let name = HeaderName::from_bytes(k.as_bytes()).map_err(|e| e.to_string())?;
            let val = HeaderValue::from_str(v).map_err(|e| e.to_string())?;
            headers.insert(name, val);
        }
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
        let mut buffer = String::new();
        let mut assistant_text = String::new();
        let mut tool_map: HashMap<u64, ToolCallAccum> = HashMap::new();
        let mut finish_reason: Option<String> = None;
        let _ = tx.send(AgentEvent::TextStart);
        while let Some(chunk) = stream.next().await {
            if cancel.load(Ordering::SeqCst) {
                break;
            }
            let chunk = chunk.map_err(|e| e.to_string())?;
            let s = String::from_utf8_lossy(&chunk);
            buffer.push_str(&s);
            while let Some(pos) = buffer.find('\n') {
                let line = buffer[..pos].trim_end_matches('\r').to_string();
                buffer.drain(..=pos);
                process_sse_line(
                    &line,
                    &mut assistant_text,
                    &mut tool_map,
                    &mut finish_reason,
                    tx,
                )?;
            }
        }
        if !buffer.trim().is_empty() {
            for line in buffer.lines() {
                process_sse_line(
                    line.trim(),
                    &mut assistant_text,
                    &mut tool_map,
                    &mut finish_reason,
                    tx,
                )?;
            }
        }
        let _ = tx.send(AgentEvent::AssistantMessageDone);
        let mut pairs: Vec<(u64, ToolCallAccum)> = tool_map.into_iter().collect();
        pairs.sort_by_key(|(i, _)| *i);
        let tool_calls: Vec<ToolCallAccum> = pairs.into_iter().map(|(_, v)| v).collect();
        let fr = finish_reason.as_deref().unwrap_or("stop");
        if fr == "tool_calls" && tool_calls.is_empty() {
            return Err("Model requested tool_calls but no tool calls were parsed".into());
        }
        if fr == "tool_calls" || !tool_calls.is_empty() {
            let mut msg = json!({ "role": "assistant", "content": assistant_text });
            if !tool_calls.is_empty() {
                let arr: Vec<Value> = tool_calls
                    .iter()
                    .filter(|t| !t.id.is_empty())
                    .map(|t| {
                        json!({
                            "id": &t.id,
                            "type": "function",
                            "function": { "name": &t.name, "arguments": &t.arguments }
                        })
                    })
                    .collect();
                if let Some(obj) = msg.as_object_mut() {
                    obj.insert("tool_calls".into(), Value::Array(arr));
                } else {
                    return Err("internal: assistant message is not a JSON object".into());
                }
            }
            messages.push(msg);
            for tc in tool_calls {
                if cancel.load(Ordering::SeqCst) {
                    break;
                }
                let args: Value = serde_json::from_str(&tc.arguments).unwrap_or(json!({}));
                let tid = tc.id.clone();
                let _ = tx.send(AgentEvent::ToolStart {
                    name: tc.name.clone(),
                    tool_call_id: tid.clone(),
                    args: Some(args.clone()),
                });
                let result = run_tool(cwd, &tc.name, &args, enabled);
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
                messages.push(json!({
                    "role": "tool",
                    "tool_call_id": tid,
                    "content": text,
                }));
            }
            continue;
        }
        messages.push(json!({
            "role": "assistant",
            "content": assistant_text,
        }));
        let _ = tx.send(AgentEvent::AgentEnd);
        break;
    }
    Ok(())
}

fn process_sse_line(
    line: &str,
    assistant_text: &mut String,
    tool_map: &mut HashMap<u64, ToolCallAccum>,
    finish_reason: &mut Option<String>,
    tx: &Sender<AgentEvent>,
) -> Result<(), String> {
    let line = line.trim();
    if line.is_empty() || line == "data: [DONE]" {
        return Ok(());
    }
    let data = line.strip_prefix("data:").unwrap_or(line).trim();
    if data == "[DONE]" {
        return Ok(());
    }
    let v: Value = serde_json::from_str(data).map_err(|e| format!("SSE JSON: {e}: {data}"))?;
    if let Some(err) = v.get("error") {
        let msg = err
            .get("message")
            .and_then(|x| x.as_str())
            .unwrap_or("API error");
        let _ = tx.send(AgentEvent::StreamError(msg.to_string()));
        return Ok(());
    }
    if let Some(fr) = v
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("finish_reason"))
        .and_then(|x| x.as_str())
    {
        if !fr.is_empty() {
            *finish_reason = Some(fr.to_string());
        }
    }
    let delta = v
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("delta"));
    if let Some(d) = delta {
        if let Some(tc) = d.get("tool_calls").and_then(|x| x.as_array()) {
            for t in tc {
                let idx = t.get("index").and_then(|x| x.as_u64()).unwrap_or(0);
                let entry = tool_map.entry(idx).or_default();
                if let Some(id) = t.get("id").and_then(|x| x.as_str()) {
                    if !id.is_empty() {
                        entry.id = id.to_string();
                    }
                }
                if let Some(f) = t.get("function") {
                    if let Some(n) = f.get("name").and_then(|x| x.as_str()) {
                        if !n.is_empty() {
                            entry.name = n.to_string();
                        }
                    }
                    if let Some(a) = f.get("arguments").and_then(|x| x.as_str()) {
                        entry.arguments.push_str(a);
                    }
                }
            }
        }
        if let Some(content) = d.get("content").and_then(|x| x.as_str()) {
            assistant_text.push_str(content);
            let _ = tx.send(AgentEvent::TextDelta(content.to_string()));
        }
    }
    Ok(())
}
