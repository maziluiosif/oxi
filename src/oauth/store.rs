//! Persist OAuth tokens in the OS keychain (see `crate::secrets`), under the account
//! `"oauth-codex"`. Used to live as plaintext JSON at `~/.config/oxi/oauth.json`;
//! [`load_oauth_store`] migrates any leftover file from that era on first read.

use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CodexOAuthRecord {
    pub refresh_token: String,
    pub access_token: String,
    pub expires_ms: i64,
    pub account_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OAuthStore {
    pub openai_codex: Option<CodexOAuthRecord>,
}

const KEYCHAIN_ACCOUNT: &str = "oauth-codex";

/// In-process cache so frequent UI-thread reads (e.g. the composer's model selector,
/// rendered every frame) don't each round-trip to the OS keychain. Kept in sync by
/// [`save_oauth_store`].
static CACHE: Mutex<Option<OAuthStore>> = Mutex::new(None);

/// Legacy location from before OAuth tokens moved into the OS keychain.
fn legacy_oauth_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("oxi")
        .join("oauth.json")
}

pub fn load_oauth_store() -> OAuthStore {
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

/// One-time migration: if a pre-keychain `oauth.json` exists, push its contents into the
/// keychain and delete the file. Runs at most once per process (guarded by the cache
/// check in [`load_oauth_store`]).
fn migrate_legacy_file() -> Option<OAuthStore> {
    let path = legacy_oauth_path();
    let bytes = fs::read(&path).ok()?;
    let store: OAuthStore = serde_json::from_slice(&bytes).ok()?;
    if store.openai_codex.is_some() {
        if let Ok(json) = serde_json::to_string(&store) {
            let _ = crate::secrets::store(KEYCHAIN_ACCOUNT, &json);
        }
    }
    let _ = fs::remove_file(&path);
    Some(store)
}

pub fn save_oauth_store(store: &OAuthStore) -> Result<(), String> {
    let result = if store.openai_codex.is_none() {
        crate::secrets::delete(KEYCHAIN_ACCOUNT)
    } else {
        let json = serde_json::to_string(store).map_err(|e| e.to_string())?;
        crate::secrets::store(KEYCHAIN_ACCOUNT, &json)
    };
    *CACHE.lock().unwrap() = Some(store.clone());
    result
}

pub fn merge_codex(store: &mut OAuthStore, rec: CodexOAuthRecord) {
    store.openai_codex = Some(rec);
}

pub fn clear_codex(store: &mut OAuthStore) {
    store.openai_codex = None;
}
