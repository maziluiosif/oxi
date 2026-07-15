//! Thin wrapper around the OS credential store — macOS Keychain Services, Windows
//! Credential Manager, or the Linux Secret Service (via D-Bus) — used for everything
//! that used to live as plaintext JSON on disk: provider API keys, OAuth tokens, and
//! SSH passwords. See `oauth::store`, `compute::store`, and `settings::ProviderProfile`
//! for the call sites.
//!
//! This talks to `keyring-core` directly instead of going through the `keyring` crate's
//! `v1` compatibility shim. That shim lazily installs the platform-native store on first
//! `Entry::new`, gated by `AtomicBool::compare_exchange(false, true, ..) == Ok(true)` — but
//! a successful compare-exchange returns `Ok(false)` (the *previous* value), so that
//! condition is never true and the store never gets installed (as of keyring 4.1.3; every
//! `Entry::new` then fails with `NoDefaultStore`, which reads as "the keychain isn't
//! saving anything"). Installing the store ourselves at first use sidesteps the bug.

use std::collections::HashMap;
use std::sync::{Mutex, Once};

use keyring_core::Entry;

/// Keychain "service" name under which all oxi credentials are grouped.
const SERVICE: &str = "oxi";

static INIT_STORE: Once = Once::new();

/// Install the platform-native credential store as the `keyring-core` default, once per
/// process. Best-effort: if it fails (headless CI, no Secret Service session, etc.) every
/// subsequent `Entry::new` will surface `NoDefaultStore`, which callers already treat as a
/// best-effort failure.
fn ensure_default_store() {
    INIT_STORE.call_once(|| {
        #[cfg(target_os = "macos")]
        let store = apple_native_keyring_store::keychain::Store::new();
        #[cfg(target_os = "windows")]
        let store = windows_native_keyring_store::Store::new();
        #[cfg(all(
            unix,
            not(any(target_os = "macos", target_os = "ios", target_os = "android"))
        ))]
        let store = zbus_secret_service_keyring_store::Store::new();

        match store {
            Ok(store) => keyring_core::set_default_store(store),
            Err(e) => eprintln!("failed to initialize OS credential store: {e}"),
        }
    });
}

/// account -> last value this process wrote, so re-saving unrelated settings fields
/// (which re-touches every profile) doesn't re-hit the OS keychain for secrets that
/// haven't actually changed.
static WRITE_CACHE: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

/// Store `value` under `account`. An empty value deletes the entry instead (so clearing
/// a field in the UI removes the credential rather than leaving an empty string behind).
/// Best-effort: failures (locked keychain, no backend available, headless CI) are
/// returned but callers generally shouldn't let them block saving the rest of settings.
pub fn store(account: &str, value: &str) -> Result<(), String> {
    {
        let mut cache = WRITE_CACHE.lock().unwrap();
        if cache
            .get_or_insert_with(HashMap::new)
            .get(account)
            .map(String::as_str)
            == Some(value)
        {
            return Ok(());
        }
    }
    ensure_default_store();
    let result = if value.is_empty() {
        delete(account)
    } else {
        Entry::new(SERVICE, account)
            .and_then(|e| e.set_password(value))
            .map_err(|e| e.to_string())
    };
    if result.is_ok() {
        let mut cache = WRITE_CACHE.lock().unwrap();
        cache
            .get_or_insert_with(HashMap::new)
            .insert(account.to_string(), value.to_string());
    }
    result
}

/// Load the value stored under `account`, or `""` if there is none (never set, deleted,
/// or the platform credential store isn't available).
pub fn load(account: &str) -> String {
    ensure_default_store();
    Entry::new(SERVICE, account)
        .and_then(|e| e.get_password())
        .unwrap_or_default()
}

/// Remove the entry for `account`. Missing entries are not an error.
pub fn delete(account: &str) -> Result<(), String> {
    ensure_default_store();
    let result = match Entry::new(SERVICE, account) {
        Ok(entry) => match entry.delete_credential() {
            Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        },
        Err(e) => Err(e.to_string()),
    };
    if result.is_ok()
        && let Some(cache) = WRITE_CACHE.lock().unwrap().as_mut()
    {
        cache.remove(account);
    }
    result
}

/// Single keychain item that holds every secret oxi manages — provider API keys, the
/// Codex OAuth record, and SSH passwords. Before this existed, each of those lived under
/// its own account (`api-key:<slug>` x6, `oauth-codex`, `ssh-credentials`), and macOS
/// shows one authorization prompt per distinct item the first time a process touches it,
/// so a single launch could rack up several separate prompts. Collapsing them into one
/// JSON blob under one account means at most one prompt, ever, per signed build.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct UnifiedSecrets {
    #[serde(default)]
    pub provider_api_keys: HashMap<String, String>,
    #[serde(default)]
    pub oauth: crate::oauth::OAuthStore,
    #[serde(default)]
    pub ssh: crate::compute::store::SshCredentialStore,
}

const UNIFIED_ACCOUNT: &str = "oxi-secrets";

static UNIFIED_CACHE: Mutex<Option<UnifiedSecrets>> = Mutex::new(None);

/// Load the unified secrets blob, migrating from the old per-item accounts on first read
/// if the unified item doesn't exist yet. Cached for the rest of the process.
pub fn load_unified() -> UnifiedSecrets {
    if let Some(s) = UNIFIED_CACHE.lock().unwrap().as_ref() {
        return s.clone();
    }
    let stored = load_blob(UNIFIED_ACCOUNT);
    let unified = if !stored.is_empty() {
        serde_json::from_str(&stored).unwrap_or_default()
    } else {
        migrate_legacy_accounts()
    };
    *UNIFIED_CACHE.lock().unwrap() = Some(unified.clone());
    unified
}

pub fn save_unified(unified: &UnifiedSecrets) -> Result<(), String> {
    let json = serde_json::to_string(unified).map_err(|e| e.to_string())?;
    store_blob(UNIFIED_ACCOUNT, &json)?;
    // Never let the in-process cache claim a failed keychain write succeeded. Callers can now
    // surface the error and retry without silently diverging from what survives a restart.
    *UNIFIED_CACHE.lock().unwrap() = Some(unified.clone());
    Ok(())
}

/// Windows Credential Manager caps a single credential blob at
/// `CRED_MAX_CREDENTIAL_BLOB_SIZE` (2560 bytes), and the native store encodes the value as
/// UTF-16 (2 bytes/char), so one entry holds at most ~1280 chars. The unified secrets blob
/// carries a large Codex OAuth JWT and blows past that, so `set_password` fails with
/// "too long" and the login never persists (this is the "have to sign in to ChatGPT every
/// launch" bug). To stay under the cap we split the blob across numbered sibling entries on
/// Windows; other platforms have no such limit (and macOS charges one auth prompt per item)
/// so they keep a single entry.
#[cfg(windows)]
const CHUNK_CHARS: usize = 1000;

/// Store a possibly-large value, transparently splitting it on Windows (see [`CHUNK_CHARS`]).
fn store_blob(account: &str, value: &str) -> Result<(), String> {
    #[cfg(not(windows))]
    {
        store(account, value)
    }
    #[cfg(windows)]
    {
        // Split on char boundaries so a multi-byte char never straddles two entries.
        let chars: Vec<char> = value.chars().collect();
        let chunks: Vec<String> = chars
            .chunks(CHUNK_CHARS)
            .map(|c| c.iter().collect())
            .collect();
        // The base entry holds the chunk count; chunk `i` lives under `account.i`. A count
        // header is always numeric, which is how [`load_blob`] tells chunked writes apart
        // from a legacy single-item JSON blob.
        store(account, &chunks.len().to_string())?;
        for (i, ch) in chunks.iter().enumerate() {
            store(&format!("{account}.{i}"), ch)?;
        }
        // Drop any higher-index chunks left over from a previously longer value.
        let mut i = chunks.len();
        while !load(&format!("{account}.{i}")).is_empty() {
            let _ = delete(&format!("{account}.{i}"));
            i += 1;
        }
        Ok(())
    }
}

/// Load a value written by [`store_blob`], reassembling Windows chunks. Falls back to
/// treating the base entry as the whole value, so pre-chunking single-item blobs still load.
fn load_blob(account: &str) -> String {
    #[cfg(not(windows))]
    {
        load(account)
    }
    #[cfg(windows)]
    {
        let header = load(account);
        // A legacy blob is JSON (starts with `{`) and won't parse as a count; return it
        // as-is. An absent entry is "" → also not a count → returns "" (empty/default).
        let Ok(count) = header.parse::<usize>() else {
            return header;
        };
        let mut out = String::new();
        for i in 0..count {
            out.push_str(&load(&format!("{account}.{i}")));
        }
        out
    }
}

/// One-time migration: pulls whatever exists under the old per-item accounts into a
/// fresh [`UnifiedSecrets`] and persists it under [`UNIFIED_ACCOUNT`], so every later
/// launch reads only that one item. The old accounts are deliberately left in place
/// (orphaned but recoverable) rather than deleted, matching this codebase's existing
/// migration convention (see `AppSettings::from_profiles_era`).
fn migrate_legacy_accounts() -> UnifiedSecrets {
    let mut unified = UnifiedSecrets::default();
    let oauth_json = load("oauth-codex");
    if !oauth_json.is_empty() {
        unified.oauth = serde_json::from_str(&oauth_json).unwrap_or_default();
    }
    let ssh_json = load("ssh-credentials");
    if !ssh_json.is_empty() {
        unified.ssh = serde_json::from_str(&ssh_json).unwrap_or_default();
    }
    for kind in crate::settings::LlmProviderKind::ALL {
        let key = load(&format!("api-key:{}", kind.slug()));
        if !key.is_empty() {
            unified
                .provider_api_keys
                .insert(kind.slug().to_string(), key);
        }
    }
    let _ = save_unified(&unified);
    unified
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exercises the real OS credential store, so it's `#[ignore]`d by default (CI/sandboxed
    /// environments may have no keychain/Secret Service session available). Run explicitly
    /// with `cargo test -- --ignored secrets::`. Uses a throwaway account and always cleans
    /// up, even on assertion failure, so it never leaves test data behind.
    #[test]
    #[ignore]
    fn store_load_delete_roundtrip() {
        let account = "test-roundtrip-account";
        let cleanup = || {
            let _ = delete(account);
        };
        cleanup();
        let result = std::panic::catch_unwind(|| {
            store(account, "hunter2").unwrap();
            assert_eq!(load(account), "hunter2");
            store(account, "").unwrap();
            assert_eq!(load(account), "");
        });
        cleanup();
        result.unwrap();
    }
}
