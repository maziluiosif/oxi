//! `chat_read`: the agent reads earlier turns of the oxi chat in full. Handoff transcripts trim
//! tool output and leave out middle turns of long chats; this is where the agent gets them back.
//! Before each prompt the app writes the chat's turns (numbered like the transcripts) into the
//! connection folder the MCP server shares with it ([`crate::evidence::DIR_ENV`]), and the server
//! reads them from there.

use std::path::Path;

use serde_json::{Value, json};

pub(super) const TOOL: &str = "chat_read";
/// Turns returned per call by default, and at most.
const DEFAULT_LIMIT: usize = 10;
const MAX_LIMIT: usize = 40;
/// Text returned per call; the reply says where to continue.
const REPLY_MAX_CHARS: usize = 40_000;

fn path(conn_dir: &Path) -> std::path::PathBuf {
    conn_dir.join("chat.json")
}

/// Save the chat's turns for this connection's `chat_read`.
pub(super) fn write(conn_dir: &Path, turns: &[(usize, String)]) -> Result<(), String> {
    let turns: Vec<Value> = turns
        .iter()
        .map(|(n, text)| json!({ "n": n, "text": text }))
        .collect();
    let bytes = serde_json::to_vec(&json!({ "turns": turns })).map_err(|e| e.to_string())?;
    crate::fsutil::write_atomic_private(&path(conn_dir), &bytes)
}

pub(super) fn definition() -> Value {
    json!({
        "name": TOOL,
        "description": "Read earlier turns of this oxi conversation in full, including turns other agents answered. Turn numbers match the [#n] markers in the transcript oxi gave you. Use it when the transcript left turns out or trimmed tool output you need. `from` is the first turn number (default 1), `limit` how many turns (default 10, max 40); `query` keeps only turns containing that text (case-insensitive).",
        "inputSchema": {
            "type": "object",
            "properties": {
                "from": { "type": "integer", "minimum": 1 },
                "limit": { "type": "integer", "minimum": 1, "maximum": MAX_LIMIT },
                "query": { "type": "string" }
            }
        }
    })
}

pub(super) fn call(args: &Value) -> Result<String, String> {
    let dir =
        crate::evidence::tools::conn_dir().ok_or("chat_read is only available in an oxi chat.")?;
    read(&dir, args)
}

fn read(conn_dir: &Path, args: &Value) -> Result<String, String> {
    let Ok(bytes) = std::fs::read(path(conn_dir)) else {
        return Ok("(No earlier turns in this conversation.)".into());
    };
    let saved: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let turns: Vec<(u64, &str)> = saved["turns"]
        .as_array()
        .map(|turns| {
            turns
                .iter()
                .filter_map(|t| Some((t["n"].as_u64()?, t["text"].as_str()?)))
                .collect()
        })
        .unwrap_or_default();
    let (Some(first), Some(last)) = (turns.first(), turns.last()) else {
        return Ok("(No earlier turns in this conversation.)".into());
    };
    let (first, last) = (first.0, last.0);
    let from = args["from"].as_u64().unwrap_or(1);
    let limit = args["limit"]
        .as_u64()
        .map_or(DEFAULT_LIMIT, |l| l as usize)
        .clamp(1, MAX_LIMIT);
    let query = args["query"]
        .as_str()
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .map(str::to_lowercase);
    let mut out = format!("Conversation turns #{first}–#{last}.");
    if let Some(q) = &query {
        out.push_str(&format!(" Turns containing \"{q}\":"));
    }
    let mut shown = 0;
    let mut next = None;
    for &(n, text) in turns.iter().filter(|(n, _)| *n >= from) {
        if query
            .as_ref()
            .is_some_and(|q| !text.to_lowercase().contains(q.as_str()))
        {
            continue;
        }
        // The first turn always goes out, so a call never comes back empty-handed.
        if shown == limit || (shown > 0 && out.len() + text.len() + 2 > REPLY_MAX_CHARS) {
            next = Some(n);
            break;
        }
        out.push_str("\n\n");
        out.push_str(text);
        shown += 1;
    }
    match next {
        Some(n) => out.push_str(&format!("\n\n[More: call chat_read with from={n}.]")),
        None if shown == 0 => out.push_str("\n\n(No matching turns.)"),
        None => {}
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_and_filters_saved_turns() {
        let dir = crate::fsutil::unique_temp_path(&std::env::temp_dir(), "oxi-chat-read", "d");
        assert!(read(&dir, &json!({})).unwrap().contains("No earlier turns"));
        let turns: Vec<(usize, String)> = (1..=15)
            .map(|n| (n, format!("[#{n}] User: step {n}")))
            .collect();
        write(&dir, &turns).unwrap();

        let page = read(&dir, &json!({})).unwrap();
        assert!(page.starts_with("Conversation turns #1–#15."));
        assert!(page.contains("[#10] User: step 10") && !page.contains("[#11]"));
        assert!(page.ends_with("[More: call chat_read with from=11.]"));

        let rest = read(&dir, &json!({"from": 11})).unwrap();
        assert!(rest.contains("[#15]") && !rest.contains("More:"));

        let hit = read(&dir, &json!({"query": "STEP 12"})).unwrap();
        assert!(hit.contains("[#12]") && !hit.contains("[#1] "));
        let miss = read(&dir, &json!({"query": "nope"})).unwrap();
        assert!(miss.ends_with("(No matching turns.)"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
