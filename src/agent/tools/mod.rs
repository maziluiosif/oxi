//! Built-in tools (read, write, edit, bash, grep, find, ls).

use std::path::Path;

use serde_json::Value;

use crate::settings::ALL_TOOL_NAMES;

/// Result returned by [`run_tool`] — carries both the text output and an optional unified diff
/// generated locally for `edit` and `write` so the UI can render a diff block.
pub struct ToolResult {
    pub output: String,
    pub is_error: bool,
    /// Unified diff produced for `edit` / `write`; `None` for all other tools.
    pub diff: Option<String>,
}

pub(crate) const MAX_TOOL_OUTPUT_CHARS: usize = 120_000;

mod definitions;
mod file_ops;
mod paths;
mod shell_search;

#[cfg(test)]
mod tests;

pub use definitions::tool_definitions_json;

/// Resolve an existing `user_path` under `cwd`; rejects paths that escape `cwd`.
///
/// Re-exported for the public `tools` API; internal modules call `paths::resolve_under_cwd` directly.
#[allow(unused_imports)]
pub use paths::resolve_under_cwd;

pub fn run_tool(cwd: &Path, name: &str, args: &Value, enabled: &[bool; 7]) -> ToolResult {
    let idx = ALL_TOOL_NAMES.iter().position(|n| *n == name);
    let Some(i) = idx else {
        return ToolResult {
            output: format!("Unknown tool: {name}"),
            is_error: true,
            diff: None,
        };
    };
    if !enabled[i] {
        return ToolResult {
            output: format!("Tool {name} is disabled in settings"),
            is_error: true,
            diff: None,
        };
    }
    match name {
        "write" => file_ops::tool_write(cwd, args),
        "edit" => file_ops::tool_edit(cwd, args),
        _ => {
            let result = match name {
                "read" => file_ops::tool_read(cwd, args),
                "bash" => shell_search::tool_bash(cwd, args),
                "grep" => shell_search::tool_grep(cwd, args),
                "find" => shell_search::tool_find(cwd, args),
                "ls" => shell_search::tool_ls(cwd, args),
                _ => Err(paths::err("unknown tool")),
            };
            match result {
                Ok(output) => ToolResult {
                    output,
                    is_error: false,
                    diff: None,
                },
                Err(output) => ToolResult {
                    output,
                    is_error: true,
                    diff: None,
                },
            }
        }
    }
}
