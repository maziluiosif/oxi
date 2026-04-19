use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use serde_json::{json, Value};

use crate::hydrate;
use crate::model::{ChatMessage, Session};

use super::dedupe::dedupe_trailing_duplicate_messages;
use super::format::{chat_message_to_json_entries, session_file_stem_or_generated};
use super::paths::session_file_path;

pub fn load_session_messages(session_file: &str) -> Option<Vec<ChatMessage>> {
    let file = File::open(session_file).ok()?;
    let reader = BufReader::new(file);
    let mut saw_header = false;
    let mut messages = Vec::new();

    for line in reader.lines().map_while(Result::ok) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value = serde_json::from_str::<Value>(trimmed).ok()?;
        if !saw_header {
            let header_type = value.get("type").and_then(Value::as_str);
            let header_id = value.get("id").and_then(Value::as_str);
            if header_type != Some("session") || header_id.is_none() {
                return None;
            }
            saw_header = true;
            continue;
        }
        if value.get("type").and_then(Value::as_str) == Some("message") {
            if let Some(message) = value.get("message") {
                messages.push(message.clone());
            }
        }
    }

    if !saw_header {
        return None;
    }

    Some(hydrate::messages_from_get_messages(&json!({
        "messages": messages,
    })))
}

pub fn save_session_messages(root_path: &str, session: &mut Session) -> Result<(), String> {
    dedupe_trailing_duplicate_messages(&mut session.messages);

    let session_path = session_file_path(root_path, session)?;
    if let Some(parent) = session_path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let is_new_file = !session_path.exists();
    let mut file = if is_new_file {
        File::create(&session_path).map_err(|e| e.to_string())?
    } else {
        OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&session_path)
            .map_err(|e| e.to_string())?
    };

    let session_id = session_file_stem_or_generated(&session_path);
    writeln!(
        file,
        "{}",
        serde_json::to_string(&json!({
            "type": "session",
            "id": session_id,
        }))
        .map_err(|e| e.to_string())?
    )
    .map_err(|e| e.to_string())?;

    if !session.title.trim().is_empty() {
        writeln!(
            file,
            "{}",
            serde_json::to_string(&json!({
                "type": "session_info",
                "name": session.title,
            }))
            .map_err(|e| e.to_string())?
        )
        .map_err(|e| e.to_string())?;
    }

    for message in &session.messages {
        for message_json in chat_message_to_json_entries(message) {
            writeln!(
                file,
                "{}",
                serde_json::to_string(&json!({
                    "type": "message",
                    "message": message_json,
                }))
                .map_err(|e| e.to_string())?
            )
            .map_err(|e| e.to_string())?;
        }
    }

    session.session_file = Some(session_path.to_string_lossy().to_string());
    session.messages_loaded = true;
    Ok(())
}

pub fn parse_session_header_and_messages(path: &Path) -> Option<(Option<String>, Option<String>)> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);
    let mut saw_header = false;
    let mut session_name: Option<String> = None;
    let mut first_user_message: Option<String> = None;

    for line in reader.lines().map_while(Result::ok) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value = serde_json::from_str::<Value>(trimmed).ok()?;
        if !saw_header {
            let header_type = value.get("type").and_then(Value::as_str);
            let header_id = value.get("id").and_then(Value::as_str);
            if header_type != Some("session") || header_id.is_none() {
                return None;
            }
            saw_header = true;
            continue;
        }

        match value.get("type").and_then(Value::as_str) {
            Some("session_info") if session_name.is_none() => {
                session_name = value
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(ToOwned::to_owned);
            }
            Some("message") if first_user_message.is_none() => {
                if let Some(message) = value.get("message") {
                    first_user_message = extract_first_user_message(message);
                }
            }
            _ => {}
        }
    }

    if !saw_header {
        return None;
    }

    Some((session_name, first_user_message))
}

fn extract_first_user_message(message: &Value) -> Option<String> {
    let role = message.get("role").and_then(Value::as_str)?;
    if role != "user" {
        return None;
    }

    if let Some(content) = message.get("content").and_then(Value::as_str) {
        let trimmed = content.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    let parts = message.get("content").and_then(Value::as_array)?;
    for part in parts {
        if part.get("type").and_then(Value::as_str) == Some("text") {
            if let Some(text) = part.get("text").and_then(Value::as_str) {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
    }
    None
}
