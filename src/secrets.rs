//! Thin wrapper around the OS credential store — macOS Keychain Services, Windows
//! Credential Manager, or the Linux Secret Service (via D-Bus) — used for everything
//! that used to live as plaintext JSON on disk: provider API keys, OAuth tokens, and
//! SSH passwords. See `oauth::store`, `compute::store`, and `settings::ProviderProfile`
//! for the call sites.

use std::collections::HashMap;
use std::sync::Mutex;

/// Keychain "service" name under which all oxi credentials are grouped.
const SERVICE: &str = "oxi";

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
    let result = if value.is_empty() {
        delete(account)
    } else {
        keyring::Entry::new(SERVICE, account)
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
    keyring::Entry::new(SERVICE, account)
        .and_then(|e| e.get_password())
        .unwrap_or_default()
}

/// Remove the entry for `account`. Missing entries are not an error.
pub fn delete(account: &str) -> Result<(), String> {
    let result = match keyring::Entry::new(SERVICE, account) {
        Ok(entry) => match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        },
        Err(e) => Err(e.to_string()),
    };
    if result.is_ok() {
        if let Some(cache) = WRITE_CACHE.lock().unwrap().as_mut() {
            cache.remove(account);
        }
    }
    result
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
