//! JSON-RPC over the agent's stdio: outgoing requests, the reader loop that routes responses,
//! notifications and agent→client requests, and the stderr tail kept for launch errors.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::ChildStdin;
use tokio::sync::{Mutex as AsyncMutex, oneshot};

use super::client_fs::fs_read_text;
use super::modes::SharedModes;
use super::permissions::PermReq;
use super::{Pending, PromptCtx, commands};
use crate::agent::activity_log::{self, ActivityKind};

/// Send a client→agent request and await its response.
pub(super) async fn request(
    stdin: &Arc<AsyncMutex<ChildStdin>>,
    next_id: &Arc<AtomicI64>,
    pending: &Pending,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    let id = next_id.fetch_add(1, Ordering::SeqCst);
    let (tx, rx) = oneshot::channel();
    pending.lock().await.insert(id, tx);
    write_line(
        stdin,
        &json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}),
    )
    .await?;
    match rx.await {
        Ok(r) => r,
        Err(_) => Err("ACP connection closed".to_string()),
    }
}

pub(super) async fn write_line(
    stdin: &Arc<AsyncMutex<ChildStdin>>,
    msg: &Value,
) -> Result<(), String> {
    if activity_log::is_enabled() {
        activity_log::log_json(
            ActivityKind::Acp,
            format!("→ {}", activity_log::rpc_title(msg)),
            msg,
        );
    }
    let mut line = serde_json::to_string(msg).map_err(|e| e.to_string())?;
    line.push('\n');
    let mut guard = stdin.lock().await;
    guard
        .write_all(line.as_bytes())
        .await
        .map_err(|e| format!("ACP write failed: {e}"))?;
    guard
        .flush()
        .await
        .map_err(|e| format!("ACP flush failed: {e}"))?;
    Ok(())
}

/// Read newline-delimited JSON-RPC messages from the agent until stdout closes, dispatching
/// each. On close, fail every outstanding request so callers don't hang.
pub(super) async fn read_loop(
    stdout: tokio::process::ChildStdout,
    pending: Pending,
    prompt_ctx: Arc<AsyncMutex<Option<PromptCtx>>>,
    stdin: Arc<AsyncMutex<ChildStdin>>,
    alive: Arc<AtomicBool>,
    modes: SharedModes,
    commands_key: CommandsKey,
) {
    let mut lines = BufReader::new(stdout).lines();
    // Streaming text arrives as one `session/update` per token chunk; logging each would push
    // everything else out of the activity log, so consecutive chunks become one entry.
    let mut chunks = String::new();
    let mut chunk_count = 0usize;
    let flush_chunks = |chunks: &mut String, count: &mut usize| {
        if *count > 0 {
            activity_log::log(
                ActivityKind::Acp,
                format!("← session/update · {count} streamed chunks"),
                &*chunks,
            );
            chunks.clear();
            *count = 0;
        }
    };
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        if activity_log::is_enabled() {
            if is_streamed_chunk(&line) {
                chunks.push_str(&line);
                chunks.push('\n');
                chunk_count += 1;
            } else {
                flush_chunks(&mut chunks, &mut chunk_count);
                match serde_json::from_str::<Value>(&line) {
                    Ok(v) => activity_log::log_json(
                        ActivityKind::Acp,
                        format!("← {}", activity_log::rpc_title(&v)),
                        &v,
                    ),
                    Err(_) => activity_log::log(ActivityKind::Acp, "← (unparsed line)", &line),
                }
            }
        }
        dispatch(&line, &pending, &prompt_ctx, &stdin, &modes, &commands_key).await;
    }
    flush_chunks(&mut chunks, &mut chunk_count);
    alive.store(false, Ordering::SeqCst);
    let mut p = pending.lock().await;
    for (_, tx) in p.drain() {
        let _ = tx.send(Err("ACP agent closed the connection".to_string()));
    }
}

/// Cheap pre-parse check for streamed message/thought chunks (the bulk of ACP traffic).
pub(super) fn is_streamed_chunk(line: &str) -> bool {
    line.contains("\"session/update\"")
        && (line.contains("\"agent_message_chunk\"") || line.contains("\"agent_thought_chunk\""))
}

/// Where a connection's advertised slash commands are recorded (see [`commands`]).
pub(super) struct CommandsKey {
    pub(super) command_line: String,
    pub(super) cwd: PathBuf,
}

pub(super) async fn dispatch(
    line: &str,
    pending: &Pending,
    prompt_ctx: &Arc<AsyncMutex<Option<PromptCtx>>>,
    stdin: &Arc<AsyncMutex<ChildStdin>>,
    modes: &SharedModes,
    commands_key: &CommandsKey,
) {
    let v: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return,
    };
    if let Some(method) = v.get("method").and_then(|m| m.as_str()) {
        let id = v.get("id").cloned().filter(|x| !x.is_null());
        if let Some(id) = id {
            let params = v.get("params").cloned().unwrap_or(Value::Null);
            handle_agent_request(method, id, params, stdin, prompt_ctx).await;
        } else if method == "session/update" {
            let update = &v["params"]["update"];
            // Commands usually arrive right after session setup and modes can change between
            // turns, so both are handled regardless of whether a turn is in flight.
            match update["sessionUpdate"].as_str() {
                Some("available_commands_update") => {
                    if let Some(list) = commands::parse_update(update) {
                        commands::store(&commands_key.command_line, &commands_key.cwd, list);
                    }
                }
                Some("config_option_update" | "current_mode_update") => {
                    modes
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .apply_notification(update);
                }
                _ => {
                    if let Some(ctx) = prompt_ctx.lock().await.as_mut() {
                        ctx.emit_notification(&v["params"]);
                    }
                }
            }
        }
    } else if let Some(id) = v.get("id").and_then(|x| x.as_i64()) {
        let waiter = pending.lock().await.remove(&id);
        if let Some(w) = waiter {
            if let Some(err) = v.get("error") {
                let msg = err
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("ACP error")
                    .to_string();
                let _ = w.send(Err(msg));
            } else {
                let _ = w.send(Ok(v.get("result").cloned().unwrap_or(Value::Null)));
            }
        }
    }
}

/// Handle an agent→client request (`fs/*`, `session/request_permission`).
pub(super) async fn handle_agent_request(
    method: &str,
    id: Value,
    params: Value,
    stdin: &Arc<AsyncMutex<ChildStdin>>,
    prompt_ctx: &Arc<AsyncMutex<Option<PromptCtx>>>,
) {
    match method {
        "fs/read_text_file" => match fs_read_text(&params) {
            Ok(content) => reply_ok(stdin, id, json!({ "content": content })).await,
            Err(e) => reply_err(stdin, id, -32000, &e).await,
        },
        "fs/write_text_file" => {
            let result = {
                let ctx = prompt_ctx.lock().await;
                ctx.as_ref()
                    .ok_or_else(|| "fs/write_text_file: no active prompt".to_string())
                    .and_then(|ctx| ctx.write_text_file(&params))
            };
            match result {
                Ok(()) => reply_ok(stdin, id, Value::Null).await,
                Err(e) => reply_err(stdin, id, -32000, &e).await,
            }
        }
        "session/request_permission" => {
            let forwarded = {
                let mut ctx = prompt_ctx.lock().await;
                ctx.as_mut()
                    .filter(|c| params["sessionId"].as_str() == Some(c.session_id.as_str()))
                    .map(|c| {
                        c.updates.emit_tool(&params["toolCall"], &c.event_tx);
                        c.perm_tx.send(PermReq {
                            id: id.clone(),
                            params,
                        })
                    })
            };
            // No active prompt (or the prompt task is gone): cancel the request so the agent
            // doesn't block forever.
            if !matches!(forwarded, Some(Ok(()))) {
                reply_ok(stdin, id, json!({ "outcome": { "outcome": "cancelled" } })).await;
            }
        }
        _ => {
            reply_err(
                stdin,
                id,
                -32601,
                &format!("method not supported: {method}"),
            )
            .await
        }
    }
}

pub(super) async fn reply_ok(stdin: &Arc<AsyncMutex<ChildStdin>>, id: Value, result: Value) {
    let _ = write_line(stdin, &json!({"jsonrpc":"2.0","id":id,"result":result})).await;
}

pub(super) async fn reply_err(
    stdin: &Arc<AsyncMutex<ChildStdin>>,
    id: Value,
    code: i64,
    message: &str,
) {
    let _ = write_line(
        stdin,
        &json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}}),
    )
    .await;
}

/// Lines of agent stderr kept for error messages.
const STDERR_TAIL_LINES: usize = 12;

pub(super) async fn drain_stderr(
    stderr: tokio::process::ChildStderr,
    tail: Arc<std::sync::Mutex<std::collections::VecDeque<String>>>,
) {
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if !line.trim().is_empty() {
            eprintln!("[acp] {line}");
            activity_log::log(ActivityKind::Acp, "stderr", &line);
            let mut tail = tail.lock().unwrap_or_else(|e| e.into_inner());
            if tail.len() == STDERR_TAIL_LINES {
                tail.pop_front();
            }
            tail.push_back(line);
        }
    }
}
