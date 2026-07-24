use super::migration::LegacyAppSettings;
use super::*;

#[test]
fn default_has_all_provider_kinds() {
    let s = AppSettings::default();
    for kind in LlmProviderKind::ALL {
        assert_eq!(s.provider(kind).provider, kind);
    }
    assert_eq!(s.active_config().provider, s.active_provider);
}

/// Provider API keys must never be written to `settings.json` — they live in the OS
/// keychain (see `migrate_secrets_to_keychain`). This doesn't touch the real keychain,
/// just the serde attribute on `ProviderConfig::api_key`.
#[test]
fn api_key_not_serialized_to_json() {
    let mut s = AppSettings::default();
    s.provider_mut(LlmProviderKind::OpenAi).api_key = "sk-super-secret-value".to_string();
    let json = serde_json::to_string(&s).unwrap();
    assert!(!json.contains("sk-super-secret-value"));
    assert!(!json.contains("api_key"));
}

#[test]
fn github_token_not_serialized_to_json() {
    let s = AppSettings {
        github_token: "github_pat_super-secret-value".to_string(),
        ..Default::default()
    };
    let json = serde_json::to_string(&s).unwrap();
    assert!(!json.contains("github_pat_super-secret-value"));
    assert!(!json.contains("github_token"));
}

/// The new shape must round-trip: the `#[serde(skip)]`ped `provider` field comes back
/// via `normalize()` re-stamping it from the map key.
#[test]
fn new_shape_roundtrips() {
    let mut s = AppSettings {
        active_provider: LlmProviderKind::OpenRouter,
        ..Default::default()
    };
    s.provider_mut(LlmProviderKind::OpenRouter).model_id = "anthropic/claude-sonnet-5".into();
    s.provider_mut(LlmProviderKind::Ollama).base_url = "http://box:11434/v1".into();
    let json = serde_json::to_string(&s).unwrap();
    let mut back: AppSettings = serde_json::from_str(&json).unwrap();
    back.normalize();
    assert_eq!(back, s);
}

#[test]
fn normalize_fills_missing_provider_kinds() {
    // A new-shape file listing only one provider still normalizes to all kinds.
    let json = r#"{
            "active_provider": "ollama",
            "providers": {"ollama": {"model_id": "qwen3:14b"}},
            "system_prompt": "hi"
        }"#;
    let mut s: AppSettings = serde_json::from_str(json).unwrap();
    s.normalize();
    assert_eq!(s.provider(LlmProviderKind::Ollama).model_id, "qwen3:14b");
    for kind in LlmProviderKind::ALL {
        assert_eq!(s.provider(kind).provider, kind);
        assert!(!s.provider(kind).model_id.is_empty());
    }
}

#[test]
fn commit_msg_config_defaults_to_active() {
    let s = AppSettings {
        active_provider: LlmProviderKind::Ollama,
        ..Default::default()
    };
    let cfg = s.commit_msg_config();
    assert_eq!(cfg.provider, LlmProviderKind::Ollama);
    assert_eq!(cfg.model_id, s.active_config().model_id);
}

#[test]
fn commit_msg_config_pins_provider_and_model() {
    let s = AppSettings {
        commit_msg_provider: Some(LlmProviderKind::OpenRouter),
        commit_msg_model_id: "openai/gpt-4o-mini".to_string(),
        ..Default::default()
    };
    let cfg = s.commit_msg_config();
    assert_eq!(cfg.provider, LlmProviderKind::OpenRouter);
    assert_eq!(cfg.model_id, "openai/gpt-4o-mini");
}

/// Profiles-era migration: the active profile wins for its kind, even when another
/// profile of the same kind comes first in the list.
#[test]
fn migrates_profiles_era_active_profile_wins_for_its_kind() {
    let json: serde_json::Value = serde_json::from_str(
            r#"{
            "active_profile_id": "openrouter-work",
            "profiles": [
                {"id":"openai-default","name":"OpenAI default","provider":"openai","model_id":"gpt-4o-mini","base_url":"","api_key":"","openrouter_http_referer":"","openrouter_title":""},
                {"id":"openrouter-default","name":"OpenRouter default","provider":"openrouter","model_id":"openai/gpt-4o-mini","base_url":"","api_key":"","openrouter_http_referer":"","openrouter_title":""},
                {"id":"openrouter-work","name":"Work","provider":"openrouter","model_id":"anthropic/claude-sonnet-5","base_url":"https://proxy.example.com/v1","api_key":"","openrouter_http_referer":"","openrouter_title":""}
            ],
            "system_prompt": "hi",
            "tools_enabled": [true, true, true, true, true, true, true, true, true]
        }"#,
        )
        .unwrap();
    let (mut s, renames) = AppSettings::from_profiles_era(json, |_| String::new());
    s.normalize();
    assert!(renames.is_empty());
    assert_eq!(s.active_provider, LlmProviderKind::OpenRouter);
    let or = s.provider(LlmProviderKind::OpenRouter);
    assert_eq!(or.model_id, "anthropic/claude-sonnet-5");
    assert_eq!(or.base_url, "https://proxy.example.com/v1");
    // Shared fields carried over.
    assert_eq!(s.system_prompt, "hi");
}

/// A profile with a configured key beats an earlier empty placeholder of the same kind.
#[test]
fn migrates_profiles_era_prefers_profile_with_key() {
    let json: serde_json::Value = serde_json::from_str(
            r#"{
            "active_profile_id": "ollama-default",
            "profiles": [
                {"id":"ollama-default","name":"Ollama","provider":"ollama","model_id":"qwen3:14b","base_url":"","api_key":"","openrouter_http_referer":"","openrouter_title":""},
                {"id":"openai-default","name":"OpenAI default","provider":"openai","model_id":"gpt-4o-mini","base_url":"","api_key":"","openrouter_http_referer":"","openrouter_title":""},
                {"id":"openai-real","name":"Real","provider":"openai","model_id":"gpt-4.1","base_url":"","api_key":"","openrouter_http_referer":"","openrouter_title":""}
            ],
            "system_prompt": "hi",
            "tools_enabled": [true, true, true, true, true, true, true, true, true]
        }"#,
        )
        .unwrap();
    // Simulate the keychain holding a key only for "openai-real".
    let (mut s, _) = AppSettings::from_profiles_era(json, |id| {
        if id == "openai-real" {
            "sk-real".to_string()
        } else {
            String::new()
        }
    });
    s.normalize();
    assert_eq!(s.active_provider, LlmProviderKind::Ollama);
    let oa = s.provider(LlmProviderKind::OpenAi);
    assert_eq!(oa.model_id, "gpt-4.1");
    assert_eq!(oa.api_key, "sk-real");
}

/// `commit_msg_profile_id` intent survives migration as (provider, model) even when
/// the referenced profile is not the one chosen as that kind's config.
#[test]
fn migrates_profiles_era_commit_msg_intent() {
    let json: serde_json::Value = serde_json::from_str(
            r#"{
            "active_profile_id": "openrouter-main",
            "profiles": [
                {"id":"openrouter-main","name":"Main","provider":"openrouter","model_id":"anthropic/claude-sonnet-5","base_url":"","api_key":"","openrouter_http_referer":"","openrouter_title":""},
                {"id":"openrouter-cheap","name":"Cheap","provider":"openrouter","model_id":"openai/gpt-4o-mini","base_url":"","api_key":"","openrouter_http_referer":"","openrouter_title":""}
            ],
            "commit_msg_profile_id": "openrouter-cheap",
            "system_prompt": "hi",
            "tools_enabled": [true, true, true, true, true, true, true, true, true]
        }"#,
        )
        .unwrap();
    let (mut s, _) = AppSettings::from_profiles_era(json, |_| String::new());
    s.normalize();
    // The kind's config came from the active profile...
    assert_eq!(
        s.provider(LlmProviderKind::OpenRouter).model_id,
        "anthropic/claude-sonnet-5"
    );
    // ...but the commit-msg generator still targets the cheap model.
    assert_eq!(s.commit_msg_provider, Some(LlmProviderKind::OpenRouter));
    assert_eq!(s.commit_msg_model_id, "openai/gpt-4o-mini");
    assert_eq!(s.commit_msg_config().model_id, "openai/gpt-4o-mini");
}

/// SSH-backed profiles report their old id for credential re-keying.
#[test]
fn migrates_profiles_era_reports_ssh_renames() {
    let json: serde_json::Value = serde_json::from_str(
            r#"{
            "active_profile_id": "ollama-box",
            "profiles": [
                {"id":"ollama-box","name":"Box","provider":"ollama","model_id":"qwen3:14b","base_url":"","api_key":"","openrouter_http_referer":"","openrouter_title":"",
                 "location":{"kind":"remote_ssh","host":"box.local","port":22,"user":"me","remote_runtime_port":11434}}
            ],
            "system_prompt": "hi",
            "tools_enabled": [true, true, true, true, true, true, true, true, true]
        }"#,
        )
        .unwrap();
    let (mut s, renames) = AppSettings::from_profiles_era(json, |_| String::new());
    s.normalize();
    assert_eq!(
        renames,
        vec![("ollama-box".to_string(), LlmProviderKind::Ollama)]
    );
    assert!(s.provider(LlmProviderKind::Ollama).ssh_config().is_some());
}

/// The oldest flat single-provider shape still migrates into the new one.
#[test]
fn migrates_flat_legacy_shape() {
    let old = LegacyAppSettings {
        provider: LlmProviderKind::OpenRouter,
        model_id: "openai/gpt-4o-mini".to_string(),
        base_url: "https://openrouter.ai/api/v1".to_string(),
        system_prompt: "legacy prompt".to_string(),
        openai_api_key: String::new(),
        openrouter_api_key: "sk-or-legacy".to_string(),
        openrouter_http_referer: "https://example.com".to_string(),
        openrouter_title: "oxi".to_string(),
        tools_enabled: [true; 7],
    };
    let mut s = AppSettings::from_legacy(old);
    s.normalize();
    assert_eq!(s.active_provider, LlmProviderKind::OpenRouter);
    let or = s.provider(LlmProviderKind::OpenRouter);
    assert_eq!(or.model_id, "openai/gpt-4o-mini");
    assert_eq!(or.api_key, "sk-or-legacy");
    assert_eq!(or.openrouter_http_referer, "https://example.com");
    assert_eq!(s.system_prompt, "legacy prompt");
}

#[test]
fn tools_enabled_default_all_true() {
    let s = AppSettings::default();
    assert_eq!(s.tools_enabled.len(), ALL_TOOL_NAMES.len());
    assert!(s.tools_enabled.iter().all(|&t| t));
}

#[test]
fn all_tool_names_has_expected_tools() {
    assert_eq!(ALL_TOOL_NAMES.len(), 15);
    assert!(ALL_TOOL_NAMES.contains(&"bash"));
    assert!(ALL_TOOL_NAMES.contains(&"codebase_search"));
    assert!(ALL_TOOL_NAMES.contains(&"git_status"));
    assert!(ALL_TOOL_NAMES.contains(&"git_diff"));
    assert!(ALL_TOOL_NAMES.contains(&"web_search"));
    assert!(ALL_TOOL_NAMES.contains(&"web_fetch"));
}

#[test]
fn bash_timeout_cap_defaults_to_300() {
    let s = AppSettings::default();
    assert_eq!(s.bash_timeout_cap_secs, 300);
}

#[test]
fn normalize_fixes_and_clamps_bash_timeout_cap() {
    let base = r#"{
            "active_provider": "openai",
            "providers": {"openai": {"model_id":"gpt-4o-mini","base_url":""}},
            "system_prompt": "hi""#;
    // 0 → default
    let mut s: AppSettings =
        serde_json::from_str(&format!("{base}, \"bash_timeout_cap_secs\": 0 }}")).unwrap();
    s.normalize();
    assert_eq!(s.bash_timeout_cap_secs, 300);
    // below floor → 5
    let mut s: AppSettings =
        serde_json::from_str(&format!("{base}, \"bash_timeout_cap_secs\": 4 }}")).unwrap();
    s.normalize();
    assert_eq!(s.bash_timeout_cap_secs, 5);
    // above ceiling → 3600
    let mut s: AppSettings =
        serde_json::from_str(&format!("{base}, \"bash_timeout_cap_secs\": 999999 }}")).unwrap();
    s.normalize();
    assert_eq!(s.bash_timeout_cap_secs, 3600);
}

#[test]
fn normalize_pads_short_tools_enabled() {
    let json = r#"{
            "active_provider": "openai",
            "providers": {"openai": {"model_id":"gpt-4o-mini","base_url":""}},
            "system_prompt": "hi",
            "tools_enabled": [true, false, true, true, true, true, true]
        }"#;
    let mut s: AppSettings = serde_json::from_str(json).unwrap();
    s.normalize();
    assert_eq!(s.tools_enabled.len(), ALL_TOOL_NAMES.len());
    // Pre-existing flags are preserved.
    assert!(!s.tools_enabled[1]);
    // Newly-added tools default to enabled.
    assert!(s.tools_enabled[7]);
    assert!(s.tools_enabled[8]);
    // Missing searxng_url falls back to the default.
    assert_eq!(s.searxng_url, default_searxng_url());
}

#[test]
fn effective_web_search_url_duckduckgo_is_empty() {
    let s = AppSettings {
        web_search_backend: WebSearchBackend::DuckDuckGo,
        searxng_url: "https://searxng.example.com".to_string(),
        ..Default::default()
    };
    // Even with a URL set, DuckDuckGo backend ignores it.
    assert_eq!(s.effective_web_search_url(), "");
}

#[test]
fn effective_web_search_url_searxng_returns_url() {
    let s = AppSettings {
        web_search_backend: WebSearchBackend::SearXng,
        searxng_url: "https://searxng.example.com/".to_string(),
        ..Default::default()
    };
    // Trailing slash trimmed.
    assert_eq!(s.effective_web_search_url(), "https://searxng.example.com");
}

#[test]
fn effective_web_search_url_searxng_empty_returns_empty_for_tool_error() {
    let s = AppSettings {
        web_search_backend: WebSearchBackend::SearXng,
        searxng_url: String::new(),
        ..Default::default()
    };
    // Empty URL is passed through so the tool can report a SearXNG configuration error.
    assert_eq!(s.effective_web_search_url(), "");
}

#[test]
fn normalize_migrates_nonempty_url_to_searxng_backend() {
    // Older settings.json: explicit `web_search_backend: "duckduckgo"` (the old default)
    // and a non-empty searxng_url. normalize() should migrate to SearXng.
    let json = r#"{
            "active_provider": "openai",
            "providers": {"openai": {"model_id":"gpt-4o-mini","base_url":""}},
            "system_prompt": "hi",
            "tools_enabled": [true, true, true, true, true, true, true, true, true],
            "web_search_backend": "duckduckgo",
            "searxng_url": "https://searxng.example.com"
        }"#;
    let mut s: AppSettings = serde_json::from_str(json).unwrap();
    s.normalize();
    assert_eq!(s.web_search_backend, WebSearchBackend::SearXng);
    assert_eq!(s.effective_web_search_url(), "https://searxng.example.com");
}

#[test]
fn normalize_upgrades_legacy_default_prompt_but_keeps_custom() {
    use crate::agent::prompt::{DEFAULT_AGENT_SYSTEM_PROMPT, LEGACY_DEFAULT_SYSTEM_PROMPTS};

    // A stored prompt equal to an old shipped default is upgraded to the current default.
    let mut s = AppSettings {
        system_prompt: LEGACY_DEFAULT_SYSTEM_PROMPTS[0].to_string(),
        ..Default::default()
    };
    s.normalize();
    assert_eq!(s.system_prompt, DEFAULT_AGENT_SYSTEM_PROMPT);

    // An empty prompt is restored to the current default.
    s.system_prompt = "   ".to_string();
    s.normalize();
    assert_eq!(s.system_prompt, DEFAULT_AGENT_SYSTEM_PROMPT);

    // A genuinely custom prompt is left untouched.
    s.system_prompt = "You are my custom agent. {tools_list}".to_string();
    s.normalize();
    assert_eq!(s.system_prompt, "You are my custom agent. {tools_list}");
}

#[test]
fn normalize_keeps_default_when_no_url() {
    // Older settings.json: no `web_search_backend` field and empty searxng_url.
    // Deserializes to Bing (current default) and stays on Bing.
    let json = r#"{
            "active_provider": "openai",
            "providers": {"openai": {"model_id":"gpt-4o-mini","base_url":""}},
            "system_prompt": "hi",
            "tools_enabled": [true, true, true, true, true, true, true, true, true]
        }"#;
    let mut s: AppSettings = serde_json::from_str(json).unwrap();
    s.normalize();
    assert_eq!(s.web_search_backend, WebSearchBackend::Bing);
    assert_eq!(s.effective_web_search_url(), "");
}
