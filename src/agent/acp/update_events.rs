//! Translation of ACP session updates into oxi's provider-neutral event stream.

use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use base64::Engine as _;
use serde_json::Value;

use crate::agent::events::AgentEvent;
use crate::model::{ToolLocation, ToolMetadata, ToolStatus, ToolUpdate};

/// Translate one ACP `session/update` payload into oxi [`AgentEvent`]s.
#[derive(Default)]
pub(super) struct UpdateState {
    tools: HashMap<String, ToolAccumulator>,
}

#[derive(Default)]
struct ToolAccumulator {
    fields: serde_json::Map<String, Value>,
    content: ToolContent,
    emitted_images: HashSet<String>,
}

impl UpdateState {
    pub(super) fn emit_update(&mut self, update: &Value, tx: &Sender<AgentEvent>) {
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
            Some("tool_call" | "tool_call_update") => self.emit_tool(update, tx),
            _ => {}
        }
    }

    pub(super) fn emit_tool(&mut self, update: &Value, tx: &Sender<AgentEvent>) {
        let Some(id) = update
            .get("toolCallId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        else {
            return;
        };
        let tool = self.tools.entry(id.to_owned()).or_default();
        // Optional/null fields do not erase earlier values. Collections replace, never append.
        for key in [
            "title",
            "name",
            "kind",
            "status",
            "rawInput",
            "rawOutput",
            "locations",
        ] {
            if let Some(value) = update.get(key).filter(|v| !v.is_null()) {
                tool.fields.insert(key.to_owned(), value.clone());
            }
        }
        if let Some(content) = update.get("content").filter(|v| v.is_array()) {
            tool.content = extract_tool_content(content);
        }
        let field = |key| tool.fields.get(key).and_then(Value::as_str).unwrap_or("");
        let kind = field("kind");
        let status = match field("status") {
            "in_progress" => ToolStatus::InProgress,
            "completed" => ToolStatus::Completed,
            "failed" => ToolStatus::Failed,
            _ => ToolStatus::Pending,
        };
        let metadata = ToolMetadata {
            title: field("title").to_owned(),
            kind: kind.to_owned(),
            name: tool
                .fields
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_owned),
            locations: tool
                .fields
                .get("locations")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| serde_json::from_value(item.clone()).ok())
                        .collect()
                })
                .unwrap_or_else(|| tool.content.locations.clone()),
            status,
        };
        let name = match kind {
            "execute" => "bash",
            "search" => "grep",
            "fetch" => "web_fetch",
            "think" | "other" | "" => metadata.name.as_deref().unwrap_or("tool"),
            other => other,
        }
        .to_owned();
        let output = if tool.content.text.is_empty() {
            tool.fields
                .get("rawOutput")
                .map(|v| {
                    v.as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| serde_json::to_string_pretty(v).unwrap_or_default())
                })
                .unwrap_or_default()
        } else {
            tool.content.text.clone()
        };
        let limit = crate::agent::tools::MAX_TOOL_OUTPUT_CHARS;
        let output_truncated = output.chars().count() > limit;
        let _ = tx.send(AgentEvent::ToolUpdate(Box::new(ToolUpdate {
            tool_call_id: id.to_owned(),
            name,
            args: tool.fields.get("rawInput").cloned(),
            output: output.chars().take(limit).collect(),
            diff: tool.content.diff.clone(),
            output_truncated,
            metadata,
        })));
        for md in &tool.content.images {
            if tool.emitted_images.insert(md.clone()) {
                let _ = tx.send(AgentEvent::TextDelta(md.clone()));
            }
        }
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
    locations: Vec<ToolLocation>,
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
                    if !out.text.is_empty() {
                        out.text.push('\n');
                    }
                    out.text.push_str(&t);
                } else if let Some(md) = content_block_image_markdown(block) {
                    out.images.push(md);
                }
            }
            Some("diff") => {
                let path = item.get("path").and_then(|v| v.as_str()).unwrap_or("");
                let old = item.get("oldText").and_then(|v| v.as_str()).unwrap_or("");
                let Some(new) = item.get("newText").and_then(Value::as_str) else {
                    continue;
                };
                if !path.is_empty() {
                    out.locations.push(ToolLocation {
                        path: path.to_owned(),
                        line: None,
                    });
                }
                if item.get("oldText").is_none_or(Value::is_null) && new.is_empty() {
                    if !out.text.is_empty() {
                        out.text.push('\n');
                    }
                    out.text.push_str(&format!("New empty file: {path}"));
                }
                let diff = crate::agent::tools::make_unified_diff(path, old, new);
                if !diff.is_empty() {
                    out.diff.get_or_insert_with(String::new).push_str(&diff);
                }
            }
            _ => {}
        }
    }
    out
}
