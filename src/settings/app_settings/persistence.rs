//! On-disk settings persistence and OS-keychain synchronization.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use serde_json::{Map, Value};

use super::migration::LegacyAppSettings;
use super::*;

impl AppSettings {
    pub fn config_path() -> PathBuf {
        crate::app_dirs::config_dir().join("settings.json")
    }

    pub fn load() -> Self {
        let path = Self::config_path();
        let bytes = fs::read(&path).unwrap_or_default();
        let value: serde_json::Value = match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(e) if !bytes.is_empty() => {
                // Preserve malformed user configuration for diagnosis/recovery instead of
                // silently making it look as if every setting disappeared.
                let timestamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
                let backup = path.with_file_name(format!("settings.corrupt-{timestamp}.json"));
                match fs::copy(&path, &backup) {
                    Ok(_) => log::error!(
                        "invalid settings JSON ({e}); preserved it at {}",
                        backup.display()
                    ),
                    Err(copy_err) => log::error!(
                        "invalid settings JSON ({e}); could not preserve it: {copy_err}"
                    ),
                }
                serde_json::Value::Null
            }
            Err(_) => serde_json::Value::Null,
        };
        // Sniff the on-disk shape by its distinctive keys rather than trying typed parses
        // in sequence: a profiles-era file must never silently parse as the new shape
        // (it would come out with empty provider configs and drop the user's setup).
        let (mut settings, ssh_renames, migrated) = if value.get("providers").is_some() {
            let (s, error) = Self::from_current_value(value);
            if let Some(e) = error {
                preserve_unreadable_copy(&path, &e);
            }
            (s, Vec::new(), false)
        } else if value.get("profiles").is_some() {
            let (s, renames) = Self::from_profiles_era(value, |profile_id| {
                crate::secrets::load(&format!("api-key:{profile_id}"))
            });
            (s, renames, true)
        } else if value.get("provider").is_some() {
            match serde_json::from_value::<LegacyAppSettings>(value) {
                Ok(old) => (Self::from_legacy(old), Vec::new(), true),
                Err(_) => (Self::default(), Vec::new(), false),
            }
        } else {
            (Self::default(), Vec::new(), false)
        };
        settings.normalize();
        settings.remember_fallbacks();
        settings.migrate_secrets_to_keychain();
        let unified = crate::secrets::load_unified();
        settings.github_token = unified.github_token.clone();
        for server in &mut settings.mcp_servers {
            if let Some(secret) = unified.mcp.get(&server.name) {
                server.bearer_token = secret.bearer_token.clone();
                server.env = secret.env.clone();
            }
        }
        if migrated {
            Self::migrate_ssh_credentials(&ssh_renames);
            // Rewrite settings.json in the new shape right away so the migration runs
            // exactly once; from here on the file parses via the `providers` branch.
            let _ = settings.save();
        }
        settings
    }

    /// Provider API keys used to be written to `settings.json` in plaintext; the field is
    /// now `skip_serializing`, so freshly-parsed JSON either has no `api_key` at all
    /// (already migrated), or still has a leftover plaintext value from before this
    /// version. For each provider: a non-empty value just parsed from the file (or carried
    /// over by migration) is pushed into the unified secrets blob so it becomes the source
    /// of truth. Otherwise, pull whatever is already there into memory for this session.
    /// Reads/writes the keychain's unified item at most once per launch (via
    /// `load_unified`'s cache), rather than once per provider.
    fn migrate_secrets_to_keychain(&mut self) {
        let mut unified = crate::secrets::load_unified();
        let mut changed = false;
        for (kind, cfg) in &mut self.providers {
            if !cfg.api_key.is_empty() {
                if unified.provider_api_keys.get(kind.slug()) != Some(&cfg.api_key) {
                    unified
                        .provider_api_keys
                        .insert(kind.slug().to_string(), cfg.api_key.clone());
                    changed = true;
                }
            } else if let Some(key) = unified.provider_api_keys.get(kind.slug()) {
                cfg.api_key = key.clone();
            }
        }
        if changed {
            let _ = crate::secrets::save_unified(&unified);
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let mut unified = crate::secrets::load_unified();
        let mut changed = false;
        for (kind, cfg) in &self.providers {
            let slug = kind.slug();
            if cfg.api_key.is_empty() {
                if unified.provider_api_keys.remove(slug).is_some() {
                    changed = true;
                }
            } else if unified.provider_api_keys.get(slug) != Some(&cfg.api_key) {
                unified
                    .provider_api_keys
                    .insert(slug.to_string(), cfg.api_key.clone());
                changed = true;
            }
        }
        if unified.github_token != self.github_token {
            unified.github_token = self.github_token.clone();
            changed = true;
        }
        let mcp: std::collections::HashMap<String, crate::secrets::McpSecrets> = self
            .mcp_servers
            .iter()
            .filter(|s| !s.bearer_token.is_empty() || !s.env.is_empty())
            .map(|s| {
                (
                    s.name.clone(),
                    crate::secrets::McpSecrets {
                        bearer_token: s.bearer_token.clone(),
                        env: s.env.clone(),
                    },
                )
            })
            .collect();
        if unified.mcp != mcp {
            unified.mcp = mcp;
            changed = true;
        }
        if changed {
            crate::secrets::save_unified(&unified)
                .map_err(|e| format!("Could not save credentials to the OS keychain: {e}"))?;
        }
        let path = Self::config_path();
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let json = self.to_json_string()?;
        // Owner-only as defense in depth for the rest of this file (base URLs, model ids,
        // etc.); the actual secrets (API keys) live in the OS keychain, not here.
        crate::fsutil::write_atomic_private(&path, json.as_bytes())
    }

    /// The `settings.json` text for these settings, with anything this version could not read
    /// put back as it was (see [`UnreadableSettings`]).
    pub(crate) fn to_json_string(&self) -> Result<String, String> {
        if self.unreadable.is_empty() {
            return serde_json::to_string_pretty(self).map_err(|e| e.to_string());
        }
        let mut value = serde_json::to_value(self).map_err(|e| e.to_string())?;
        self.unreadable.restore_into(&mut value);
        serde_json::to_string_pretty(&value).map_err(|e| e.to_string())
    }

    /// Parse current-shape settings JSON. When this version can't fully read it (typically a
    /// newer oxi wrote a provider or option this one doesn't know), keep everything it can
    /// read instead of starting from defaults, and remember the rest, plus any fields it has
    /// never heard of, so saves carry them through. Returns the strict parse error, if any.
    pub(crate) fn from_current_value(value: Value) -> (Self, Option<serde_json::Error>) {
        let (mut settings, error) = match serde_json::from_value::<Self>(value.clone()) {
            Ok(settings) => (settings, None),
            Err(e) => (Self::from_value_lenient(value.clone()), Some(e)),
        };
        if let Value::Object(file) = &value
            && let Ok(Value::Object(known)) = serde_json::to_value(Self::default())
        {
            for (key, raw) in file {
                if !known.contains_key(key) && !NOT_SAVED.contains(&key.as_str()) {
                    settings
                        .unreadable
                        .fields
                        .insert(key.clone(), Unreadable::new(raw.clone()));
                }
            }
        }
        if !settings.unreadable.is_empty() {
            log::warn!(
                "settings.json: keeping what this version can't read: {}",
                settings.unreadable.describe()
            );
        }
        (settings, error)
    }

    /// Parse settings JSON one top-level field (and one provider entry) at a time: whatever
    /// parses is kept, the rest falls back to its default and goes to `unreadable`.
    fn from_value_lenient(value: Value) -> Self {
        let Value::Object(file) = value else {
            return Self::default();
        };
        let Ok(Value::Object(mut kept)) = serde_json::to_value(Self::default()) else {
            return Self::default();
        };
        let parses = |candidate: &Map<String, Value>| {
            serde_json::from_value::<Self>(Value::Object(candidate.clone())).is_ok()
        };
        let mut unreadable = UnreadableSettings::default();
        for (key, raw) in file {
            if key == PROVIDERS
                && let Value::Object(entries) = raw
            {
                let mut readable = Map::new();
                for (kind, entry) in entries {
                    let mut trial = readable.clone();
                    trial.insert(kind.clone(), entry.clone());
                    kept.insert(PROVIDERS.to_string(), Value::Object(trial.clone()));
                    if parses(&kept) {
                        readable = trial;
                    } else {
                        let mut entry = entry;
                        // API keys belong in the keychain, never back in settings.json.
                        if let Value::Object(fields) = &mut entry {
                            fields.remove("api_key");
                        }
                        unreadable.providers.insert(kind, Unreadable::new(entry));
                    }
                }
                kept.insert(PROVIDERS.to_string(), Value::Object(readable));
                continue;
            }
            let previous = kept.insert(key.clone(), raw.clone());
            if !parses(&kept) {
                match previous {
                    Some(previous) => kept.insert(key.clone(), previous),
                    None => kept.remove(&key),
                };
                unreadable.fields.insert(key, Unreadable::new(raw));
            }
        }
        let mut settings = serde_json::from_value::<Self>(Value::Object(kept)).unwrap_or_default();
        settings.unreadable = unreadable;
        settings
    }

    /// Record what each unreadable entry falls back to in this version, so a save can tell
    /// whether the user changed it here (their change wins) or not (the original is restored).
    pub(crate) fn remember_fallbacks(&mut self) {
        if self.unreadable.is_empty() {
            return;
        }
        let Ok(value) = serde_json::to_value(&*self) else {
            return;
        };
        for (key, entry) in &mut self.unreadable.fields {
            entry.fallback = value.get(key).cloned();
        }
        for (kind, entry) in &mut self.unreadable.providers {
            entry.fallback = value.get(PROVIDERS).and_then(|p| p.get(kind)).cloned();
        }
    }
}

const PROVIDERS: &str = "providers";

/// Fields this version reads but deliberately never writes (legacy or secret): dropping them
/// on save is intended, so they are not "unknown".
const NOT_SAVED: [&str; 2] = ["require_approval", "github_token"];

/// Parts of `settings.json` that did not parse in this version, with what they fell back to.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct UnreadableSettings {
    /// Top-level fields, by key.
    fields: BTreeMap<String, Unreadable>,
    /// Entries of the `providers` object, by provider slug.
    providers: BTreeMap<String, Unreadable>,
}

/// One unreadable value: what the file had, and what this version used instead (`None` when
/// it has no such entry at all, e.g. a provider it doesn't know).
#[derive(Debug, Clone, PartialEq)]
struct Unreadable {
    raw: Value,
    fallback: Option<Value>,
}

impl Unreadable {
    fn new(raw: Value) -> Self {
        Self {
            raw,
            fallback: None,
        }
    }
}

impl UnreadableSettings {
    pub(crate) fn is_empty(&self) -> bool {
        self.fields.is_empty() && self.providers.is_empty()
    }

    fn describe(&self) -> String {
        self.fields
            .keys()
            .cloned()
            .chain(self.providers.keys().map(|k| format!("{PROVIDERS}.{k}")))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Put each original value back into `value` (serialized settings) unless the user changed
    /// that entry in this version.
    fn restore_into(&self, value: &mut Value) {
        let Value::Object(root) = value else {
            return;
        };
        restore(root, &self.fields);
        if let Some(Value::Object(providers)) = root.get_mut(PROVIDERS) {
            restore(providers, &self.providers);
        }
    }
}

fn restore(target: &mut Map<String, Value>, unreadable: &BTreeMap<String, Unreadable>) {
    for (key, entry) in unreadable {
        if target.get(key) == entry.fallback.as_ref() {
            target.insert(key.clone(), entry.raw.clone());
        }
    }
}

/// Keep one copy of a `settings.json` this version can't fully read, per version, so a newer
/// file is never lost even if something goes wrong later. Launching repeatedly doesn't pile up
/// copies.
fn preserve_unreadable_copy(path: &std::path::Path, error: &serde_json::Error) {
    let backup = path.with_file_name(format!(
        "settings.unreadable-by-{}.json",
        crate::update::APP_VERSION
    ));
    if backup.exists() {
        log::warn!("settings.json: {error}; reading what this version can");
        return;
    }
    match fs::copy(path, &backup) {
        Ok(_) => log::warn!(
            "settings.json: {error}; reading what this version can (original copied to {})",
            backup.display()
        ),
        Err(copy_err) => {
            log::error!("settings.json: {error}; could not copy it aside: {copy_err}")
        }
    }
}
