//! read / write / edit and unified diff helpers.

use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::Value;

use super::MAX_TOOL_OUTPUT_CHARS;
use super::paths::{err, resolve_under_cwd, resolve_under_cwd_for_create};
use super::{ToolResult, TurnUndoJournal};

const READ_MAX_LINES: usize = 2000;

pub(crate) fn floor_char_boundary(s: &str, max: usize) -> usize {
    let mut cut = max.min(s.len());
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    cut
}

pub(crate) fn truncate_out(s: String) -> String {
    if s.len() <= MAX_TOOL_OUTPUT_CHARS {
        s
    } else {
        let cut = floor_char_boundary(&s, MAX_TOOL_OUTPUT_CHARS);
        format!(
            "{}\n\n[output truncated to {} chars]",
            &s[..cut],
            MAX_TOOL_OUTPUT_CHARS
        )
    }
}

/// If `s` exceeds the tool-output cap, write the full text to a temp file and return
/// `(truncated, Some(path))`. Otherwise return `(s, None)`.
pub(crate) fn maybe_spill_truncated(s: String) -> (String, Option<String>) {
    if s.len() <= MAX_TOOL_OUTPUT_CHARS {
        return (s, None);
    }
    let path = spill_full_output(&s);
    (truncate_out(s), path)
}

pub(crate) fn cleanup_stale_spill_files() {
    let dir = std::env::temp_dir().join("oxi-tool-output");
    let Ok(entries) = fs::read_dir(&dir) else {
        return;
    };
    let cutoff =
        std::time::SystemTime::now().checked_sub(std::time::Duration::from_secs(7 * 24 * 60 * 60));
    for entry in entries.flatten() {
        let path = entry.path();
        let stale = cutoff.is_some_and(|cutoff| {
            entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .is_some_and(|modified| modified < cutoff)
        });
        if stale {
            let _ = fs::remove_file(path);
        }
    }
}

fn spill_full_output(s: &str) -> Option<String> {
    let dir = std::env::temp_dir().join("oxi-tool-output");
    fs::create_dir_all(&dir).ok()?;
    let path = unique_temp_path(&dir, "tool-output", "txt");
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path).ok()?;
    file.write_all(s.as_bytes()).ok()?;
    file.sync_all().ok()?;
    Some(path.to_string_lossy().into_owned())
}

fn unique_temp_path(dir: &Path, prefix: &str, extension: &str) -> PathBuf {
    use rand::RngExt;
    let random: u64 = rand::rng().random();
    dir.join(format!(
        "{prefix}-{}-{random:016x}.{extension}",
        std::process::id()
    ))
}

/// Replace a file without exposing a partially-written destination. The temporary file lives in
/// the destination directory, so the final rename is atomic on supported local filesystems.
fn atomic_write(path: &Path, content: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| err("destination has no parent directory"))?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let tmp = unique_temp_path(parent, ".oxi-write", "tmp");
    let existing_permissions = fs::metadata(path).ok().map(|m| m.permissions());
    let result = (|| -> Result<(), String> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp).map_err(|e| e.to_string())?;
        file.write_all(content).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        if let Some(permissions) = existing_permissions {
            fs::set_permissions(&tmp, permissions).map_err(|e| e.to_string())?;
        }
        #[cfg(windows)]
        if path.exists() {
            // Windows rename does not replace an existing destination. Move the original aside
            // and roll back if installing the synced replacement fails, so an error never leaves
            // the destination missing.
            let backup = unique_temp_path(parent, ".oxi-backup", "tmp");
            fs::rename(path, &backup).map_err(|e| e.to_string())?;
            if let Err(e) = fs::rename(&tmp, path) {
                let _ = fs::rename(&backup, path);
                return Err(e.to_string());
            }
            let _ = fs::remove_file(backup);
        } else {
            fs::rename(&tmp, path).map_err(|e| e.to_string())?;
        }
        #[cfg(not(windows))]
        fs::rename(&tmp, path).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        if let Ok(dir) = File::open(parent) {
            let _ = dir.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Produce a minimal-ish unified diff between `before` and `after` text.
/// Context lines: 3 (standard). Output is capped at 8 000 chars so it stays
/// lightweight in the UI. Small files use an LCS diff; large files use a
/// bounded prefix/suffix diff so a big edit cannot allocate an O(m*n) matrix.
pub(crate) fn make_unified_diff(path: &str, before: &str, after: &str) -> String {
    if before == after {
        return String::new();
    }

    let before_lines: Vec<&str> = before.lines().collect();
    let after_lines: Vec<&str> = after.lines().collect();
    let m = before_lines.len();
    let n = after_lines.len();

    const MAX_LCS_CELLS: usize = 1_000_000;
    let ops = if m.saturating_mul(n) <= MAX_LCS_CELLS {
        lcs_diff_ops(&before_lines, &after_lines)
    } else {
        bounded_diff_ops(&before_lines, &after_lines)
    };

    render_unified_diff(path, &ops)
}

fn lcs_diff_ops<'a>(before_lines: &[&'a str], after_lines: &[&'a str]) -> Vec<(char, &'a str)> {
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
    ops
}

fn bounded_diff_ops<'a>(before_lines: &[&'a str], after_lines: &[&'a str]) -> Vec<(char, &'a str)> {
    let mut prefix = 0usize;
    while prefix < before_lines.len()
        && prefix < after_lines.len()
        && before_lines[prefix] == after_lines[prefix]
    {
        prefix += 1;
    }

    let mut suffix = 0usize;
    while suffix < before_lines.len().saturating_sub(prefix)
        && suffix < after_lines.len().saturating_sub(prefix)
        && before_lines[before_lines.len() - 1 - suffix]
            == after_lines[after_lines.len() - 1 - suffix]
    {
        suffix += 1;
    }

    let mut ops = Vec::with_capacity(before_lines.len() + after_lines.len());
    ops.extend(before_lines[..prefix].iter().map(|line| (' ', *line)));
    ops.extend(
        before_lines[prefix..before_lines.len() - suffix]
            .iter()
            .map(|line| ('-', *line)),
    );
    ops.extend(
        after_lines[prefix..after_lines.len() - suffix]
            .iter()
            .map(|line| ('+', *line)),
    );
    ops.extend(
        before_lines[before_lines.len() - suffix..]
            .iter()
            .map(|line| (' ', *line)),
    );
    ops
}

fn render_unified_diff(path: &str, ops: &[(char, &str)]) -> String {
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
    for (k, _) in ops {
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
    let f = File::open(&abs).map_err(|e| e.to_string())?;
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
    let mut numbered_lines = Vec::new();
    let mut real_end = start;
    for (idx, line) in BufReader::new(f).lines().enumerate() {
        if idx < start {
            continue;
        }
        if numbered_lines.len() >= limit {
            break;
        }
        let line = line.map_err(|e| e.to_string())?;
        real_end = idx + 1;
        numbered_lines.push(format!("{:>6}\t{}", idx + 1, line));
    }
    let slice = numbered_lines.join("\n");
    Ok(truncate_out(format!(
        "File: {}\nLines {}-{}\n---\n{}",
        abs.display(),
        offset,
        real_end,
        slice
    )))
}

pub(crate) fn tool_write(
    cwd: &Path,
    args: &Value,
    undo: Option<&Arc<Mutex<TurnUndoJournal>>>,
) -> ToolResult {
    let path = match args.get("path").and_then(|x| x.as_str()) {
        Some(p) => p,
        None => {
            return ToolResult {
                output: err("missing path"),
                is_error: true,
                diff: None,
                full_output_path: None,
            };
        }
    };
    let content = match args.get("content").and_then(|x| x.as_str()) {
        Some(c) => c,
        None => {
            return ToolResult {
                output: err("missing content"),
                is_error: true,
                diff: None,
                full_output_path: None,
            };
        }
    };
    let abs = match resolve_under_cwd_for_create(cwd, path) {
        Ok(p) => p,
        Err(e) => {
            return ToolResult {
                output: e,
                is_error: true,
                diff: None,
                full_output_path: None,
            };
        }
    };
    // Read existing file for diff (empty string if new file).
    let before = fs::read_to_string(&abs).unwrap_or_default();
    if let Some(journal) = undo
        && let Err(e) = journal
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .record_before(&abs)
    {
        return ToolResult {
            output: e,
            is_error: true,
            diff: None,
            full_output_path: None,
        };
    }
    if let Err(e) = atomic_write(&abs, content.as_bytes()) {
        return ToolResult {
            output: e.to_string(),
            is_error: true,
            diff: None,
            full_output_path: None,
        };
    }
    if let Some(journal) = undo
        && let Err(e) = journal
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .record_after(&abs)
    {
        return ToolResult {
            output: e,
            is_error: true,
            diff: None,
            full_output_path: None,
        };
    }
    let diff = make_unified_diff(path, &before, content);
    ToolResult {
        output: format!("Wrote {} bytes to {}", content.len(), abs.display()),
        is_error: false,
        diff: if diff.is_empty() { None } else { Some(diff) },
        full_output_path: None,
    }
}

pub(crate) fn tool_edit(
    cwd: &Path,
    args: &Value,
    undo: Option<&Arc<Mutex<TurnUndoJournal>>>,
) -> ToolResult {
    let path = match args.get("path").and_then(|x| x.as_str()) {
        Some(p) => p,
        None => {
            return ToolResult {
                output: err("missing path"),
                is_error: true,
                diff: None,
                full_output_path: None,
            };
        }
    };
    let abs = match resolve_under_cwd(cwd, path) {
        Ok(p) => p,
        Err(e) => {
            return ToolResult {
                output: e,
                is_error: true,
                diff: None,
                full_output_path: None,
            };
        }
    };
    let before = match fs::read_to_string(&abs) {
        Ok(s) => s,
        Err(e) => {
            return ToolResult {
                output: e.to_string(),
                is_error: true,
                diff: None,
                full_output_path: None,
            };
        }
    };
    let mut content = before.clone();
    // Each entry: (oldText, newText, replace_all).
    let mut edits: Vec<(String, String, bool)> = Vec::new();
    if let Some(arr) = args.get("edits").and_then(|x| x.as_array()) {
        for e in arr {
            let old = e.get("oldText").and_then(|x| x.as_str());
            let new = e.get("newText").and_then(|x| x.as_str());
            let replace_all = e
                .get("replaceAll")
                .and_then(|x| x.as_bool())
                .unwrap_or(false);
            if let (Some(o), Some(n)) = (old, new) {
                edits.push((o.to_string(), n.to_string(), replace_all));
            }
        }
    }
    if edits.is_empty() {
        let old = args.get("oldText").and_then(|x| x.as_str());
        let new = args.get("newText").and_then(|x| x.as_str());
        let replace_all = args
            .get("replaceAll")
            .and_then(|x| x.as_bool())
            .unwrap_or(false);
        if let (Some(o), Some(n)) = (old, new) {
            edits.push((o.to_string(), n.to_string(), replace_all));
        }
    }
    if edits.is_empty() {
        return ToolResult {
            output: err("no edits"),
            is_error: true,
            diff: None,
            full_output_path: None,
        };
    }
    // An empty oldText matches between every char; `str::replace("")` would interleave the
    // replacement everywhere. Reject it outright.
    if edits.iter().any(|(old, _, _)| old.is_empty()) {
        return ToolResult {
            output: err("oldText must not be empty"),
            is_error: true,
            diff: None,
            full_output_path: None,
        };
    }
    // Validate and apply against the evolving in-memory document. This makes dependent edits
    // deterministic and transactional: if any entry fails, nothing is written to disk.
    let mut total_replacements = 0usize;
    for (edit_idx, (old, new, replace_all)) in edits.into_iter().enumerate() {
        let count = content.matches(old.as_str()).count();
        if replace_all {
            if count == 0 {
                return ToolResult {
                    output: err(format!("edit {}: oldText not found in file", edit_idx + 1)),
                    is_error: true,
                    diff: None,
                    full_output_path: None,
                };
            }
            total_replacements += count;
            content = content.replace(old.as_str(), &new);
        } else if count != 1 {
            return ToolResult {
                output: err(format!(
                    "edit {}: oldText must match exactly once in the current document, found {count} occurrences \
                     (pass replaceAll: true to replace every occurrence)",
                    edit_idx + 1
                )),
                is_error: true,
                diff: None,
                full_output_path: None,
            };
        } else if let Some(idx) = content.find(old.as_str()) {
            content.replace_range(idx..idx + old.len(), &new);
            total_replacements += 1;
        }
    }
    if let Some(journal) = undo
        && let Err(e) = journal
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .record_before(&abs)
    {
        return ToolResult {
            output: e,
            is_error: true,
            diff: None,
            full_output_path: None,
        };
    }
    if let Err(e) = atomic_write(&abs, content.as_bytes()) {
        return ToolResult {
            output: e.to_string(),
            is_error: true,
            diff: None,
            full_output_path: None,
        };
    }
    if let Some(journal) = undo
        && let Err(e) = journal
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .record_after(&abs)
    {
        return ToolResult {
            output: e,
            is_error: true,
            diff: None,
            full_output_path: None,
        };
    }
    let diff = make_unified_diff(path, &before, &content);
    ToolResult {
        output: format!(
            "Edited {} ({} replacement{})",
            abs.display(),
            total_replacements,
            if total_replacements == 1 { "" } else { "s" }
        ),
        is_error: false,
        diff: if diff.is_empty() { None } else { Some(diff) },
        full_output_path: None,
    }
}

fn mutation_error(output: impl Into<String>) -> ToolResult {
    ToolResult {
        output: output.into(),
        is_error: true,
        diff: None,
        full_output_path: None,
    }
}

fn journal_before(undo: Option<&Arc<Mutex<TurnUndoJournal>>>, path: &Path) -> Result<(), String> {
    if let Some(journal) = undo {
        journal
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .record_before(path)?;
    }
    Ok(())
}

fn journal_after(undo: Option<&Arc<Mutex<TurnUndoJournal>>>, path: &Path) -> Result<(), String> {
    if let Some(journal) = undo {
        journal
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .record_after(path)?;
    }
    Ok(())
}

pub(crate) fn tool_delete(
    cwd: &Path,
    args: &Value,
    undo: Option<&Arc<Mutex<TurnUndoJournal>>>,
) -> ToolResult {
    let Some(path) = args.get("path").and_then(Value::as_str) else {
        return mutation_error(err("missing path"));
    };
    let abs = match resolve_under_cwd(cwd, path) {
        Ok(path) => path,
        Err(e) => return mutation_error(e),
    };
    if abs == cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf()) {
        return mutation_error("refusing to delete the workspace root");
    }
    if let Err(e) = journal_before(undo, &abs) {
        return mutation_error(e);
    }
    let metadata = match fs::symlink_metadata(&abs) {
        Ok(metadata) => metadata,
        Err(e) => return mutation_error(e.to_string()),
    };
    let result = if metadata.is_dir() && !metadata.file_type().is_symlink() {
        // Deliberately non-recursive: recursive deletion would need to journal every child.
        fs::remove_dir(&abs)
    } else {
        fs::remove_file(&abs)
    };
    if let Err(e) = result {
        return mutation_error(format!(
            "Delete {}: {e}. Directories must be empty; delete their children first.",
            abs.display()
        ));
    }
    if let Err(e) = journal_after(undo, &abs) {
        return mutation_error(e);
    }
    ToolResult {
        output: format!("Deleted {}", abs.display()),
        is_error: false,
        diff: None,
        full_output_path: None,
    }
}

pub(crate) fn tool_mkdir(
    cwd: &Path,
    args: &Value,
    undo: Option<&Arc<Mutex<TurnUndoJournal>>>,
) -> ToolResult {
    let Some(path) = args.get("path").and_then(Value::as_str) else {
        return mutation_error(err("missing path"));
    };
    let abs = match resolve_under_cwd_for_create(cwd, path) {
        Ok(path) => path,
        Err(e) => return mutation_error(e),
    };
    if abs.exists() {
        return mutation_error(format!("Path already exists: {}", abs.display()));
    }
    let Some(parent) = abs.parent() else {
        return mutation_error("directory has no parent");
    };
    if !parent.is_dir() {
        return mutation_error("parent directory does not exist; create it first");
    }
    if let Err(e) = journal_before(undo, &abs) {
        return mutation_error(e);
    }
    if let Err(e) = fs::create_dir(&abs) {
        return mutation_error(format!("Create directory {}: {e}", abs.display()));
    }
    if let Err(e) = journal_after(undo, &abs) {
        return mutation_error(e);
    }
    ToolResult {
        output: format!("Created directory {}", abs.display()),
        is_error: false,
        diff: None,
        full_output_path: None,
    }
}

pub(crate) fn tool_move(
    cwd: &Path,
    args: &Value,
    undo: Option<&Arc<Mutex<TurnUndoJournal>>>,
) -> ToolResult {
    let Some(from) = args.get("from").and_then(Value::as_str) else {
        return mutation_error(err("missing from"));
    };
    let Some(to) = args.get("to").and_then(Value::as_str) else {
        return mutation_error(err("missing to"));
    };
    let source = match resolve_under_cwd(cwd, from) {
        Ok(path) => path,
        Err(e) => return mutation_error(e),
    };
    let destination = match resolve_under_cwd_for_create(cwd, to) {
        Ok(path) => path,
        Err(e) => return mutation_error(e),
    };
    if destination.exists() {
        return mutation_error(format!(
            "Destination already exists: {}",
            destination.display()
        ));
    }
    if source.is_dir()
        && fs::read_dir(&source)
            .map(|mut entries| entries.next().is_some())
            .unwrap_or(true)
    {
        return mutation_error(
            "moving non-empty directories is not supported; move their children first",
        );
    }
    let Some(parent) = destination.parent() else {
        return mutation_error("destination has no parent");
    };
    if !parent.is_dir() {
        return mutation_error("destination parent does not exist; create it first");
    }
    if let Err(e) = journal_before(undo, &source).and_then(|_| journal_before(undo, &destination)) {
        return mutation_error(e);
    }
    if let Err(e) = fs::rename(&source, &destination) {
        return mutation_error(format!(
            "Move {} to {}: {e}",
            source.display(),
            destination.display()
        ));
    }
    if let Err(e) = journal_after(undo, &source).and_then(|_| journal_after(undo, &destination)) {
        return mutation_error(e);
    }
    ToolResult {
        output: format!("Moved {} to {}", source.display(), destination.display()),
        is_error: false,
        diff: None,
        full_output_path: None,
    }
}
