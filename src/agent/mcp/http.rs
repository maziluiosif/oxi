//! Streamable HTTP transport (MCP 2025-03-26+): every JSON-RPC message is a POST to one
//! endpoint. The server answers a request with either a JSON body or an SSE stream that
//! eventually carries the response; notifications get `202 Accepted`. A session id handed out
//! at `initialize` (`Mcp-Session-Id`) is echoed on every later request.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use serde_json::{Value, json};

use super::stdio::rpc_error_message;
use super::{IO_RT, Transport, run_io};
use crate::agent::activity_log::{self, ActivityKind};
use crate::settings::McpServerConfig;

const SESSION_HEADER: &str = "mcp-session-id";
const PROTOCOL_HEADER: &str = "mcp-protocol-version";

struct Inner {
    name: String,
    url: String,
    token: String,
    client: reqwest::Client,
    session: Mutex<Option<String>>,
    protocol: Mutex<Option<String>>,
    alive: AtomicBool,
}

pub(super) struct HttpTransport {
    inner: Arc<Inner>,
    next_id: AtomicU64,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl HttpTransport {
    pub(super) fn new(cfg: &McpServerConfig) -> Result<Self, String> {
        let url = cfg.url.trim().to_string();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(format!(
                "MCP `{}`: URL must start with http:// or https://",
                cfg.name
            ));
        }
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            inner: Arc::new(Inner {
                name: cfg.name.clone(),
                url,
                token: cfg.bearer_token.trim().to_string(),
                client,
                session: Mutex::new(None),
                protocol: Mutex::new(None),
                alive: AtomicBool::new(true),
            }),
            next_id: AtomicU64::new(1),
        })
    }
}

impl Transport for HttpTransport {
    fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let inner = self.inner.clone();
        let name = self.inner.name.clone();
        run_io(async move { post(&inner, &msg, Some(id)).await }, timeout)
            .map_err(|e| format!("MCP `{name}`: {method} {e}"))?
            .map(|v| v.unwrap_or(Value::Null))
    }

    fn notify(&self, method: &str, params: Value) {
        let msg = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        let inner = self.inner.clone();
        let _ = run_io(
            async move { post(&inner, &msg, None).await },
            Duration::from_secs(15),
        );
    }

    fn is_alive(&self) -> bool {
        self.inner.alive.load(Ordering::SeqCst)
    }

    fn set_protocol_version(&self, version: &str) {
        *lock(&self.inner.protocol) = Some(version.to_string());
    }
}

impl Drop for HttpTransport {
    fn drop(&mut self) {
        // Politely end the session so the server can free it; best effort, never waited on.
        let Some(session) = lock(&self.inner.session).clone() else {
            return;
        };
        let inner = self.inner.clone();
        IO_RT.spawn(async move {
            let mut req = inner
                .client
                .delete(&inner.url)
                .header(SESSION_HEADER, session);
            if !inner.token.is_empty() {
                req = req.bearer_auth(&inner.token);
            }
            let _ = req.timeout(Duration::from_secs(5)).send().await;
        });
    }
}

/// POST one message. For a request (`expect_id`), wait for the matching response and return its
/// `result`; for a notification, return `None` once the server accepted it.
async fn post(inner: &Inner, msg: &Value, expect_id: Option<u64>) -> Result<Option<Value>, String> {
    activity_log::log_json(
        ActivityKind::Mcp,
        format!("{} → {}", inner.name, activity_log::rpc_title(msg)),
        msg,
    );
    let mut req = inner
        .client
        .post(&inner.url)
        .header(
            reqwest::header::ACCEPT,
            "application/json, text/event-stream",
        )
        .json(msg);
    if !inner.token.is_empty() {
        req = req.bearer_auth(&inner.token);
    }
    let had_session = if let Some(session) = lock(&inner.session).clone() {
        req = req.header(SESSION_HEADER, session);
        true
    } else {
        false
    };
    if let Some(protocol) = lock(&inner.protocol).clone() {
        req = req.header(PROTOCOL_HEADER, protocol);
    }
    let res = req.send().await.map_err(|e| error_chain(&e))?;
    if let Some(session) = res
        .headers()
        .get(SESSION_HEADER)
        .and_then(|v| v.to_str().ok())
    {
        *lock(&inner.session) = Some(session.to_string());
    }
    let status = res.status();
    if status == reqwest::StatusCode::NOT_FOUND && had_session {
        // The server forgot our session; the manager reconnects on the next call.
        *lock(&inner.session) = None;
        inner.alive.store(false, Ordering::SeqCst);
        return Err("session expired (will reconnect on the next call)".into());
    }
    if !status.is_success() {
        let body = res.text().await.unwrap_or_default();
        activity_log::log(
            ActivityKind::Error,
            format!("{} ← HTTP {status}", inner.name),
            &body,
        );
        let hint = if status.as_u16() == 401 || status.as_u16() == 403 {
            " Check the bearer token in Settings → Tools & safety → MCP servers."
        } else {
            ""
        };
        let body = body.trim();
        let snippet: String = body.chars().take(300).collect();
        return Err(if snippet.is_empty() {
            format!("HTTP {status}.{hint}")
        } else {
            format!("HTTP {status}: {snippet}{hint}")
        });
    }
    let Some(id) = expect_id else {
        return Ok(None);
    };
    let is_sse = res
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.to_ascii_lowercase().starts_with("text/event-stream"));
    if is_sse {
        let mut stream = res.bytes_stream();
        let mut buf = String::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| error_chain(&e))?;
            buf.push_str(&String::from_utf8_lossy(&chunk).replace("\r\n", "\n"));
            while let Some(pos) = buf.find("\n\n") {
                let event: String = buf.drain(..pos + 2).collect();
                if let Some(found) = sse_event_response(inner, &event, id) {
                    return found.map(Some);
                }
            }
        }
        if let Some(found) = sse_event_response(inner, &buf, id) {
            return found.map(Some);
        }
        Err("the server's event stream ended without a response".into())
    } else {
        let body: Value = res
            .json()
            .await
            .map_err(|e| format!("invalid JSON response: {e}"))?;
        let messages = match body {
            Value::Array(items) => items,
            other => vec![other],
        };
        for message in messages {
            if let Some(found) = match_response(inner, message, id) {
                return found.map(Some);
            }
        }
        Err("the server answered without a matching response".into())
    }
}

/// `e` plus its underlying causes: reqwest's top-level message ("error sending request") hides
/// the useful part (connection refused, DNS failure, TLS error).
fn error_chain(e: &dyn std::error::Error) -> String {
    let mut out = e.to_string();
    let mut source = e.source();
    while let Some(cause) = source {
        let text = cause.to_string();
        if !out.contains(&text) {
            out.push_str(": ");
            out.push_str(&text);
        }
        source = cause.source();
    }
    out
}

/// The `data:` payload of one SSE event, if it is the response we are waiting for.
fn sse_event_response(inner: &Inner, event: &str, id: u64) -> Option<Result<Value, String>> {
    let data: Vec<&str> = event
        .lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .map(|d| d.strip_prefix(' ').unwrap_or(d))
        .collect();
    if data.is_empty() {
        return None;
    }
    let message: Value = serde_json::from_str(&data.join("\n")).ok()?;
    match_response(inner, message, id)
}

fn match_response(inner: &Inner, message: Value, id: u64) -> Option<Result<Value, String>> {
    activity_log::log_json(
        ActivityKind::Mcp,
        format!("{} ← {}", inner.name, activity_log::rpc_title(&message)),
        &message,
    );
    if message.get("method").is_some() || message.get("id").and_then(|v| v.as_u64()) != Some(id) {
        // Server-initiated requests/notifications on the stream are not needed by oxi.
        return None;
    }
    Some(match message.get("error") {
        Some(err) => Err(rpc_error_message(err)),
        None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
    })
}
