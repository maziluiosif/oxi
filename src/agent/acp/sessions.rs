//! Persisted ACP session ids, so a chat reopened after an oxi restart (or after its agent
//! subprocess was relaunched) resumes the agent's own session instead of starting a blank one.
//!
//! ```text
//! <data_dir>/oxi/acp/sessions.json   { "<oxi session key>": [{ command_line, cwd, session_id }] }
//! ```
//!
//! One entry per agent (launch command), so a chat that moves Claude Code → Codex → Claude Code
//! returns to Claude Code's own session. Only file-backed oxi sessions are stored; synthetic
//! `mem:` keys don't survive a restart. An entry is only reused when the workspace still
//! matches, since a session id is bound to the cwd it was created in.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize, PartialEq, Debug)]
struct Stored {
    command_line: String,
    cwd: String,
    session_id: String,
    /// Legacy deferred fork: never resume or copy this mutable parent session. The new
    /// session receives the fork's saved transcript instead.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    fork: bool,
}

/// A chat's agent sessions. Older builds stored a single one.
#[derive(Clone, Serialize, Deserialize, PartialEq, Debug)]
#[serde(untagged)]
enum Entry {
    Many(Vec<Stored>),
    One(Stored),
}

impl Entry {
    fn into_vec(self) -> Vec<Stored> {
        match self {
            Entry::Many(list) => list,
            Entry::One(one) => vec![one],
        }
    }
}

/// Serializes read-modify-write cycles of the store file across concurrent session launches.
static LOCK: Mutex<()> = Mutex::new(());

fn store_path() -> PathBuf {
    crate::app_dirs::data_dir()
        .join("acp")
        .join("sessions.json")
}

fn persistable(key: &str) -> bool {
    !key.is_empty() && !key.starts_with("mem:")
}

fn read(path: &Path) -> HashMap<String, Vec<Stored>> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<HashMap<String, Entry>>(&s).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|(key, entry)| (key, entry.into_vec()))
        .collect()
}

fn write(path: &Path, map: &HashMap<String, Vec<Stored>>) {
    let Ok(json) = serde_json::to_string_pretty(map) else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = crate::fsutil::write_atomic(path, json.as_bytes());
}

fn find_in(path: &Path, key: &str, command_line: &str, cwd: &Path) -> Option<Stored> {
    if !persistable(key) {
        return None;
    }
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    read(path)
        .remove(key)?
        .into_iter()
        .find(|s| s.command_line == command_line && s.cwd == cwd.to_string_lossy())
}

fn lookup_in(path: &Path, key: &str, command_line: &str, cwd: &Path) -> Option<String> {
    find_in(path, key, command_line, cwd)
        .filter(|s| !s.fork)
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
        fork: false,
    };
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut map = read(path);
    let list = map.entry(key.to_string()).or_default();
    if list.contains(&entry) {
        return;
    }
    list.retain(|s| s.command_line != command_line);
    list.push(entry);
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

/// Every agent session an oxi chat is bound to.
pub(super) fn known_ids() -> std::collections::HashSet<String> {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    read(&store_path())
        .into_values()
        .flatten()
        .map(|s| s.session_id)
        .collect()
}

/// The agent session id previously bound to this oxi session, if it was created by the same
/// launch command in the same workspace.
pub(super) fn lookup(key: &str, command_line: &str, cwd: &Path) -> Option<String> {
    lookup_in(&store_path(), key, command_line, cwd)
}

/// Look up a persisted session id under `command_line`, falling back to legacy launch commands
/// (e.g. an older ACP adapter command whose native session id is reusable by the official CLI).
pub(super) fn lookup_with_fallbacks(
    key: &str,
    command_line: &str,
    cwd: &Path,
    fallbacks: &[&str],
) -> Option<String> {
    let path = store_path();
    if let Some(id) = lookup_in(&path, key, command_line, cwd) {
        return Some(id);
    }
    fallbacks
        .iter()
        .find_map(|legacy| lookup_in(&path, key, legacy, cwd))
}

/// Bind an agent session id to this oxi session, replacing that agent's previous binding.
pub(super) fn remember(key: &str, command_line: &str, cwd: &Path, session_id: &str) {
    remember_in(&store_path(), key, command_line, cwd, session_id)
}

/// Drop every agent's binding (the oxi session was deleted or its transcript rewritten).
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

    #[test]
    fn each_agent_keeps_its_own_session() {
        let dir =
            std::env::temp_dir().join(format!("oxi-acp-sessions-many-{}", std::process::id()));
        let path = dir.join("sessions.json");
        let cwd = Path::new("/work");
        remember_in(&path, "/s/a.json", "claude-acp", cwd, "c-1");
        remember_in(&path, "/s/a.json", "codex-acp", cwd, "x-1");
        remember_in(&path, "/s/a.json", "claude-acp", cwd, "c-2");
        assert_eq!(
            lookup_in(&path, "/s/a.json", "claude-acp", cwd).as_deref(),
            Some("c-2")
        );
        assert_eq!(
            lookup_in(&path, "/s/a.json", "codex-acp", cwd).as_deref(),
            Some("x-1")
        );
        forget_in(&path, "/s/a.json");
        assert_eq!(lookup_in(&path, "/s/a.json", "codex-acp", cwd), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn legacy_deferred_forks_start_from_the_saved_transcript() {
        let dir =
            std::env::temp_dir().join(format!("oxi-acp-sessions-fork-{}", std::process::id()));
        let path = dir.join("sessions.json");
        let cwd = Path::new("/work");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, r#"{"/s/b.json":[{"command_line":"claude-acp","cwd":"/work","session_id":"mutable-parent","fork":true}]}"#).unwrap();
        assert_eq!(lookup_in(&path, "/s/b.json", "claude-acp", cwd), None);
        remember_in(&path, "/s/b.json", "claude-acp", cwd, "independent-session");
        assert_eq!(
            lookup_in(&path, "/s/b.json", "claude-acp", cwd).as_deref(),
            Some("independent-session")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reads_the_single_session_format() {
        let dir = std::env::temp_dir().join(format!("oxi-acp-sessions-old-{}", std::process::id()));
        let path = dir.join("sessions.json");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            &path,
            r#"{"/s/a.json":{"command_line":"codex-acp","cwd":"/work","session_id":"old"}}"#,
        )
        .unwrap();
        assert_eq!(
            lookup_in(&path, "/s/a.json", "codex-acp", Path::new("/work")).as_deref(),
            Some("old")
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
