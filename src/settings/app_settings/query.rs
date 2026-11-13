//! Runtime accessors and derived provider/search configuration.

use super::super::provider::{LlmProviderKind, ProviderConfig, WebSearchBackend};
use super::*;

impl AppSettings {
    /// The MCP servers agents get: the user's, plus agent-desktop when computer use is on. A
    /// server the user set up under that name wins.
    pub fn effective_mcp_servers(&self) -> Vec<super::McpServerConfig> {
        let mut servers = self.mcp_servers.clone();
        let name = crate::computer_use::SERVER_NAME;
        if self.computer_use && !servers.iter().any(|s| s.name.trim() == name) {
            servers.extend(crate::computer_use::server_config());
        }
        servers
    }

    /// Config for the given provider kind. Infallible: `AppSettings::normalize` runs on
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

    /// Providers with credentials, installed ACP clients, or explicitly configured local
    /// runtimes. Call off the UI thread: ACP detection resolves the login-shell environment.
    pub fn configured_provider_kinds(
        &self,
        oauth: &crate::oauth::OAuthStore,
    ) -> Vec<LlmProviderKind> {
        LlmProviderKind::ALL
            .into_iter()
            .filter(|&kind| self.provider_is_configured(kind, oauth))
            .collect()
    }

    /// Whether one provider is usable as configured; see [`Self::configured_provider_kinds`].
    pub fn provider_is_configured(
        &self,
        kind: LlmProviderKind,
        oauth: &crate::oauth::OAuthStore,
    ) -> bool {
        let has_profile_key =
            |kind: LlmProviderKind| !self.provider(kind).api_key.trim().is_empty();
        match kind {
            LlmProviderKind::LmStudio
            | LlmProviderKind::LlamaCpp
            | LlmProviderKind::Ollama
            | LlmProviderKind::LocalHf
            | LlmProviderKind::RemoteHf => {
                let config = self.provider(kind);
                let defaults = ProviderConfig::new(kind);
                config
                    .ssh_config()
                    .is_some_and(|ssh| !ssh.host.trim().is_empty())
                    || (!config.base_url.trim().is_empty() && config.base_url != defaults.base_url)
                    || (!config.model_id.trim().is_empty() && config.model_id != defaults.model_id)
                    || has_profile_key(kind)
                    || (kind == LlmProviderKind::LocalHf
                        && crate::local_models::load_manifest()
                            .models
                            .iter()
                            .any(|model| std::path::Path::new(&model.path).is_file()))
            }
            LlmProviderKind::ClaudeCodeAcp
            | LlmProviderKind::CursorAcp
            | LlmProviderKind::CodexAcp
            | LlmProviderKind::MuseCodeAcp
            | LlmProviderKind::AntigravityAcp
            | LlmProviderKind::CopilotAcp
            | LlmProviderKind::OpenCodeAcp => {
                super::super::availability::acp_client_installed(self.provider(kind))
            }
            LlmProviderKind::AzureOpenAi => {
                let config = self.provider(kind);
                (has_profile_key(kind) || env_key("AZURE_OPENAI_API_KEY"))
                    && !config.base_url.trim().is_empty()
                    && !config.base_url.contains("YOUR_RESOURCE")
                    && !config.base_url.contains("YOUR_DEPLOYMENT")
            }
            LlmProviderKind::Router => LlmProviderKind::ALL.into_iter().any(|candidate| {
                candidate != LlmProviderKind::Router
                    && self.provider_is_configured(candidate, oauth)
            }),
            LlmProviderKind::CustomAnthropic => {
                has_profile_key(kind)
                    || env_key("CUSTOM_ANTHROPIC_API_KEY")
                    || env_key("ANTHROPIC_API_KEY")
            }
            LlmProviderKind::OpenAi => has_profile_key(kind) || env_key("OPENAI_API_KEY"),
            LlmProviderKind::OpenRouter => has_profile_key(kind) || env_key("OPENROUTER_API_KEY"),
            LlmProviderKind::MetaMuse => has_profile_key(kind) || env_key("MODEL_API_KEY"),
            LlmProviderKind::OpenCodeGo => {
                has_profile_key(kind)
                    || env_key("OPENCODE_GO_API_KEY")
                    || env_key("OPENCODE_API_KEY")
            }
            LlmProviderKind::GptCodex => {
                oauth.openai_codex.is_some() || has_profile_key(kind) || env_key("OPENAI_API_KEY")
            }
        }
    }

    /// Tools that run without asking in the workspace at `root` (see
    /// [`Self::workspace_tool_allowlist`]).
    pub fn allowed_tools_for(&self, root: &std::path::Path) -> Vec<String> {
        self.workspace_tool_allowlist
            .get(root.to_string_lossy().trim())
            .cloned()
            .unwrap_or_default()
    }

    /// Let `tool` run without asking in the workspace at `root`. Returns whether it was new.
    pub fn allow_tool_in(&mut self, root: &std::path::Path, tool: &str) -> bool {
        let list = self
            .workspace_tool_allowlist
            .entry(root.to_string_lossy().trim().to_owned())
            .or_default();
        if list.iter().any(|t| t == tool) {
            return false;
        }
        list.push(tool.to_owned());
        true
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

/// Empty environment variables are not credentials.
fn env_key(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| !value.trim().is_empty())
}
