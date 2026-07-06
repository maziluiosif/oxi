use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use serde_json::{Value, json};

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
        if value.get("type").and_then(Value::as_str) == Some("message")
            && let Some(message) = value.get("message")
        {
            messages.push(message.clone());
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
    // Any save counts as activity; keeps the sidebar age label in sync with file mtime.
    session.modified = std::time::SystemTime::now();

    let session_path = session_file_path(root_path, session)?;
    if let Some(parent) = session_path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let tmp_path = session_path.with_extension("jsonl.tmp");
    let mut file = File::create(&tmp_path).map_err(|e| e.to_string())?;

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

    file.sync_all().map_err(|e| e.to_string())?;
    drop(file);
    fs::rename(&tmp_path, &session_path).map_err(|e| e.to_string())?;
    if let Some(parent) = session_path.parent()
        && let Ok(dir) = File::open(parent)
    {
        let _ = dir.sync_all();
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
        if part.get("type").and_then(Value::as_str) == Some("text")
            && let Some(text) = part.get("text").and_then(Value::as_str)
        {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    /// Writes `bytes` to a fresh temp file and returns its path. Session files on disk
    /// can be corrupted by a crash mid-write, manual editing, or a bug in an older
    /// version, so `load_session_messages`/`parse_session_header_and_messages` need to
    /// degrade gracefully rather than panic on any of these shapes.
    fn temp_file(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_else(|_| Duration::from_secs(0))
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("oxi-session-io-test-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let mut f = File::create(&path).unwrap();
        f.write_all(bytes).unwrap();
        path
    }

    #[test]
    fn load_session_messages_empty_file_returns_none() {
        let path = temp_file("empty.jsonl", b"");
        assert!(load_session_messages(path.to_str().unwrap()).is_none());
    }

    #[test]
    fn load_session_messages_missing_file_returns_none() {
        assert!(load_session_messages("/nonexistent/path/session.jsonl").is_none());
    }

    #[test]
    fn load_session_messages_missing_header_returns_none() {
        let path = temp_file(
            "no-header.jsonl",
            b"{\"type\":\"message\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n",
        );
        assert!(load_session_messages(path.to_str().unwrap()).is_none());
    }

    #[test]
    fn load_session_messages_wrong_header_type_returns_none() {
        let path = temp_file(
            "wrong-header.jsonl",
            b"{\"type\":\"not_session\",\"id\":\"abc\"}\n",
        );
        assert!(load_session_messages(path.to_str().unwrap()).is_none());
    }

    #[test]
    fn load_session_messages_header_missing_id_returns_none() {
        let path = temp_file("no-id.jsonl", b"{\"type\":\"session\"}\n");
        assert!(load_session_messages(path.to_str().unwrap()).is_none());
    }

    #[test]
    fn load_session_messages_invalid_json_line_returns_none() {
        let path = temp_file(
            "bad-json.jsonl",
            b"{\"type\":\"session\",\"id\":\"abc\"}\nnot json at all\n",
        );
        assert!(load_session_messages(path.to_str().unwrap()).is_none());
    }

    #[test]
    fn load_session_messages_truncated_last_line_returns_none() {
        // Simulates a crash mid-`writeln!`: the file ends with a half-written JSON object
        // (valid UTF-8, invalid JSON), which fails the `serde_json::from_str(..).ok()?`
        // in the read loop and bails the whole call out to `None`.
        let path = temp_file(
            "truncated.jsonl",
            b"{\"type\":\"session\",\"id\":\"abc\"}\n{\"type\":\"message\",\"message\":{\"rol",
        );
        assert!(load_session_messages(path.to_str().unwrap()).is_none());
    }

    #[test]
    fn load_session_messages_non_utf8_line_is_silently_dropped_not_panicking() {
        // `BufReader::lines()` yields an `Err` for a non-UTF-8 line; the read loop uses
        // `.map_while(Result::ok)`, which stops iterating at that first `Err` rather than
        // propagating it. So a non-UTF-8 line doesn't fail the load the way a UTF-8-but-
        // invalid-JSON line does (see the truncated-line test above) — everything read
        // before the bad line is kept, and nothing after it is. Documented here as the
        // actual (silent-truncation) behavior, not a panic.
        let path = temp_file(
            "non-utf8.jsonl",
            b"{\"type\":\"session\",\"id\":\"abc\"}\n\
              {\"type\":\"message\",\"message\":{\"role\":\"user\",\"content\":\"kept\"}}\n\
              \xff\xfe\x00garbage\n\
              {\"type\":\"message\",\"message\":{\"role\":\"user\",\"content\":\"dropped\"}}\n",
        );
        let messages = load_session_messages(path.to_str().unwrap()).expect("header was valid");
        assert_eq!(messages.len(), 1);
    }

    #[test]
    fn load_session_messages_valid_file_returns_messages() {
        let path = temp_file(
            "valid.jsonl",
            b"{\"type\":\"session\",\"id\":\"abc\"}\n\
              {\"type\":\"message\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n",
        );
        assert!(load_session_messages(path.to_str().unwrap()).is_some());
    }

    #[test]
    fn parse_session_header_and_messages_empty_file_returns_none() {
        let path = temp_file("empty2.jsonl", b"");
        assert!(parse_session_header_and_messages(&path).is_none());
    }

    #[test]
    fn parse_session_header_and_messages_missing_header_returns_none() {
        let path = temp_file(
            "no-header2.jsonl",
            b"{\"type\":\"session_info\",\"name\":\"Chat\"}\n",
        );
        assert!(parse_session_header_and_messages(&path).is_none());
    }

    #[test]
    fn parse_session_header_and_messages_invalid_json_returns_none() {
        let path = temp_file(
            "bad-json2.jsonl",
            b"{\"type\":\"session\",\"id\":\"abc\"}\n{{{\n",
        );
        assert!(parse_session_header_and_messages(&path).is_none());
    }

    #[test]
    fn parse_session_header_and_messages_header_only_returns_empty_fields() {
        let path = temp_file(
            "header-only.jsonl",
            b"{\"type\":\"session\",\"id\":\"abc\"}\n",
        );
        let (name, first_message) = parse_session_header_and_messages(&path).unwrap();
        assert_eq!(name, None);
        assert_eq!(first_message, None);
    }

    #[test]
    fn parse_session_header_and_messages_extracts_name_and_first_user_message() {
        let path = temp_file(
            "full.jsonl",
            b"{\"type\":\"session\",\"id\":\"abc\"}\n\
              {\"type\":\"session_info\",\"name\":\"My Chat\"}\n\
              {\"type\":\"message\",\"message\":{\"role\":\"user\",\"content\":\"hello there\"}}\n",
        );
        let (name, first_message) = parse_session_header_and_messages(&path).unwrap();
        assert_eq!(name, Some("My Chat".to_string()));
        assert_eq!(first_message, Some("hello there".to_string()));
    }
}
