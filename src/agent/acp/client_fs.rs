//! ACP client filesystem request handlers.

use serde_json::Value;

pub(super) fn fs_read_text(params: &Value) -> Result<String, String> {
    let path = params
        .get("path")
        .and_then(|p| p.as_str())
        .ok_or("fs/read_text_file: missing path")?;
    let content = std::fs::read_to_string(path).map_err(|e| format!("read {path} failed: {e}"))?;
    let line = params.get("line").and_then(|v| v.as_u64());
    let limit = params.get("limit").and_then(|v| v.as_u64());
    if line.is_none() && limit.is_none() {
        return Ok(content);
    }
    // `line` is 1-based; `limit` is a line count.
    let start = line.map(|l| l.saturating_sub(1) as usize).unwrap_or(0);
    let mut out: Vec<&str> = content.lines().skip(start).collect();
    if let Some(lim) = limit {
        out.truncate(lim as usize);
    }
    Ok(out.join("\n"))
}

pub(super) fn fs_write_text(params: &Value) -> Result<(), String> {
    let path = params
        .get("path")
        .and_then(|p| p.as_str())
        .ok_or("fs/write_text_file: missing path")?;
    let content = params
        .get("content")
        .and_then(|c| c.as_str())
        .unwrap_or_default();
    if let Some(parent) = std::path::Path::new(path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(path, content).map_err(|e| format!("write {path} failed: {e}"))
}
