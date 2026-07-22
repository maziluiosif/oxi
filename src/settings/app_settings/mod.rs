//! The top-level [`AppSettings`] struct: defaults, on-disk load/save, migration from
//! older settings shapes, and per-provider config accessors.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use super::provider::{LlmProviderKind, ProviderConfig, UiDensity, WebSearchBackend};

pub const ALL_TOOL_NAMES: [&str; 15] = [
    "read",
    "write",
    "edit",
    "bash",
    "grep",
    "find",
    "ls",
    "codebase_search",
    "git_status",
    "git_diff",
    "web_search",
    "web_fetch",
    // Keep new tools appended so persisted positional enable flags retain their meaning.
    "delete",
    "move",
    "mkdir",
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
    /// Include root-level `AGENTS.md` project instructions in the agent system prompt when present.
    #[serde(default = "default_include_agents_md")]
    pub include_agents_md: bool,
    /// Include `.oxi/rules/` and `.cursor/rules/` markdown files in the agent system prompt.
    #[serde(default = "default_include_oxi_rules")]
    pub include_oxi_rules: bool,
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
    /// Require explicit user approval before each built-in filesystem-changing tool.
    #[serde(default = "default_require_approval")]
    pub require_write_edit_approval: bool,
    /// Require explicit user approval before each `bash` tool call.
    #[serde(default = "default_require_approval")]
    pub require_bash_approval: bool,
    /// Legacy single approval switch. Migrated in [`AppSettings::normalize`] and no longer saved.
    #[serde(default, skip_serializing)]
    pub require_approval: Option<bool>,
    /// Persisted width of the main app/sidebar split.
    #[serde(default = "default_sidebar_width")]
    pub sidebar_width: f32,
    /// Persisted height of the bottom terminal panel.
    #[serde(default = "default_terminal_height")]
    pub terminal_height: f32,
    /// Whether the bottom terminal panel is shown.
    #[serde(default)]
    pub terminal_open: bool,
    /// Shell used by the embedded terminal on Windows.
    #[serde(default)]
    pub windows_terminal: WindowsTerminal,
    /// Whether the right source-control (git) panel is shown.
    #[serde(default)]
    pub git_open: bool,
    /// Persisted width of the right git panel.
    #[serde(default = "default_git_width")]
    pub git_width: f32,
    /// Max width of the chat message/composer column. Wider than the sidebar/git panel
    /// split above, this lets the transcript use more of a large screen (or the space
    /// freed by hiding side panels) instead of staying pinned to a fixed column.
    #[serde(default = "default_chat_column_max_width")]
    pub chat_column_max_width: f32,
    /// Active color theme id (see [`crate::theme`]: `dark`, `light`, `midnight`, or
    /// `custom:<name>`). Falls back to the default theme if unknown.
    #[serde(default = "default_theme_id")]
    pub theme_id: String,
    /// Overall text/UI density (zoom). Defaults to [`UiDensity::Normal`].
    #[serde(default)]
    pub ui_density: UiDensity,
    /// Interface font id — `"default"` is bundled; `system:<family>` selects an installed font.
    #[serde(default = "default_font_id")]
    pub ui_font: String,
    /// Code (monospace) font id — see [`crate::theme::mono_font_options`].
    #[serde(default = "default_font_id")]
    pub mono_font: String,
    /// Maximum number of agent tool rounds per run. `0` means unlimited. Default unlimited.
    #[serde(default = "default_max_tool_rounds")]
    pub max_tool_rounds: u32,
    /// Upper bound (seconds) for a single `bash` tool call. The model's own `timeout`
    /// argument is clamped to this. Default 300.
    #[serde(default = "default_bash_timeout_cap_secs")]
    pub bash_timeout_cap_secs: u32,
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
    /// Last active workspace root path, used to reopen the conversation the user last had open.
    #[serde(default)]
    pub last_active_workspace_root_path: Option<String>,
    /// Last active chat file inside the active workspace, if the chat has been saved to disk.
    #[serde(default)]
    pub last_active_session_file: Option<String>,
    /// Local voice dictation (see [`crate::voice_engine`]).
    #[serde(default)]
    pub dictation: DictationSettings,
    /// Local HF (`llama-server`) runtime tuning: port, context size, GPU layers.
    #[serde(default)]
    pub local_hf: LocalHfSettings,
    /// Author identity used for commits created by the native Git engine. When empty, libgit2
    /// falls back to the repository/global Git configuration.
    #[serde(default)]
    pub git_author_name: String,
    #[serde(default)]
    pub git_author_email: String,
    /// GitHub account name used as the HTTPS username. The PAT remains the actual credential.
    #[serde(default)]
    pub github_username: String,
    /// GitHub token is held in memory for editing but never serialized to settings.json.
    /// [`Self::load`] hydrates it from the OS keychain and [`Self::save`] writes it back.
    #[serde(default, skip_serializing)]
    pub github_token: String,
    /// MCP servers to spawn (stdio). Tools appear as `mcp_<server>_<tool>`.
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,
}

fn default_require_approval() -> bool {
    true
}

fn default_max_tool_rounds() -> u32 {
    0
}

fn default_bash_timeout_cap_secs() -> u32 {
    300
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

fn default_chat_column_max_width() -> f32 {
    crate::theme::CHAT_COLUMN_MAX_DEFAULT
}

fn default_include_agents_md() -> bool {
    true
}

fn default_include_oxi_rules() -> bool {
    true
}

/// Clamp bounds for the bottom terminal panel height.
pub const TERMINAL_H_MIN: f32 = 96.0;
pub const TERMINAL_H_MAX: f32 = 900.0;

fn default_theme_id() -> String {
    crate::theme::DEFAULT_THEME_ID.to_string()
}

fn default_font_id() -> String {
    "default".to_string()
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
            include_agents_md: default_include_agents_md(),
            include_oxi_rules: default_include_oxi_rules(),
            tools_enabled: default_tools_enabled(),
            web_search_backend: WebSearchBackend::default(),
            searxng_url: default_searxng_url(),
            require_write_edit_approval: default_require_approval(),
            require_bash_approval: default_require_approval(),
            require_approval: None,
            sidebar_width: default_sidebar_width(),
            terminal_height: default_terminal_height(),
            terminal_open: false,
            windows_terminal: WindowsTerminal::default(),
            git_open: false,
            git_width: default_git_width(),
            chat_column_max_width: default_chat_column_max_width(),
            theme_id: default_theme_id(),
            ui_density: UiDensity::Normal,
            ui_font: default_font_id(),
            mono_font: default_font_id(),
            max_tool_rounds: default_max_tool_rounds(),
            bash_timeout_cap_secs: default_bash_timeout_cap_secs(),
            context_window_default: default_context_window(),
            commit_msg_provider: None,
            commit_msg_model_id: String::new(),
            commit_msg_system_prompt: default_commit_msg_system_prompt(),
            workspaces: Vec::new(),
            last_active_workspace_root_path: None,
            last_active_session_file: None,
            dictation: DictationSettings::default(),
            local_hf: LocalHfSettings::default(),
            git_author_name: String::new(),
            git_author_email: String::new(),
            github_username: String::new(),
            github_token: String::new(),
            mcp_servers: Vec::new(),
        }
    }
}

mod migration;
mod persistence;
mod query;
mod types;

pub use types::{
    DictationSettings, LocalHfSettings, McpServerConfig, WindowsTerminal, WorkspaceEntry,
};

#[cfg(test)]
mod tests;
