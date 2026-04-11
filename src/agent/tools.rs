//! Built-in tools (read, write, edit, bash, grep, find, ls).

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use glob::glob;
use regex::Regex;
use serde_json::Value;
use walkdir::{DirEntry, WalkDir};

use crate::settings::ALL_TOOL_NAMES;

const MAX_TOOL_OUTPUT_CHARS: usize = 120_000;
const READ_MAX_LINES: usize = 2000;
const GREP_MAX_MATCHES: usize = 100;
const FIND_MAX: usize = 1000;
const LS_MAX: usize = 500;
const BASH_MAX_SECONDS: f64 = 30.0;

fn err(s: impl Into<String>) -> String {
    s.into()
}

/// Resolve an existing `user_path` under `cwd`; rejects paths that escape `cwd`.
pub fn resolve_under_cwd(cwd: &Path, user_path: &str) -> Result<PathBuf, String> {
    let p = PathBuf::from(user_path);
    let abs = if p.is_absolute() { p } else { cwd.join(p) };
    let cwd_can = cwd.canonicalize().map_err(|e| e.to_string())?;
    let abs_can = abs.canonicalize().map_err(|e| e.to_string())?;
    if !abs_can.starts_with(&cwd_can) {
        return Err("Path escapes workspace root".to_string());
    }
    Ok(abs_can)
}

/// Resolve a path that may not exist yet, as long as its closest existing parent stays under `cwd`.
fn resolve_under_cwd_for_create(cwd: &Path, user_path: &str) -> Result<PathBuf, String> {
    let p = PathBuf::from(user_path);
    let abs = if p.is_absolute() { p } else { cwd.join(p) };
    let cwd_can = cwd.canonicalize().map_err(|e| e.to_string())?;

    let mut existing_parent = abs.as_path();
    while !existing_parent.exists() {
        existing_parent = existing_parent
            .parent()
            .ok_or_else(|| err("invalid path outside workspace"))?;
    }

    let parent_can = existing_parent.canonicalize().map_err(|e| e.to_string())?;
    if !parent_can.starts_with(&cwd_can) {
        return Err("Path escapes workspace root".to_string());
    }
    Ok(abs)
}

pub fn tool_definitions_json(enabled: &[bool; 7]) -> Vec<Value> {
    let mut out = Vec::new();
    for (i, name) in ALL_TOOL_NAMES.iter().enumerate() {
        if !enabled[i] {
            continue;
        }
        let def = match *name {
            "read" => serde_json::json!({
                "type": "function",
                "function": {
                    "name": "read",
                    "description": "Read a text file from the workspace. Optionally limit by line range.",
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
                    "description": "Replace text in a file. Each oldText must match exactly once in the file.",
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
                                        "newText": { "type": "string" }
                                    },
                                    "required": ["oldText", "newText"]
                                }
                            }
                        },
                        "required": ["path", "edits"]
                    }
                }
            }),
            "bash" => serde_json::json!({
                "type": "function",
                "function": {
                    "name": "bash",
                    "description": "Run a shell command in the workspace directory.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "command": { "type": "string" },
                            "timeout": { "type": "number", "description": "Timeout in seconds (optional)" }
                        },
                        "required": ["command"]
                    }
                }
            }),
            "grep" => serde_json::json!({
                "type": "function",
                "function": {
                    "name": "grep",
                    "description": "Search for a regex pattern in files under the workspace.",
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
                    "description": "Find files matching a glob pattern.",
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
                    "description": "List directory entries.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "path": { "type": "string" },
                            "limit": { "type": "integer" }
                        }
                    }
                }
            }),
            _ => continue,
        };
        out.push(def);
    }
    out
}

pub fn run_tool(
    cwd: &Path,
    name: &str,
    args: &Value,
    enabled: &[bool; 7],
) -> Result<String, String> {
    let idx = ALL_TOOL_NAMES.iter().position(|n| *n == name);
    let Some(i) = idx else {
        return Err(err(format!("Unknown tool: {name}")));
    };
    if !enabled[i] {
        return Err(err(format!("Tool {name} is disabled in settings")));
    }
    match name {
        "read" => tool_read(cwd, args),
        "write" => tool_write(cwd, args),
        "edit" => tool_edit(cwd, args),
        "bash" => tool_bash(cwd, args),
        "grep" => tool_grep(cwd, args),
        "find" => tool_find(cwd, args),
        "ls" => tool_ls(cwd, args),
        _ => Err(err("unknown tool")),
    }
}

fn should_skip_search_entry(entry: &DirEntry) -> bool {
    let name = entry.file_name().to_string_lossy();
    matches!(name.as_ref(), ".git" | "target" | "node_modules")
}

fn is_reasonably_small_text_file(path: &Path) -> bool {
    const MAX_SEARCH_FILE_BYTES: u64 = 2 * 1024 * 1024;
    match fs::metadata(path) {
        Ok(meta) if meta.len() <= MAX_SEARCH_FILE_BYTES => {}
        _ => return false,
    }
    fs::read(path)
        .map(|bytes| !bytes.contains(&0))
        .unwrap_or(false)
}

fn truncate_out(s: String) -> String {
    if s.len() <= MAX_TOOL_OUTPUT_CHARS {
        s
    } else {
        format!(
            "{}\n\n[output truncated to {} chars]",
            &s[..MAX_TOOL_OUTPUT_CHARS],
            MAX_TOOL_OUTPUT_CHARS
        )
    }
}

fn tool_read(cwd: &Path, args: &Value) -> Result<String, String> {
    let path = args
        .get("path")
        .or_else(|| args.get("file_path"))
        .and_then(|x| x.as_str())
        .ok_or_else(|| err("missing path"))?;
    let abs = resolve_under_cwd(cwd, path)?;
    let mut f = File::open(&abs).map_err(|e| e.to_string())?;
    let mut buf = String::new();
    f.read_to_string(&mut buf).map_err(|e| e.to_string())?;
    let lines: Vec<&str> = buf.lines().collect();
    let offset = args
        .get("offset")
        .and_then(|x| x.as_u64())
        .unwrap_or(1)
        .max(1) as usize;
    let limit = args
        .get("limit")
        .and_then(|x| x.as_u64())
        .map(|n| n as usize)
        .unwrap_or(READ_MAX_LINES)
        .min(READ_MAX_LINES);
    let start = offset.saturating_sub(1);
    let end = (start + limit).min(lines.len());
    let slice = if start < lines.len() {
        lines[start..end].join("\n")
    } else {
        String::new()
    };
    Ok(truncate_out(format!(
        "File: {}\nLines {}-{}\n---\n{}",
        abs.display(),
        offset,
        offset.saturating_add(limit).saturating_sub(1),
        slice
    )))
}

fn tool_write(cwd: &Path, args: &Value) -> Result<String, String> {
    let path = args
        .get("path")
        .and_then(|x| x.as_str())
        .ok_or_else(|| err("missing path"))?;
    let content = args
        .get("content")
        .and_then(|x| x.as_str())
        .ok_or_else(|| err("missing content"))?;
    let abs = resolve_under_cwd_for_create(cwd, path)?;
    if let Some(parent) = abs.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut f = File::create(&abs).map_err(|e| e.to_string())?;
    f.write_all(content.as_bytes()).map_err(|e| e.to_string())?;
    Ok(format!(
        "Wrote {} bytes to {}",
        content.len(),
        abs.display()
    ))
}

fn tool_edit(cwd: &Path, args: &Value) -> Result<String, String> {
    let path = args
        .get("path")
        .and_then(|x| x.as_str())
        .ok_or_else(|| err("missing path"))?;
    let abs = resolve_under_cwd(cwd, path)?;
    let mut content = fs::read_to_string(&abs).map_err(|e| e.to_string())?;
    let mut edits = Vec::new();
    if let Some(arr) = args.get("edits").and_then(|x| x.as_array()) {
        for e in arr {
            let old = e.get("oldText").and_then(|x| x.as_str());
            let new = e.get("newText").and_then(|x| x.as_str());
            if let (Some(o), Some(n)) = (old, new) {
                edits.push((o.to_string(), n.to_string()));
            }
        }
    }
    if edits.is_empty() {
        let old = args.get("oldText").and_then(|x| x.as_str());
        let new = args.get("newText").and_then(|x| x.as_str());
        if let (Some(o), Some(n)) = (old, new) {
            edits.push((o.to_string(), n.to_string()));
        }
    }
    if edits.is_empty() {
        return Err(err("no edits"));
    }
    for (old, _) in &edits {
        let count = content.matches(old.as_str()).count();
        if count != 1 {
            return Err(err(format!(
                "oldText must match exactly once in file, found {count} occurrences"
            )));
        }
    }
    for (old, new) in edits {
        if let Some(idx) = content.find(old.as_str()) {
            content.replace_range(idx..idx + old.len(), &new);
        }
    }
    fs::write(&abs, &content).map_err(|e| e.to_string())?;
    Ok(format!("Edited {}", abs.display()))
}

fn tool_bash(cwd: &Path, args: &Value) -> Result<String, String> {
    let cmd = args
        .get("command")
        .and_then(|x| x.as_str())
        .ok_or_else(|| err("missing command"))?;
    let lowered = cmd.to_ascii_lowercase();
    for denied in ["rm -rf /", "sudo ", "mkfs", "dd if="] {
        if lowered.contains(denied) {
            return Err(format!("Refusing risky bash command containing: {denied}"));
        }
    }
    let timeout_s = args
        .get("timeout")
        .and_then(|x| x.as_f64().or_else(|| x.as_u64().map(|u| u as f64)))
        .unwrap_or(15.0)
        .clamp(0.1, BASH_MAX_SECONDS);
    let start = Instant::now();
    let mut child = if cfg!(unix) {
        Command::new("/bin/sh")
            .arg("-c")
            .arg(cmd)
            .current_dir(cwd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?
    } else {
        Command::new("cmd")
            .args(["/C", cmd])
            .current_dir(cwd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?
    };
    let timeout = Some(Duration::from_secs_f64(timeout_s));
    let status = loop {
        if let Some(t) = timeout {
            if start.elapsed() > t {
                let _ = child.kill();
                return Ok(truncate_out(format!(
                    "[timeout after {}s]\n",
                    t.as_secs_f64()
                )));
            }
        }
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(e.to_string()),
        }
    };
    let mut out = String::new();
    if let Some(mut o) = child.stdout {
        let _ = o.read_to_string(&mut out);
    }
    if let Some(mut e) = child.stderr {
        let mut err = String::new();
        let _ = e.read_to_string(&mut err);
        if !err.is_empty() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&err);
        }
    }
    Ok(truncate_out(format!(
        "exit code: {}\n{}",
        status.code().unwrap_or(-1),
        out
    )))
}

fn tool_grep(cwd: &Path, args: &Value) -> Result<String, String> {
    let pattern = args
        .get("pattern")
        .and_then(|x| x.as_str())
        .ok_or_else(|| err("missing pattern"))?;
    let literal = args
        .get("literal")
        .and_then(|x| x.as_bool())
        .unwrap_or(false);
    let re = if literal {
        Regex::new(&regex::escape(pattern)).map_err(|e| e.to_string())?
    } else {
        Regex::new(pattern).map_err(|e| e.to_string())?
    };
    let search_root = args
        .get("path")
        .and_then(|x| x.as_str())
        .map(|p| resolve_under_cwd(cwd, p))
        .transpose()?
        .unwrap_or_else(|| cwd.to_path_buf());
    let limit = args
        .get("limit")
        .and_then(|x| x.as_u64())
        .unwrap_or(GREP_MAX_MATCHES as u64) as usize;
    let mut matches = 0usize;
    let mut out = String::new();
    if search_root.is_file() {
        let txt = fs::read_to_string(&search_root).map_err(|e| e.to_string())?;
        for (i, line) in txt.lines().enumerate() {
            if re.is_match(line) {
                matches += 1;
                let rel = search_root.strip_prefix(cwd).unwrap_or(&search_root);
                out.push_str(&format!("{}:{}:{}\n", rel.display(), i + 1, line));
                if matches >= limit {
                    break;
                }
            }
        }
    } else {
        'outer: for entry in WalkDir::new(&search_root)
            .into_iter()
            .filter_entry(|e| !should_skip_search_entry(e))
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let p = entry.path();
            if !is_reasonably_small_text_file(p) {
                continue;
            }
            if let Ok(txt) = fs::read_to_string(p) {
                for (i, line) in txt.lines().enumerate() {
                    if re.is_match(line) {
                        matches += 1;
                        let rel = p.strip_prefix(cwd).unwrap_or(p);
                        out.push_str(&format!("{}:{}:{}\n", rel.display(), i + 1, line));
                        if matches >= limit {
                            break 'outer;
                        }
                    }
                }
            }
        }
    }
    if matches >= limit {
        out.push_str(&format!("\n[match limit {limit} reached]\n"));
    }
    Ok(truncate_out(if out.is_empty() {
        "No matches".to_string()
    } else {
        out
    }))
}

fn tool_find(cwd: &Path, args: &Value) -> Result<String, String> {
    let pattern = args
        .get("pattern")
        .and_then(|x| x.as_str())
        .ok_or_else(|| err("missing pattern"))?;
    let base = args
        .get("path")
        .and_then(|x| x.as_str())
        .map(|p| resolve_under_cwd(cwd, p))
        .transpose()?
        .unwrap_or_else(|| cwd.to_path_buf());
    let limit = args
        .get("limit")
        .and_then(|x| x.as_u64())
        .unwrap_or(FIND_MAX as u64) as usize;
    let glob_pat = format!("{}/{}", base.display(), pattern);
    let mut out = String::new();
    let mut n = 0usize;
    for entry in glob(&glob_pat).map_err(|e| e.to_string())? {
        let path = entry.map_err(|e| e.to_string())?;
        if path.components().any(|c| {
            matches!(
                c.as_os_str().to_string_lossy().as_ref(),
                ".git" | "target" | "node_modules"
            )
        }) {
            continue;
        }
        let rel = path.strip_prefix(cwd).unwrap_or(&path);
        out.push_str(&format!("{}\n", rel.display()));
        n += 1;
        if n >= limit {
            out.push_str(&format!("\n[limit {limit} reached]\n"));
            break;
        }
    }
    Ok(truncate_out(out))
}

fn tool_ls(cwd: &Path, args: &Value) -> Result<String, String> {
    let base = args
        .get("path")
        .and_then(|x| x.as_str())
        .map(|p| resolve_under_cwd(cwd, p))
        .transpose()?
        .unwrap_or_else(|| cwd.to_path_buf());
    let limit = args
        .get("limit")
        .and_then(|x| x.as_u64())
        .unwrap_or(LS_MAX as u64) as usize;
    let rd = fs::read_dir(&base).map_err(|e| e.to_string())?;
    let mut names: Vec<String> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let total = names.len();
    names.truncate(limit);
    let mut out = names.join("\n");
    if total > limit {
        out.push_str(&format!("\n[limit {limit} reached]\n"));
    }
    if out.is_empty() {
        out = "[empty directory]".to_string();
    }
    Ok(truncate_out(out))
}
