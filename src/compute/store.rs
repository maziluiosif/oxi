//! Persist SSH passwords for remote compute targets, separate from `settings.json`
//! (`~/.config/oxi/ssh_credentials.json`), keyed by [`crate::settings::ProviderProfile::id`].
//!
//! Same trust model as `oauth.json` (see [`crate::oauth::store`]): plaintext JSON on disk
//! in the app config directory, not an OS keychain. Kept out of `settings.json` so the
//! password isn't dragged along whenever settings are read, logged, or exported.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SshCredentialStore {
    /// Provider profile id -> SSH password.
    #[serde(default)]
    passwords: HashMap<String, String>,
}

pub fn ssh_credentials_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("oxi")
        .join("ssh_credentials.json")
}

pub fn load_ssh_credentials() -> SshCredentialStore {
    let path = ssh_credentials_path();
    if let Ok(bytes) = fs::read(&path) {
        if let Ok(s) = serde_json::from_slice::<SshCredentialStore>(&bytes) {
            return s;
        }
    }
    SshCredentialStore::default()
}

pub fn save_ssh_credentials(store: &SshCredentialStore) -> Result<(), String> {
    let path = ssh_credentials_path();
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(store).map_err(|e| e.to_string())?;
    fs::write(&path, json).map_err(|e| e.to_string())?;
    restrict_permissions(&path);
    Ok(())
}

/// Best-effort: restrict the credentials file to owner read/write on Unix. A failure here
/// shouldn't block saving the credentials.
#[cfg(unix)]
fn restrict_permissions(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &std::path::Path) {}

impl SshCredentialStore {
    pub fn get(&self, profile_id: &str) -> Option<&str> {
        self.passwords.get(profile_id).map(String::as_str)
    }

    pub fn set(&mut self, profile_id: impl Into<String>, password: impl Into<String>) {
        self.passwords.insert(profile_id.into(), password.into());
    }

    pub fn clear(&mut self, profile_id: &str) {
        self.passwords.remove(profile_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_missing_returns_none() {
        let s = SshCredentialStore::default();
        assert_eq!(s.get("nope"), None);
    }

    #[test]
    fn set_then_get_roundtrips() {
        let mut s = SshCredentialStore::default();
        s.set("p1", "hunter2");
        assert_eq!(s.get("p1"), Some("hunter2"));
    }

    #[test]
    fn clear_removes_entry() {
        let mut s = SshCredentialStore::default();
        s.set("p1", "hunter2");
        s.clear("p1");
        assert_eq!(s.get("p1"), None);
    }

    #[test]
    fn serde_roundtrip() {
        let mut s = SshCredentialStore::default();
        s.set("p1", "hunter2");
        let json = serde_json::to_string(&s).unwrap();
        let back: SshCredentialStore = serde_json::from_str(&json).unwrap();
        assert_eq!(back.get("p1"), Some("hunter2"));
    }
}
