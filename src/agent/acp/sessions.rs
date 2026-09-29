//! Persisted ACP session ids, so a chat reopened after an oxi restart (or after its agent
//! subprocess was relaunched) resumes the agent's own session instead of starting a blank one.
//!
//! ```text
//! <data_dir>/oxi/acp/sessions.json   { "<oxi session key>": { command_line, cwd, session_id } }
//! ```
//!
//! Only file-backed oxi sessions are stored; synthetic `mem:` keys don't survive a restart. An
//! entry is only reused when the launch command and workspace still match, since a session id is
//! meaningless to a different agent and bound to the cwd it was created in.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize, PartialEq, Debug)]
struct Stored {
    command_line: String,
    cwd: String,
    session_id: String,
}

/// Serializes read-modify-write cycles of the store file across concurrent session launches.
static LOCK: Mutex<()> = Mutex::new(());

fn store_path() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("oxi")
        .join("acp")
        .join("sessions.json")
}

fn persistable(key: &str) -> bool {
    !key.is_empty() && !key.starts_with("mem:")
}

fn read(path: &Path) -> HashMap<String, Stored> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn write(path: &Path, map: &HashMap<String, Stored>) {
    let Ok(json) = serde_json::to_string_pretty(map) else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, json).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

fn lookup_in(path: &Path, key: &str, command_line: &str, cwd: &Path) -> Option<String> {
    if !persistable(key) {
        return None;
    }
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    read(path)
        .remove(key)
        .filter(|s| s.command_line == command_line && s.cwd == cwd.to_string_lossy())
        .map(|s| s.session_id)
}

fn remember_in(path: &Path, key: &str, command_line: &str, cwd: &Path, session_id: &str) {
    if !persistable(key) {
        return;
    }
    let entry = Stored {
        command_line: command_line.to_string(),
        cwd: cwd.to_string_lossy().into_owned(),
        session_id: session_id.to_string(),
    };
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut map = read(path);
    if map.get(key) == Some(&entry) {
        return;
    }
    map.insert(key.to_string(), entry);
    write(path, &map);
}

fn forget_in(path: &Path, key: &str) {
    if !persistable(key) {
        return;
    }
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut map = read(path);
    if map.remove(key).is_some() {
        write(path, &map);
    }
}

/// The agent session id previously bound to this oxi session, if it was created by the same
/// launch command in the same workspace.
pub(super) fn lookup(key: &str, command_line: &str, cwd: &Path) -> Option<String> {
    lookup_in(&store_path(), key, command_line, cwd)
}

/// Bind an agent session id to this oxi session, replacing any previous binding.
pub(super) fn remember(key: &str, command_line: &str, cwd: &Path, session_id: &str) {
    remember_in(&store_path(), key, command_line, cwd, session_id)
}

/// Drop the binding (the oxi session was deleted).
pub(super) fn forget(key: &str) {
    forget_in(&store_path(), key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_matches_command_and_cwd() {
        let dir = std::env::temp_dir().join(format!("oxi-acp-sessions-{}", std::process::id()));
        let path = dir.join("sessions.json");
        let cwd = Path::new("/work");
        remember_in(&path, "/s/a.json", "codex-acp", cwd, "sid-1");
        assert_eq!(
            lookup_in(&path, "/s/a.json", "codex-acp", cwd).as_deref(),
            Some("sid-1")
        );
        assert_eq!(lookup_in(&path, "/s/a.json", "other", cwd), None);
        assert_eq!(
            lookup_in(&path, "/s/a.json", "codex-acp", Path::new("/else")),
            None
        );
        remember_in(&path, "mem:0:0", "codex-acp", cwd, "sid-2");
        assert_eq!(lookup_in(&path, "mem:0:0", "codex-acp", cwd), None);
        forget_in(&path, "/s/a.json");
        assert_eq!(lookup_in(&path, "/s/a.json", "codex-acp", cwd), None);
        let _ = std::fs::remove_dir_all(dir);
    }
}
