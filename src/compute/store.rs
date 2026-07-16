//! Persist SSH passwords for remote compute targets in the OS keychain, as part of the
//! unified secrets blob (see `crate::secrets::UnifiedSecrets`), keyed internally by
//! [`crate::settings::LlmProviderKind::slug`] (old files used profile ids; settings
//! migration re-keys them). Used to live as plaintext JSON at
//! `~/.config/oxi/ssh_credentials.json`, then under its own keychain item
//! (`"ssh-credentials"`); [`load_ssh_credentials`] migrates any leftover file from the
//! plaintext era on first read (the keychain-item era is migrated by
//! `crate::secrets::load_unified`).

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SshCredentialStore {
    /// Provider slug -> SSH password (old entries may still be keyed by profile id).
    #[serde(default)]
    passwords: HashMap<String, String>,
}

/// Legacy location from before SSH passwords moved into the OS keychain.
fn legacy_credentials_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("oxi")
        .join("ssh_credentials.json")
}

pub fn load_ssh_credentials() -> SshCredentialStore {
    let unified = crate::secrets::load_unified();
    if !unified.ssh.passwords.is_empty() {
        return unified.ssh;
    }
    migrate_legacy_file().unwrap_or(unified.ssh)
}

/// One-time migration: if a pre-keychain `ssh_credentials.json` exists, push its contents
/// into the unified secrets blob and delete the file.
fn migrate_legacy_file() -> Option<SshCredentialStore> {
    let path = legacy_credentials_path();
    let bytes = fs::read(&path).ok()?;
    let store: SshCredentialStore = serde_json::from_slice(&bytes).ok()?;
    if !store.passwords.is_empty() && save_ssh_credentials(&store).is_err() {
        // Preserve the legacy source until the credential-store migration really succeeded.
        return Some(store);
    }
    let _ = fs::remove_file(&path);
    Some(store)
}

pub fn save_ssh_credentials(store: &SshCredentialStore) -> Result<(), String> {
    let mut unified = crate::secrets::load_unified();
    unified.ssh = store.clone();
    crate::secrets::save_unified(&unified)
}

impl SshCredentialStore {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.passwords.get(key).map(String::as_str)
    }

    pub fn set(&mut self, key: impl Into<String>, password: impl Into<String>) {
        self.passwords.insert(key.into(), password.into());
    }

    pub fn clear(&mut self, key: &str) {
        self.passwords.remove(key);
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
