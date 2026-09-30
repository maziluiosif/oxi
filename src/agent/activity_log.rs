//! Opt-in activity log: a bounded in-memory record of everything the agent exchanges with the
//! outside world (provider HTTP requests and raw response streams, retries and errors, ACP and
//! MCP JSON-RPC traffic, tool calls). Shown in the Activity window for troubleshooting, mostly
//! for local models whose failures are otherwise opaque.
//!
//! Recording is off by default and every entry point is a cheap atomic check when it is, so the
//! hooks can stay in the hot paths. Secrets are redacted before anything is stored: JSON fields
//! whose name looks like a credential, `Bearer` tokens, and `api-key` headers. Huge strings
//! (base64 images, long files) are truncated so a single request can't balloon the log.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use serde_json::Value;

/// Newest entries kept; older ones are dropped first.
const MAX_ENTRIES: usize = 1_000;
/// Upper bound for the summed body size of all kept entries.
const MAX_TOTAL_BYTES: usize = 24 * 1024 * 1024;
/// Upper bound for one entry body (a whole raw response stream, for instance).
const MAX_BODY_BYTES: usize = 512 * 1024;
/// JSON string values longer than this are shortened (base64 images, file contents).
const MAX_JSON_STRING_CHARS: usize = 4_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityKind {
    /// Outgoing provider HTTP request.
    Request,
    /// Raw provider response (status, SSE stream, non-streaming body).
    Response,
    /// A request that will be retried.
    Retry,
    /// Transport or protocol failure.
    Error,
    /// Tool call issued by the agent and its result.
    Tool,
    /// Agent Client Protocol message to or from an external agent.
    Acp,
    /// Model Context Protocol message to or from an MCP server.
    Mcp,
}

impl ActivityKind {
    pub const ALL: [ActivityKind; 7] = [
        ActivityKind::Request,
        ActivityKind::Response,
        ActivityKind::Retry,
        ActivityKind::Error,
        ActivityKind::Tool,
        ActivityKind::Acp,
        ActivityKind::Mcp,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ActivityKind::Request => "Request",
            ActivityKind::Response => "Response",
            ActivityKind::Retry => "Retry",
            ActivityKind::Error => "Error",
            ActivityKind::Tool => "Tool",
            ActivityKind::Acp => "ACP",
            ActivityKind::Mcp => "MCP",
        }
    }
}

#[derive(Debug)]
pub struct ActivityEntry {
    pub id: u64,
    pub at: chrono::DateTime<chrono::Local>,
    pub kind: ActivityKind,
    pub title: String,
    pub body: String,
}

#[derive(Default)]
struct State {
    entries: VecDeque<Arc<ActivityEntry>>,
    total_bytes: usize,
    next_id: u64,
}

static ENABLED: AtomicBool = AtomicBool::new(false);
static STATE: LazyLock<Mutex<State>> = LazyLock::new(Default::default);

pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

/// Current entries, oldest first. Entries are shared, so this is cheap to call per frame.
pub fn snapshot() -> Vec<Arc<ActivityEntry>> {
    STATE
        .lock()
        .map(|s| s.entries.iter().cloned().collect())
        .unwrap_or_default()
}

pub fn clear() {
    if let Ok(mut s) = STATE.lock() {
        s.entries.clear();
        s.total_bytes = 0;
    }
}

/// Record a free-form text entry (redacted and capped). No-op while recording is off.
pub fn log(kind: ActivityKind, title: impl Into<String>, body: impl AsRef<str>) {
    if !is_enabled() {
        return;
    }
    push(kind, title.into(), redact_text(body.as_ref()));
}

/// Record a JSON payload, pretty-printed after redaction. No-op while recording is off.
pub fn log_json(kind: ActivityKind, title: impl Into<String>, value: &Value) {
    if !is_enabled() {
        return;
    }
    let redacted = redact_json(value);
    let body = serde_json::to_string_pretty(&redacted).unwrap_or_default();
    push(kind, title.into(), body);
}

fn push(kind: ActivityKind, title: String, mut body: String) {
    truncate_in_place(&mut body, MAX_BODY_BYTES);
    let Ok(mut s) = STATE.lock() else {
        return;
    };
    let id = s.next_id;
    s.next_id += 1;
    s.total_bytes += body.len();
    s.entries.push_back(Arc::new(ActivityEntry {
        id,
        at: chrono::Local::now(),
        kind,
        title,
        body,
    }));
    while s.entries.len() > MAX_ENTRIES || s.total_bytes > MAX_TOTAL_BYTES {
        let Some(old) = s.entries.pop_front() else {
            break;
        };
        s.total_bytes = s.total_bytes.saturating_sub(old.body.len());
    }
}

/// Accumulates a raw response stream while recording is on, then logs it as one entry.
/// Costs nothing (no buffer) when recording is off at creation time.
pub struct StreamCapture {
    buf: Option<String>,
}

impl StreamCapture {
    pub fn new() -> Self {
        Self {
            buf: is_enabled().then(String::new),
        }
    }

    pub fn push(&mut self, chunk: &str) {
        if let Some(buf) = self.buf.as_mut()
            && buf.len() < MAX_BODY_BYTES
        {
            buf.push_str(chunk);
        }
    }

    pub fn finish(self, title: impl Into<String>) {
        if let Some(buf) = self.buf {
            log(ActivityKind::Response, title, buf);
        }
    }
}

/// Activity-log title for a JSON-RPC message: the method, or which request a response answers.
pub fn rpc_title(msg: &Value) -> String {
    match (msg.get("method").and_then(|m| m.as_str()), msg.get("id")) {
        (Some(method), _) => method.to_string(),
        (None, Some(id)) if msg.get("error").is_some() => format!("error for #{id}"),
        (None, Some(id)) => format!("response #{id}"),
        _ => "message".to_string(),
    }
}

/// Short `…/path` form of a URL for entry titles, without query strings (which may carry keys).
pub fn url_title(method: &str, url: &str) -> String {
    let clean = url.split(['?', '#']).next().unwrap_or(url);
    format!("{method} {clean}")
}

fn is_secret_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase().replace(['-', '_'], "");
    [
        "apikey",
        "authorization",
        "accesstoken",
        "refreshtoken",
        "idtoken",
        "token",
        "secret",
        "password",
        "clientsecret",
        "xapikey",
    ]
    .iter()
    .any(|m| k == *m || k.ends_with(m))
        && !k.ends_with("tokens") // `max_tokens`, `input_tokens`, …: counts, not secrets.
}

fn redact_json(v: &Value) -> Value {
    match v {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| {
                    let v = if is_secret_key(k) && !v.is_null() && !v.is_number() {
                        Value::String("«redacted»".into())
                    } else {
                        redact_json(v)
                    };
                    (k.clone(), v)
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(redact_json).collect()),
        Value::String(s) => {
            let count = s.chars().count();
            if count > MAX_JSON_STRING_CHARS {
                let head: String = s.chars().take(MAX_JSON_STRING_CHARS).collect();
                Value::String(format!(
                    "{}… «{} more chars»",
                    redact_text(&head),
                    count - MAX_JSON_STRING_CHARS
                ))
            } else {
                Value::String(redact_text(s))
            }
        }
        other => other.clone(),
    }
}

static BEARER_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(?i)\b(bearer)\s+[A-Za-z0-9._~+/=-]{8,}").unwrap());
static KEY_FIELD_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r#"(?i)("?(?:api[_-]?key|access[_-]?token|refresh[_-]?token|authorization|password|secret)"?\s*[:=]\s*"?)[^"\s,}]+"#,
    )
    .unwrap()
});
static SK_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\b(sk-[A-Za-z0-9_-]{4})[A-Za-z0-9_-]{12,}").unwrap());

/// Redact credentials that can appear in free text: `Bearer …`, `"api_key": "…"`, `sk-…` keys.
pub fn redact_text(s: &str) -> String {
    let s = BEARER_RE.replace_all(s, "$1 «redacted»");
    let s = KEY_FIELD_RE.replace_all(&s, "$1«redacted»");
    SK_RE.replace_all(&s, "$1…«redacted»").into_owned()
}

fn truncate_in_place(s: &mut String, max: usize) {
    if s.len() <= max {
        return;
    }
    let mut cut = max;
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    let dropped = s.len() - cut;
    s.truncate(cut);
    s.push_str(&format!("\n… «{dropped} more bytes»"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn json_redaction_hides_credentials_but_keeps_token_counts() {
        let v = json!({
            "api_key": "sk-abcdefghijklmnopqrstuvwxyz",
            "headers": { "Authorization": "Bearer abc.def.ghi.jkl" },
            "max_tokens": 1024,
            "usage": { "input_tokens": 5 },
            "model": "qwen",
        });
        let r = redact_json(&v);
        assert_eq!(r["api_key"], "«redacted»");
        assert_eq!(r["headers"]["Authorization"], "«redacted»");
        assert_eq!(r["max_tokens"], 1024);
        assert_eq!(r["usage"]["input_tokens"], 5);
        assert_eq!(r["model"], "qwen");
    }

    #[test]
    fn long_json_strings_are_shortened() {
        let v = json!({ "image": "A".repeat(MAX_JSON_STRING_CHARS + 50) });
        let r = redact_json(&v);
        let s = r["image"].as_str().unwrap();
        assert!(s.ends_with("«50 more chars»"));
        assert!(s.len() < MAX_JSON_STRING_CHARS + 40);
    }

    #[test]
    fn text_redaction_catches_bearer_fields_and_sk_keys() {
        let t = redact_text(
            "Authorization: Bearer abcdefghijkl\n{\"api_key\":\"zzz-secret\"} key sk-proj1234567890abcdefgh",
        );
        assert!(!t.contains("abcdefghijkl"), "{t}");
        assert!(!t.contains("zzz-secret"), "{t}");
        assert!(!t.contains("567890abcdefgh"), "{t}");
        assert!(t.contains("«redacted»"));
    }

    #[test]
    fn url_title_drops_query_strings() {
        assert_eq!(
            url_title("POST", "https://x.test/v1/chat?key=abc"),
            "POST https://x.test/v1/chat"
        );
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        let mut s = "ééééé".to_string();
        truncate_in_place(&mut s, 3);
        assert!(s.starts_with('é'));
        assert!(s.contains("more bytes"));
    }
}
