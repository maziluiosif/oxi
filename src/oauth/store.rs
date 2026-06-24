//! Persist OAuth tokens next to app settings (`~/.config/oxi/oauth.json`).

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

pub fn oauth_config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("oxi")
        .join("oauth.json")
}

pub fn load_oauth_store() -> OAuthStore {
    let path = oauth_config_path();
    if let Ok(bytes) = fs::read(&path) {
        if let Ok(s) = serde_json::from_slice::<OAuthStore>(&bytes) {
            return s;
        }
    }
    OAuthStore::default()
}

pub fn save_oauth_store(store: &OAuthStore) -> Result<(), String> {
    let path = oauth_config_path();
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(store).map_err(|e| e.to_string())?;
    fs::write(&path, json).map_err(|e| e.to_string())
}

pub fn merge_codex(store: &mut OAuthStore, rec: CodexOAuthRecord) {
    store.openai_codex = Some(rec);
}

pub fn clear_codex(store: &mut OAuthStore) {
    store.openai_codex = None;
}
