//! Persist OAuth tokens in the OS keychain, as part of the unified secrets blob (see
//! `crate::secrets::UnifiedSecrets`). Used to live as plaintext JSON at
//! `~/.config/oxi/oauth.json`, then under its own keychain item (`"oauth-codex"`);
//! [`load_oauth_store`] migrates any leftover file from the plaintext era on first read
//! (the keychain-item era is migrated by `crate::secrets::load_unified`).

use std::fs;
use std::path::PathBuf;

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

/// Legacy location from before OAuth tokens moved into the OS keychain.
fn legacy_oauth_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("oxi")
        .join("oauth.json")
}

pub fn load_oauth_store() -> OAuthStore {
    let unified = crate::secrets::load_unified();
    if unified.oauth.openai_codex.is_some() {
        return unified.oauth;
    }
    migrate_legacy_file().unwrap_or(unified.oauth)
}

/// One-time migration: if a pre-keychain `oauth.json` exists, push its contents into the
/// unified secrets blob and delete the file.
fn migrate_legacy_file() -> Option<OAuthStore> {
    let path = legacy_oauth_path();
    let bytes = fs::read(&path).ok()?;
    let store: OAuthStore = serde_json::from_slice(&bytes).ok()?;
    if store.openai_codex.is_some() && save_oauth_store(&store).is_err() {
        // Keep the plaintext legacy file until the keychain write is confirmed. Deleting it on a
        // failed migration would permanently sign the user out.
        return Some(store);
    }
    let _ = fs::remove_file(&path);
    Some(store)
}

pub fn save_oauth_store(store: &OAuthStore) -> Result<(), String> {
    let mut unified = crate::secrets::load_unified();
    unified.oauth = store.clone();
    crate::secrets::save_unified(&unified)
}

pub fn merge_codex(store: &mut OAuthStore, rec: CodexOAuthRecord) {
    store.openai_codex = Some(rec);
}

pub fn clear_codex(store: &mut OAuthStore) {
    store.openai_codex = None;
}
