//! What the oxi app needs from a server it reaches over SSH (`src/remote`), on top of what
//! editors use: raw agent events, whole transcripts in and out, and a folder browser.
//!
//! The app keeps its own copy of each chat and runs the same event handling on it as for a
//! local turn; the server stays the owner of the session files. Before a prompt the app sends
//! a fingerprint of its transcript, and the transcript itself only when the server's differs
//! (the app rewound, retried or compacted the chat).

use serde_json::{Value, json};

use super::{Chat, Server, root_param, str_param, unix_secs};
use crate::agent::AgentEvent;
use crate::remote::protocol;
use std::path::Path;

/// `prompt.send` error: the server's transcript is not the one the app has; send it along.
pub const HISTORY_MISMATCH: &str = "history_mismatch";

impl Server {
    /// `client.features`: what the connecting client wants and what this server offers.
    pub(super) fn client_features(&mut self, client: u64, params: &Value) -> Value {
        if params["events"].as_bool() == Some(true) {
            self.event_clients.insert(client);
        } else {
            self.event_clients.remove(&client);
        }
        json!({"features": [protocol::FEATURE]})
    }

    /// Send a chat's agent event to the clients that take raw events.
    pub(super) fn forward_event(&self, chat: &str, event: &AgentEvent) {
        if self.event_clients.is_empty() {
            return;
        }
        let Some(event) = protocol::event_to_json(event) else {
            return;
        };
        let line: std::sync::Arc<str> = json!({
            "method": "agent_event",
            "params": {"chat": chat, "event": event},
        })
        .to_string()
        .into();
        for client in &self.event_clients {
            if let Some(writer) = self.clients.get(client) {
                let _ = writer.send(line.clone());
            }
        }
    }

    /// `chat.get`: the chat as the server has it, from message `from` on (all by default).
    pub(super) fn get_chat(&mut self, params: &Value) -> Result<Value, String> {
        let root = root_param(params)?;
        let id = str_param(params, "chat")?;
        self.load_chat(&root, &id)?;
        let config = self.ensure_config(&id);
        let chat = &self.chats[&id];
        let messages = &chat.session.messages;
        let from = (params["from"].as_u64().unwrap_or(0) as usize).min(messages.len());
        let streaming = chat.run.is_some() && messages.last().is_some_and(|m| m.streaming);
        Ok(json!({
            "id": id,
            "title": chat.session.title,
            "delegated": chat.session.delegated,
            "config": config,
            "modified": unix_secs(chat.session.modified),
            "from": from,
            "count": messages.len(),
            "messages": protocol::messages_to_json(&messages[from..]),
            "hash": protocol::messages_hash(messages),
            "streaming": streaming,
            "running": chat.run.is_some(),
            "queued": chat.queued.len(),
            "approval": chat.pending_approval,
            "plan": chat.plan_mode,
            "evidence": chat.evidence_mode.enabled(),
            "context": chat.session.agent_context.map(|(_, used, size)| json!({"used": used, "size": size})),
        }))
    }

    /// `chat.save`: replace an idle chat's transcript, title or config with the app's.
    pub(super) fn save_chat(&mut self, params: &Value) -> Result<Value, String> {
        let root = root_param(params)?;
        let id = str_param(params, "chat")?;
        let chat = self.load_chat(&root, &id)?;
        if chat.run.is_some() {
            return Err("The chat is answering".into());
        }
        let mut changed = false;
        let mut reset_agent = false;
        if let Some(entries) = params.get("messages").filter(|m| m.is_array()) {
            let messages = protocol::messages_from_json(entries);
            if protocol::messages_hash(&messages) != protocol::messages_hash(&chat.session.messages)
            {
                chat.session.messages = messages;
                chat.session.wire_cache = None;
                changed = true;
                reset_agent = true;
            }
        }
        if let Some(title) = params["title"].as_str().map(str::trim)
            && !title.is_empty()
            && title != chat.session.title
        {
            chat.session.title = title.to_string();
            changed = true;
        }
        if let Some(config) = params
            .get("config")
            .and_then(|c| serde_json::from_value(c.clone()).ok())
            && chat.session.config.as_ref() != Some(&config)
        {
            chat.session.config = Some(config);
            chat.session.wire_cache = None;
            changed = true;
        }
        if !changed {
            return Ok(json!({"changed": false}));
        }
        chat.save()?;
        let title = chat.session.title.clone();
        if reset_agent {
            // The agent's own memory of the chat no longer matches the transcript.
            self.acp.close(&id);
        }
        self.notify("chat_reloaded", json!({"chat": id}));
        self.notify("title", json!({"chat": id, "title": title}));
        self.notify("chats_changed", json!({"root": root}));
        Ok(json!({"changed": true}))
    }

    /// Apply the app's transcript and config sent with `prompt.send` to the chat about to run.
    pub(super) fn apply_app_prompt_params(
        &mut self,
        chat: &mut Chat,
        params: &Value,
    ) -> Result<(), String> {
        if let Some(entries) = params.get("history").filter(|h| h.is_array()) {
            let messages = protocol::messages_from_json(entries);
            if protocol::messages_hash(&messages) != protocol::messages_hash(&chat.session.messages)
            {
                log::info!(
                    "remote app: {} takes the app's transcript ({} messages)",
                    chat.id().unwrap_or("new chat"),
                    messages.len()
                );
                chat.session.messages = messages;
                chat.session.wire_cache = None;
                if let Some(id) = chat.id() {
                    self.acp.close(id);
                }
            }
        }
        if let Some(config) = params
            .get("config")
            .and_then(|c| serde_json::from_value(c.clone()).ok())
            && chat.session.config.as_ref() != Some(&config)
        {
            chat.session.config = Some(config);
            chat.session.wire_cache = None;
        }
        if let Some(title) = params["title"].as_str().map(str::trim)
            && !title.is_empty()
            && chat.session.messages.is_empty()
        {
            chat.session.title = title.to_string();
        }
        Ok(())
    }
}

/// Refuse a prompt whose sender has another transcript than the server, unless it sent its own.
pub(super) fn check_app_history(chat: &Chat, params: &Value) -> Result<(), String> {
    if let Some(hash) = params["history_hash"].as_str()
        && params.get("history").is_none_or(Value::is_null)
        && protocol::messages_hash(&chat.session.messages) != hash
    {
        return Err(HISTORY_MISMATCH.into());
    }
    Ok(())
}

/// `server.info`: who and where the server is, for the app's connect dialog.
pub(super) fn server_info() -> Value {
    let home = dirs::home_dir().map(|h| h.to_string_lossy().to_string());
    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "os": std::env::consts::OS,
        "host": host_name(),
        "home": home,
        "general": crate::app_dirs::general_chats_dir().to_string_lossy(),
        "features": [protocol::FEATURE],
    })
}

pub(super) fn host_name() -> String {
    #[cfg(windows)]
    if let Some(name) = std::env::var("COMPUTERNAME").ok().filter(|s| !s.is_empty()) {
        return name;
    }
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().trim_end_matches(".local").to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "server".into())
}

/// `fs.list`: the folders (and, with `files`, the files) in `path`, for picking a workspace.
pub(super) fn list_dir(params: &Value) -> Result<Value, String> {
    let path = str_param(params, "path")?;
    let path = if path == "~" {
        dirs::home_dir().ok_or("No home folder")?
    } else {
        std::path::PathBuf::from(path)
    };
    let with_files = params["files"].as_bool().unwrap_or(false);
    let hidden = params["hidden"].as_bool().unwrap_or(false);
    let mut entries: Vec<(bool, String)> = std::fs::read_dir(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            if !hidden && name.starts_with('.') {
                return None;
            }
            let meta = std::fs::metadata(entry.path()).ok()?;
            let dir = meta.is_dir();
            (dir || (with_files && meta.is_file())).then_some((dir, name))
        })
        .collect();
    entries.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase()))
    });
    let canonical = path.canonicalize().unwrap_or(path);
    Ok(json!({
        "path": canonical.to_string_lossy(),
        "parent": canonical.parent().map(|p| p.to_string_lossy().to_string()),
        "entries": entries
            .into_iter()
            .map(|(dir, name)| json!({"name": name, "dir": dir}))
            .collect::<Vec<_>>(),
    }))
}

fn unix_millis(time: Option<std::time::SystemTime>) -> Option<u64> {
    time?
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as u64)
}

fn meta_json(meta: &std::fs::Metadata) -> Value {
    json!({
        "exists": true,
        "dir": meta.is_dir(),
        "file": meta.is_file(),
        "len": meta.len(),
        "modified": unix_millis(meta.modified().ok()),
    })
}

/// `fs.stat`: what [`crate::vfs::Meta`] holds; `exists: false` rather than an error.
pub(super) fn fs_stat(params: &Value) -> Result<Value, String> {
    let path = str_param(params, "path")?;
    Ok(match std::fs::metadata(&path) {
        Ok(meta) => meta_json(&meta),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({"exists": false}),
        Err(e) => return Err(format!("{path}: {e}")),
    })
}

/// `fs.read`: a file's bytes (base64) and metadata. `max` refuses larger files.
pub(super) fn fs_read(params: &Value) -> Result<Value, String> {
    use base64::Engine as _;
    let path = str_param(params, "path")?;
    let meta = std::fs::metadata(&path).map_err(|e| io_error(&path, &e))?;
    if let Some(max) = params["max"].as_u64()
        && meta.len() > max
    {
        return Err(format!("{path}: the file is larger than {max} bytes"));
    }
    let bytes = std::fs::read(&path).map_err(|e| io_error(&path, &e))?;
    Ok(json!({
        "data": base64::engine::general_purpose::STANDARD.encode(bytes),
        "meta": meta_json(&meta),
    }))
}

/// `fs.write`: replace a file's bytes (base64), creating it.
pub(super) fn fs_write(params: &Value) -> Result<Value, String> {
    use base64::Engine as _;
    let path = str_param(params, "path")?;
    let data = base64::engine::general_purpose::STANDARD
        .decode(params["data"].as_str().unwrap_or_default())
        .map_err(|e| format!("bad data: {e}"))?;
    std::fs::write(&path, data).map_err(|e| io_error(&path, &e))?;
    let meta = std::fs::metadata(&path).map_err(|e| io_error(&path, &e))?;
    Ok(meta_json(&meta))
}

/// `fs.op`: the explorer's file operations.
pub(super) fn fs_op(params: &Value) -> Result<Value, String> {
    let path = str_param(params, "path")?;
    let result = match params["op"].as_str().unwrap_or_default() {
        "create_file" => std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map(|_| ()),
        "create_dir" => std::fs::create_dir(&path),
        "create_dir_all" => std::fs::create_dir_all(&path),
        "remove_file" => std::fs::remove_file(&path),
        "remove_dir" => std::fs::remove_dir(&path),
        "rename" => std::fs::rename(&path, str_param(params, "to")?),
        other => return Err(format!("unknown fs op: {other}")),
    };
    result.map_err(|e| io_error(&path, &e))?;
    Ok(Value::Null)
}

/// `kind:` prefix lets the app rebuild the `io::ErrorKind` callers branch on.
fn io_error(path: &str, e: &std::io::Error) -> String {
    let kind = match e.kind() {
        std::io::ErrorKind::NotFound => "not_found",
        std::io::ErrorKind::AlreadyExists => "already_exists",
        std::io::ErrorKind::PermissionDenied => "permission_denied",
        std::io::ErrorKind::InvalidData => "invalid_data",
        _ => "other",
    };
    format!("{kind}:{path}: {e}")
}

/// The `fs.*` requests, answered off the server's main thread.
pub(super) fn fs_request(method: &str, params: &Value) -> Result<Value, String> {
    match method {
        "fs.list" => list_dir(params),
        "fs.read" => fs_read(params),
        "fs.write" => fs_write(params),
        "fs.stat" => fs_stat(params),
        "fs.stat_many" => {
            let mut out = serde_json::Map::new();
            for path in params["paths"].as_array().into_iter().flatten() {
                if let Some(path) = path.as_str() {
                    out.insert(path.to_string(), fs_stat(&json!({"path": path}))?);
                }
            }
            Ok(Value::Object(out))
        }
        "fs.op" => fs_op(params),
        "fs.git" => crate::git::remote::serve(params),
        "fs.walk" => {
            root_param(params).map(|root| json!(crate::app::workspace_file_list(Path::new(&root))))
        }
        "fs.search" => {
            root_param(params).map(|root| crate::app::search_json(Path::new(&root), params))
        }
        other => Err(format!("unknown method: {other}")),
    }
}
