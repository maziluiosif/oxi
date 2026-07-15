//! Model discovery & per-model metadata.
//!
//! Providers expose their model list through OpenAI-style `GET /v1/models` endpoints,
//! but those endpoints do **not** advertise context-window sizes. This module:
//!
//! - exposes a typed list-model request (`fetch_models`) used to populate the model dropdown,
//! - keeps a small built-in [`CONTEXT_CATALOG`] mapping model id (lowercased, prefix-stripped)
//!   -> context window in tokens, so history trimming can target the actual model limit.
//!
//! When a model isn't found here we fall back to `AppSettings::context_window_default`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Codex backend model discovery is gated by the Codex CLI client version.
///
/// Do not use Oxi's own crate version here: Oxi is currently `0.x`, while the
/// ChatGPT Codex backend compares this query parameter with Codex CLI versions
/// such as `0.142.5` and can filter out the entire catalog for an older client.
const CODEX_MODELS_CLIENT_VERSION: &str = "0.198.0";

/// One entry in the OpenAI-style `/v1/models` list response.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ModelEntry {
    pub id: String,
    #[serde(default)]
    pub object: Option<String>,
    #[serde(default)]
    pub created: Option<i64>,
    #[serde(default)]
    pub owned_by: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ListModelsResponse {
    #[serde(default)]
    data: Vec<ModelEntry>,
}

/// Codex ChatGPT backend `/models` response shape.
///
/// Unlike OpenAI-compatible `/v1/models`, Codex returns `{ "models": [...] }`
/// entries keyed by `slug` instead of `id`, plus richer metadata such as
/// `context_window`. We currently normalize those entries into [`ModelEntry`]
/// so the rest of the UI can share the same model dropdown plumbing.
#[derive(Debug, Clone, Deserialize)]
struct CodexModelsResponse {
    #[serde(default)]
    models: Vec<CodexModelEntry>,
}

#[derive(Debug, Clone, Deserialize)]
struct CodexModelEntry {
    slug: String,
    #[serde(default)]
    context_window: Option<i64>,
    #[serde(default)]
    max_context_window: Option<i64>,
}

impl From<CodexModelEntry> for ModelEntry {
    fn from(m: CodexModelEntry) -> Self {
        let context_window = m.context_window.or(m.max_context_window);
        Self {
            id: m.slug,
            object: Some("model".to_string()),
            created: None,
            owned_by: context_window.map(|cw| format!("context_window={cw}")),
        }
    }
}

/// Fetch the list of models for an OpenAI-compatible provider.
///
/// `base_url` should be the provider root (e.g. `https://opencode.ai/zen/go`); the
/// `/v1/models` suffix is appended here. If the profile already includes `/v1` it is
/// reused as-is.
pub async fn fetch_models(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    extra_headers: &[(String, String)],
) -> Result<Vec<ModelEntry>, String> {
    let base = base_url.trim_end_matches('/');
    let mut url = if base.ends_with("/v1") || base.ends_with("/codex") {
        format!("{base}/models")
    } else {
        format!("{base}/v1/models")
    };
    if base.ends_with("/codex") {
        url.push_str("?client_version=");
        url.push_str(
            std::env::var("OXI_CODEX_CLIENT_VERSION")
                .as_deref()
                .unwrap_or(CODEX_MODELS_CLIENT_VERSION),
        );
    }

    let mut req = client.get(&url);
    if !api_key.trim().is_empty() {
        req = req.bearer_auth(api_key.trim());
    }
    for (k, v) in extra_headers {
        req = req.header(k, v);
    }

    let resp = req
        .send()
        .await
        .map_err(|e| format!("models request failed: {e}"))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| format!("models body read failed: {e}"))?;
    if !status.is_success() {
        // Trim the server error text so it stays readable in the UI.
        let snippet: String = text.chars().take(300).collect();
        return Err(format!("models HTTP {status}: {snippet}"));
    }
    let value: Value = serde_json::from_str(&text)
        .map_err(|e| format!("models parse failed: invalid JSON: {e}"))?;
    let mut models = if value.get("models").and_then(Value::as_array).is_some() {
        let parsed: CodexModelsResponse = serde_json::from_value(value)
            .map_err(|e| format!("models parse failed (Codex format): {e}"))?;
        parsed.models.into_iter().map(ModelEntry::from).collect()
    } else if value.get("data").and_then(Value::as_array).is_some() {
        let parsed: ListModelsResponse = serde_json::from_value(value)
            .map_err(|e| format!("models parse failed (OpenAI format): {e}"))?;
        parsed.data
    } else {
        return Err(
            "models parse failed: response has neither array `models` nor array `data`".to_string(),
        );
    };
    models.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(models)
}

/// Normalize a model id for catalog matching: lowercase, strip common provider prefixes
/// (`opencode/`, `openai/`, `openrouter/`, `anthropic/`, `opencode-go/`).
fn normalize_model_key(model: &str) -> String {
    let mut m = model.trim().to_ascii_lowercase();
    for prefix in [
        "opencode-go/",
        "opencode/",
        "openrouter/",
        "openai/",
        "anthropic/",
    ] {
        if let Some(rest) = m.strip_prefix(prefix) {
            m = rest.to_string();
            break;
        }
    }
    m
}

/// Look up the context window (in tokens) for a model id from the built-in catalog.
/// Returns `None` when the model is unknown.
pub fn context_window_for_model(model: &str) -> Option<usize> {
    let key = normalize_model_key(model);
    if key.is_empty() {
        return None;
    }
    if let Some(&cw) = CONTEXT_CATALOG.iter().find(|(k, _)| *k == key) {
        return Some(cw.1);
    }
    // Best-effort prefix matching (e.g. "gpt-4o-mini" matches "gpt-4o" family).
    // Prefer the longest matching key.
    let mut best: Option<(&str, usize)> = None;
    for (k, v) in CONTEXT_CATALOG {
        if key == *k {
            return Some(*v);
        }
        if key.starts_with(k) {
            match best {
                Some((bk, _)) if bk.len() >= k.len() => {}
                _ => best = Some((k, *v)),
            }
        }
    }
    best.map(|(_, v)| v)
}

/// Built-in catalog: model id (normalized, lowercased) -> context window in tokens.
///
/// Values sourced from provider documentation (OpenCode Zen docs, OpenRouter model pages,
/// and vendor spec sheets). Context windows are the advertised maximum input context.
/// When a model is missing, `AppSettings::context_window_default` is used instead.
#[rustfmt::skip]
static CONTEXT_CATALOG: &[(&str, usize)] = &[
    // ── OpenCode Go / Zen open-source models ─────────────────────────────────
    ("kimi-k2.7-code", 256_000),
    ("kimi-k2.6", 256_000),
    ("kimi-k2.5", 256_000),
    ("glm-5.2", 1_000_000),
    ("glm-5.1", 1_000_000),
    ("glm-5", 1_000_000),
    ("deepseek-v4-pro", 1_000_000),
    ("deepseek-v4-flash", 1_000_000),
    ("qwen3.7-max", 1_000_000),
    ("qwen3.7-plus", 1_000_000),
    ("qwen3.6-plus", 1_000_000),
    ("qwen3.5-plus", 1_000_000),
    ("minimax-m3", 1_000_000),
    ("minimax-m2.7", 1_000_000),
    ("minimax-m2.5", 1_000_000),
    ("mimo-v2.5-pro", 256_000),
    ("mimo-v2.5", 256_000),
    ("mimo-v2-pro", 256_000),
    ("mimo-v2-omni", 256_000),
    ("hy3-preview", 256_000),

    // ── Anthropic Claude (also served via OpenCode Zen) ──────────────────────
    ("claude-opus-4-8", 200_000),
    ("claude-opus-4-7", 200_000),
    ("claude-opus-4-6", 200_000),
    ("claude-opus-4-5", 200_000),
    ("claude-opus-4-1", 200_000),
    ("claude-sonnet-4-6", 200_000),
    ("claude-sonnet-4-5", 200_000),
    ("claude-sonnet-4", 200_000),
    ("claude-haiku-4-5", 200_000),
    ("claude-fable-5", 200_000),
    ("claude-3-5-haiku", 200_000),

    // ── OpenAI GPT family ─────────────────────────────────────────────────────
    ("gpt-5.6-pro", 400_000),
    ("gpt-5.6-codex", 400_000),
    // ChatGPT Codex reports ~258K usable context for the gpt-5.5/5.6 line
    // (272K raw with a 95% effective window), so keep the local fallback aligned
    // with the context shown by Codex instead of the broader GPT-5 family default.
    ("gpt-5.6", 258_000),
    ("gpt-5.5-pro", 400_000),
    ("gpt-5.5-codex", 400_000),
    ("gpt-5.5", 258_000),
    ("gpt-5.4-pro", 400_000),
    ("gpt-5.4-mini", 400_000),
    ("gpt-5.4-nano", 400_000),
    ("gpt-5.4", 400_000),
    ("gpt-5.3-codex-spark", 400_000),
    ("gpt-5.3-codex", 400_000),
    ("gpt-5.2-codex", 400_000),
    ("gpt-5.2", 400_000),
    ("gpt-5.1-codex-max", 400_000),
    ("gpt-5.1-codex-mini", 400_000),
    ("gpt-5.1-codex", 400_000),
    ("gpt-5.1", 400_000),
    ("gpt-5-codex", 400_000),
    ("gpt-5-nano", 400_000),
    ("gpt-5", 400_000),
    ("gpt-4o-mini", 128_000),
    ("gpt-4o", 128_000),
    ("gpt-4.1", 1_000_000),
    ("gpt-4.1-mini", 1_000_000),
    ("gpt-4-turbo", 128_000),
    ("o4-mini", 200_000),
    ("o3", 200_000),
    ("o3-mini", 200_000),

    // ── Google Gemini (also served via OpenCode Zen) ──────────────────────────
    ("gemini-3.5-flash", 1_000_000),
    ("gemini-3.1-pro", 1_000_000),
    ("gemini-3-flash", 1_000_000),

    // ── DeepSeek native ───────────────────────────────────────────────────────
    ("deepseek-v3", 128_000),
    ("deepseek-chat", 128_000),
    ("deepseek-reasoner", 128_000),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_lookup_strips_prefixes() {
        assert_eq!(context_window_for_model("kimi-k2.7-code"), Some(256_000));
        assert_eq!(
            context_window_for_model("opencode/kimi-k2.7-code"),
            Some(256_000)
        );
        assert_eq!(
            context_window_for_model("opencode-go/kimi-k2.7-code"),
            Some(256_000)
        );
        assert_eq!(
            context_window_for_model("OPENROUTER/gpt-4o-mini"),
            Some(128_000)
        );
    }

    #[test]
    fn unknown_model_returns_none() {
        assert!(context_window_for_model("totally-fake-model").is_none());
        assert!(context_window_for_model("").is_none());
    }

    #[test]
    fn prefix_match_prefers_longest_key() {
        // "gpt-4o" exact match first
        assert_eq!(context_window_for_model("gpt-4o"), Some(128_000));
        // "gpt-4o-mini" exact entry wins
        assert_eq!(
            context_window_for_model("gpt-4o-mini-2024-07-18"),
            Some(128_000)
        );
    }

    #[test]
    fn parses_real_opencode_go_models_response() {
        // Captured from https://opencode.ai/zen/go/v1/models (no auth required).
        let body = r#"{"object":"list","data":[
            {"id":"minimax-m3","object":"model","created":1782750904,"owned_by":"opencode"},
            {"id":"kimi-k2.7-code","object":"model","created":1782750904,"owned_by":"opencode"},
            {"id":"glm-5.2","object":"model","created":1782750904,"owned_by":"opencode"},
            {"id":"qwen3.7-max","object":"model","created":1782750904,"owned_by":"opencode"}
        ]}"#;
        let parsed: ListModelsResponse =
            serde_json::from_str(body).expect("opencode go models must parse");
        let mut ids: Vec<String> = parsed.data.into_iter().map(|m| m.id).collect();
        ids.sort();
        assert_eq!(
            ids,
            vec!["glm-5.2", "kimi-k2.7-code", "minimax-m3", "qwen3.7-max"]
        );
    }
}
