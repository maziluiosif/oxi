//! Supporting settings value types embedded in [`super::AppSettings`].

use serde::{Deserialize, Serialize};

/// Shell hosted by the embedded terminal on Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WindowsTerminal {
    #[default]
    Cmd,
    PowerShell,
    Wsl,
}

#[cfg(windows)]
impl WindowsTerminal {
    pub const ALL: [Self; 3] = [Self::Cmd, Self::PowerShell, Self::Wsl];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Cmd => "Command Prompt",
            Self::PowerShell => "PowerShell",
            Self::Wsl => "WSL",
        }
    }
}

/// How oxi reaches an MCP server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum McpTransport {
    /// Spawn `command` and speak newline-delimited JSON-RPC over its stdin/stdout.
    #[default]
    Stdio,
    /// POST JSON-RPC to `url` (MCP "Streamable HTTP"; responses may be JSON or SSE).
    Http,
}

/// One MCP server entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Short id used in tool names (e.g. `filesystem`).
    pub name: String,
    #[serde(default)]
    pub transport: McpTransport,
    /// Executable to spawn (e.g. `npx`) for [`McpTransport::Stdio`].
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Endpoint for [`McpTransport::Http`], e.g. `https://example.com/mcp`.
    #[serde(default)]
    pub url: String,
    #[serde(default = "default_mcp_enabled")]
    pub enabled: bool,
    /// Per-call timeout in seconds for `tools/call`. `None` = [`DEFAULT_MCP_TIMEOUT_SECS`].
    #[serde(default)]
    pub timeout_secs: Option<u32>,
    /// Bearer token sent to HTTP servers. Lives in the OS keychain, never in `settings.json`.
    #[serde(default, skip_serializing)]
    pub bearer_token: String,
    /// Extra environment for stdio servers, one `KEY=VALUE` per line. Often holds API keys,
    /// so it lives in the OS keychain too.
    #[serde(default, skip_serializing)]
    pub env: String,
}

pub const DEFAULT_MCP_TIMEOUT_SECS: u32 = 120;

fn default_mcp_enabled() -> bool {
    true
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            transport: McpTransport::Stdio,
            command: String::new(),
            args: Vec::new(),
            url: String::new(),
            enabled: true,
            timeout_secs: None,
            bearer_token: String::new(),
            env: String::new(),
        }
    }
}

impl McpServerConfig {
    /// Enabled and filled in enough to attempt a connection.
    pub fn is_usable(&self) -> bool {
        self.enabled
            && !self.name.trim().is_empty()
            && match self.transport {
                McpTransport::Stdio => !self.command.trim().is_empty(),
                McpTransport::Http => !self.url.trim().is_empty(),
            }
    }

    pub fn call_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(
            self.timeout_secs
                .filter(|s| *s > 0)
                .unwrap_or(DEFAULT_MCP_TIMEOUT_SECS) as u64,
        )
    }

    /// `KEY=VALUE` lines from [`Self::env`]; blank lines and `#` comments are skipped.
    pub fn env_pairs(&self) -> Vec<(String, String)> {
        self.env
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .filter_map(|l| {
                let (k, v) = l.split_once('=')?;
                let k = k.trim();
                (!k.is_empty()).then(|| (k.to_string(), v.trim().to_string()))
            })
            .collect()
    }
}

/// Persisted Local HF runtime parameters (port / context / GPU offload).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LocalHfSettings {
    #[serde(default = "default_local_hf_port")]
    pub runtime_port: u16,
    #[serde(default = "default_local_hf_context")]
    pub context_size: usize,
    #[serde(default = "default_local_hf_gpu_layers")]
    pub gpu_layers: i32,
}

pub(super) fn default_local_hf_port() -> u16 {
    18080
}

pub(super) fn default_local_hf_context() -> usize {
    32768
}

fn default_local_hf_gpu_layers() -> i32 {
    999
}

impl Default for LocalHfSettings {
    fn default() -> Self {
        Self {
            runtime_port: default_local_hf_port(),
            context_size: default_local_hf_context(),
            gpu_layers: default_local_hf_gpu_layers(),
        }
    }
}

/// Settings for local speech-to-text dictation. The whisper model itself is loaded lazily
/// by [`crate::voice_engine::VoiceManager`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DictationSettings {
    /// Master on/off switch.
    #[serde(default)]
    pub enabled: bool,
    /// Catalog id of the selected downloaded model.
    #[serde(default)]
    pub model_id: Option<String>,
    /// Keep the whisper model resident after transcription.
    #[serde(default)]
    pub keep_loaded: bool,
    /// Whisper language hint, or `"auto"` for detection.
    #[serde(default = "default_dictation_language")]
    pub language: String,
}

fn default_dictation_language() -> String {
    "auto".to_string()
}

impl Default for DictationSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            model_id: None,
            keep_loaded: false,
            language: default_dictation_language(),
        }
    }
}

/// One persisted sidebar workspace and its folded state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceEntry {
    pub root_path: String,
    #[serde(default)]
    pub folded: bool,
    /// Session files pinned to the top of this workspace's chat list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pinned: Vec<String>,
    /// Sidebar date groups ("today", "older", …) folded in this workspace's chat list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub folded_groups: Vec<String>,
}
