//! Anthropic Messages API streaming (used by OpenCode Go's Anthropic-compatible models).

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::mpsc::Sender;

use futures_util::StreamExt;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde_json::{Value, json};

use super::events::{AgentEvent, TokenUsage};
use super::loop_ctx::LoopCtx;
use super::net::{MAX_STREAM_RETRIES, backoff_delay, send_with_retry, sleep_cancellable};
use super::tools::{MAX_TOOL_OUTPUT_CHARS, ToolResult, run_tool};

/// Overwrite `dst` with the u64 field `key` from a `usage` JSON object, if present.
fn read_usage_field(usage: &Value, key: &str, dst: &mut u64) {
    if let Some(n) = usage.get(key).and_then(|x| x.as_u64()) {
        *dst = n;
    }
}

#[derive(Default, Clone)]
struct ToolUseAccum {
    id: String,
    name: String,
    input_json: String,
    /// Whether we already emitted an early [`AgentEvent::ToolStart`] for this call
    /// while `input_json` was still streaming (so the UI can show a running pill immediately).
    started: bool,
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
                if let Some(tx) = m.get("content").and_then(|x| x.as_str())
                    && !tx.is_empty()
                {
                    blocks.push(json!({"type": "text", "text": tx}));
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
        let mut added = 0usize;
        for msg in msgs.iter_mut().rev() {
            if added >= 2 {
                break;
            }
            if msg.get("role").and_then(|x| x.as_str()) != Some("user") {
                continue;
            }
            if let Some(content) = msg.get_mut("content").and_then(|x| x.as_array_mut())
                && let Some(last_block) = content.last_mut()
                && let Some(obj) = last_block.as_object_mut()
            {
                obj.insert("cache_control".to_string(), cache_control.clone());
                added += 1;
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
                            && let Some((media_type, data)) = parse_data_url(url)
                        {
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

pub(crate) fn is_valid_effort(effort: &str) -> bool {
    matches!(effort.trim(), "low" | "medium" | "high" | "xhigh" | "max")
}

pub(crate) fn thinking_config(model: &str) -> Option<Value> {
    let m = model.trim().to_ascii_lowercase();
    let adaptive = [
        "claude-opus-4-6",
        "claude-opus-4-7",
        "claude-opus-4-8",
        "claude-sonnet-4-6",
        "claude-sonnet-5",
        "claude-fable-5",
        "claude-mythos-",
    ];
    if adaptive.iter().any(|prefix| m.starts_with(prefix)) {
        return Some(json!({ "type": "adaptive" }));
    }
    let budgeted = [
        "claude-sonnet-4",
        "claude-opus-4",
        "claude-4",
        "claude-3.7-sonnet",
        "claude-3-7-sonnet",
        "claude-3.5-sonnet",
        "claude-3-5-sonnet",
    ];
    budgeted
        .iter()
        .any(|prefix| m.starts_with(prefix))
        .then(|| json!({ "type": "enabled", "budget_tokens": 10240 }))
}

pub async fn run_anthropic_loop(
    ctx: &mut LoopCtx<'_>,
    bearer_token: &str,
    extra_headers: &[(String, String)],
    openai_messages: &mut Vec<Value>,
    tools_openai: &[Value],
) -> Result<(), String> {
    let client = ctx.client;
    let base_url = ctx.base_url;
    let model = ctx.model;
    let cwd = ctx.cwd;
    let env = ctx.env;
    let tx = ctx.tx;
    let cancel = ctx.cancel;
    let max_rounds = ctx.max_rounds;
    let effort_override = ctx.effort_override;
    let gate = &mut *ctx.gate;
    let url = format!("{}/v1/messages", base_url.trim_end_matches('/'));
    let anthropic_tools = to_anthropic_tools(tools_openai);
    let cache_control = Some(json!({ "type": "ephemeral" }));
    let thinking = thinking_config(model);
    let mut round = 0u32;
    let mut stream_retries = 0u32;
    loop {
        if cancel.load(Ordering::SeqCst) {
            let _ = tx.send(AgentEvent::StreamError("Cancelled".into()));
            break;
        }
        round += 1;
        if max_rounds != 0 && round > max_rounds {
            return Err(format!("Too many tool rounds (>{max_rounds})"));
        }
        let (system, anth_msgs) = to_anthropic_messages(openai_messages, cache_control.clone());
        let _ = tx.send(AgentEvent::AgentStart);
        let mut body = json!({
            "model": model,
            "max_tokens": if thinking.is_some() { 16384 } else { 8192 },
            "stream": true,
            "system": [{"type":"text","text":system,"cache_control":{"type":"ephemeral"}}],
            "messages": anth_msgs,
            "tools": anthropic_tools,
        });
        if let Some(thinking) = thinking.clone() {
            body["thinking"] = thinking;
        }
        if thinking
            .as_ref()
            .and_then(|v| v.get("type"))
            .and_then(|v| v.as_str())
            == Some("adaptive")
            && let Some(effort) = effort_override.filter(|e| is_valid_effort(e))
        {
            body["output_config"] = json!({ "effort": effort });
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
            reqwest::header::ACCEPT,
            HeaderValue::from_static("application/json"),
        );
        for (k, v) in extra_headers {
            let name = HeaderName::from_bytes(k.as_bytes()).map_err(|e| e.to_string())?;
            let val = HeaderValue::from_str(v).map_err(|e| e.to_string())?;
            headers.insert(name, val);
        }
        let res = send_with_retry(client.post(&url).headers(headers).json(&body), cancel).await?;
        let mut stream = res.bytes_stream();
        let mut buf = String::new();
        let mut text_out = String::new();
        let mut tool_uses: HashMap<u64, ToolUseAccum> = HashMap::new();
        let mut stop_reason: Option<String> = None;
        let mut stream_error: Option<String> = None;
        let mut round_usage = TokenUsage::default();
        let _ = tx.send(AgentEvent::TextStart);
        while let Some(chunk) = stream.next().await {
            if cancel.load(Ordering::SeqCst) {
                break;
            }
            let chunk = match chunk {
                Ok(c) => c,
                Err(e) => {
                    stream_error = Some(e.to_string());
                    break;
                }
            };
            buf.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(pos) = buf.find("\n\n") {
                let event_block = buf[..pos].to_string();
                buf.drain(..=pos + 1);
                parse_anthropic_event(
                    &event_block,
                    &mut text_out,
                    &mut tool_uses,
                    &mut stop_reason,
                    &mut stream_error,
                    &mut round_usage,
                    tx,
                );
            }
        }
        if stream_error.is_none() {
            for block in buf.split("\n\n") {
                if !block.trim().is_empty() {
                    parse_anthropic_event(
                        block,
                        &mut text_out,
                        &mut tool_uses,
                        &mut stop_reason,
                        &mut stream_error,
                        &mut round_usage,
                        tx,
                    );
                }
            }
        }
        // The stream died (dropped connection or in-band error event) before the round
        // completed. No tool has been executed yet, so re-sending the round is safe.
        if let Some(err) = stream_error {
            if cancel.load(Ordering::SeqCst) {
                let _ = tx.send(AgentEvent::StreamError("Cancelled".into()));
                break;
            }
            stream_retries += 1;
            if stream_retries > MAX_STREAM_RETRIES {
                return Err(err);
            }
            let _ = tx.send(AgentEvent::StreamRetry {
                attempt: stream_retries,
                reason: err,
            });
            if !sleep_cancellable(backoff_delay(stream_retries), cancel).await {
                let _ = tx.send(AgentEvent::StreamError("Cancelled".into()));
                break;
            }
            round -= 1;
            continue;
        }
        stream_retries = 0;
        if !round_usage.is_zero() {
            let _ = tx.send(AgentEvent::Usage(round_usage));
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
            let is_readonly = |name: &str| {
                matches!(
                    name,
                    "read"
                        | "grep"
                        | "find"
                        | "ls"
                        | "codebase_search"
                        | "git_status"
                        | "git_diff"
                        | "web_search"
                        | "web_fetch"
                )
            };
            struct ToolCall {
                id: String,
                name: String,
                args: Value,
            }
            let parsed: Vec<ToolCall> = tool_list
                .into_iter()
                .map(|tu| {
                    let args: Value = serde_json::from_str(&tu.input_json).unwrap_or(json!({}));
                    ToolCall {
                        id: tu.id,
                        name: tu.name,
                        args,
                    }
                })
                .collect();

            let mut i = 0;
            while i < parsed.len() {
                if cancel.load(Ordering::SeqCst) {
                    break;
                }
                if is_readonly(&parsed[i].name) {
                    let batch_start = i;
                    while i < parsed.len() && is_readonly(&parsed[i].name) {
                        i += 1;
                    }
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
                        let env_copy = env.clone();
                        handles.push(tokio::task::spawn_blocking(move || {
                            run_tool(&cwd_owned, &name, &args, &env_copy)
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
                            truncated: text.len() >= MAX_TOOL_OUTPUT_CHARS,
                        });
                        let _ = tx.send(AgentEvent::ToolEnd {
                            tool_call_id: tc.id.clone(),
                            is_error: Some(is_err),
                            full_output_path: result.full_output_path,
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
                    let result = match gate.request(tx, cancel, &tc.name, &tc.args) {
                        Ok(()) => run_tool(cwd, &tc.name, &tc.args, env),
                        Err(reason) => ToolResult {
                            output: reason,
                            is_error: true,
                            diff: None,
                            full_output_path: None,
                        },
                    };
                    let text = result.output.clone();
                    let is_err = result.is_error;
                    let _ = tx.send(AgentEvent::ToolOutput {
                        tool_call_id: tc.id.clone(),
                        text: text.clone(),
                        truncated: text.len() >= MAX_TOOL_OUTPUT_CHARS,
                    });
                    let _ = tx.send(AgentEvent::ToolEnd {
                        tool_call_id: tc.id.clone(),
                        is_error: Some(is_err),
                        full_output_path: result.full_output_path,
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
        let _ = tx.send(AgentEvent::ProviderDone);
        break;
    }
    Ok(())
}

fn parse_anthropic_event(
    block: &str,
    text_out: &mut String,
    tool_uses: &mut HashMap<u64, ToolUseAccum>,
    stop_reason: &mut Option<String>,
    stream_error: &mut Option<String>,
    usage: &mut TokenUsage,
    tx: &Sender<AgentEvent>,
) {
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
        return;
    }
    let data_line = data_lines.join("\n");
    if data_line == "[DONE]" {
        return;
    }
    // A malformed event from a flaky provider should not kill the whole run.
    let v: Value = match serde_json::from_str(&data_line) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[oxi] skipping malformed Anthropic SSE event ({e}): {data_line}");
            return;
        }
    };
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
            if let Some(cb) = v.get("content_block")
                && cb.get("type").and_then(|x| x.as_str()) == Some("tool_use")
            {
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
                // Anthropic provides name+id at block start — emit ToolStart immediately so
                // the UI shows a running pill while `partial_json` arguments still stream in.
                if !entry.started && !entry.id.is_empty() && !entry.name.is_empty() {
                    entry.started = true;
                    let _ = tx.send(AgentEvent::ToolStart {
                        name: entry.name.clone(),
                        tool_call_id: entry.id.clone(),
                        args: None,
                    });
                }
            }
        }
        "message_start" => {
            if let Some(u) = v.pointer("/message/usage") {
                read_usage_field(u, "input_tokens", &mut usage.input_tokens);
                read_usage_field(
                    u,
                    "cache_read_input_tokens",
                    &mut usage.cache_read_input_tokens,
                );
                read_usage_field(
                    u,
                    "cache_creation_input_tokens",
                    &mut usage.cache_creation_input_tokens,
                );
            }
        }
        "message_delta" => {
            if let Some(sr) = v.get("delta").and_then(|d| d.get("stop_reason"))
                && let Some(s) = sr.as_str()
            {
                *stop_reason = Some(s.to_string());
            }
            // `output_tokens` on message_delta is cumulative; the last value wins.
            if let Some(u) = v.get("usage") {
                read_usage_field(u, "output_tokens", &mut usage.output_tokens);
            }
        }
        // In-band errors (e.g. `overloaded_error`): surface for the round-retry logic.
        "error" => {
            let msg = v
                .pointer("/error/message")
                .and_then(|x| x.as_str())
                .unwrap_or("Anthropic stream error");
            *stream_error = Some(msg.to_string());
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn thinking_budgeted_for_known_older_models() {
        assert_eq!(
            thinking_config("claude-sonnet-4").unwrap()["type"],
            "enabled"
        );
        assert_eq!(thinking_config("claude-opus-4").unwrap()["type"], "enabled");
        assert_eq!(
            thinking_config("claude-3.7-sonnet").unwrap()["type"],
            "enabled"
        );
        assert_eq!(
            thinking_config("claude-3-5-sonnet-20241022").unwrap()["type"],
            "enabled"
        );
    }

    #[test]
    fn thinking_adaptive_for_new_claude_models() {
        assert_eq!(
            thinking_config("claude-opus-4-6").unwrap()["type"],
            "adaptive"
        );
        assert_eq!(
            thinking_config("claude-sonnet-5").unwrap()["type"],
            "adaptive"
        );
        assert_eq!(
            thinking_config("claude-fable-5").unwrap()["type"],
            "adaptive"
        );
        assert_eq!(
            thinking_config("claude-mythos-next").unwrap()["type"],
            "adaptive"
        );
    }

    #[test]
    fn thinking_not_supported_for_old_models() {
        assert!(thinking_config("claude-3-opus-20240229").is_none());
        assert!(thinking_config("claude-3-haiku-20240307").is_none());
        assert!(thinking_config("claude-3.5-haiku-20241022").is_none());
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
