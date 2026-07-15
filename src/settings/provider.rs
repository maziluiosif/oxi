//! Provider domain types: which LLM backend a config talks to, where its runtime lives
//! (local vs. an SSH-tunneled remote), and the per-provider configuration itself.

use serde::{Deserialize, Serialize};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
#[serde(rename_all = "lowercase")]
pub enum LlmProviderKind {
    #[default]
    OpenAi,
    OpenRouter,
    /// Azure OpenAI deployment endpoint.
    AzureOpenAi,
    /// User-configured Anthropic Messages-compatible endpoint.
    CustomAnthropic,
    /// GPT Codex family via OpenAI Chat Completions (`api.openai.com`).
    GptCodex,
    /// OpenCode Go subscription models (OpenAI/Anthropic-compatible endpoints).
    OpenCodeGo,
    /// LM Studio local server (OpenAI-compatible API, on this machine or a LAN host).
    LmStudio,
    /// Ollama local server (OpenAI-compatible API at `/v1`, on this machine or a LAN host).
    Ollama,
    /// oxi-managed HuggingFace GGUF models run via a local llama.cpp `llama-server` process.
    LocalHf,
    /// Claude Code driven over the Agent Client Protocol (ACP): oxi spawns the
    /// `claude-code-acp` adapter as a subprocess and speaks JSON-RPC over stdio, letting
    /// Claude Code run its own agent loop and tools. Unlike every other provider, oxi is an
    /// ACP *client* here rather than the agent. See [`crate::agent::acp`].
    ClaudeCodeAcp,
}

impl LlmProviderKind {
    /// Order here drives the provider pill-tab order in Settings → Providers. Ollama and
    /// LM Studio lead the list since they're the local/self-hosted runtimes oxi is built
    /// around; the hosted API providers follow.
    pub const ALL: [LlmProviderKind; 10] = [
        LlmProviderKind::LocalHf,
        LlmProviderKind::Ollama,
        LlmProviderKind::LmStudio,
        LlmProviderKind::ClaudeCodeAcp,
        LlmProviderKind::AzureOpenAi,
        LlmProviderKind::CustomAnthropic,
        LlmProviderKind::OpenAi,
        LlmProviderKind::OpenRouter,
        LlmProviderKind::GptCodex,
        LlmProviderKind::OpenCodeGo,
    ];

    /// Canonical string id for this provider. Matches the serde `lowercase` variant names
    /// exactly, so the `settings.json` map key, the keychain account (`api-key:{slug}`),
    /// and the SSH-credential key are all the same string.
    pub fn slug(&self) -> &'static str {
        match self {
            LlmProviderKind::OpenAi => "openai",
            LlmProviderKind::OpenRouter => "openrouter",
            LlmProviderKind::AzureOpenAi => "azureopenai",
            LlmProviderKind::CustomAnthropic => "customanthropic",
            LlmProviderKind::GptCodex => "gptcodex",
            LlmProviderKind::OpenCodeGo => "opencodego",
            LlmProviderKind::LmStudio => "lmstudio",
            LlmProviderKind::Ollama => "ollama",
            LlmProviderKind::LocalHf => "localhf",
            LlmProviderKind::ClaudeCodeAcp => "claudecodeacp",
        }
    }

    pub fn default_base_url(&self) -> &'static str {
        match self {
            LlmProviderKind::OpenAi | LlmProviderKind::GptCodex => "https://api.openai.com/v1",
            LlmProviderKind::OpenRouter => "https://openrouter.ai/api/v1",
            LlmProviderKind::AzureOpenAi => {
                "https://YOUR_RESOURCE.openai.azure.com/openai/deployments/YOUR_DEPLOYMENT"
            }
            LlmProviderKind::CustomAnthropic => "http://localhost:8000",
            LlmProviderKind::OpenCodeGo => "https://opencode.ai/zen/go",
            // LM Studio's built-in server speaks plain HTTP on port 1234 by default.
            // (HTTPS would need a separate reverse proxy.)
            LlmProviderKind::LmStudio => "http://localhost:1234/v1",
            // Ollama's OpenAI-compatible API lives under `/v1` on its default port 11434.
            LlmProviderKind::Ollama => "http://localhost:11434/v1",
            LlmProviderKind::LocalHf => "http://127.0.0.1:18080/v1",
            // ACP does not use an HTTP base URL; it launches a subprocess (see `acp_command`).
            LlmProviderKind::ClaudeCodeAcp => "",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            LlmProviderKind::OpenAi => "OpenAI",
            LlmProviderKind::OpenRouter => "OpenRouter",
            LlmProviderKind::AzureOpenAi => "Azure OpenAI",
            LlmProviderKind::CustomAnthropic => "Custom Anthropic",
            LlmProviderKind::GptCodex => "GPT Codex",
            LlmProviderKind::OpenCodeGo => "OpenCode Go",
            LlmProviderKind::LmStudio => "LM Studio",
            LlmProviderKind::Ollama => "Ollama",
            LlmProviderKind::LocalHf => "Local HF",
            LlmProviderKind::ClaudeCodeAcp => "Claude Code (ACP)",
        }
    }

    pub fn default_model_id(&self) -> &'static str {
        match self {
            LlmProviderKind::OpenAi => "gpt-4o-mini",
            LlmProviderKind::OpenRouter => "openai/gpt-4o-mini",
            LlmProviderKind::AzureOpenAi => "gpt-4o-mini",
            LlmProviderKind::CustomAnthropic => "claude-sonnet-4-5",
            LlmProviderKind::GptCodex => "gpt-4o-mini",
            LlmProviderKind::OpenCodeGo => "kimi-k2.7-code",
            // LM Studio / Ollama model ids depend on what's loaded/pulled; fetch the real
            // list from the dropdown.
            LlmProviderKind::LmStudio => "local-model",
            LlmProviderKind::Ollama => "qwen2.5-coder:7b",
            LlmProviderKind::LocalHf => "local-hf-model",
            // Informational only: Claude Code picks the model from its own config/login.
            LlmProviderKind::ClaudeCodeAcp => "sonnet",
        }
    }

    /// Default port for a `RemoteSsh` compute target's runtime, i.e. the port the runtime
    /// listens on locally on the remote host. Used to pre-fill [`SshConfig`] when a provider
    /// config is switched to Remote, so e.g. LM Studio defaults to `1234` instead of
    /// Ollama's `11434`.
    pub fn default_remote_runtime_port(&self) -> u16 {
        match self {
            LlmProviderKind::LmStudio => 1234,
            LlmProviderKind::LocalHf => 18080,
            _ => 11434,
        }
    }

    /// Whether HTTP clients for this provider should accept self-signed / invalid TLS certs.
    ///
    /// Enabled for LM Studio and Ollama, which often run on a trusted LAN host behind
    /// HTTPS with a self-signed cert — same trust model as a local SearXNG instance.
    /// Stays off for public providers so their certs are always validated.
    pub fn allows_self_signed_tls(&self) -> bool {
        matches!(
            self,
            LlmProviderKind::LmStudio
                | LlmProviderKind::Ollama
                | LlmProviderKind::LocalHf
                | LlmProviderKind::AzureOpenAi
                | LlmProviderKind::CustomAnthropic
        )
    }
}

/// Where the model server for a provider actually runs.
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
/// stored here — it lives in the OS keychain (see [`crate::compute::store`]), keyed by
/// [`LlmProviderKind::slug`], so it never ends up in `settings.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshConfig {
    pub host: String,
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    pub user: String,
    /// Port the model runtime (Ollama/LM Studio) listens on on the remote host.
    pub remote_runtime_port: u16,
    /// SHA-256 host key fingerprint ("SHA256:<base64>") pinned on the first successful
    /// connection (trust-on-first-use). `None` = not yet pinned; the next successful
    /// connect records it. A later connection presenting a different key is refused until
    /// the user accepts the new key. Not a secret, so it lives here in `settings.json`.
    #[serde(default)]
    pub pinned_host_key: Option<String>,
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
            pinned_host_key: None,
        }
    }
}

/// Connection/model configuration for one provider. Exactly one exists per
/// [`LlmProviderKind`], stored as the values of `AppSettings::providers`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ProviderConfig {
    /// Duplicated from the map key in `AppSettings::providers` so a config can be passed
    /// around as one self-contained value. `#[serde(skip)]` keeps it out of the JSON —
    /// the map key is the on-disk source of truth — and `AppSettings::normalize`
    /// re-stamps it after every load.
    #[serde(skip)]
    pub provider: LlmProviderKind,
    #[serde(default)]
    pub model_id: String,
    /// Override base URL (empty = use default for provider).
    #[serde(default)]
    pub base_url: String,
    /// Never written to `settings.json` — persisted in the OS keychain instead, keyed
    /// `api-key:{provider.slug()}`. See `AppSettings::migrate_secrets_to_keychain`.
    #[serde(default, skip_serializing)]
    pub api_key: String,
    #[serde(default)]
    pub openrouter_http_referer: String,
    #[serde(default)]
    pub openrouter_title: String,
    /// Optional explicit context window in tokens for this provider's model.
    /// `None` (or `0`) = auto: look it up from the built-in model catalog, then fall
    /// back to a conservative default. Set to a number to override the history trim budget.
    #[serde(default)]
    pub context_window: Option<usize>,
    /// Claude 4.6+ adaptive thinking effort. Empty = provider default (high).
    #[serde(default)]
    pub effort: String,
    /// Where the model server for this provider runs. Defaults to [`ComputeLocation::Local`]
    /// so existing/older settings files (no `location` field) behave exactly as before.
    #[serde(default)]
    pub location: ComputeLocation,
    /// Command line used to launch the ACP agent subprocess (only for
    /// [`LlmProviderKind::ClaudeCodeAcp`]). Empty = the built-in default
    /// (`npx @zed-industries/claude-code-acp`). Run through the platform shell so `npx`,
    /// PATH lookup, and arguments all work as typed.
    #[serde(default)]
    pub acp_command: String,
}

impl ProviderConfig {
    pub fn new(provider: LlmProviderKind) -> Self {
        Self {
            provider,
            model_id: provider.default_model_id().to_string(),
            ..Self::default()
        }
    }

    /// `Some(&SshConfig)` when this provider's runtime is reached over an SSH tunnel.
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

    /// Compact "Provider · model" label (unit-tested; handy for status/chrome surfaces).
    #[allow(dead_code)]
    pub fn subtitle(&self) -> String {
        format!("{} · {}", self.provider.label(), self.model_id)
    }

    /// Default command line for launching an ACP agent subprocess. Uses the actively-maintained
    /// adapter (the older `@zed-industries/claude-code-acp` is deprecated and pins older model
    /// versions).
    pub const DEFAULT_ACP_COMMAND: &'static str = "npx @agentclientprotocol/claude-agent-acp";

    /// The command line used to spawn the ACP agent, falling back to the built-in default
    /// when the user hasn't overridden it.
    pub fn effective_acp_command(&self) -> String {
        let t = self.acp_command.trim();
        if t.is_empty() {
            Self::DEFAULT_ACP_COMMAND.to_string()
        } else {
            t.to_string()
        }
    }

    /// Resolve the effective context window in tokens for this provider.
    ///
    /// Order: explicit override > built-in catalog > provider/model default.
    pub fn effective_context_window(&self, fallback_default: usize) -> usize {
        if let Some(cw) = self.context_window
            && cw > 0
        {
            return cw;
        }
        crate::agent::models::context_window_for_model(&self.model_id).unwrap_or(fallback_default)
    }
}

/// Legacy per-profile shape from the profiles era (multiple named profiles per provider).
/// Only used to migrate old `settings.json` files — see `AppSettings::load`. Must keep
/// deserializing a plaintext `api_key` so pre-keychain files still migrate their secrets.
#[derive(Debug, Clone, Deserialize)]
pub struct ProviderProfile {
    #[serde(default)]
    pub id: String,
    pub provider: LlmProviderKind,
    #[serde(default)]
    pub model_id: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub openrouter_http_referer: String,
    #[serde(default)]
    pub openrouter_title: String,
    #[serde(default)]
    pub context_window: Option<usize>,
    #[serde(default)]
    pub effort: String,
    #[serde(default)]
    pub location: ComputeLocation,
}

impl From<ProviderProfile> for ProviderConfig {
    fn from(p: ProviderProfile) -> Self {
        Self {
            provider: p.provider,
            model_id: p.model_id,
            base_url: p.base_url,
            api_key: p.api_key,
            openrouter_http_referer: p.openrouter_http_referer,
            openrouter_title: p.openrouter_title,
            context_window: p.context_window,
            effort: p.effort,
            location: p.location,
            acp_command: String::new(),
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A `settings.json` written by a pre-keychain version of oxi still has a plaintext
    /// `api_key` in its profiles; deserializing the legacy shape must still read that
    /// value so migration has something to move into the keychain on first load.
    #[test]
    fn legacy_plaintext_api_key_still_deserializes() {
        let json = r#"{"id":"t","name":"t","provider":"openai","model_id":"gpt-4o-mini","base_url":"","api_key":"sk-legacy","openrouter_http_referer":"","openrouter_title":""}"#;
        let p: ProviderProfile = serde_json::from_str(json).unwrap();
        assert_eq!(p.api_key, "sk-legacy");
    }

    #[test]
    fn new_config_defaults_to_local_compute() {
        let c = ProviderConfig::new(LlmProviderKind::Ollama);
        assert_eq!(c.location, ComputeLocation::Local);
        assert!(c.ssh_config().is_none());
    }

    #[test]
    fn ssh_config_returns_some_for_remote() {
        let mut c = ProviderConfig::new(LlmProviderKind::Ollama);
        c.location = ComputeLocation::RemoteSsh(SshConfig {
            host: "test-host".to_string(),
            port: 22,
            user: "testuser".to_string(),
            remote_runtime_port: 11434,
            pinned_host_key: None,
        });
        let cfg = c.ssh_config().expect("remote ssh config");
        assert_eq!(cfg.host, "test-host");
        assert_eq!(cfg.remote_runtime_port, 11434);
    }

    #[test]
    fn provider_config_missing_location_deserializes_to_local() {
        // Settings files written before compute targets existed have no `location` field.
        let json = r#"{"model_id":"x","base_url":""}"#;
        let c: ProviderConfig = serde_json::from_str(json).unwrap();
        assert_eq!(c.location, ComputeLocation::Local);
    }

    #[test]
    fn ssh_config_serde_roundtrip() {
        let loc = ComputeLocation::RemoteSsh(SshConfig {
            host: "test-host.local".to_string(),
            port: 2222,
            user: "testuser".to_string(),
            remote_runtime_port: 1234,
            pinned_host_key: Some("SHA256:abc123".to_string()),
        });
        let json = serde_json::to_string(&loc).unwrap();
        let back: ComputeLocation = serde_json::from_str(&json).unwrap();
        assert_eq!(loc, back);
    }

    #[test]
    fn ssh_config_missing_pinned_host_key_deserializes_to_none() {
        // Settings files written before host-key pinning existed have no field.
        let json =
            r#"{"kind":"remote_ssh","host":"h","port":22,"user":"u","remote_runtime_port":11434}"#;
        let loc: ComputeLocation = serde_json::from_str(json).unwrap();
        match loc {
            ComputeLocation::RemoteSsh(cfg) => assert_eq!(cfg.pinned_host_key, None),
            _ => panic!("expected remote ssh"),
        }
    }

    #[test]
    fn effective_base_url_uses_default_when_empty() {
        let c = ProviderConfig::new(LlmProviderKind::OpenAi);
        assert_eq!(c.effective_base_url(), "https://api.openai.com/v1");
    }

    #[test]
    fn effective_base_url_uses_override() {
        let mut c = ProviderConfig::new(LlmProviderKind::OpenAi);
        c.base_url = "http://localhost:8080/v1/".to_string();
        assert_eq!(c.effective_base_url(), "http://localhost:8080/v1");
    }

    #[test]
    fn provider_labels_not_empty() {
        for kind in LlmProviderKind::ALL {
            assert!(!kind.label().is_empty());
            assert!(!kind.default_model_id().is_empty());
            // Claude Code (ACP) launches a subprocess and has no HTTP base URL.
            if kind != LlmProviderKind::ClaudeCodeAcp {
                assert!(!kind.default_base_url().is_empty());
            }
        }
    }

    /// The slug must match the serde `lowercase` rename exactly — it's used as the
    /// `settings.json` map key, so a mismatch would silently break round-tripping.
    #[test]
    fn slug_matches_serde_name() {
        for kind in LlmProviderKind::ALL {
            let json = serde_json::to_string(&kind).unwrap();
            assert_eq!(json, format!("\"{}\"", kind.slug()));
        }
    }

    #[test]
    fn config_subtitle_format() {
        let c = ProviderConfig::new(LlmProviderKind::OpenAi);
        let sub = c.subtitle();
        assert!(sub.contains("OpenAI"));
        assert!(sub.contains("gpt-4o-mini"));
    }
}
