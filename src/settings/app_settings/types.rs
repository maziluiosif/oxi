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

/// One MCP server entry (stdio transport).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Short id used in tool names (e.g. `filesystem`).
    pub name: String,
    /// Executable to spawn (e.g. `npx`).
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "default_mcp_enabled")]
    pub enabled: bool,
}

fn default_mcp_enabled() -> bool {
    true
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            command: String::new(),
            args: Vec::new(),
            enabled: true,
        }
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
}
