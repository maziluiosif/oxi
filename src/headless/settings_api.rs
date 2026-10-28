//! `settings.*`: the app's settings for frontends that edit them (the Tauri app).
//!
//! `settings.get` returns `settings.json` as the app writes it plus what a settings screen needs
//! around it: the provider catalog, which providers have a key, the tool names. Secrets never
//! leave the process; `settings.set_secret` writes them. `settings.set` merges a JSON patch into
//! the stored settings, so a frontend only sends what it changed and keeps edits the app made.

use serde_json::{Value, json};

use crate::settings::{ALL_TOOL_NAMES, AppSettings, LlmProviderKind, ProviderConfig};

use super::{Server, provider_from_slug, str_param};

impl Server {
    pub(super) fn settings_get(&mut self) -> Result<Value, String> {
        self.refresh_settings();
        settings_json(&self.settings)
    }

    pub(super) fn settings_set(&mut self, params: &Value) -> Result<Value, String> {
        let patch = params.get("patch").ok_or("missing parameter: patch")?;
        if !patch.is_object() {
            return Err("patch must be an object".into());
        }
        let current = AppSettings::load();
        let mut value = serde_json::to_value(&current).map_err(|e| e.to_string())?;
        merge(&mut value, patch);
        let mut next: AppSettings =
            serde_json::from_value(value).map_err(|e| format!("Invalid settings: {e}"))?;
        next.normalize();
        carry_secrets(&current, &mut next);
        next.save()?;
        self.settings = next;
        self.settings_mtime = super::settings_mtime();
        self.notify("settings_changed", Value::Null);
        settings_json(&self.settings)
    }

    /// `{"provider": slug, "key": "..."}` or `{"github_token": "..."}`; an empty value clears it.
    pub(super) fn settings_set_secret(&mut self, params: &Value) -> Result<Value, String> {
        let mut settings = AppSettings::load();
        if let Some(token) = params["github_token"].as_str() {
            settings.github_token = token.trim().to_string();
        } else {
            let kind = provider_from_slug(&str_param(params, "provider")?)?;
            let key = str_param(params, "key")?.trim().to_string();
            settings
                .providers
                .entry(kind)
                .or_insert_with(|| ProviderConfig::new(kind))
                .api_key = key;
        }
        settings.save()?;
        self.settings = settings;
        self.settings_mtime = super::settings_mtime();
        self.notify("settings_changed", Value::Null);
        settings_json(&self.settings)
    }
}

fn settings_json(settings: &AppSettings) -> Result<Value, String> {
    let oauth = crate::oauth::load_oauth_store();
    let providers: Vec<Value> = LlmProviderKind::ALL
        .into_iter()
        .map(|kind| {
            let cfg = settings.provider(kind);
            json!({
                "id": kind.slug(),
                "label": kind.label(),
                "short": crate::app::composer::composer_provider_label(kind),
                "acp": kind.is_acp(),
                "configured": settings.provider_is_configured(kind, &oauth),
                "has_key": !cfg.api_key.trim().is_empty(),
                "default_base_url": kind.default_base_url(),
                "default_model": kind.default_model_id(),
                "default_acp_command": kind.is_acp().then(|| cfg.default_acp_command()),
            })
        })
        .collect();
    Ok(json!({
        "settings": serde_json::to_value(settings).map_err(|e| e.to_string())?,
        "providers": providers,
        "tools": ALL_TOOL_NAMES,
        "has_github_token": !settings.github_token.trim().is_empty(),
        "path": AppSettings::config_path().to_string_lossy(),
    }))
}

/// RFC 7396 merge patch: objects merge key by key, `null` removes, anything else replaces.
fn merge(target: &mut Value, patch: &Value) {
    let Value::Object(patch) = patch else {
        *target = patch.clone();
        return;
    };
    if !target.is_object() {
        *target = json!({});
    }
    let target = target.as_object_mut().expect("made an object above");
    for (key, value) in patch {
        if value.is_null() {
            target.remove(key);
        } else {
            merge(target.entry(key.clone()).or_insert(Value::Null), value);
        }
    }
}

/// The secrets aren't serialized, so the patched settings come back without them.
fn carry_secrets(from: &AppSettings, to: &mut AppSettings) {
    for (kind, cfg) in &from.providers {
        if let Some(next) = to.providers.get_mut(kind) {
            next.api_key = cfg.api_key.clone();
        }
    }
    to.github_token = from.github_token.clone();
    for server in &mut to.mcp_servers {
        if let Some(old) = from.mcp_servers.iter().find(|s| s.name == server.name) {
            server.bearer_token = old.bearer_token.clone();
            server.env = old.env.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_patch_replaces_merges_and_removes() {
        let mut target = json!({"a": 1, "b": {"c": 2, "d": 3}, "e": [1]});
        merge(&mut target, &json!({"a": 5, "b": {"c": null, "x": 1}, "e": [2, 3]}));
        assert_eq!(target, json!({"a": 5, "b": {"d": 3, "x": 1}, "e": [2, 3]}));
    }

    #[test]
    fn patched_settings_keep_their_secrets() {
        let mut from = AppSettings::default();
        from.normalize();
        from.providers
            .get_mut(&LlmProviderKind::OpenAi)
            .unwrap()
            .api_key = "sk-1".into();
        from.github_token = "ghp".into();
        let mut value = serde_json::to_value(&from).unwrap();
        merge(&mut value, &json!({"theme_id": "light"}));
        let mut to: AppSettings = serde_json::from_value(value).unwrap();
        to.normalize();
        carry_secrets(&from, &mut to);
        assert_eq!(to.theme_id, "light");
        assert_eq!(to.provider(LlmProviderKind::OpenAi).api_key, "sk-1");
        assert_eq!(to.github_token, "ghp");
    }
}
