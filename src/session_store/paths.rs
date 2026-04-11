use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::model::Session;

pub fn configured_session_dir(root_path: &Path, agent_dir: &Path) -> Option<PathBuf> {
    let project = read_session_dir_setting(&root_path.join(".pi/settings.json"));
    let global = read_session_dir_setting(&agent_dir.join("settings.json"));
    project
        .or(global)
        .map(|value| resolve_session_dir(root_path, &value))
}

pub fn default_session_dir(root_path: &Path, agent_dir: &Path) -> PathBuf {
    let cwd = root_path.to_string_lossy();
    let trimmed = cwd.trim_start_matches(['/', '\\']);
    let safe_path: String = trimmed
        .chars()
        .map(|ch| {
            if matches!(ch, '/' | '\\' | ':') {
                '-'
            } else {
                ch
            }
        })
        .collect();
    agent_dir.join("sessions").join(format!("--{safe_path}--"))
}

pub fn session_file_path(root_path: &str, session: &Session) -> Result<PathBuf, String> {
    if let Some(existing) = session.session_file.as_deref() {
        return Ok(PathBuf::from(existing));
    }
    let dir = configured_session_dir(Path::new(root_path), &agent_dir())
        .unwrap_or_else(|| default_session_dir(Path::new(root_path), &agent_dir()));
    Ok(dir.join(format!("{}.jsonl", generate_session_id())))
}

pub fn generate_session_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("session-{nanos}")
}

pub fn agent_dir() -> PathBuf {
    if let Ok(value) = std::env::var("PI_CODING_AGENT_DIR") {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return expand_tilde(trimmed);
        }
    }

    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".config").join("oxi")
}

fn read_session_dir_setting(path: &Path) -> Option<String> {
    let content = fs::read_to_string(path).ok()?;
    let value = serde_json::from_str::<Value>(&content).ok()?;
    value
        .get("sessionDir")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn resolve_session_dir(root_path: &Path, raw: &str) -> PathBuf {
    let expanded = expand_tilde(raw);
    if expanded.is_absolute() {
        expanded
    } else {
        root_path.join(expanded)
    }
}

fn expand_tilde(raw: &str) -> PathBuf {
    if raw == "~" {
        return std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(raw));
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(raw)
}
