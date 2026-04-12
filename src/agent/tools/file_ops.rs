//! read / write / edit and unified diff helpers.

use std::fs::{self, File};
use std::io::Read;
use std::path::Path;

use serde_json::Value;

use super::paths::{err, resolve_under_cwd, resolve_under_cwd_for_create};
use super::ToolResult;
use super::MAX_TOOL_OUTPUT_CHARS;

const READ_MAX_LINES: usize = 2000;

pub(crate) fn truncate_out(s: String) -> String {
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
pub(crate) fn make_unified_diff(path: &str, before: &str, after: &str) -> String {
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
    let mut after_nums = Vec::with_capacity(ops.len());
    let (mut bl, mut al) = (1usize, 1usize);
    for (k, _) in &ops {
        before_nums.push(bl);
        after_nums.push(al);
        match k {
            ' ' => {
                bl += 1;
                al += 1;
            }
            '-' => {
                bl += 1;
            }
            '+' => {
                al += 1;
            }
            _ => {}
        }
    }

    for (hs, he) in hunks {
        // hunk header counts
        let hunk_before_start = before_nums[hs];
        let hunk_after_start = after_nums[hs];
        let hunk_before_count = ops[hs..he].iter().filter(|(k, _)| *k != '+').count();
        let hunk_after_count = ops[hs..he].iter().filter(|(k, _)| *k != '-').count();
        out.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            hunk_before_start, hunk_before_count, hunk_after_start, hunk_after_count,
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

pub(crate) fn tool_read(cwd: &Path, args: &Value) -> Result<String, String> {
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

pub(crate) fn tool_write(cwd: &Path, args: &Value) -> ToolResult {
    let path = match args.get("path").and_then(|x| x.as_str()) {
        Some(p) => p,
        None => {
            return ToolResult {
                output: err("missing path"),
                is_error: true,
                diff: None,
            }
        }
    };
    let content = match args.get("content").and_then(|x| x.as_str()) {
        Some(c) => c,
        None => {
            return ToolResult {
                output: err("missing content"),
                is_error: true,
                diff: None,
            }
        }
    };
    let abs = match resolve_under_cwd_for_create(cwd, path) {
        Ok(p) => p,
        Err(e) => {
            return ToolResult {
                output: e,
                is_error: true,
                diff: None,
            }
        }
    };
    // Read existing file for diff (empty string if new file).
    let before = fs::read_to_string(&abs).unwrap_or_default();
    if let Some(parent) = abs.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            return ToolResult {
                output: e.to_string(),
                is_error: true,
                diff: None,
            };
        }
    }
    if let Err(e) = fs::write(&abs, content.as_bytes()) {
        return ToolResult {
            output: e.to_string(),
            is_error: true,
            diff: None,
        };
    }
    let diff = make_unified_diff(path, &before, content);
    ToolResult {
        output: format!("Wrote {} bytes to {}", content.len(), abs.display()),
        is_error: false,
        diff: if diff.is_empty() { None } else { Some(diff) },
    }
}

pub(crate) fn tool_edit(cwd: &Path, args: &Value) -> ToolResult {
    let path = match args.get("path").and_then(|x| x.as_str()) {
        Some(p) => p,
        None => {
            return ToolResult {
                output: err("missing path"),
                is_error: true,
                diff: None,
            }
        }
    };
    let abs = match resolve_under_cwd(cwd, path) {
        Ok(p) => p,
        Err(e) => {
            return ToolResult {
                output: e,
                is_error: true,
                diff: None,
            }
        }
    };
    let before = match fs::read_to_string(&abs) {
        Ok(s) => s,
        Err(e) => {
            return ToolResult {
                output: e.to_string(),
                is_error: true,
                diff: None,
            }
        }
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
        return ToolResult {
            output: err("no edits"),
            is_error: true,
            diff: None,
        };
    }
    for (old, _) in &edits {
        let count = content.matches(old.as_str()).count();
        if count != 1 {
            return ToolResult {
                output: err(format!(
                    "oldText must match exactly once in file, found {count} occurrences"
                )),
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
        return ToolResult {
            output: e.to_string(),
            is_error: true,
            diff: None,
        };
    }
    let diff = make_unified_diff(path, &before, &content);
    ToolResult {
        output: format!("Edited {}", abs.display()),
        is_error: false,
        diff: if diff.is_empty() { None } else { Some(diff) },
    }
}
