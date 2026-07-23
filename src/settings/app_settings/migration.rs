//! Migration from historical settings formats and post-deserialization normalization.

use serde::{Deserialize, Serialize};

use super::super::provider::{
    ComputeLocation, LlmProviderKind, ProviderConfig, ProviderProfile, WebSearchBackend,
};
use super::types::{default_local_hf_context, default_local_hf_port};
use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct LegacyAppSettings {
    pub(super) provider: LlmProviderKind,
    pub(super) model_id: String,
    pub(super) base_url: String,
    pub(super) system_prompt: String,
    pub(super) openai_api_key: String,
    pub(super) openrouter_api_key: String,
    pub(super) openrouter_http_referer: String,
    pub(super) openrouter_title: String,
    pub(super) tools_enabled: [bool; 7],
}

impl AppSettings {
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
    pub(super) fn from_profiles_era(
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
    pub(super) fn migrate_ssh_credentials(renames: &[(String, LlmProviderKind)]) {
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
    pub(super) fn from_legacy(old: LegacyAppSettings) -> Self {
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
            LlmProviderKind::AzureOpenAi
            | LlmProviderKind::CustomAnthropic
            | LlmProviderKind::OpenCodeGo
            | LlmProviderKind::LmStudio
            | LlmProviderKind::Ollama
            | LlmProviderKind::LocalHf
            | LlmProviderKind::RemoteHf
            | LlmProviderKind::ClaudeCodeAcp
            | LlmProviderKind::CursorAcp
            | LlmProviderKind::CodexAcp => String::new(),
        };
        cfg.openrouter_http_referer = old.openrouter_http_referer;
        cfg.openrouter_title = old.openrouter_title;
        s
    }

    pub(super) fn normalize(&mut self) {
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
        if !self.chat_column_max_width.is_finite() || self.chat_column_max_width <= 0.0 {
            self.chat_column_max_width = default_chat_column_max_width();
        }
        self.chat_column_max_width = self.chat_column_max_width.clamp(
            crate::theme::CHAT_COLUMN_WIDTH_MIN,
            crate::theme::CHAT_COLUMN_WIDTH_MAX,
        );
        // Split the former dual-purpose Local HF config into dedicated providers. Preserve
        // an existing SSH setup by moving it to Remote HF the first time this version loads.
        if !self.providers.contains_key(&LlmProviderKind::RemoteHf)
            && let Some(local) = self.providers.get(&LlmProviderKind::LocalHf).cloned()
            && matches!(local.location, ComputeLocation::RemoteSsh(_))
        {
            let mut remote = local;
            remote.provider = LlmProviderKind::RemoteHf;
            self.providers.insert(LlmProviderKind::RemoteHf, remote);
            self.providers.insert(
                LlmProviderKind::LocalHf,
                ProviderConfig::new(LlmProviderKind::LocalHf),
            );
            if self.active_provider == LlmProviderKind::LocalHf {
                self.active_provider = LlmProviderKind::RemoteHf;
            }
            if self.commit_msg_provider == Some(LlmProviderKind::LocalHf) {
                self.commit_msg_provider = Some(LlmProviderKind::RemoteHf);
            }
        }

        // Remote HF used to inherit Local HF's managed port. Move that old default to the
        // dedicated Remote HF port; custom non-default ports remain untouched. This matters
        // when the SSH target is this same machine, where both runtimes otherwise bind 18080.
        if let Some(remote) = self.providers.get_mut(&LlmProviderKind::RemoteHf)
            && let ComputeLocation::RemoteSsh(ssh) = &mut remote.location
            && ssh.remote_runtime_port == LlmProviderKind::LocalHf.default_remote_runtime_port()
        {
            ssh.remote_runtime_port = LlmProviderKind::RemoteHf.default_remote_runtime_port();
        }

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
        // Migrate ids from the former hard-coded picker to runtime family ids. Missing fonts
        // then fall back to the bundled defaults, keeping settings portable between machines.
        self.ui_font = match self.ui_font.as_str() {
            "helvetica" => "system:Helvetica".to_string(),
            "helvetica_neue" => "system:Helvetica Neue".to_string(),
            "arial" => "system:Arial".to_string(),
            "georgia" => "system:Georgia".to_string(),
            // The old "System" entry was an OS-dependent alias rather than a font family.
            "system" => default_font_id(),
            _ => self.ui_font.clone(),
        };
        self.mono_font = match self.mono_font.as_str() {
            "sf_mono" => "system:SF Mono".to_string(),
            "menlo" => "system:Menlo".to_string(),
            "monaco" => "system:Monaco".to_string(),
            "courier" => "system:Courier New".to_string(),
            "dejavu_mono" => "system:DejaVu Sans Mono".to_string(),
            _ => self.mono_font.clone(),
        };
        if !crate::theme::ui_font_is_known(&self.ui_font) {
            self.ui_font = default_font_id();
        }
        if !crate::theme::mono_font_is_known(&self.mono_font) {
            self.mono_font = default_font_id();
        }
        if let Some(legacy_require_approval) = self.require_approval.take() {
            self.require_write_edit_approval = legacy_require_approval;
            self.require_bash_approval = legacy_require_approval;
        }
        if self.context_window_default == 0 {
            self.context_window_default = default_context_window();
        }
        if self.bash_timeout_cap_secs == 0 {
            self.bash_timeout_cap_secs = default_bash_timeout_cap_secs();
        }
        self.bash_timeout_cap_secs = self.bash_timeout_cap_secs.clamp(5, 3600);
        if self.local_hf.runtime_port == 0 {
            self.local_hf.runtime_port = default_local_hf_port();
        }
        if self.local_hf.context_size == 0 {
            self.local_hf.context_size = default_local_hf_context();
        }
        // Workspaces: drop entries whose folder vanished, dedupe by path.
        let mut seen = std::collections::HashSet::new();
        self.workspaces.retain(|w| {
            std::path::Path::new(&w.root_path).is_dir() && seen.insert(w.root_path.clone())
        });
        if let Some(root) = self.last_active_workspace_root_path.as_deref()
            && !std::path::Path::new(root).is_dir()
        {
            self.last_active_workspace_root_path = None;
            self.last_active_session_file = None;
        }
        if let Some(file) = self.last_active_session_file.as_deref()
            && !std::path::Path::new(file).is_file()
        {
            self.last_active_session_file = None;
        }
    }
}
