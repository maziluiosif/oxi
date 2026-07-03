//! OpenAI Chat Completions streaming (used for OpenAI, OpenRouter, GPT Codex via same API).

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::mpsc::Sender;

use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Value};

use super::events::AgentEvent;
use super::loop_ctx::LoopCtx;
use super::net::{backoff_delay, send_with_retry, sleep_cancellable, MAX_STREAM_RETRIES};
use super::tools::{run_tool, ToolResult};

#[derive(Default, Clone)]
struct ToolCallAccum {
    id: String,
    name: String,
    arguments: String,
}

pub async fn run_chat_loop(
    ctx: &mut LoopCtx<'_>,
    api_key: &str,
    extra_headers: &[(String, String)],
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
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
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
        let res = send_with_retry(client.post(&url).headers(headers).json(&body), cancel).await?;
        let mut stream = res.bytes_stream();
        let mut buffer = String::new();
        let mut assistant_text = String::new();
        let mut tool_map: HashMap<u64, ToolCallAccum> = HashMap::new();
        let mut finish_reason: Option<String> = None;
        let mut stream_error: Option<String> = None;
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
                    &mut stream_error,
                    tx,
                );
            }
        }
        if stream_error.is_none() && !buffer.trim().is_empty() {
            for line in buffer.lines() {
                process_sse_line(
                    line.trim(),
                    &mut assistant_text,
                    &mut tool_map,
                    &mut finish_reason,
                    &mut stream_error,
                    tx,
                );
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
            // Execute tool calls in parallel where safe.
            // Mutating tools (write, edit, bash) are run sequentially to avoid races.
            // Read-only tools (read, grep, find, ls) can run concurrently.
            let is_readonly = |name: &str| {
                matches!(
                    name,
                    "read" | "grep" | "find" | "ls" | "web_search" | "web_fetch"
                )
            };

            // Split into consecutive groups: each group is either all-readonly (parallel) or a single mutating call.
            struct ToolCall {
                id: String,
                name: String,
                args: Value,
            }
            let parsed: Vec<ToolCall> = tool_calls
                .into_iter()
                .map(|tc| {
                    let args: Value = serde_json::from_str(&tc.arguments).unwrap_or(json!({}));
                    ToolCall {
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
                    // Collect consecutive readonly calls
                    let batch_start = i;
                    while i < parsed.len() && is_readonly(&parsed[i].name) {
                        i += 1;
                    }
                    let batch = &parsed[batch_start..i];
                    // Send all ToolStart events
                    for tc in batch {
                        let _ = tx.send(AgentEvent::ToolStart {
                            name: tc.name.clone(),
                            tool_call_id: tc.id.clone(),
                            args: Some(tc.args.clone()),
                        });
                    }
                    // Spawn all in parallel
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
                    // Collect results in order
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
                        messages.push(json!({
                            "role": "tool",
                            "tool_call_id": tc.id,
                            "content": text,
                        }));
                    }
                } else {
                    // Mutating tool: request approval, then run sequentially.
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
                        },
                    };
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
    stream_error: &mut Option<String>,
    tx: &Sender<AgentEvent>,
) {
    let line = line.trim();
    if line.is_empty() || line == "data: [DONE]" {
        return;
    }
    if line.starts_with(':') {
        return;
    }
    let Some(data) = line.strip_prefix("data:") else {
        return;
    };
    let data = data.trim();
    if data == "[DONE]" || data.is_empty() {
        return;
    }
    // A malformed line from a flaky provider should not kill the whole run.
    let v: Value = match serde_json::from_str(data) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[oxi] skipping malformed SSE line ({e}): {data}");
            return;
        }
    };
    if let Some(err) = v.get("error") {
        let msg = err
            .get("message")
            .and_then(|x| x.as_str())
            .unwrap_or("API error");
        *stream_error = Some(msg.to_string());
        return;
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
        // Extended thinking / reasoning (OpenAI o-series models send `reasoning_content`)
        if let Some(reasoning) = d
            .get("reasoning_content")
            .or_else(|| d.get("reasoning"))
            .and_then(|x| x.as_str())
        {
            if !reasoning.is_empty() {
                let _ = tx.send(AgentEvent::ThinkingDelta(reasoning.to_string()));
            }
        }
    }
}

/// End-to-end coverage for the full agent turn: fake provider SSE response -> tool-call
/// parsing -> the approval gate -> a real tool execution against the filesystem -> the
/// tool result serialized back into the next request -> a second round producing the
/// final answer. Every other test in this module/crate exercises one of these pieces in
/// isolation (SSE parsing here, `ApprovalGate` in `approval.rs`, tool dispatch in
/// `tools/tests.rs`); this is the one place that proves they actually fit together.
#[cfg(test)]
mod integration_tests {
    use super::*;
    use crate::agent::approval::{ApprovalDecision, ApprovalGate};
    use crate::agent::loop_ctx::LoopCtx;
    use crate::agent::tools::ToolEnv;
    use crate::settings::{WebSearchBackend, ALL_TOOL_NAMES};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use wiremock::matchers::{method, path as path_matcher};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    fn temp_workspace(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_else(|_| Duration::from_secs(0))
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("oxi-agent-loop-test-{name}-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sse_body(chunks: &[Value]) -> String {
        let mut body = String::new();
        for chunk in chunks {
            body.push_str("data: ");
            body.push_str(&chunk.to_string());
            body.push_str("\n\n");
        }
        body.push_str("data: [DONE]\n\n");
        body
    }

    /// First request served returns a streamed `write` tool call; every request after
    /// that returns a final text answer. Mirrors a real two-round agent turn (tool call
    /// -> tool result -> final answer) without depending on wiremock's mock
    /// ordering/priority rules for sequencing responses.
    struct RoundResponder {
        call_count: AtomicUsize,
    }

    impl Respond for RoundResponder {
        fn respond(&self, _req: &Request) -> ResponseTemplate {
            let n = self.call_count.fetch_add(1, AtomicOrdering::SeqCst);
            let body = if n == 0 {
                sse_body(&[
                    json!({"choices": [{"index": 0, "delta": {"tool_calls": [
                        {"index": 0, "id": "call_1", "type": "function",
                         "function": {"name": "write", "arguments": ""}}
                    ]}}]}),
                    json!({"choices": [{"index": 0, "delta": {"tool_calls": [
                        {"index": 0, "function": {"arguments":
                            "{\"path\":\"hello.txt\",\"content\":\"hi from the model\"}"}}
                    ]}}]}),
                    json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]}),
                ])
            } else {
                sse_body(&[
                    json!({"choices": [{"index": 0, "delta": {"content": "Done!"}}]}),
                    json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]}),
                ])
            };
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body)
        }
    }

    #[tokio::test]
    async fn full_loop_drives_tool_call_through_approval_to_final_answer() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_matcher("/chat/completions"))
            .respond_with(RoundResponder {
                call_count: AtomicUsize::new(0),
            })
            .mount(&server)
            .await;

        let cwd = temp_workspace("full-loop");
        let client = reqwest::Client::new();
        let (tx, rx) = mpsc::channel::<AgentEvent>();
        let (approval_tx, approval_rx) = mpsc::channel::<ApprovalDecision>();
        // `ApprovalGate::request` blocks on this channel; since it's an unbounded std
        // channel, sending the decision before the gate asks for it is fine — it's just
        // sitting in the buffer by the time `write` triggers the approval request.
        approval_tx.send(ApprovalDecision::Approve).unwrap();

        let cancel = Arc::new(AtomicBool::new(false));
        let mut gate = ApprovalGate::new(true, approval_rx);
        let env = ToolEnv {
            enabled: vec![true; ALL_TOOL_NAMES.len()],
            web_search_url: String::new(),
            web_search_backend: WebSearchBackend::default(),
        };
        let mut messages = vec![json!({"role": "user", "content": "write hello.txt"})];
        let tools = vec![json!({
            "type": "function",
            "function": {
                "name": "write",
                "description": "write a file",
                "parameters": {"type": "object", "properties": {}}
            }
        })];
        let base_url = server.uri();

        let mut ctx = LoopCtx {
            client: &client,
            base_url: &base_url,
            model: "test-model",
            cwd: &cwd,
            env: &env,
            tx: &tx,
            cancel: &cancel,
            gate: &mut gate,
            max_rounds: 10,
        };
        let result = run_chat_loop(&mut ctx, "test-key", &[], &mut messages, &tools).await;
        assert!(result.is_ok(), "agent loop failed: {result:?}");

        // The `write` tool call actually ran against the real filesystem after approval.
        let written = std::fs::read_to_string(cwd.join("hello.txt")).unwrap();
        assert_eq!(written, "hi from the model");

        // The tool result made it back into the conversation history sent to the model.
        let tool_messages: Vec<&Value> = messages
            .iter()
            .filter(|m| m.get("role").and_then(Value::as_str) == Some("tool"))
            .collect();
        assert_eq!(tool_messages.len(), 1);

        // The event stream reflects the full round trip: an approval request for the
        // mutating tool, the regular tool lifecycle, then the final answer.
        let events: Vec<AgentEvent> = rx.try_iter().collect();
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::ApprovalRequest { name, .. } if name == "write")));
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::ToolEnd {
                is_error: Some(false),
                ..
            }
        )));
        assert!(events.iter().any(|e| matches!(e, AgentEvent::AgentEnd)));
    }

    #[tokio::test]
    async fn full_loop_denied_approval_reports_error_to_model_without_running_tool() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_matcher("/chat/completions"))
            .respond_with(RoundResponder {
                call_count: AtomicUsize::new(0),
            })
            .mount(&server)
            .await;

        let cwd = temp_workspace("full-loop-denied");
        let client = reqwest::Client::new();
        let (tx, _rx) = mpsc::channel::<AgentEvent>();
        let (approval_tx, approval_rx) = mpsc::channel::<ApprovalDecision>();
        approval_tx.send(ApprovalDecision::Deny).unwrap();

        let cancel = Arc::new(AtomicBool::new(false));
        let mut gate = ApprovalGate::new(true, approval_rx);
        let env = ToolEnv {
            enabled: vec![true; ALL_TOOL_NAMES.len()],
            web_search_url: String::new(),
            web_search_backend: WebSearchBackend::default(),
        };
        let mut messages = vec![json!({"role": "user", "content": "write hello.txt"})];
        let tools = vec![json!({
            "type": "function",
            "function": {"name": "write", "description": "write a file",
                          "parameters": {"type": "object", "properties": {}}}
        })];
        let base_url = server.uri();

        let mut ctx = LoopCtx {
            client: &client,
            base_url: &base_url,
            model: "test-model",
            cwd: &cwd,
            env: &env,
            tx: &tx,
            cancel: &cancel,
            gate: &mut gate,
            max_rounds: 10,
        };
        let result = run_chat_loop(&mut ctx, "test-key", &[], &mut messages, &tools).await;
        assert!(result.is_ok(), "agent loop failed: {result:?}");

        // Denied: the file must never have been written...
        assert!(!cwd.join("hello.txt").exists());
        // ...but the model still gets a tool-result message explaining the denial, so it
        // can react instead of the conversation just hanging.
        let tool_message = messages
            .iter()
            .find(|m| m.get("role").and_then(Value::as_str) == Some("tool"))
            .expect("a tool result message for the denied call");
        let content = tool_message
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert!(content.contains("denied"), "unexpected content: {content}");
    }
}
