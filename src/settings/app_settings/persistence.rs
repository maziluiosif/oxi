//! On-disk settings persistence and OS-keychain synchronization.

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use super::migration::LegacyAppSettings;
use super::*;

impl AppSettings {
    pub fn config_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("oxi")
            .join("settings.json")
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
                    Ok(_) => eprintln!(
                        "[oxi] invalid settings JSON ({e}); preserved it at {}",
                        backup.display()
                    ),
                    Err(copy_err) => eprintln!(
                        "[oxi] invalid settings JSON ({e}); could not preserve it: {copy_err}"
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
            let s = serde_json::from_value::<AppSettings>(value).unwrap_or_default();
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
        settings.migrate_secrets_to_keychain();
        settings.github_token = crate::secrets::load_unified().github_token;
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
        if changed {
            crate::secrets::save_unified(&unified)
                .map_err(|e| format!("Could not save credentials to the OS keychain: {e}"))?;
        }
        let path = Self::config_path();
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        let tmp_path = path.with_extension("json.tmp");
        {
            let mut file = fs::File::create(&tmp_path).map_err(|e| e.to_string())?;
            file.write_all(json.as_bytes()).map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
        }
        // Restrict to the owner as defense in depth for the rest of this file (base
        // URLs, model ids, etc.); the actual secrets (API keys) live in the OS keychain,
        // not here.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o600));
        }
        fs::rename(&tmp_path, &path).map_err(|e| e.to_string())?;
        Ok(())
    }
}
