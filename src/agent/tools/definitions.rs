//! OpenAI-style tool definition JSON for enabled tools.

use serde_json::Value;

use crate::settings::ALL_TOOL_NAMES;

pub fn tool_definitions_json(enabled: &[bool], bash_timeout_cap_secs: u32) -> Vec<Value> {
    let mut out = Vec::new();
    for (i, name) in ALL_TOOL_NAMES.iter().enumerate() {
        if !enabled.get(i).copied().unwrap_or(false) {
            continue;
        }
        let def = match *name {
            "read" => serde_json::json!({
                "type": "function",
                "function": {
                    "name": "read",
                    "description": "Read a text file from the workspace. Output is line-numbered. For large files, first locate the region with grep, then read with offset/limit instead of the whole file.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "path": { "type": "string", "description": "File path relative to workspace or absolute under workspace" },
                            "offset": { "type": "integer", "description": "1-based start line (optional)" },
                            "limit": { "type": "integer", "description": "Max lines to read (optional)" }
                        },
                        "required": ["path"]
                    }
                }
            }),
            "write" => serde_json::json!({
                "type": "function",
                "function": {
                    "name": "write",
                    "description": "Write or overwrite a file (creates parent directories).",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "path": { "type": "string" },
                            "content": { "type": "string" }
                        },
                        "required": ["path", "content"]
                    }
                }
            }),
            "edit" => serde_json::json!({
                "type": "function",
                "function": {
                    "name": "edit",
                    "description": "Replace text in a file. Each oldText must match exactly once unless replaceAll is true; LF and CRLF are treated as equivalent so text copied from read works on Windows. Never include the line-number gutter from read output.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "path": { "type": "string" },
                            "edits": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "oldText": { "type": "string" },
                                        "newText": { "type": "string" },
                                        "replaceAll": { "type": "boolean", "description": "Replace every occurrence of oldText (default false: must match exactly once)" }
                                    },
                                    "required": ["oldText", "newText"]
                                }
                            }
                        },
                        "required": ["path", "edits"]
                    }
                }
            }),
            "delete" => serde_json::json!({
                "type": "function",
                "function": {
                    "name": "delete",
                    "description": "Delete a file or an empty directory inside the workspace. Directory deletion is non-recursive; delete children first so every change remains reversible.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "path": { "type": "string" }
                        },
                        "required": ["path"]
                    }
                }
            }),
            "move" => serde_json::json!({
                "type": "function",
                "function": {
                    "name": "move",
                    "description": "Move or rename one file or empty directory inside the workspace. The destination must not exist and its parent must exist.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "from": { "type": "string" },
                            "to": { "type": "string" }
                        },
                        "required": ["from", "to"]
                    }
                }
            }),
            "mkdir" => serde_json::json!({
                "type": "function",
                "function": {
                    "name": "mkdir",
                    "description": "Create one directory inside the workspace. Its parent directory must already exist.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "path": { "type": "string" }
                        },
                        "required": ["path"]
                    }
                }
            }),
            "bash" => serde_json::json!({
                "type": "function",
                "function": {
                    "name": "bash",
                    "description": "Run a non-mutating shell command in the workspace directory (for example builds, checks, and tests). Never use bash to modify workspace files; use write/edit/delete/move/mkdir. Output beyond 40k chars is truncated — pipe through head/tail.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "command": { "type": "string" },
                            "timeout": { "type": "number", "description": format!("Timeout in seconds (optional, default 15, max {bash_timeout_cap_secs})") }
                        },
                        "required": ["command"]
                    }
                }
            }),
            "grep" => serde_json::json!({
                "type": "function",
                "function": {
                    "name": "grep",
                    "description": "Search for a regex pattern in files under the workspace. Prefer this over bash grep/rg — results come back as path:line: text you can pass to read's offset.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "pattern": { "type": "string" },
                            "path": { "type": "string", "description": "File or directory to search (optional)" },
                            "limit": { "type": "integer" }
                        },
                        "required": ["pattern"]
                    }
                }
            }),
            "find" => serde_json::json!({
                "type": "function",
                "function": {
                    "name": "find",
                    "description": "Find files matching a glob pattern. Prefer over bash find.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "pattern": { "type": "string", "description": "Glob e.g. **/*.rs" },
                            "path": { "type": "string" },
                            "limit": { "type": "integer" }
                        },
                        "required": ["pattern"]
                    }
                }
            }),
            "ls" => serde_json::json!({
                "type": "function",
                "function": {
                    "name": "ls",
                    "description": "List directory entries. Prefer over bash ls.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "path": { "type": "string" },
                            "limit": { "type": "integer" }
                        }
                    }
                }
            }),
            "codebase_search" => serde_json::json!({
                "type": "function",
                "function": {
                    "name": "codebase_search",
                    "description": "Search the workspace for code related to a natural-language query. Ranks file paths and matching lines by keyword relevance. Prefer this for 'where is X implemented?' questions; use grep for exact regex.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "query": { "type": "string", "description": "Natural-language or keyword query" },
                            "path": { "type": "string", "description": "Optional subdirectory to search under" },
                            "limit": { "type": "integer", "description": "Max results (1-40, default 12)" }
                        },
                        "required": ["query"]
                    }
                }
            }),
            "git_status" => serde_json::json!({
                "type": "function",
                "function": {
                    "name": "git_status",
                    "description": "Show git status --short for the workspace (staged/unstaged/untracked).",
                    "parameters": {
                        "type": "object",
                        "properties": {}
                    }
                }
            }),
            "git_diff" => serde_json::json!({
                "type": "function",
                "function": {
                    "name": "git_diff",
                    "description": "Show a git diff. Defaults to unstaged changes; set staged=true for the index, or pass a commit/ref as revision.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "staged": { "type": "boolean", "description": "If true, show --cached (staged) diff" },
                            "path": { "type": "string", "description": "Optional path to limit the diff" },
                            "revision": { "type": "string", "description": "Optional commit/ref to diff against (e.g. HEAD~1)" }
                        }
                    }
                }
            }),
            "web_search" => serde_json::json!({
                "type": "function",
                "function": {
                    "name": "web_search",
                    "description": "Search the web (DuckDuckGo, or a configured SearXNG instance). Returns a list of titles, URLs, and snippets. Use web_fetch to read a result in full.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "query": { "type": "string", "description": "Search query" },
                            "count": { "type": "integer", "description": "Max results to return (1-20, default 8)" }
                        },
                        "required": ["query"]
                    }
                }
            }),
            "web_fetch" => serde_json::json!({
                "type": "function",
                "function": {
                    "name": "web_fetch",
                    "description": "Fetch a URL over HTTP(S) and return its content as readable text (HTML is stripped to plain text).",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "url": { "type": "string", "description": "Absolute http:// or https:// URL" },
                            "max_chars": { "type": "integer", "description": "Max characters to return (optional)" }
                        },
                        "required": ["url"]
                    }
                }
            }),
            _ => continue,
        };
        out.push(def);
    }
    out
}
