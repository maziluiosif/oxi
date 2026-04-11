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
    /// GitHub Copilot (Anthropic-compatible Messages API at Copilot base URL).
    GitHubCopilot,
}

impl LlmProviderKind {
    pub fn default_base_url(&self) -> &'static str {
        match self {
            LlmProviderKind::OpenAi | LlmProviderKind::GptCodex => "https://api.openai.com/v1",
            LlmProviderKind::OpenRouter => "https://openrouter.ai/api/v1",
            LlmProviderKind::GitHubCopilot => "https://api.individual.githubcopilot.com",
        }
    }
}

pub const ALL_TOOL_NAMES: [&str; 7] = ["read", "write", "edit", "bash", "grep", "find", "ls"];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    pub provider: LlmProviderKind,
    pub model_id: String,
    /// Override base URL (empty = use default for provider).
    pub base_url: String,
    pub system_prompt: String,
    /// Preloaded editable template for the built-in agent prompt.
    pub agent_system_prompt: String,
    pub tools_enabled: [bool; 7],
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            provider: LlmProviderKind::OpenAi,
            model_id: "gpt-4o-mini".to_string(),
            base_url: String::new(),
            system_prompt: String::new(),
            agent_system_prompt: String::new(),
            tools_enabled: [true; 7],
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
            if let Ok(s) = serde_json::from_slice::<AppSettings>(&bytes) {
                return s;
            }
        }
        Self::default()
    }

    pub fn save(&self) -> Result<(), String> {
        let path = Self::config_path();
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        fs::write(&path, json).map_err(|e| e.to_string())
    }

    pub fn effective_base_url(&self) -> String {
        let t = self.base_url.trim();
        if !t.is_empty() {
            t.trim_end_matches('/').to_string()
        } else {
            self.provider.default_base_url().to_string()
        }
    }
}
