//! Provider/profile domain types: which LLM backend a profile talks to, where its
//! runtime lives (local vs. an SSH-tunneled remote), and the profile itself.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LlmProviderKind {
    #[default]
    OpenAi,
    OpenRouter,
    /// GPT Codex family via OpenAI Chat Completions (`api.openai.com`).
    GptCodex,
    /// OpenCode Go subscription models (OpenAI/Anthropic-compatible endpoints).
    OpenCodeGo,
    /// LM Studio local server (OpenAI-compatible API, on this machine or a LAN host).
    LmStudio,
    /// Ollama local server (OpenAI-compatible API at `/v1`, on this machine or a LAN host).
    Ollama,
}

impl LlmProviderKind {
    /// Order here drives the provider pill-tab order in Settings → Providers. Ollama and
    /// LM Studio lead the list since they're the local/self-hosted runtimes oxi is built
    /// around; the hosted API providers follow.
    pub const ALL: [LlmProviderKind; 6] = [
        LlmProviderKind::Ollama,
        LlmProviderKind::LmStudio,
        LlmProviderKind::OpenAi,
        LlmProviderKind::OpenRouter,
        LlmProviderKind::GptCodex,
        LlmProviderKind::OpenCodeGo,
    ];

    pub fn default_base_url(&self) -> &'static str {
        match self {
            LlmProviderKind::OpenAi | LlmProviderKind::GptCodex => "https://api.openai.com/v1",
            LlmProviderKind::OpenRouter => "https://openrouter.ai/api/v1",
            LlmProviderKind::OpenCodeGo => "https://opencode.ai/zen/go",
            // LM Studio's built-in server speaks plain HTTP on port 1234 by default.
            // (HTTPS would need a separate reverse proxy.)
            LlmProviderKind::LmStudio => "http://localhost:1234/v1",
            // Ollama's OpenAI-compatible API lives under `/v1` on its default port 11434.
            LlmProviderKind::Ollama => "http://localhost:11434/v1",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            LlmProviderKind::OpenAi => "OpenAI",
            LlmProviderKind::OpenRouter => "OpenRouter",
            LlmProviderKind::GptCodex => "GPT Codex",
            LlmProviderKind::OpenCodeGo => "OpenCode Go",
            LlmProviderKind::LmStudio => "LM Studio",
            LlmProviderKind::Ollama => "Ollama",
        }
    }

    pub fn default_model_id(&self) -> &'static str {
        match self {
            LlmProviderKind::OpenAi => "gpt-4o-mini",
            LlmProviderKind::OpenRouter => "openai/gpt-4o-mini",
            LlmProviderKind::GptCodex => "gpt-4o-mini",
            LlmProviderKind::OpenCodeGo => "kimi-k2.7-code",
            // LM Studio / Ollama model ids depend on what's loaded/pulled; fetch the real
            // list from the dropdown.
            LlmProviderKind::LmStudio => "local-model",
            LlmProviderKind::Ollama => "qwen2.5-coder:7b",
        }
    }

    /// Default port for a `RemoteSsh` compute target's runtime, i.e. the port the runtime
    /// listens on locally on the remote host. Used to pre-fill [`SshConfig`] when a profile
    /// is switched to Remote, so e.g. an LM Studio profile defaults to `1234` instead of
    /// Ollama's `11434`.
    pub fn default_remote_runtime_port(&self) -> u16 {
        match self {
            LlmProviderKind::LmStudio => 1234,
            _ => 11434,
        }
    }

    /// Whether HTTP clients for this provider should accept self-signed / invalid TLS certs.
    ///
    /// Enabled for LM Studio and Ollama, which often run on a trusted LAN host behind
    /// HTTPS with a self-signed cert — same trust model as a local SearXNG instance.
    /// Stays off for public providers so their certs are always validated.
    pub fn allows_self_signed_tls(&self) -> bool {
        matches!(self, LlmProviderKind::LmStudio | LlmProviderKind::Ollama)
    }
}

/// Where the model server for a profile actually runs.
///
/// `Local` covers the common case (the runtime listens on this machine, or on a LAN host
/// reachable directly via `base_url`). `RemoteSsh` tunnels the connection through SSH port
/// forwarding so a runtime bound to `127.0.0.1` on a remote host (reachable only over SSH)
/// can still be reached as if it were local. See [`crate::compute`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ComputeLocation {
    #[default]
    Local,
    RemoteSsh(SshConfig),
}

/// SSH connection details for a remote compute target. The password itself is **not**
/// stored here — it lives in `ssh_credentials.json` (see [`crate::compute::store`]), keyed
/// by [`ProviderProfile::id`], so it never ends up in `settings.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshConfig {
    pub host: String,
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    pub user: String,
    /// Port the model runtime (Ollama/LM Studio) listens on on the remote host.
    pub remote_runtime_port: u16,
}

fn default_ssh_port() -> u16 {
    22
}

impl Default for SshConfig {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: default_ssh_port(),
            user: String::new(),
            remote_runtime_port: 11434,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderProfile {
    pub id: String,
    pub name: String,
    pub provider: LlmProviderKind,
    pub model_id: String,
    /// Override base URL (empty = use default for provider).
    pub base_url: String,
    /// Never written to `settings.json` — persisted in the OS keychain instead, keyed
    /// by [`ProviderProfile::id`]. See [`super::AppSettings::migrate_secrets_to_keychain`]
    /// for how an existing plaintext key gets moved out on first load with this version.
    #[serde(default, skip_serializing)]
    pub api_key: String,
    pub openrouter_http_referer: String,
    pub openrouter_title: String,
    /// Optional explicit context window in tokens for this model/profile.
    /// `None` (or `0`) = auto: look it up from the built-in model catalog, then fall
    /// back to a conservative default. Set to a number to override the history trim budget.
    #[serde(default)]
    pub context_window: Option<usize>,
    /// Where the model server for this profile runs. Defaults to [`ComputeLocation::Local`]
    /// so existing/older settings files (no `location` field) behave exactly as before.
    #[serde(default)]
    pub location: ComputeLocation,
}

impl ProviderProfile {
    pub fn new(id: impl Into<String>, provider: LlmProviderKind, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            provider,
            model_id: provider.default_model_id().to_string(),
            base_url: String::new(),
            api_key: String::new(),
            openrouter_http_referer: String::new(),
            openrouter_title: String::new(),
            context_window: None,
            location: ComputeLocation::Local,
        }
    }

    /// `Some(&SshConfig)` when this profile's runtime is reached over an SSH tunnel.
    pub fn ssh_config(&self) -> Option<&SshConfig> {
        match &self.location {
            ComputeLocation::Local => None,
            ComputeLocation::RemoteSsh(cfg) => Some(cfg),
        }
    }

    pub fn effective_base_url(&self) -> String {
        let t = self.base_url.trim();
        if !t.is_empty() {
            t.trim_end_matches('/').to_string()
        } else {
            self.provider.default_base_url().to_string()
        }
    }

    pub fn subtitle(&self) -> String {
        format!("{} · {}", self.provider.label(), self.model_id)
    }

    /// Resolve the effective context window in tokens for this profile.
    ///
    /// Order: explicit profile override > built-in catalog > provider/model default.
    pub fn effective_context_window(&self, fallback_default: usize) -> usize {
        if let Some(cw) = self.context_window {
            if cw > 0 {
                return cw;
            }
        }
        crate::agent::models::context_window_for_model(&self.model_id).unwrap_or(fallback_default)
    }
}

/// Which web search backend the `web_search` tool uses. Bing is the zero-config default:
/// it serves a stable RSS feed of results with no API key and no bot-challenge page.
/// DuckDuckGo's HTML endpoint is available as an explicit selection, but is currently
/// blocked by an anomaly challenge. SearXNG routes through a user-configured instance
/// (see `searxng_url`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum WebSearchBackend {
    #[default]
    Bing,
    DuckDuckGo,
    SearXng,
}

impl WebSearchBackend {
    pub const ALL: [WebSearchBackend; 3] = [
        WebSearchBackend::Bing,
        WebSearchBackend::DuckDuckGo,
        WebSearchBackend::SearXng,
    ];

    pub fn label(self) -> &'static str {
        match self {
            WebSearchBackend::Bing => "Bing",
            WebSearchBackend::DuckDuckGo => "DuckDuckGo",
            WebSearchBackend::SearXng => "SearXNG",
        }
    }
}

/// Overall text/UI density. Applied via egui's zoom factor so fonts and spacing scale together
/// and the layout stays coherent at every step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum UiDensity {
    Compact,
    #[default]
    Normal,
    Comfortable,
}

impl UiDensity {
    pub const ALL: [UiDensity; 3] = [
        UiDensity::Compact,
        UiDensity::Normal,
        UiDensity::Comfortable,
    ];

    pub fn label(self) -> &'static str {
        match self {
            UiDensity::Compact => "Compact",
            UiDensity::Normal => "Normal",
            UiDensity::Comfortable => "Comfortable",
        }
    }

    /// egui zoom factor relative to the base 14px scale (Normal = 1.0).
    pub fn zoom_factor(self) -> f32 {
        match self {
            UiDensity::Compact => 0.96,
            UiDensity::Normal => 1.0,
            UiDensity::Comfortable => 1.04,
        }
    }
}

pub(crate) fn sanitize_profile_name(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    s.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `settings.json` written by a pre-keychain version of oxi still has a plaintext
    /// `api_key` in it; deserializing must still read that value so
    /// `migrate_secrets_to_keychain` has something to migrate out on first load.
    #[test]
    fn legacy_plaintext_api_key_still_deserializes() {
        let json = r#"{"id":"t","name":"t","provider":"openai","model_id":"gpt-4o-mini","base_url":"","api_key":"sk-legacy","openrouter_http_referer":"","openrouter_title":""}"#;
        let p: ProviderProfile = serde_json::from_str(json).unwrap();
        assert_eq!(p.api_key, "sk-legacy");
    }

    #[test]
    fn new_profile_defaults_to_local_compute() {
        let p = ProviderProfile::new("test", LlmProviderKind::Ollama, "test");
        assert_eq!(p.location, ComputeLocation::Local);
        assert!(p.ssh_config().is_none());
    }

    #[test]
    fn ssh_config_returns_some_for_remote() {
        let mut p = ProviderProfile::new("test", LlmProviderKind::Ollama, "test");
        p.location = ComputeLocation::RemoteSsh(SshConfig {
            host: "test-host".to_string(),
            port: 22,
            user: "testuser".to_string(),
            remote_runtime_port: 11434,
        });
        let cfg = p.ssh_config().expect("remote ssh config");
        assert_eq!(cfg.host, "test-host");
        assert_eq!(cfg.remote_runtime_port, 11434);
    }

    #[test]
    fn compute_location_missing_field_deserializes_to_local() {
        // Older settings.json files have no `location` field at all.
        let json = r#"{"id":"t","name":"t","provider":"ollama","model_id":"x","base_url":"","api_key":"","openrouter_http_referer":"","openrouter_title":""}"#;
        let p: ProviderProfile = serde_json::from_str(json).unwrap();
        assert_eq!(p.location, ComputeLocation::Local);
    }

    #[test]
    fn ssh_config_serde_roundtrip() {
        let loc = ComputeLocation::RemoteSsh(SshConfig {
            host: "test-host.local".to_string(),
            port: 2222,
            user: "testuser".to_string(),
            remote_runtime_port: 1234,
        });
        let json = serde_json::to_string(&loc).unwrap();
        let back: ComputeLocation = serde_json::from_str(&json).unwrap();
        assert_eq!(loc, back);
    }

    #[test]
    fn effective_base_url_uses_default_when_empty() {
        let p = ProviderProfile::new("test", LlmProviderKind::OpenAi, "test");
        assert_eq!(p.effective_base_url(), "https://api.openai.com/v1");
    }

    #[test]
    fn effective_base_url_uses_override() {
        let mut p = ProviderProfile::new("test", LlmProviderKind::OpenAi, "test");
        p.base_url = "http://localhost:8080/v1/".to_string();
        assert_eq!(p.effective_base_url(), "http://localhost:8080/v1");
    }

    #[test]
    fn provider_labels_not_empty() {
        for kind in LlmProviderKind::ALL {
            assert!(!kind.label().is_empty());
            assert!(!kind.default_base_url().is_empty());
            assert!(!kind.default_model_id().is_empty());
        }
    }

    #[test]
    fn sanitize_profile_name_handles_special_chars() {
        assert_eq!(sanitize_profile_name("My Profile!"), "my-profile");
        assert_eq!(sanitize_profile_name("---test---"), "test");
        assert_eq!(sanitize_profile_name("abc123"), "abc123");
    }

    #[test]
    fn profile_subtitle_format() {
        let p = ProviderProfile::new("test", LlmProviderKind::OpenAi, "test");
        let sub = p.subtitle();
        assert!(sub.contains("OpenAI"));
        assert!(sub.contains("gpt-4o-mini"));
    }
}
