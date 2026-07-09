//! Built-in tools (read, write, edit, bash, grep, find, ls).

use std::path::Path;

use serde_json::Value;

use crate::settings::{ALL_TOOL_NAMES, WebSearchBackend};

/// Result returned by [`run_tool`] — carries both the text output and an optional unified diff
/// generated locally for `edit` and `write` so the UI can render a diff block.
pub struct ToolResult {
    pub output: String,
    pub is_error: bool,
    /// Unified diff produced for `edit` / `write`; `None` for all other tools.
    pub diff: Option<String>,
    /// When output was truncated, path to a temp file with the full original text.
    pub full_output_path: Option<String>,
}

pub(crate) const MAX_TOOL_OUTPUT_CHARS: usize = 40_000;

mod definitions;
mod file_ops;
mod paths;
mod shell_search;
mod web;

#[cfg(test)]
mod tests;

pub use definitions::tool_definitions_json;
pub(crate) use file_ops::floor_char_boundary;
pub use paths::resolve_under_cwd;

/// Runtime configuration passed to [`run_tool`]: which tools are enabled (one flag per entry in
/// [`ALL_TOOL_NAMES`]) plus any tool-specific settings such as the SearXNG endpoint.
#[derive(Clone, Debug)]
pub struct ToolEnv {
    pub enabled: Vec<bool>,
    pub web_search_url: String,
    /// Which zero-config search backend to prefer when `web_search_url` is empty (i.e. not
    /// routing through a user-configured SearXNG instance).
    pub web_search_backend: WebSearchBackend,
    /// Upper bound (seconds) for a single `bash` call; the tool's own `timeout` argument is
    /// clamped to this.
    pub bash_timeout_cap_secs: u32,
    /// Optional MCP manager for `mcp_*` tools.
    pub mcp: Option<crate::agent::mcp::McpManager>,
}

pub fn run_tool(cwd: &Path, name: &str, args: &Value, env: &ToolEnv) -> ToolResult {
    if crate::agent::mcp::McpManager::is_mcp_tool(name) {
        let Some(mcp) = env.mcp.as_ref() else {
            return ToolResult {
                output: format!("MCP tool `{name}` requested but no MCP manager is configured"),
                is_error: true,
                diff: None,
                full_output_path: None,
            };
        };
        return match mcp.call_tool(name, args) {
            Ok(output) => {
                let (output, full_output_path) = file_ops::maybe_spill_truncated(output);
                ToolResult {
                    output,
                    is_error: false,
                    diff: None,
                    full_output_path,
                }
            }
            Err(output) => ToolResult {
                output,
                is_error: true,
                diff: None,
                full_output_path: None,
            },
        };
    }
    let idx = ALL_TOOL_NAMES.iter().position(|n| *n == name);
    let Some(i) = idx else {
        return ToolResult {
            output: format!("Unknown tool: {name}"),
            is_error: true,
            diff: None,
            full_output_path: None,
        };
    };
    if !env.enabled.get(i).copied().unwrap_or(false) {
        return ToolResult {
            output: format!("Tool {name} is disabled in settings"),
            is_error: true,
            diff: None,
            full_output_path: None,
        };
    }
    match name {
        "write" => file_ops::tool_write(cwd, args),
        "edit" => file_ops::tool_edit(cwd, args),
        _ => {
            let result = match name {
                "read" => file_ops::tool_read(cwd, args),
                "bash" => shell_search::tool_bash(cwd, args, env.bash_timeout_cap_secs),
                "grep" => shell_search::tool_grep(cwd, args),
                "find" => shell_search::tool_find(cwd, args),
                "ls" => shell_search::tool_ls(cwd, args),
                "codebase_search" => shell_search::tool_codebase_search(cwd, args),
                "git_status" => shell_search::tool_git_status(cwd, args),
                "git_diff" => shell_search::tool_git_diff(cwd, args),
                "web_search" => {
                    web::tool_web_search(&env.web_search_url, env.web_search_backend, args)
                }
                "web_fetch" => web::tool_web_fetch(args),
                _ => Err(paths::err("unknown tool")),
            };
            match result {
                Ok(output) => {
                    let (output, full_output_path) = file_ops::maybe_spill_truncated(output);
                    ToolResult {
                        output,
                        is_error: false,
                        diff: None,
                        full_output_path,
                    }
                }
                Err(output) => {
                    let (output, full_output_path) = file_ops::maybe_spill_truncated(output);
                    ToolResult {
                        output,
                        is_error: true,
                        diff: None,
                        full_output_path,
                    }
                }
            }
        }
    }
}
