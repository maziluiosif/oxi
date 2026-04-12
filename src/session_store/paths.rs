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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn temp_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_else(|_| Duration::from_secs(0))
            .as_nanos();
        let path = std::env::temp_dir().join(format!("oxi-paths-{name}-{nanos}"));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn generate_session_id_unique() {
        let a = generate_session_id();
        std::thread::sleep(Duration::from_millis(1));
        let b = generate_session_id();
        assert_ne!(a, b);
        assert!(a.starts_with("session-"));
    }

    #[test]
    fn default_session_dir_under_agent_dir() {
        let root = Path::new("/tmp/my-project");
        let agent = Path::new("/home/user/.config/oxi");
        let dir = default_session_dir(root, agent);
        assert!(dir.starts_with(agent.join("sessions")));
        // Should contain sanitized path
        let dir_str = dir.to_string_lossy();
        assert!(dir_str.contains("tmp-my-project"));
    }

    #[test]
    fn default_session_dir_sanitizes_slashes() {
        let root = Path::new("/a/b/c");
        let agent = Path::new("/agent");
        let dir = default_session_dir(root, agent);
        let name = dir.file_name().unwrap().to_string_lossy();
        assert!(!name.contains('/'));
        assert!(name.starts_with("--"));
        assert!(name.ends_with("--"));
    }

    #[test]
    fn configured_session_dir_returns_none_without_settings() {
        let root = temp_dir("no-settings");
        let agent = temp_dir("agent-no-settings");
        assert!(configured_session_dir(&root, &agent).is_none());
    }

    #[test]
    fn configured_session_dir_reads_project_setting() {
        let root = temp_dir("with-settings");
        let agent = temp_dir("agent-with-settings");
        let pi_dir = root.join(".pi");
        fs::create_dir_all(&pi_dir).unwrap();
        fs::write(
            pi_dir.join("settings.json"),
            r#"{"sessionDir": ".sessions"}"#,
        )
        .unwrap();
        let dir = configured_session_dir(&root, &agent);
        assert!(dir.is_some());
        assert!(dir.unwrap().ends_with(".sessions"));
    }

    #[test]
    fn expand_tilde_home() {
        let expanded = expand_tilde("~");
        if std::env::var_os("HOME").is_some() {
            assert_ne!(expanded, PathBuf::from("~"));
        }
    }

    #[test]
    fn expand_tilde_with_path() {
        let expanded = expand_tilde("~/projects");
        if std::env::var_os("HOME").is_some() {
            assert!(expanded.to_string_lossy().contains("projects"));
            assert!(!expanded.to_string_lossy().starts_with('~'));
        }
    }

    #[test]
    fn expand_tilde_absolute() {
        let expanded = expand_tilde("/absolute/path");
        assert_eq!(expanded, PathBuf::from("/absolute/path"));
    }
}
