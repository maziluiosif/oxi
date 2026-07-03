//! Persist SSH passwords for remote compute targets in the OS keychain (see
//! `crate::secrets`), under the account `"ssh-credentials"`, keyed internally by
//! [`crate::settings::ProviderProfile::id`]. Used to live as plaintext JSON at
//! `~/.config/oxi/ssh_credentials.json`; [`load_ssh_credentials`] migrates any leftover
//! file from that era on first read.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SshCredentialStore {
    /// Provider profile id -> SSH password.
    #[serde(default)]
    passwords: HashMap<String, String>,
}

const KEYCHAIN_ACCOUNT: &str = "ssh-credentials";

/// In-process cache so repeated lazy-loads in the settings UI don't each round-trip to
/// the OS keychain. Kept in sync by [`save_ssh_credentials`].
static CACHE: Mutex<Option<SshCredentialStore>> = Mutex::new(None);

/// Legacy location from before SSH passwords moved into the OS keychain.
fn legacy_credentials_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("oxi")
        .join("ssh_credentials.json")
}

pub fn load_ssh_credentials() -> SshCredentialStore {
    if let Some(s) = CACHE.lock().unwrap().as_ref() {
        return s.clone();
    }
    let stored = crate::secrets::load(KEYCHAIN_ACCOUNT);
    let store = if !stored.is_empty() {
        serde_json::from_str(&stored).unwrap_or_default()
    } else {
        migrate_legacy_file().unwrap_or_default()
    };
    *CACHE.lock().unwrap() = Some(store.clone());
    store
}

/// One-time migration: if a pre-keychain `ssh_credentials.json` exists, push its contents
/// into the keychain and delete the file. Runs at most once per process (guarded by the
/// cache check in [`load_ssh_credentials`]).
fn migrate_legacy_file() -> Option<SshCredentialStore> {
    let path = legacy_credentials_path();
    let bytes = fs::read(&path).ok()?;
    let store: SshCredentialStore = serde_json::from_slice(&bytes).ok()?;
    if !store.passwords.is_empty() {
        if let Ok(json) = serde_json::to_string(&store) {
            let _ = crate::secrets::store(KEYCHAIN_ACCOUNT, &json);
        }
    }
    let _ = fs::remove_file(&path);
    Some(store)
}

pub fn save_ssh_credentials(store: &SshCredentialStore) -> Result<(), String> {
    let result = if store.passwords.is_empty() {
        crate::secrets::delete(KEYCHAIN_ACCOUNT)
    } else {
        let json = serde_json::to_string(store).map_err(|e| e.to_string())?;
        crate::secrets::store(KEYCHAIN_ACCOUNT, &json)
    };
    *CACHE.lock().unwrap() = Some(store.clone());
    result
}

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
