//! Translation of ACP session updates into oxi's provider-neutral event stream.

use std::fmt::Write as _;
use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use base64::Engine as _;
use serde_json::Value;

use crate::agent::events::AgentEvent;

/// Translate one ACP `session/update` payload into oxi [`AgentEvent`]s.
pub(super) fn emit_update(update: &Value, tx: &Sender<AgentEvent>) {
    let kind = update.get("sessionUpdate").and_then(|v| v.as_str());
    match kind {
        Some("agent_message_chunk") => {
            let content = update.get("content");
            if let Some(text) = content.and_then(content_block_text) {
                let _ = tx.send(AgentEvent::TextDelta(text));
            } else if let Some(md) = content.and_then(content_block_image_markdown) {
                let _ = tx.send(AgentEvent::TextDelta(md));
            }
        }
        Some("agent_thought_chunk") => {
            if let Some(text) = update.get("content").and_then(content_block_text) {
                let _ = tx.send(AgentEvent::ThinkingDelta(text));
            }
        }
        Some("tool_call") => {
            let tool_call_id = update
                .get("toolCallId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let name = update
                .get("kind")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .or_else(|| update.get("title").and_then(|v| v.as_str()))
                .unwrap_or("tool")
                .to_string();
            let _ = tx.send(AgentEvent::ToolStart {
                name,
                tool_call_id: tool_call_id.clone(),
                args: update.get("rawInput").cloned(),
            });
            emit_tool_content_and_status(update, &tool_call_id, tx);
        }
        Some("tool_call_update") => {
            let tool_call_id = update
                .get("toolCallId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            emit_tool_content_and_status(update, &tool_call_id, tx);
        }
        _ => {}
    }
}

fn emit_tool_content_and_status(update: &Value, tool_call_id: &str, tx: &Sender<AgentEvent>) {
    let ToolContent { text, diff, images } = update
        .get("content")
        .map(extract_tool_content)
        .unwrap_or_default();
    if !text.is_empty() {
        let _ = tx.send(AgentEvent::ToolOutput {
            tool_call_id: tool_call_id.to_string(),
            text,
            truncated: false,
        });
    }
    // Tool-produced images (e.g. Codex image generation) are shown in the answer stream, where
    // the Markdown renderer displays them inline and the session file keeps a reference to them.
    for md in images {
        let _ = tx.send(AgentEvent::TextDelta(md));
    }
    match update.get("status").and_then(|v| v.as_str()) {
        Some("completed") | Some("failed") => {
            let is_error = update.get("status").and_then(|v| v.as_str()) == Some("failed");
            let _ = tx.send(AgentEvent::ToolEnd {
                tool_call_id: tool_call_id.to_string(),
                is_error: Some(is_error),
                full_output_path: None,
                diff,
            });
        }
        _ => {}
    }
}

fn content_block_text(block: &Value) -> Option<String> {
    match block.get("type").and_then(|t| t.as_str()) {
        Some("text") => block
            .get("text")
            .and_then(|t| t.as_str())
            .map(|s| s.to_string()),
        _ => None,
    }
}

/// Markdown image for an ACP `image` block (or a `resource_link` to a local image file),
/// pointing at a file on disk so the transcript never carries the base64 payload.
fn content_block_image_markdown(block: &Value) -> Option<String> {
    let path = match block.get("type").and_then(|t| t.as_str()) {
        Some("image") => {
            let uri = block.get("uri").and_then(|v| v.as_str());
            match uri.and_then(local_image_path) {
                Some(path) => path,
                None => {
                    let data = block.get("data").and_then(|v| v.as_str())?;
                    let mime = block
                        .get("mimeType")
                        .and_then(|v| v.as_str())
                        .unwrap_or("image/png");
                    save_image(data, mime)?
                }
            }
        }
        Some("resource_link") => {
            let uri = block.get("uri").and_then(|v| v.as_str())?;
            let mime = block.get("mimeType").and_then(|v| v.as_str());
            let path = local_image_path(uri)?;
            let is_image =
                mime.is_some_and(|m| m.starts_with("image/")) || image_extension(&path).is_some();
            if !is_image {
                return None;
            }
            path
        }
        _ => return None,
    };
    Some(image_markdown(&path))
}

/// `uri` as an existing local file (plain path or `file://` URI).
fn local_image_path(uri: &str) -> Option<PathBuf> {
    let path = file_uri_to_path(uri);
    (path.is_absolute() && path.is_file()).then_some(path)
}

/// Path for a plain path or a `file://` URI. On Windows `file:///C:/x` carries a slash before
/// the drive letter that must go, or the result is not an absolute path.
pub(super) fn file_uri_to_path(uri: &str) -> PathBuf {
    let rest = uri.strip_prefix("file://").unwrap_or(uri);
    if cfg!(windows) {
        let bytes = rest.as_bytes();
        if bytes.len() >= 3
            && bytes[0] == b'/'
            && bytes[1].is_ascii_alphabetic()
            && bytes[2] == b':'
        {
            return PathBuf::from(&rest[1..]);
        }
    }
    PathBuf::from(rest)
}

fn image_extension(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some("png"),
        "jpg" | "jpeg" => Some("jpg"),
        "gif" => Some("gif"),
        "webp" => Some("webp"),
        _ => None,
    }
}

/// Decode a base64 image into oxi's data dir, named by content hash so replays reuse the file.
fn save_image(data_b64: &str, mime: &str) -> Option<PathBuf> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data_b64.trim().as_bytes())
        .ok()?;
    let ext = match mime {
        "image/jpeg" | "image/jpg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => "png",
    };
    let mut h = DefaultHasher::new();
    bytes.hash(&mut h);
    let dir = dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("oxi")
        .join("images");
    let path = dir.join(format!("{:016x}.{ext}", h.finish()));
    if !path.is_file() {
        std::fs::create_dir_all(&dir).ok()?;
        std::fs::write(&path, &bytes).ok()?;
    }
    Some(path)
}

/// Standalone Markdown image paragraph. The destination is wrapped in `<...>` because data
/// dirs often contain spaces (`Application Support`), and egui's file loader doesn't decode `%20`.
fn image_markdown(path: &Path) -> String {
    let p = path.to_string_lossy();
    let uri = if cfg!(windows) {
        format!("file:///{}", p.replace('\\', "/"))
    } else {
        format!("file://{p}")
    };
    format!("\n\n![image](<{uri}>)\n\n")
}

#[derive(Default)]
struct ToolContent {
    text: String,
    diff: Option<String>,
    /// Markdown image paragraphs, one per image block.
    images: Vec<String>,
}

fn extract_tool_content(content: &Value) -> ToolContent {
    let mut out = ToolContent::default();
    let Some(items) = content.as_array() else {
        return out;
    };
    for item in items {
        match item.get("type").and_then(|t| t.as_str()) {
            Some("content") => {
                let Some(block) = item.get("content") else {
                    continue;
                };
                if let Some(t) = content_block_text(block) {
                    out.text.push_str(&t);
                } else if let Some(md) = content_block_image_markdown(block) {
                    out.images.push(md);
                }
            }
            Some("diff") => {
                let path = item.get("path").and_then(|v| v.as_str()).unwrap_or("");
                let old = item.get("oldText").and_then(|v| v.as_str()).unwrap_or("");
                let new = item.get("newText").and_then(|v| v.as_str()).unwrap_or("");
                out.diff = Some(build_unified_diff(path, old, new));
            }
            _ => {}
        }
    }
    out
}

fn build_unified_diff(path: &str, old: &str, new: &str) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "--- {path}");
    let _ = writeln!(s, "+++ {path}");
    for line in old.lines() {
        let _ = writeln!(s, "-{line}");
    }
    for line in new.lines() {
        let _ = writeln!(s, "+{line}");
    }
    s
}
