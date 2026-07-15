//! OpenAI Codex ChatGPT backend (`/codex/responses` SSE) — OAuth access token + `chatgpt-account-id`.

use std::sync::atomic::Ordering;
use std::sync::mpsc::Sender;

use futures_util::StreamExt;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde_json::{Value, json};

use super::events::{AgentEvent, TokenUsage};
use super::loop_ctx::LoopCtx;
use super::net::{MAX_STREAM_RETRIES, backoff_delay, send_with_retry, sleep_cancellable};
use super::tools::{MAX_TOOL_OUTPUT_CHARS, ToolResult, run_tool};

#[derive(Default, Clone)]
struct ToolCallAccum {
    id: String,
    name: String,
    arguments: String,
}

/// Deduplicate `output_item.done` reasoning summary when streaming deltas already filled thinking.
#[derive(Default)]
struct CodexStreamState {
    got_thinking_delta: bool,
}

/// Find first SSE record boundary (`\r\n\r\n` or `\n\n`).
fn sse_record_end(buffer: &str) -> Option<(usize, usize)> {
    if let Some(i) = buffer.find("\r\n\r\n") {
        return Some((i, 4));
    }
    buffer.find("\n\n").map(|i| (i, 2))
}

/// Parse buffered SSE like `packages/ai` `parseSSE`: each record is separated by blank line;
/// only `data:` lines are joined and JSON-parsed (`event:` lines are ignored).
fn drain_codex_sse_blocks(
    buffer: &mut String,
    assistant_text: &mut String,
    pending_tools: &mut Vec<ToolCallAccum>,
    sse_state: &mut CodexStreamState,
    stream_error: &mut Option<String>,
    usage: &mut TokenUsage,
    tx: &Sender<AgentEvent>,
) {
    while let Some((idx, sep_len)) = sse_record_end(buffer) {
        let chunk = buffer[..idx].to_string();
        buffer.drain(..idx + sep_len);
        let data_payload: String = chunk
            .lines()
            .map(|l| l.trim_end_matches('\r'))
            .filter(|l| l.starts_with("data:"))
            .map(|l| l.strip_prefix("data:").unwrap_or(l).trim())
            .collect::<Vec<_>>()
            .join("\n");
        if data_payload.is_empty() || data_payload == "[DONE]" {
            continue;
        }
        let v: Value = match serde_json::from_str(&data_payload) {
            Ok(v) => v,
            Err(_) => continue,
        };
        process_responses_event(
            &v,
            assistant_text,
            pending_tools,
            sse_state,
            stream_error,
            usage,
            tx,
        );
    }
}

fn format_codex_http_error(status: reqwest::StatusCode, body: &str) -> String {
    if let Ok(v) = serde_json::from_str::<Value>(body) {
        if let Some(d) = v.get("detail").and_then(|x| x.as_str()) {
            let mut s = format!("HTTP {}: {}", status.as_u16(), d);
            if d.contains("not supported") && d.contains("Codex") {
                s.push_str(
                    " Pick a model allowed for ChatGPT + Codex (not all GPT-5.x ids work with this account).",
                );
            }
            return s;
        }
        if let Some(m) = v.get("message").and_then(|x| x.as_str()) {
            return format!("HTTP {}: {}", status.as_u16(), m);
        }
    }
    format!("HTTP {}: {}", status.as_u16(), body)
}

/// Re-parse a `"HTTP {status}: {body}"` string from `send_with_retry` so the
/// Codex-specific hints in [`format_codex_http_error`] still apply.
fn reformat_codex_error(err: String) -> String {
    let Some(rest) = err.strip_prefix("HTTP ") else {
        return err;
    };
    let Some((status, body)) = rest.split_once(": ") else {
        return err;
    };
    // `StatusCode` displays as e.g. "429 Too Many Requests": take the leading number.
    let code = status
        .split_whitespace()
        .next()
        .and_then(|s| s.parse::<u16>().ok())
        .and_then(|c| reqwest::StatusCode::from_u16(c).ok());
    match code {
        Some(code) => format_codex_http_error(code, body),
        None => err,
    }
}

fn resolve_codex_post_url(base_url: &str) -> String {
    let raw = base_url.trim().trim_end_matches('/');
    if raw.ends_with("/codex/responses") {
        return raw.to_string();
    }
    if raw.ends_with("/codex") {
        return format!("{}/responses", raw);
    }
    format!("{}/codex/responses", raw)
}

fn responses_tools(tools: &[Value]) -> Vec<Value> {
    let mut out = Vec::new();
    for t in tools {
        if let Some(f) = t.get("function") {
            let name = f.get("name").and_then(|x| x.as_str()).unwrap_or("");
            let desc = f.get("description").and_then(|x| x.as_str()).unwrap_or("");
            let params = f.get("parameters").cloned().unwrap_or(json!({}));
            out.push(json!({
                "type": "function",
                "name": name,
                "description": desc,
                "parameters": params,
                "strict": false
            }));
        }
    }
    out
}

fn split_system(messages: &[Value]) -> (String, Vec<Value>) {
    let mut rest = messages.to_vec();
    if rest
        .first()
        .and_then(|m| m.get("role"))
        .and_then(|r| r.as_str())
        == Some("system")
    {
        let sys = rest
            .remove(0)
            .get("content")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string();
        return (sys, rest);
    }
    (String::new(), rest)
}

fn chat_to_input(messages: &[Value]) -> Result<Vec<Value>, String> {
    let mut input = Vec::new();
    for m in messages {
        let role = m.get("role").and_then(|x| x.as_str()).unwrap_or("");
        if role == "user" {
            input.push(json!({
                "role": "user",
                "content": openai_user_to_responses_input(m.get("content"))
            }));
            continue;
        }
        if role == "assistant" {
            if let Some(tcs) = m.get("tool_calls").and_then(|x| x.as_array()) {
                if let Some(content) = m.get("content").and_then(|c| c.as_str())
                    && !content.is_empty()
                {
                    input.push(json!({
                        "type": "message",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": content, "annotations": []}],
                        "status": "completed"
                    }));
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
                    input.push(json!({
                        "type": "function_call",
                        "call_id": id,
                        "name": name,
                        "arguments": args
                    }));
                }
            } else {
                let text = m
                    .get("content")
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .to_string();
                if !text.is_empty() {
                    input.push(json!({
                        "type": "message",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": text, "annotations": []}],
                        "status": "completed"
                    }));
                }
            }
            continue;
        }
        if role == "tool" {
            let call_id = m.get("tool_call_id").and_then(|x| x.as_str()).unwrap_or("");
            let output = m.get("content").and_then(|x| x.as_str()).unwrap_or("");
            input.push(json!({
                "type": "function_call_output",
                "call_id": call_id,
                "output": output
            }));
            continue;
        }
    }
    Ok(input)
}

fn openai_user_to_responses_input(content: Option<&Value>) -> Vec<Value> {
    match content {
        Some(Value::Array(items)) => {
            let mut out = Vec::new();
            for item in items {
                let typ = item.get("type").and_then(|x| x.as_str()).unwrap_or("");
                match typ {
                    "text" => {
                        if let Some(text) = item.get("text").and_then(|x| x.as_str()) {
                            out.push(json!({ "type": "input_text", "text": text }));
                        }
                    }
                    "image_url" => {
                        if let Some(url) = item
                            .get("image_url")
                            .and_then(|x| x.get("url"))
                            .and_then(|x| x.as_str())
                        {
                            out.push(json!({ "type": "input_image", "image_url": url }));
                        }
                    }
                    _ => {}
                }
            }
            if out.is_empty() {
                vec![json!({ "type": "input_text", "text": "" })]
            } else {
                out
            }
        }
        Some(Value::String(s)) => vec![json!({ "type": "input_text", "text": s })],
        _ => vec![json!({ "type": "input_text", "text": "" })],
    }
}

/// Text chunk from Responses API stream events (`delta` string or `delta.text`).
fn responses_stream_text_chunk(v: &Value) -> Option<&str> {
    match v.get("delta") {
        Some(d) => d
            .as_str()
            .or_else(|| d.get("text").and_then(|x| x.as_str())),
        None => v.get("text").and_then(|x| x.as_str()),
    }
}

/// Join `item.summary[].text` from a completed reasoning output item (pi-mono `output_item.done`).
fn reasoning_item_summary_joined(item: &Value) -> Option<String> {
    let arr = item.get("summary")?.as_array()?;
    let parts: Vec<&str> = arr
        .iter()
        .filter_map(|p| p.get("text").and_then(|x| x.as_str()))
        .collect();
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("\n\n"))
}

fn process_responses_event(
    v: &Value,
    assistant_text: &mut String,
    pending_tools: &mut Vec<ToolCallAccum>,
    state: &mut CodexStreamState,
    stream_error: &mut Option<String>,
    usage: &mut TokenUsage,
    tx: &Sender<AgentEvent>,
) {
    let typ = v.get("type").and_then(|x| x.as_str()).unwrap_or("");

    match typ {
        "response.completed" => {
            if let Some(u) = v.pointer("/response/usage") {
                read_responses_usage(u, usage);
            }
        }
        "response.created" | "response.incomplete" => {}
        // New reasoning item in the stream — allow a later `output_item.done` fallback for this item.
        "response.output_item.added" => {
            if let Some(item) = v.get("item")
                && item.get("type").and_then(|x| x.as_str()) == Some("reasoning")
            {
                state.got_thinking_delta = false;
            }
        }
        "response.output_text.delta" => {
            if let Some(d) = responses_stream_text_chunk(v) {
                assistant_text.push_str(d);
                let _ = tx.send(AgentEvent::TextDelta(d.to_string()));
            }
        }
        // ChatGPT Codex / Responses API (pi-mono `processResponsesStream`).
        "response.reasoning_summary_text.delta" => {
            if let Some(d) = responses_stream_text_chunk(v)
                && !d.is_empty()
            {
                state.got_thinking_delta = true;
                let _ = tx.send(AgentEvent::ThinkingDelta(d.to_string()));
            }
        }
        // Alternate / older event names (direct deltas without summary-part handshake).
        "response.reasoning.delta"
        | "response.reasoning_text.delta"
        | "response.reasoning_text_delta"
        | "response.reasoning_summary_text_delta" => {
            if let Some(d) = responses_stream_text_chunk(v)
                && !d.is_empty()
            {
                state.got_thinking_delta = true;
                let _ = tx.send(AgentEvent::ThinkingDelta(d.to_string()));
            }
        }
        "response.output_item.done" => {
            if let Some(item) = v.get("item") {
                match item.get("type").and_then(|x| x.as_str()) {
                    Some("reasoning") if !state.got_thinking_delta => {
                        if let Some(text) = reasoning_item_summary_joined(item)
                            && !text.is_empty()
                        {
                            let _ = tx.send(AgentEvent::ThinkingDelta(text));
                        }
                    }
                    Some("function_call") => {
                        let name = item.get("name").and_then(|x| x.as_str()).unwrap_or("");
                        let args = item
                            .get("arguments")
                            .and_then(|x| x.as_str())
                            .unwrap_or("{}");
                        let call_id = item.get("call_id").and_then(|x| x.as_str()).unwrap_or("");
                        // Emit ToolStart as soon as the completed function_call item arrives —
                        // don't wait for the rest of the SSE stream / response.completed.
                        if !call_id.is_empty() && !name.is_empty() {
                            let parsed_args: Value =
                                serde_json::from_str(args).unwrap_or(json!({}));
                            let _ = tx.send(AgentEvent::ToolStart {
                                name: name.to_string(),
                                tool_call_id: call_id.to_string(),
                                args: Some(parsed_args),
                            });
                        }
                        pending_tools.push(ToolCallAccum {
                            id: call_id.to_string(),
                            name: name.to_string(),
                            arguments: args.to_string(),
                        });
                    }
                    _ => {}
                }
            }
        }
        // In-band failures: surface for the round-retry logic instead of aborting.
        "error" => {
            let msg = v
                .get("message")
                .and_then(|x| x.as_str())
                .or_else(|| v.get("code").and_then(|x| x.as_str()))
                .unwrap_or("Codex API error");
            *stream_error = Some(msg.to_string());
        }
        "response.failed" => {
            let msg = v
                .pointer("/response/error/message")
                .and_then(|x| x.as_str())
                .unwrap_or("response.failed");
            *stream_error = Some(msg.to_string());
        }
        _ => {}
    }
}

fn read_responses_usage(v: &Value, usage: &mut TokenUsage) {
    if let Some(n) = v.get("input_tokens").and_then(|x| x.as_u64()) {
        usage.input_tokens = n;
    }
    if let Some(n) = v.get("output_tokens").and_then(|x| x.as_u64()) {
        usage.output_tokens = n;
    }
    if let Some(n) = v
        .pointer("/input_tokens_details/cached_tokens")
        .and_then(|x| x.as_u64())
    {
        usage.cache_read_input_tokens = n;
        usage.input_tokens = usage.input_tokens.saturating_sub(n);
    }
}

pub async fn run_codex_responses_loop(
    ctx: &mut LoopCtx<'_>,
    access_token: &str,
    account_id: &str,
    messages: &mut Vec<Value>,
    tools: &[Value],
) -> Result<(), String> {
    let client = ctx.client;
    let base_url = ctx.base_url;
    let model = ctx.model;
    let cwd = ctx.cwd;
    let env = ctx.env;
    let tx = ctx.tx;
    let cancel = ctx.cancel;
    let max_rounds = ctx.max_rounds;
    let gate = &mut *ctx.gate;
    let effort_override = ctx.effort_override;
    let url = resolve_codex_post_url(base_url);
    let rtools = responses_tools(tools);
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
        let (instructions, rest) = split_system(messages);
        let input = chat_to_input(&rest)?;
        let reasoning_effort = effort_override
            .filter(|e| crate::agent::openai::is_valid_reasoning_effort(e))
            .unwrap_or("medium");
        let body = json!({
            "model": model,
            "stream": true,
            "store": false,
            "instructions": instructions,
            "input": input,
            "tools": rtools,
            "tool_choice": "auto",
            "parallel_tool_calls": true,
            "text": { "verbosity": "medium" },
            "reasoning": { "effort": reasoning_effort, "summary": "auto" },
            "include": ["reasoning.encrypted_content"]
        });
        let _ = tx.send(AgentEvent::AgentStart);
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {access_token}")).map_err(|e| e.to_string())?,
        );
        headers.insert(
            reqwest::header::HeaderName::from_static("chatgpt-account-id"),
            HeaderValue::from_str(account_id).map_err(|e| e.to_string())?,
        );
        headers.insert(
            reqwest::header::HeaderName::from_static("originator"),
            HeaderValue::from_static("oxi"),
        );
        headers.insert(
            reqwest::header::HeaderName::from_static("openai-beta"),
            HeaderValue::from_static("responses=experimental"),
        );
        headers.insert("accept", HeaderValue::from_static("text/event-stream"));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let res =
            match send_with_retry(client.post(&url).headers(headers).json(&body), cancel).await {
                Ok(r) => r,
                // Re-shape "HTTP {status}: {body}" errors with the Codex-specific hints.
                Err(e) => return Err(reformat_codex_error(e)),
            };
        let mut stream = res.bytes_stream();
        let mut buffer = String::new();
        let mut assistant_text = String::new();
        let mut pending_tools: Vec<ToolCallAccum> = Vec::new();
        let mut sse_state = CodexStreamState::default();
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
            buffer.push_str(&String::from_utf8_lossy(&chunk));
            drain_codex_sse_blocks(
                &mut buffer,
                &mut assistant_text,
                &mut pending_tools,
                &mut sse_state,
                &mut stream_error,
                &mut round_usage,
                tx,
            );
        }
        if stream_error.is_none() && !buffer.trim().is_empty() {
            buffer.push('\n');
            buffer.push('\n');
            drain_codex_sse_blocks(
                &mut buffer,
                &mut assistant_text,
                &mut pending_tools,
                &mut sse_state,
                &mut stream_error,
                &mut round_usage,
                tx,
            );
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

        let tool_calls = pending_tools;

        if !tool_calls.is_empty() {
            let mut msg = json!({ "role": "assistant", "content": assistant_text });
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
            if !arr.is_empty() {
                if let Some(obj) = msg.as_object_mut() {
                    obj.insert("tool_calls".into(), Value::Array(arr));
                } else {
                    return Err("internal: assistant message is not a JSON object".into());
                }
            }
            messages.push(msg);
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
            struct ToolCallP {
                id: String,
                name: String,
                args: Value,
            }
            let parsed: Vec<ToolCallP> = tool_calls
                .into_iter()
                .map(|tc| {
                    let args: Value = serde_json::from_str(&tc.arguments).unwrap_or(json!({}));
                    ToolCallP {
                        id: tc.id,
                        name: tc.name,
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
                        messages.push(json!({
                            "role": "tool",
                            "tool_call_id": tc.id,
                            "content": text,
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
                    messages.push(json!({
                        "role": "tool",
                        "tool_call_id": tc.id,
                        "content": text,
                    }));
                    i += 1;
                }
            }
            continue;
        }
        messages.push(json!({
            "role": "assistant",
            "content": assistant_text,
        }));
        let _ = tx.send(AgentEvent::ProviderDone);
        break;
    }
    Ok(())
}
