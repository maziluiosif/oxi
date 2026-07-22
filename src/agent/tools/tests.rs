use super::diff::make_unified_diff;
use super::file_ops::{floor_char_boundary, truncate_out};
use super::shell_search::validate_bash_command;
use super::{
    MAX_TOOL_OUTPUT_CHARS, ToolEnv, paths::resolve_under_cwd, run_tool, tool_definitions_json,
};
use crate::settings::{ALL_TOOL_NAMES, WebSearchBackend};
use serde_json::json;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn temp_workspace(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_nanos();
    let path = std::env::temp_dir().join(format!("oxi-tools-{name}-{nanos}"));
    fs::create_dir_all(&path).unwrap();
    path
}

fn all_enabled() -> ToolEnv {
    ToolEnv {
        enabled: vec![true; ALL_TOOL_NAMES.len()],
        web_search_url: "https://search.invalid".to_string(),
        web_search_backend: WebSearchBackend::default(),
        bash_timeout_cap_secs: 300,
        mcp: None,
        undo_journal: None,
    }
}

mod definitions_and_diff;
mod file_tools;
mod path_and_shell_safety;
mod routing_and_undo;
mod search_tools;
