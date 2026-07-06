//! The top-level [`AppSettings`] struct: defaults, on-disk load/save, migration from
//! older settings shapes, and per-provider config accessors.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::provider::{
    ComputeLocation, LlmProviderKind, ProviderConfig, ProviderProfile, UiDensity, WebSearchBackend,
};

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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppSettings {
    /// Which provider the composer currently talks to.
    #[serde(default)]
    pub active_provider: LlmProviderKind,
    /// One config per provider kind. Serialized as a JSON object keyed by
    /// [`LlmProviderKind::slug`]; [`AppSettings::normalize`] guarantees every kind has an
    /// entry, so lookups through [`AppSettings::provider`] are infallible.
    #[serde(default)]
    pub providers: BTreeMap<LlmProviderKind, ProviderConfig>,
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
    /// Provider pinned for the "generate commit message" feature. `None` = use the
    /// active provider.
    #[serde(default)]
    pub commit_msg_provider: Option<LlmProviderKind>,
    /// Model pinned for the "generate commit message" feature. Empty = use the pinned
    /// (or active) provider's selected model.
    #[serde(default)]
    pub commit_msg_model_id: String,
    /// System prompt for the "generate commit message" feature.
    #[serde(default = "default_commit_msg_system_prompt")]
    pub commit_msg_system_prompt: String,
    /// Sidebar workspaces (project folders) and their fold state, restored on startup.
    /// The cwd workspace is always present at runtime even if missing here.
    #[serde(default)]
    pub workspaces: Vec<WorkspaceEntry>,
}

/// One persisted sidebar workspace: its root folder and whether its chat list is folded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceEntry {
    pub root_path: String,
    #[serde(default)]
    pub folded: bool,
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
pub const DEFAULT_COMMIT_MSG_SYSTEM_PROMPT: &str = "You generate concise, well-formed git commit messages from a staged/unstaged diff. \
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
        let providers = LlmProviderKind::ALL
            .into_iter()
            .map(|kind| (kind, ProviderConfig::new(kind)))
            .collect();
        Self {
            active_provider: LlmProviderKind::OpenAi,
            providers,
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
            commit_msg_provider: None,
            commit_msg_model_id: String::new(),
            commit_msg_system_prompt: default_commit_msg_system_prompt(),
            workspaces: Vec::new(),
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
        let bytes = fs::read(&path).unwrap_or_default();
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        // Sniff the on-disk shape by its distinctive keys rather than trying typed parses
        // in sequence: a profiles-era file must never silently parse as the new shape
        // (it would come out with empty provider configs and drop the user's setup).
        let (mut settings, ssh_renames, migrated) = if value.get("providers").is_some() {
            let s = serde_json::from_value::<AppSettings>(value).unwrap_or_default();
            (s, Vec::new(), false)
        } else if value.get("profiles").is_some() {
            let (s, renames) = Self::from_profiles_era(value, |profile_id| {
                crate::secrets::load(&format!("api-key:{profile_id}"))
            });
            (s, renames, true)
        } else if value.get("provider").is_some() {
            match serde_json::from_value::<LegacyAppSettings>(value) {
                Ok(old) => (Self::from_legacy(old), Vec::new(), true),
                Err(_) => (Self::default(), Vec::new(), false),
            }
        } else {
            (Self::default(), Vec::new(), false)
        };
        settings.normalize();
        settings.migrate_secrets_to_keychain();
        if migrated {
            Self::migrate_ssh_credentials(&ssh_renames);
            // Rewrite settings.json in the new shape right away so the migration runs
            // exactly once; from here on the file parses via the `providers` branch.
            let _ = settings.save();
        }
        settings
    }

    /// Provider API keys used to be written to `settings.json` in plaintext; the field is
    /// now `skip_serializing`, so freshly-parsed JSON either has no `api_key` at all
    /// (already migrated), or still has a leftover plaintext value from before this
    /// version. For each provider: a non-empty value just parsed from the file (or carried
    /// over by migration) is pushed into the unified secrets blob so it becomes the source
    /// of truth. Otherwise, pull whatever is already there into memory for this session.
    /// Reads/writes the keychain's unified item at most once per launch (via
    /// `load_unified`'s cache), rather than once per provider.
    fn migrate_secrets_to_keychain(&mut self) {
        let mut unified = crate::secrets::load_unified();
        let mut changed = false;
        for (kind, cfg) in &mut self.providers {
            if !cfg.api_key.is_empty() {
                if unified.provider_api_keys.get(kind.slug()) != Some(&cfg.api_key) {
                    unified
                        .provider_api_keys
                        .insert(kind.slug().to_string(), cfg.api_key.clone());
                    changed = true;
                }
            } else if let Some(key) = unified.provider_api_keys.get(kind.slug()) {
                cfg.api_key = key.clone();
            }
        }
        if changed {
            let _ = crate::secrets::save_unified(&unified);
        }
    }

    /// Migrate a profiles-era `settings.json` (multiple named profiles per provider,
    /// selected via `active_profile_id`) into the per-provider shape.
    ///
    /// Collapsing rule per provider kind: the active profile wins for its own kind;
    /// otherwise the first profile of that kind with a configured API key (so an empty
    /// default placeholder never shadows the one the user actually set up); otherwise the
    /// first profile of that kind; otherwise defaults. `hydrate_key` resolves a profile
    /// id to its keychain API key (injectable so migration tests don't touch the OS
    /// keychain). Returns the settings plus `(old profile id, kind)` pairs whose SSH
    /// credentials need re-keying.
    ///
    /// The old `api-key:{profile-id}` keychain entries are deliberately *not* deleted:
    /// they are orphaned but recoverable, so a migration bug can never lose a key. The
    /// new entries live under `api-key:{slug}`, a namespace old ids can't collide with.
    fn from_profiles_era(
        value: serde_json::Value,
        hydrate_key: impl Fn(&str) -> String,
    ) -> (Self, Vec<(String, LlmProviderKind)>) {
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct ProfilesEra {
            active_profile_id: String,
            profiles: Vec<ProviderProfile>,
            commit_msg_profile_id: String,
        }
        let era: ProfilesEra = serde_json::from_value(value.clone()).unwrap_or_default();
        // All shared (non-profile) fields parse straight into the new struct; the
        // profile-era keys are ignored as unknown fields and `providers` starts empty.
        let mut s: AppSettings = serde_json::from_value(value).unwrap_or_default();

        let mut profiles = era.profiles;
        for p in &mut profiles {
            if p.api_key.is_empty() {
                p.api_key = hydrate_key(&p.id);
            }
        }
        let active = profiles.iter().find(|p| p.id == era.active_profile_id);
        s.active_provider = active
            .or(profiles.first())
            .map(|p| p.provider)
            .unwrap_or_default();

        let mut ssh_renames = Vec::new();
        s.providers.clear();
        for kind in LlmProviderKind::ALL {
            let chosen = active
                .filter(|p| p.provider == kind)
                .or_else(|| {
                    profiles
                        .iter()
                        .find(|p| p.provider == kind && !p.api_key.trim().is_empty())
                })
                .or_else(|| profiles.iter().find(|p| p.provider == kind));
            let cfg = match chosen {
                Some(p) => {
                    if matches!(p.location, ComputeLocation::RemoteSsh(_)) {
                        ssh_renames.push((p.id.clone(), kind));
                    }
                    ProviderConfig::from(p.clone())
                }
                None => ProviderConfig::new(kind),
            };
            s.providers.insert(kind, cfg);
        }

        // Preserve the commit-message generator's intent even when the profile it
        // referenced wasn't the one chosen for its kind above.
        if !era.commit_msg_profile_id.trim().is_empty()
            && let Some(p) = profiles.iter().find(|p| p.id == era.commit_msg_profile_id)
        {
            s.commit_msg_provider = Some(p.provider);
            s.commit_msg_model_id = p.model_id.clone();
        }
        (s, ssh_renames)
    }

    /// Re-key SSH passwords from old profile ids to provider slugs (see
    /// [`Self::from_profiles_era`]). Old entries are removed after copying — the store is
    /// a single keychain blob, so this is one atomic rewrite.
    fn migrate_ssh_credentials(renames: &[(String, LlmProviderKind)]) {
        if renames.is_empty() {
            return;
        }
        let mut creds = crate::compute::store::load_ssh_credentials();
        let mut changed = false;
        for (old_id, kind) in renames {
            if let Some(pw) = creds.get(old_id).map(str::to_string) {
                creds.set(kind.slug(), pw);
                creds.clear(old_id);
                changed = true;
            }
        }
        if changed {
            let _ = crate::compute::store::save_ssh_credentials(&creds);
        }
    }

    /// Migrate the oldest flat single-provider settings shape (pre-profiles).
    fn from_legacy(old: LegacyAppSettings) -> Self {
        let provider = old.provider;
        let mut s = Self {
            system_prompt: if old.system_prompt.trim().is_empty() {
                crate::agent::prompt::DEFAULT_AGENT_SYSTEM_PROMPT.to_string()
            } else {
                old.system_prompt
            },
            tools_enabled: old.tools_enabled.to_vec(),
            active_provider: provider,
            ..Self::default()
        };
        let cfg = s.provider_mut(provider);
        cfg.model_id = old.model_id;
        cfg.base_url = old.base_url;
        cfg.api_key = match provider {
            LlmProviderKind::OpenAi | LlmProviderKind::GptCodex => old.openai_api_key,
            LlmProviderKind::OpenRouter => old.openrouter_api_key,
            LlmProviderKind::OpenCodeGo | LlmProviderKind::LmStudio | LlmProviderKind::Ollama => {
                String::new()
            }
        };
        cfg.openrouter_http_referer = old.openrouter_http_referer;
        cfg.openrouter_title = old.openrouter_title;
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
        // Every provider kind gets an entry (files written by older versions, or with
        // kinds added since, may miss some), and the `#[serde(skip)]`ped `provider` field
        // is re-stamped from the map key it was deserialized under.
        for kind in LlmProviderKind::ALL {
            let cfg = self
                .providers
                .entry(kind)
                .or_insert_with(|| ProviderConfig::new(kind));
            cfg.provider = kind;
            if cfg.model_id.trim().is_empty() {
                cfg.model_id = kind.default_model_id().to_string();
            }
            if cfg.context_window == Some(0) {
                // 0/null means "auto".
                cfg.context_window = None;
            }
            if !matches!(
                cfg.effort.trim(),
                "" | "low" | "medium" | "high" | "xhigh" | "max"
            ) {
                cfg.effort.clear();
            }
        }
        if self.context_window_default == 0 {
            self.context_window_default = default_context_window();
        }
        // Workspaces: drop entries whose folder vanished, dedupe by path.
        let mut seen = std::collections::HashSet::new();
        self.workspaces.retain(|w| {
            std::path::Path::new(&w.root_path).is_dir() && seen.insert(w.root_path.clone())
        });
    }

    pub fn save(&self) -> Result<(), String> {
        let mut unified = crate::secrets::load_unified();
        let mut changed = false;
        for (kind, cfg) in &self.providers {
            let slug = kind.slug();
            if cfg.api_key.is_empty() {
                if unified.provider_api_keys.remove(slug).is_some() {
                    changed = true;
                }
            } else if unified.provider_api_keys.get(slug) != Some(&cfg.api_key) {
                unified
                    .provider_api_keys
                    .insert(slug.to_string(), cfg.api_key.clone());
                changed = true;
            }
        }
        if changed {
            let _ = crate::secrets::save_unified(&unified);
        }
        let path = Self::config_path();
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        fs::write(&path, json).map_err(|e| e.to_string())?;
        // Restrict to the owner as defense in depth for the rest of this file (base
        // URLs, model ids, etc.); the actual secrets (API keys) live in the OS keychain,
        // not here.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    /// Config for the given provider kind. Infallible: [`AppSettings::normalize`] runs on
    /// every load and guarantees an entry per kind.
    pub fn provider(&self, kind: LlmProviderKind) -> &ProviderConfig {
        self.providers
            .get(&kind)
            .expect("normalize() guarantees every provider kind has a config")
    }

    pub fn provider_mut(&mut self, kind: LlmProviderKind) -> &mut ProviderConfig {
        self.providers
            .entry(kind)
            .or_insert_with(|| ProviderConfig::new(kind))
    }

    /// Config the composer currently talks to.
    pub fn active_config(&self) -> &ProviderConfig {
        self.provider(self.active_provider)
    }

    /// Config used by the "generate commit message" feature: the pinned provider (or the
    /// active one), with the pinned model overriding the provider's selected model.
    pub fn commit_msg_config(&self) -> ProviderConfig {
        let kind = self.commit_msg_provider.unwrap_or(self.active_provider);
        let mut cfg = self.provider(kind).clone();
        let pinned_model = self.commit_msg_model_id.trim();
        if !pinned_model.is_empty() {
            cfg.model_id = pinned_model.to_string();
        }
        cfg
    }

    /// Provider kinds the user has actually configured (has usable credentials for),
    /// in the same display order as [`LlmProviderKind::ALL`]. Local runtimes (Ollama, LM
    /// Studio) need no key so they always count as configured; hosted providers need a
    /// configured API key or the matching env var (mirrors the fallback chain in
    /// `agent::runner::configured_*_key`); GPT Codex additionally counts as configured via
    /// its "Sign in with ChatGPT" OAuth token.
    pub fn configured_provider_kinds(
        &self,
        oauth: &crate::oauth::OAuthStore,
    ) -> Vec<LlmProviderKind> {
        let has_profile_key =
            |kind: LlmProviderKind| !self.provider(kind).api_key.trim().is_empty();
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
            WebSearchBackend::SearXng => self.searxng_url.trim().trim_end_matches('/').to_string(),
            WebSearchBackend::Bing | WebSearchBackend::DuckDuckGo => String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_has_all_provider_kinds() {
        let s = AppSettings::default();
        for kind in LlmProviderKind::ALL {
            assert_eq!(s.provider(kind).provider, kind);
        }
        assert_eq!(s.active_config().provider, s.active_provider);
    }

    /// Provider API keys must never be written to `settings.json` — they live in the OS
    /// keychain (see `migrate_secrets_to_keychain`). This doesn't touch the real keychain,
    /// just the serde attribute on `ProviderConfig::api_key`.
    #[test]
    fn api_key_not_serialized_to_json() {
        let mut s = AppSettings::default();
        s.provider_mut(LlmProviderKind::OpenAi).api_key = "sk-super-secret-value".to_string();
        let json = serde_json::to_string(&s).unwrap();
        assert!(!json.contains("sk-super-secret-value"));
        assert!(!json.contains("api_key"));
    }

    /// The new shape must round-trip: the `#[serde(skip)]`ped `provider` field comes back
    /// via `normalize()` re-stamping it from the map key.
    #[test]
    fn new_shape_roundtrips() {
        let mut s = AppSettings {
            active_provider: LlmProviderKind::OpenRouter,
            ..Default::default()
        };
        s.provider_mut(LlmProviderKind::OpenRouter).model_id = "anthropic/claude-sonnet-5".into();
        s.provider_mut(LlmProviderKind::Ollama).base_url = "http://box:11434/v1".into();
        let json = serde_json::to_string(&s).unwrap();
        let mut back: AppSettings = serde_json::from_str(&json).unwrap();
        back.normalize();
        assert_eq!(back, s);
    }

    #[test]
    fn normalize_fills_missing_provider_kinds() {
        // A new-shape file listing only one provider still normalizes to all kinds.
        let json = r#"{
            "active_provider": "ollama",
            "providers": {"ollama": {"model_id": "qwen3:14b"}},
            "system_prompt": "hi"
        }"#;
        let mut s: AppSettings = serde_json::from_str(json).unwrap();
        s.normalize();
        assert_eq!(s.provider(LlmProviderKind::Ollama).model_id, "qwen3:14b");
        for kind in LlmProviderKind::ALL {
            assert_eq!(s.provider(kind).provider, kind);
            assert!(!s.provider(kind).model_id.is_empty());
        }
    }

    #[test]
    fn commit_msg_config_defaults_to_active() {
        let s = AppSettings {
            active_provider: LlmProviderKind::Ollama,
            ..Default::default()
        };
        let cfg = s.commit_msg_config();
        assert_eq!(cfg.provider, LlmProviderKind::Ollama);
        assert_eq!(cfg.model_id, s.active_config().model_id);
    }

    #[test]
    fn commit_msg_config_pins_provider_and_model() {
        let s = AppSettings {
            commit_msg_provider: Some(LlmProviderKind::OpenRouter),
            commit_msg_model_id: "openai/gpt-4o-mini".to_string(),
            ..Default::default()
        };
        let cfg = s.commit_msg_config();
        assert_eq!(cfg.provider, LlmProviderKind::OpenRouter);
        assert_eq!(cfg.model_id, "openai/gpt-4o-mini");
    }

    /// Profiles-era migration: the active profile wins for its kind, even when another
    /// profile of the same kind comes first in the list.
    #[test]
    fn migrates_profiles_era_active_profile_wins_for_its_kind() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{
            "active_profile_id": "openrouter-work",
            "profiles": [
                {"id":"openai-default","name":"OpenAI default","provider":"openai","model_id":"gpt-4o-mini","base_url":"","api_key":"","openrouter_http_referer":"","openrouter_title":""},
                {"id":"openrouter-default","name":"OpenRouter default","provider":"openrouter","model_id":"openai/gpt-4o-mini","base_url":"","api_key":"","openrouter_http_referer":"","openrouter_title":""},
                {"id":"openrouter-work","name":"Work","provider":"openrouter","model_id":"anthropic/claude-sonnet-5","base_url":"https://proxy.example.com/v1","api_key":"","openrouter_http_referer":"","openrouter_title":""}
            ],
            "system_prompt": "hi",
            "tools_enabled": [true, true, true, true, true, true, true, true, true]
        }"#,
        )
        .unwrap();
        let (mut s, renames) = AppSettings::from_profiles_era(json, |_| String::new());
        s.normalize();
        assert!(renames.is_empty());
        assert_eq!(s.active_provider, LlmProviderKind::OpenRouter);
        let or = s.provider(LlmProviderKind::OpenRouter);
        assert_eq!(or.model_id, "anthropic/claude-sonnet-5");
        assert_eq!(or.base_url, "https://proxy.example.com/v1");
        // Shared fields carried over.
        assert_eq!(s.system_prompt, "hi");
    }

    /// A profile with a configured key beats an earlier empty placeholder of the same kind.
    #[test]
    fn migrates_profiles_era_prefers_profile_with_key() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{
            "active_profile_id": "ollama-default",
            "profiles": [
                {"id":"ollama-default","name":"Ollama","provider":"ollama","model_id":"qwen3:14b","base_url":"","api_key":"","openrouter_http_referer":"","openrouter_title":""},
                {"id":"openai-default","name":"OpenAI default","provider":"openai","model_id":"gpt-4o-mini","base_url":"","api_key":"","openrouter_http_referer":"","openrouter_title":""},
                {"id":"openai-real","name":"Real","provider":"openai","model_id":"gpt-4.1","base_url":"","api_key":"","openrouter_http_referer":"","openrouter_title":""}
            ],
            "system_prompt": "hi",
            "tools_enabled": [true, true, true, true, true, true, true, true, true]
        }"#,
        )
        .unwrap();
        // Simulate the keychain holding a key only for "openai-real".
        let (mut s, _) = AppSettings::from_profiles_era(json, |id| {
            if id == "openai-real" {
                "sk-real".to_string()
            } else {
                String::new()
            }
        });
        s.normalize();
        assert_eq!(s.active_provider, LlmProviderKind::Ollama);
        let oa = s.provider(LlmProviderKind::OpenAi);
        assert_eq!(oa.model_id, "gpt-4.1");
        assert_eq!(oa.api_key, "sk-real");
    }

    /// `commit_msg_profile_id` intent survives migration as (provider, model) even when
    /// the referenced profile is not the one chosen as that kind's config.
    #[test]
    fn migrates_profiles_era_commit_msg_intent() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{
            "active_profile_id": "openrouter-main",
            "profiles": [
                {"id":"openrouter-main","name":"Main","provider":"openrouter","model_id":"anthropic/claude-sonnet-5","base_url":"","api_key":"","openrouter_http_referer":"","openrouter_title":""},
                {"id":"openrouter-cheap","name":"Cheap","provider":"openrouter","model_id":"openai/gpt-4o-mini","base_url":"","api_key":"","openrouter_http_referer":"","openrouter_title":""}
            ],
            "commit_msg_profile_id": "openrouter-cheap",
            "system_prompt": "hi",
            "tools_enabled": [true, true, true, true, true, true, true, true, true]
        }"#,
        )
        .unwrap();
        let (mut s, _) = AppSettings::from_profiles_era(json, |_| String::new());
        s.normalize();
        // The kind's config came from the active profile...
        assert_eq!(
            s.provider(LlmProviderKind::OpenRouter).model_id,
            "anthropic/claude-sonnet-5"
        );
        // ...but the commit-msg generator still targets the cheap model.
        assert_eq!(s.commit_msg_provider, Some(LlmProviderKind::OpenRouter));
        assert_eq!(s.commit_msg_model_id, "openai/gpt-4o-mini");
        assert_eq!(s.commit_msg_config().model_id, "openai/gpt-4o-mini");
    }

    /// SSH-backed profiles report their old id for credential re-keying.
    #[test]
    fn migrates_profiles_era_reports_ssh_renames() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{
            "active_profile_id": "ollama-box",
            "profiles": [
                {"id":"ollama-box","name":"Box","provider":"ollama","model_id":"qwen3:14b","base_url":"","api_key":"","openrouter_http_referer":"","openrouter_title":"",
                 "location":{"kind":"remote_ssh","host":"box.local","port":22,"user":"me","remote_runtime_port":11434}}
            ],
            "system_prompt": "hi",
            "tools_enabled": [true, true, true, true, true, true, true, true, true]
        }"#,
        )
        .unwrap();
        let (mut s, renames) = AppSettings::from_profiles_era(json, |_| String::new());
        s.normalize();
        assert_eq!(
            renames,
            vec![("ollama-box".to_string(), LlmProviderKind::Ollama)]
        );
        assert!(s.provider(LlmProviderKind::Ollama).ssh_config().is_some());
    }

    /// The oldest flat single-provider shape still migrates into the new one.
    #[test]
    fn migrates_flat_legacy_shape() {
        let old = LegacyAppSettings {
            provider: LlmProviderKind::OpenRouter,
            model_id: "openai/gpt-4o-mini".to_string(),
            base_url: "https://openrouter.ai/api/v1".to_string(),
            system_prompt: "legacy prompt".to_string(),
            openai_api_key: String::new(),
            openrouter_api_key: "sk-or-legacy".to_string(),
            openrouter_http_referer: "https://example.com".to_string(),
            openrouter_title: "oxi".to_string(),
            tools_enabled: [true; 7],
        };
        let mut s = AppSettings::from_legacy(old);
        s.normalize();
        assert_eq!(s.active_provider, LlmProviderKind::OpenRouter);
        let or = s.provider(LlmProviderKind::OpenRouter);
        assert_eq!(or.model_id, "openai/gpt-4o-mini");
        assert_eq!(or.api_key, "sk-or-legacy");
        assert_eq!(or.openrouter_http_referer, "https://example.com");
        assert_eq!(s.system_prompt, "legacy prompt");
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
            "active_provider": "openai",
            "providers": {"openai": {"model_id":"gpt-4o-mini","base_url":""}},
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
        let s = AppSettings {
            web_search_backend: WebSearchBackend::DuckDuckGo,
            searxng_url: "https://searxng.example.com".to_string(),
            ..Default::default()
        };
        // Even with a URL set, DuckDuckGo backend ignores it.
        assert_eq!(s.effective_web_search_url(), "");
    }

    #[test]
    fn effective_web_search_url_searxng_returns_url() {
        let s = AppSettings {
            web_search_backend: WebSearchBackend::SearXng,
            searxng_url: "https://searxng.example.com/".to_string(),
            ..Default::default()
        };
        // Trailing slash trimmed.
        assert_eq!(s.effective_web_search_url(), "https://searxng.example.com");
    }

    #[test]
    fn effective_web_search_url_searxng_empty_returns_empty_for_tool_error() {
        let s = AppSettings {
            web_search_backend: WebSearchBackend::SearXng,
            searxng_url: String::new(),
            ..Default::default()
        };
        // Empty URL is passed through so the tool can report a SearXNG configuration error.
        assert_eq!(s.effective_web_search_url(), "");
    }

    #[test]
    fn normalize_migrates_nonempty_url_to_searxng_backend() {
        // Older settings.json: explicit `web_search_backend: "duckduckgo"` (the old default)
        // and a non-empty searxng_url. normalize() should migrate to SearXng.
        let json = r#"{
            "active_provider": "openai",
            "providers": {"openai": {"model_id":"gpt-4o-mini","base_url":""}},
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
            "active_provider": "openai",
            "providers": {"openai": {"model_id":"gpt-4o-mini","base_url":""}},
            "system_prompt": "hi",
            "tools_enabled": [true, true, true, true, true, true, true, true, true]
        }"#;
        let mut s: AppSettings = serde_json::from_str(json).unwrap();
        s.normalize();
        assert_eq!(s.web_search_backend, WebSearchBackend::Bing);
        assert_eq!(s.effective_web_search_url(), "");
    }
}
