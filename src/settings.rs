//! Persistent settings (`~/.config/oxi/settings.json`).

use std::fs;
use std::path::PathBuf;

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

pub const ALL_TOOL_NAMES: [&str; 9] = [
    "read",
    "write",
    "edit",
    "bash",
    "grep",
    "find",
    "ls",
    "web_search",
    "web_fetch",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderProfile {
    pub id: String,
    pub name: String,
    pub provider: LlmProviderKind,
    pub model_id: String,
    /// Override base URL (empty = use default for provider).
    pub base_url: String,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppSettings {
    pub active_profile_id: String,
    pub profiles: Vec<ProviderProfile>,
    /// Single editable system prompt template.
    pub system_prompt: String,
    /// One flag per entry in [`ALL_TOOL_NAMES`]. Stored as a `Vec` so older settings files
    /// with fewer tools still deserialize; [`AppSettings::normalize`] resizes it to the
    /// current tool count, enabling any newly-added tools by default.
    #[serde(default = "default_tools_enabled")]
    pub tools_enabled: Vec<bool>,
    /// Which web search backend the `web_search` tool uses. See [`WebSearchBackend`].
    #[serde(default)]
    pub web_search_backend: WebSearchBackend,
    /// Base URL of the SearXNG instance used by the `web_search` tool when
    /// [`AppSettings::web_search_backend`] is [`WebSearchBackend::SearXng`]. If the backend
    /// is set to SearXNG but this is empty, `web_search` returns a configuration error rather
    /// than falling back to another provider.
    #[serde(default = "default_searxng_url")]
    pub searxng_url: String,
    /// Require explicit user approval before each mutating tool (`bash` / `write` / `edit`).
    #[serde(default = "default_require_approval")]
    pub require_approval: bool,
    /// Persisted width of the main app/sidebar split.
    #[serde(default = "default_sidebar_width")]
    pub sidebar_width: f32,
    /// Persisted height of the bottom terminal panel.
    #[serde(default = "default_terminal_height")]
    pub terminal_height: f32,
    /// Whether the bottom terminal panel is shown.
    #[serde(default)]
    pub terminal_open: bool,
    /// Whether the right source-control (git) panel is shown.
    #[serde(default)]
    pub git_open: bool,
    /// Persisted width of the right git panel.
    #[serde(default = "default_git_width")]
    pub git_width: f32,
    /// Active color theme id (see [`crate::theme`]: `dark`, `light`, `midnight`, or
    /// `custom:<name>`). Falls back to the default theme if unknown.
    #[serde(default = "default_theme_id")]
    pub theme_id: String,
    /// Overall text/UI density (zoom). Defaults to [`UiDensity::Normal`].
    #[serde(default)]
    pub ui_density: UiDensity,
    /// Maximum number of agent tool rounds per run. `0` means unlimited. Default unlimited.
    #[serde(default = "default_max_tool_rounds")]
    pub max_tool_rounds: u32,
    /// Fallback context window in tokens used when no per-profile override and no catalog
    /// match is found. Defaults to 128k (safe across all current providers).
    #[serde(default = "default_context_window")]
    pub context_window_default: usize,
    /// Profile id used by the "generate commit message" feature. Empty = use the active
    /// profile. Must reference an existing [`ProviderProfile`] id; unknown ids fall back
    /// to the active profile at use time.
    #[serde(default)]
    pub commit_msg_profile_id: String,
    /// System prompt for the "generate commit message" feature.
    #[serde(default = "default_commit_msg_system_prompt")]
    pub commit_msg_system_prompt: String,
}

fn default_require_approval() -> bool {
    true
}

fn default_max_tool_rounds() -> u32 {
    0
}

fn default_context_window() -> usize {
    128_000
}

fn default_commit_msg_system_prompt() -> String {
    DEFAULT_COMMIT_MSG_SYSTEM_PROMPT.to_string()
}

/// Default system prompt for the "generate commit message" feature.
pub const DEFAULT_COMMIT_MSG_SYSTEM_PROMPT: &str =
    "You generate concise, well-formed git commit messages from a staged/unstaged diff. \
     Rules:\n\
     - Output ONLY the commit message, no preamble, no code fences, no explanations.\n\
     - Start with a single imperative subject line up to ~50 characters, lowercase where natural.\n\
     - If the change is non-trivial, add a blank line then a short body (bullet points OK) wrapping at ~72 chars.\n\
     - Do not mention the diff itself, file counts, or that this was AI-generated.\n\
     - Follow Conventional Commits (e.g. feat:, fix:, refactor:, docs:, chore:) when it fits.";

fn default_tools_enabled() -> Vec<bool> {
    vec![true; ALL_TOOL_NAMES.len()]
}

fn default_searxng_url() -> String {
    // No universal public SearXNG instance exists (public ones rate-limit and rarely
    // expose the JSON API), so this ships empty. When SearXNG is selected, the user must
    // configure an instance URL; web_search will not fall back to another provider.
    String::new()
}

fn default_sidebar_width() -> f32 {
    168.0
}

fn default_terminal_height() -> f32 {
    260.0
}

fn default_git_width() -> f32 {
    360.0
}

/// Clamp bounds for the bottom terminal panel height.
pub const TERMINAL_H_MIN: f32 = 96.0;
pub const TERMINAL_H_MAX: f32 = 900.0;

fn default_theme_id() -> String {
    crate::theme::DEFAULT_THEME_ID.to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LegacyAppSettings {
    pub provider: LlmProviderKind,
    pub model_id: String,
    pub base_url: String,
    pub system_prompt: String,
    pub openai_api_key: String,
    pub openrouter_api_key: String,
    pub openrouter_http_referer: String,
    pub openrouter_title: String,
    pub tools_enabled: [bool; 7],
}

impl Default for AppSettings {
    fn default() -> Self {
        let profiles = vec![
            ProviderProfile::new("openai-default", LlmProviderKind::OpenAi, "OpenAI default"),
            ProviderProfile::new(
                "openrouter-default",
                LlmProviderKind::OpenRouter,
                "OpenRouter default",
            ),
            ProviderProfile::new("codex-default", LlmProviderKind::GptCodex, "Codex default"),
            ProviderProfile::new(
                "opencode-go-default",
                LlmProviderKind::OpenCodeGo,
                "OpenCode Go default",
            ),
            ProviderProfile::new(
                "lmstudio-default",
                LlmProviderKind::LmStudio,
                "LM Studio default",
            ),
            ProviderProfile::new("ollama-default", LlmProviderKind::Ollama, "Ollama default"),
        ];
        Self {
            active_profile_id: "openai-default".to_string(),
            profiles,
            system_prompt: crate::agent::prompt::DEFAULT_AGENT_SYSTEM_PROMPT.to_string(),
            tools_enabled: default_tools_enabled(),
            web_search_backend: WebSearchBackend::default(),
            searxng_url: default_searxng_url(),
            require_approval: default_require_approval(),
            sidebar_width: default_sidebar_width(),
            terminal_height: default_terminal_height(),
            terminal_open: false,
            git_open: false,
            git_width: default_git_width(),
            theme_id: default_theme_id(),
            ui_density: UiDensity::Normal,
            max_tool_rounds: default_max_tool_rounds(),
            context_window_default: default_context_window(),
            commit_msg_profile_id: String::new(),
            commit_msg_system_prompt: default_commit_msg_system_prompt(),
        }
    }
}

impl AppSettings {
    pub fn config_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("oxi")
            .join("settings.json")
    }

    pub fn load() -> Self {
        let path = Self::config_path();
        if let Ok(bytes) = fs::read(&path) {
            if let Ok(mut s) = serde_json::from_slice::<AppSettings>(&bytes) {
                s.normalize();
                return s;
            }
            if let Ok(old) = serde_json::from_slice::<LegacyAppSettings>(&bytes) {
                let mut s = Self::from_legacy(old);
                s.normalize();
                return s;
            }
        }
        Self::default()
    }

    #[allow(clippy::field_reassign_with_default)]
    fn from_legacy(old: LegacyAppSettings) -> Self {
        let provider = old.provider;
        let mut s = Self::default();
        s.system_prompt = if old.system_prompt.trim().is_empty() {
            crate::agent::prompt::DEFAULT_AGENT_SYSTEM_PROMPT.to_string()
        } else {
            old.system_prompt
        };
        s.tools_enabled = old.tools_enabled.to_vec();
        s.profiles = vec![
            ProviderProfile {
                id: "migrated-active".to_string(),
                name: format!("{} migrated", provider.label()),
                provider,
                model_id: old.model_id,
                base_url: old.base_url,
                api_key: match provider {
                    LlmProviderKind::OpenAi | LlmProviderKind::GptCodex => old.openai_api_key,
                    LlmProviderKind::OpenRouter => old.openrouter_api_key,
                    LlmProviderKind::OpenCodeGo
                    | LlmProviderKind::LmStudio
                    | LlmProviderKind::Ollama => String::new(),
                },
                openrouter_http_referer: old.openrouter_http_referer,
                openrouter_title: old.openrouter_title,
                context_window: None,
                location: ComputeLocation::Local,
            },
            ProviderProfile::new("openai-default", LlmProviderKind::OpenAi, "OpenAI default"),
            ProviderProfile::new(
                "openrouter-default",
                LlmProviderKind::OpenRouter,
                "OpenRouter default",
            ),
            ProviderProfile::new("codex-default", LlmProviderKind::GptCodex, "Codex default"),
            ProviderProfile::new(
                "opencode-go-default",
                LlmProviderKind::OpenCodeGo,
                "OpenCode Go default",
            ),
            ProviderProfile::new(
                "lmstudio-default",
                LlmProviderKind::LmStudio,
                "LM Studio default",
            ),
            ProviderProfile::new("ollama-default", LlmProviderKind::Ollama, "Ollama default"),
        ];
        s.active_profile_id = "migrated-active".to_string();
        s
    }

    fn normalize(&mut self) {
        // Migrate: older settings files had only `searxng_url` (no `web_search_backend`),
        // wrote the then-default `"duckduckgo"` explicitly. If such a file also has a
        // non-empty SearXNG URL configured, assume the user meant the SearXNG backend;
        // leaving it on DuckDuckGo would silently ignore the configured URL.
        // (A missing `web_search_backend` field now deserializes to the current default,
        // Bing — which is zero-config like DuckDuckGo, so no migration is needed for it.)
        if !self.searxng_url.trim().is_empty()
            && self.web_search_backend == WebSearchBackend::DuckDuckGo
        {
            self.web_search_backend = WebSearchBackend::SearXng;
        }
        // Resize to the current tool count: older settings files have fewer flags, and any
        // newly-added tools default to enabled.
        if self.tools_enabled.len() != ALL_TOOL_NAMES.len() {
            self.tools_enabled.resize(ALL_TOOL_NAMES.len(), true);
        }
        if self.system_prompt.trim().is_empty() {
            self.system_prompt = crate::agent::prompt::DEFAULT_AGENT_SYSTEM_PROMPT.to_string();
        }
        if self.commit_msg_system_prompt.trim().is_empty() {
            self.commit_msg_system_prompt = default_commit_msg_system_prompt();
        }
        if !self.sidebar_width.is_finite() || self.sidebar_width <= 0.0 {
            self.sidebar_width = default_sidebar_width();
        }
        self.sidebar_width = self.sidebar_width.clamp(120.0, 520.0);
        if !self.terminal_height.is_finite() || self.terminal_height <= 0.0 {
            self.terminal_height = default_terminal_height();
        }
        self.terminal_height = self.terminal_height.clamp(TERMINAL_H_MIN, TERMINAL_H_MAX);
        if self.profiles.is_empty() {
            *self = Self::default();
            return;
        }
        for profile in &mut self.profiles {
            if profile.id.trim().is_empty() {
                profile.id = format!(
                    "{}-{}",
                    profile.provider.label().to_lowercase().replace(' ', "-"),
                    sanitize_profile_name(&profile.name)
                );
            }
            if profile.name.trim().is_empty() {
                profile.name = format!("{} profile", profile.provider.label());
            }
            if profile.model_id.trim().is_empty() {
                profile.model_id = profile.provider.default_model_id().to_string();
            }
            if let Some(cw) = profile.context_window {
                if cw > 0 {
                    profile.context_window = Some(cw);
                } else {
                    // 0/null means "auto".
                    profile.context_window = None;
                }
            }
        }
        if self.context_window_default == 0 {
            self.context_window_default = default_context_window();
        }
        if self.active_profile().is_none() {
            self.active_profile_id = self.profiles[0].id.clone();
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let path = Self::config_path();
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        fs::write(&path, json).map_err(|e| e.to_string())
    }

    pub fn active_profile(&self) -> Option<&ProviderProfile> {
        self.profiles
            .iter()
            .find(|p| p.id == self.active_profile_id)
    }

    /// Profile used by the "generate commit message" feature. Falls back to the active
    /// profile when no explicit id is configured or the stored id no longer exists.
    pub fn commit_msg_profile(&self) -> Option<&ProviderProfile> {
        if !self.commit_msg_profile_id.trim().is_empty() {
            if let Some(p) = self
                .profiles
                .iter()
                .find(|p| p.id == self.commit_msg_profile_id)
            {
                return Some(p);
            }
        }
        self.active_profile()
    }

    pub fn set_active_profile(&mut self, id: impl AsRef<str>) {
        let id = id.as_ref();
        if self.profiles.iter().any(|p| p.id == id) {
            self.active_profile_id = id.to_string();
        }
    }

    /// First profile of the given provider kind, if one exists. Used to resolve which
    /// profile becomes active when the composer's provider dropdown picks a kind rather
    /// than a specific profile.
    pub fn first_profile_for(&self, kind: LlmProviderKind) -> Option<&ProviderProfile> {
        self.profiles.iter().find(|p| p.provider == kind)
    }

    /// Provider kinds the user has actually configured (has usable credentials for),
    /// in the same display order as [`LlmProviderKind::ALL`]. Local runtimes (Ollama, LM
    /// Studio) need no key so they always count as configured; hosted providers need a
    /// profile API key or the matching env var (mirrors the fallback chain in
    /// `agent::runner::configured_*_key`); GPT Codex additionally counts as configured via
    /// its "Sign in with ChatGPT" OAuth token.
    pub fn configured_provider_kinds(
        &self,
        oauth: &crate::oauth::OAuthStore,
    ) -> Vec<LlmProviderKind> {
        let has_profile_key = |kind: LlmProviderKind| {
            self.profiles
                .iter()
                .any(|p| p.provider == kind && !p.api_key.trim().is_empty())
        };
        LlmProviderKind::ALL
            .into_iter()
            .filter(|&kind| match kind {
                LlmProviderKind::LmStudio | LlmProviderKind::Ollama => true,
                LlmProviderKind::OpenAi => {
                    has_profile_key(kind) || std::env::var("OPENAI_API_KEY").is_ok()
                }
                LlmProviderKind::OpenRouter => {
                    has_profile_key(kind) || std::env::var("OPENROUTER_API_KEY").is_ok()
                }
                LlmProviderKind::OpenCodeGo => {
                    has_profile_key(kind)
                        || std::env::var("OPENCODE_GO_API_KEY").is_ok()
                        || std::env::var("OPENCODE_API_KEY").is_ok()
                }
                LlmProviderKind::GptCodex => {
                    oauth.openai_codex.is_some()
                        || has_profile_key(kind)
                        || std::env::var("OPENAI_API_KEY").is_ok()
                }
            })
            .collect()
    }

    /// URL passed to the `web_search` tool as its base.
    ///
    /// Returns the SearXNG URL only when the SearXNG backend is selected; otherwise returns
    /// empty so `web_search` can call the selected zero-config backend directly. If SearXNG
    /// is selected and the URL is empty, this returns empty and the tool reports a
    /// configuration error rather than falling back to another provider.
    pub fn effective_web_search_url(&self) -> String {
        match self.web_search_backend {
            WebSearchBackend::SearXng => {
                let u = self.searxng_url.trim().trim_end_matches('/').to_string();
                u
            }
            WebSearchBackend::Bing | WebSearchBackend::DuckDuckGo => String::new(),
        }
    }

    pub fn add_profile(&mut self, provider: LlmProviderKind) -> String {
        let base_name = format!("{} profile", provider.label());
        let mut n = 1usize;
        let name = loop {
            let candidate = if n == 1 {
                base_name.clone()
            } else {
                format!("{} {}", base_name, n)
            };
            if !self.profiles.iter().any(|p| p.name == candidate) {
                break candidate;
            }
            n += 1;
        };
        let id = format!(
            "{}-{}",
            provider.label().to_lowercase().replace(' ', "-"),
            sanitize_profile_name(&name)
        );
        self.profiles
            .push(ProviderProfile::new(id.clone(), provider, name));
        id
    }

    pub fn remove_profile(&mut self, id: &str) {
        if self.profiles.len() <= 1 {
            return;
        }
        let removed_active = self.active_profile_id == id;
        self.profiles.retain(|p| p.id != id);
        if self.profiles.is_empty() {
            *self = Self::default();
            return;
        }
        if removed_active {
            self.active_profile_id = self.profiles[0].id.clone();
        }
    }
}

fn sanitize_profile_name(name: &str) -> String {
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

    #[test]
    fn default_has_profiles() {
        let s = AppSettings::default();
        assert!(!s.profiles.is_empty());
        assert!(s.active_profile().is_some());
    }

    #[test]
    fn active_profile_matches_active_id() {
        let s = AppSettings::default();
        let p = s.active_profile().unwrap();
        assert_eq!(p.id, s.active_profile_id);
    }

    #[test]
    fn set_active_profile_valid() {
        let mut s = AppSettings::default();
        let second_id = s.profiles[1].id.clone();
        s.set_active_profile(&second_id);
        assert_eq!(s.active_profile_id, second_id);
    }

    #[test]
    fn set_active_profile_invalid_ignored() {
        let mut s = AppSettings::default();
        let old_id = s.active_profile_id.clone();
        s.set_active_profile("nonexistent");
        assert_eq!(s.active_profile_id, old_id);
    }

    #[test]
    fn add_profile_returns_id() {
        let mut s = AppSettings::default();
        let initial_count = s.profiles.len();
        let id = s.add_profile(LlmProviderKind::OpenAi);
        assert!(!id.is_empty());
        assert_eq!(s.profiles.len(), initial_count + 1);
        assert!(s.profiles.iter().any(|p| p.id == id));
    }

    #[test]
    fn add_profile_deduplicates_names() {
        let mut s = AppSettings::default();
        let id1 = s.add_profile(LlmProviderKind::OpenAi);
        let id2 = s.add_profile(LlmProviderKind::OpenAi);
        assert_ne!(id1, id2);
        let p1 = s.profiles.iter().find(|p| p.id == id1).unwrap();
        let p2 = s.profiles.iter().find(|p| p.id == id2).unwrap();
        assert_ne!(p1.name, p2.name);
    }

    #[test]
    fn remove_profile_last_one_resets_to_default() {
        let mut s = AppSettings::default();
        let ids: Vec<String> = s.profiles.iter().map(|p| p.id.clone()).collect();
        // Remove all but one
        for id in &ids[1..] {
            s.remove_profile(id);
        }
        assert_eq!(s.profiles.len(), 1);
        // Try removing the last one - should not remove
        s.remove_profile(&ids[0]);
        assert_eq!(s.profiles.len(), 1);
    }

    #[test]
    fn remove_active_profile_switches_to_first() {
        let mut s = AppSettings::default();
        let active = s.active_profile_id.clone();
        s.remove_profile(&active);
        assert_ne!(s.active_profile_id, active);
        assert!(s.active_profile().is_some());
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

    #[test]
    fn tools_enabled_default_all_true() {
        let s = AppSettings::default();
        assert_eq!(s.tools_enabled.len(), ALL_TOOL_NAMES.len());
        assert!(s.tools_enabled.iter().all(|&t| t));
    }

    #[test]
    fn all_tool_names_has_expected_tools() {
        assert_eq!(ALL_TOOL_NAMES.len(), 9);
        assert!(ALL_TOOL_NAMES.contains(&"bash"));
        assert!(ALL_TOOL_NAMES.contains(&"web_search"));
        assert!(ALL_TOOL_NAMES.contains(&"web_fetch"));
    }

    #[test]
    fn normalize_pads_short_tools_enabled() {
        let json = r#"{
            "active_profile_id": "openai-default",
            "profiles": [{"id":"openai-default","name":"x","provider":"openai","model_id":"gpt-4o-mini","base_url":"","api_key":"","openrouter_http_referer":"","openrouter_title":""}],
            "system_prompt": "hi",
            "tools_enabled": [true, false, true, true, true, true, true]
        }"#;
        let mut s: AppSettings = serde_json::from_str(json).unwrap();
        s.normalize();
        assert_eq!(s.tools_enabled.len(), ALL_TOOL_NAMES.len());
        // Pre-existing flags are preserved.
        assert!(!s.tools_enabled[1]);
        // Newly-added tools default to enabled.
        assert!(s.tools_enabled[7]);
        assert!(s.tools_enabled[8]);
        // Missing searxng_url falls back to the default.
        assert_eq!(s.searxng_url, default_searxng_url());
    }

    #[test]
    fn effective_web_search_url_duckduckgo_is_empty() {
        let mut s = AppSettings::default();
        s.web_search_backend = WebSearchBackend::DuckDuckGo;
        s.searxng_url = "https://searxng.example.com".to_string();
        // Even with a URL set, DuckDuckGo backend ignores it.
        assert_eq!(s.effective_web_search_url(), "");
    }

    #[test]
    fn effective_web_search_url_searxng_returns_url() {
        let mut s = AppSettings::default();
        s.web_search_backend = WebSearchBackend::SearXng;
        s.searxng_url = "https://searxng.example.com/".to_string();
        // Trailing slash trimmed.
        assert_eq!(s.effective_web_search_url(), "https://searxng.example.com");
    }

    #[test]
    fn effective_web_search_url_searxng_empty_returns_empty_for_tool_error() {
        let mut s = AppSettings::default();
        s.web_search_backend = WebSearchBackend::SearXng;
        s.searxng_url = String::new();
        // Empty URL is passed through so the tool can report a SearXNG configuration error.
        assert_eq!(s.effective_web_search_url(), "");
    }

    #[test]
    fn normalize_migrates_nonempty_url_to_searxng_backend() {
        // Older settings.json: explicit `web_search_backend: "duckduckgo"` (the old default)
        // and a non-empty searxng_url. normalize() should migrate to SearXng.
        let json = r#"{
            "active_profile_id": "openai-default",
            "profiles": [{"id":"openai-default","name":"x","provider":"openai","model_id":"gpt-4o-mini","base_url":"","api_key":"","openrouter_http_referer":"","openrouter_title":""}],
            "system_prompt": "hi",
            "tools_enabled": [true, true, true, true, true, true, true, true, true],
            "web_search_backend": "duckduckgo",
            "searxng_url": "https://searxng.example.com"
        }"#;
        let mut s: AppSettings = serde_json::from_str(json).unwrap();
        s.normalize();
        assert_eq!(s.web_search_backend, WebSearchBackend::SearXng);
        assert_eq!(s.effective_web_search_url(), "https://searxng.example.com");
    }

    #[test]
    fn normalize_keeps_default_when_no_url() {
        // Older settings.json: no `web_search_backend` field and empty searxng_url.
        // Deserializes to Bing (current default) and stays on Bing.
        let json = r#"{
            "active_profile_id": "openai-default",
            "profiles": [{"id":"openai-default","name":"x","provider":"openai","model_id":"gpt-4o-mini","base_url":"","api_key":"","openrouter_http_referer":"","openrouter_title":""}],
            "system_prompt": "hi",
            "tools_enabled": [true, true, true, true, true, true, true, true, true]
        }"#;
        let mut s: AppSettings = serde_json::from_str(json).unwrap();
        s.normalize();
        assert_eq!(s.web_search_backend, WebSearchBackend::Bing);
        assert_eq!(s.effective_web_search_url(), "");
    }
}
