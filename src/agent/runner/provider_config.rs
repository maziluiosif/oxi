//! Provider credentials, protocol selection, and request headers.

use crate::settings::ProviderConfig;

pub(crate) fn configured_openai_key(cfg: &ProviderConfig) -> Result<String, String> {
    let key = cfg.api_key.trim();
    if !key.is_empty() {
        return Ok(key.to_string());
    }
    std::env::var("OPENAI_API_KEY")
        .map_err(|_| "Set OpenAI API key in Settings or OPENAI_API_KEY, or sign in with ChatGPT (Codex) OAuth.".into())
}

pub(crate) fn configured_azure_openai_key(cfg: &ProviderConfig) -> Result<String, String> {
    let key = cfg.api_key.trim();
    if !key.is_empty() {
        return Ok(key.to_string());
    }
    std::env::var("AZURE_OPENAI_API_KEY").map_err(|_| {
        "Set Azure OpenAI API key in Settings or AZURE_OPENAI_API_KEY in the environment.".into()
    })
}

pub(crate) fn azure_openai_api_version() -> String {
    std::env::var("AZURE_OPENAI_API_VERSION").unwrap_or_else(|_| "2024-10-21".to_string())
}

pub(crate) fn configured_custom_anthropic_key(cfg: &ProviderConfig) -> Result<String, String> {
    let key = cfg.api_key.trim();
    if !key.is_empty() {
        return Ok(key.to_string());
    }
    std::env::var("CUSTOM_ANTHROPIC_API_KEY")
        .or_else(|_| std::env::var("ANTHROPIC_API_KEY"))
        .map_err(|_| "Set Custom Anthropic API key in Settings or CUSTOM_ANTHROPIC_API_KEY / ANTHROPIC_API_KEY in the environment.".into())
}

pub(crate) fn configured_openrouter_key(cfg: &ProviderConfig) -> Result<String, String> {
    let key = cfg.api_key.trim();
    if !key.is_empty() {
        return Ok(key.to_string());
    }
    std::env::var("OPENROUTER_API_KEY").map_err(|_| {
        "Set OpenRouter API key in Settings or OPENROUTER_API_KEY in the environment.".into()
    })
}

pub(crate) fn configured_opencode_go_key(cfg: &ProviderConfig) -> Result<String, String> {
    let key = cfg.api_key.trim();
    if !key.is_empty() {
        return Ok(key.to_string());
    }
    std::env::var("OPENCODE_GO_API_KEY").or_else(|_| std::env::var("OPENCODE_API_KEY")).map_err(|_| {
        "Set OpenCode Go API key in Settings or OPENCODE_GO_API_KEY / OPENCODE_API_KEY in the environment."
            .into()
    })
}

/// LM Studio's local server ignores the bearer token, so an API key is optional. Use the
/// configured value (or `LMSTUDIO_API_KEY`) if present, otherwise fall back to an empty key.
pub(crate) fn configured_lmstudio_key(cfg: &ProviderConfig) -> String {
    let key = cfg.api_key.trim();
    if !key.is_empty() {
        return key.to_string();
    }
    std::env::var("LMSTUDIO_API_KEY").unwrap_or_default()
}

/// Ollama's local server has no auth by default, so an API key is optional. Use the
/// configured value (or `OLLAMA_API_KEY`) if present, otherwise fall back to an empty key.
pub(crate) fn configured_ollama_key(cfg: &ProviderConfig) -> String {
    let key = cfg.api_key.trim();
    if !key.is_empty() {
        return key.to_string();
    }
    std::env::var("OLLAMA_API_KEY").unwrap_or_default()
}

pub(crate) fn opencode_go_model_uses_anthropic(model: &str) -> bool {
    let m = model
        .trim()
        .strip_prefix("opencode-go/")
        .unwrap_or(model.trim())
        .to_ascii_lowercase();
    m.starts_with("minimax-") || m.starts_with("qwen")
}

pub fn openrouter_extra_headers(cfg: &ProviderConfig) -> Vec<(String, String)> {
    let mut h = Vec::new();
    let referer = if cfg.openrouter_http_referer.trim().is_empty() {
        std::env::var("OPENROUTER_HTTP_REFERER").ok()
    } else {
        Some(cfg.openrouter_http_referer.trim().to_string())
    };
    if let Some(r) = referer.filter(|s| !s.is_empty()) {
        h.push(("HTTP-Referer".to_string(), r));
    }
    let title = if cfg.openrouter_title.trim().is_empty() {
        std::env::var("OPENROUTER_TITLE").ok()
    } else {
        Some(cfg.openrouter_title.trim().to_string())
    };
    if let Some(t) = title.filter(|s| !s.is_empty()) {
        h.push(("X-Title".to_string(), t));
    }
    h
}
