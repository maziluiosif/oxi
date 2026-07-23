//! Runtime accessors and derived provider/search configuration.

use super::super::provider::{LlmProviderKind, ProviderConfig, WebSearchBackend};
use super::*;

impl AppSettings {
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
                LlmProviderKind::LmStudio
                | LlmProviderKind::Ollama
                | LlmProviderKind::LocalHf
                | LlmProviderKind::RemoteHf => true,
                // Claude Code handles its own auth (subscription login or ANTHROPIC_API_KEY),
                // so it's always offered; the subprocess reports a clear error if not logged in.
                LlmProviderKind::ClaudeCodeAcp
                | LlmProviderKind::CursorAcp
                | LlmProviderKind::CodexAcp => true,
                LlmProviderKind::AzureOpenAi => true,
                LlmProviderKind::CustomAnthropic => {
                    has_profile_key(kind)
                        || std::env::var("CUSTOM_ANTHROPIC_API_KEY").is_ok()
                        || std::env::var("ANTHROPIC_API_KEY").is_ok()
                }
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
