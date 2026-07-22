use super::*;

#[test]
fn wire_fingerprint_is_stable_and_versioned() {
    let settings = AppSettings::default();
    let tools = vec![serde_json::json!({"type":"function","function":{"name":"read"}})];
    let a = wire_fingerprint_for(&settings, "system", &tools);
    let b = wire_fingerprint_for(&settings, "system", &tools);
    assert_eq!(a, b);
    assert!(a.starts_with("v1:sha256:"));
    assert_eq!(a.len(), "v1:sha256:".len() + 64);
}

#[test]
fn wire_fingerprint_changes_with_prompt_or_tools() {
    let settings = AppSettings::default();
    let tools = vec![serde_json::json!({"type":"function","function":{"name":"read"}})];
    let changed_tools = vec![serde_json::json!({"type":"function","function":{"name":"edit"}})];
    assert_ne!(
        wire_fingerprint_for(&settings, "system-a", &tools),
        wire_fingerprint_for(&settings, "system-b", &tools)
    );
    assert_ne!(
        wire_fingerprint_for(&settings, "system-a", &tools),
        wire_fingerprint_for(&settings, "system-a", &changed_tools)
    );
}

#[test]
fn opencode_go_anthropic_models() {
    assert!(opencode_go_model_uses_anthropic("minimax-01"));
    assert!(opencode_go_model_uses_anthropic("qwen-max"));
    assert!(opencode_go_model_uses_anthropic("qwen2.5-coder"));
}

#[test]
fn opencode_go_strips_provider_prefix() {
    assert!(opencode_go_model_uses_anthropic("opencode-go/minimax-text"));
    assert!(opencode_go_model_uses_anthropic("opencode-go/qwen-max"));
    assert!(!opencode_go_model_uses_anthropic(
        "opencode-go/kimi-k2.7-code"
    ));
}

#[test]
fn opencode_go_normalizes_case_and_whitespace() {
    assert!(opencode_go_model_uses_anthropic("  MiniMax-01  "));
    assert!(opencode_go_model_uses_anthropic("QWEN-MAX"));
}

#[test]
fn opencode_go_chat_models_are_not_anthropic() {
    assert!(!opencode_go_model_uses_anthropic("kimi-k2.7-code"));
    assert!(!opencode_go_model_uses_anthropic("gpt-4o-mini"));
    assert!(!opencode_go_model_uses_anthropic(""));
}

fn cfg_with_key(key: &str) -> ProviderConfig {
    let mut c = ProviderConfig::new(LlmProviderKind::OpenAi);
    c.api_key = key.to_string();
    c
}

#[test]
fn ollama_key_prefers_configured_value() {
    let mut c = ProviderConfig::new(LlmProviderKind::Ollama);
    c.api_key = "ollama-config".to_string();
    assert_eq!(configured_ollama_key(&c), "ollama-config");
}

#[test]
fn ollama_key_defaults_empty_without_config_or_env() {
    let c = ProviderConfig::new(LlmProviderKind::Ollama);
    // No key configured and (in test environments) no OLLAMA_API_KEY: falls back to "".
    if std::env::var("OLLAMA_API_KEY").is_err() {
        assert_eq!(configured_ollama_key(&c), "");
    }
}

#[test]
fn configured_key_prefers_configured_value() {
    // A non-empty configured key is returned regardless of environment.
    assert_eq!(
        configured_openai_key(&cfg_with_key("sk-profile")).unwrap(),
        "sk-profile"
    );
    assert_eq!(
        configured_openrouter_key(&cfg_with_key("or-profile")).unwrap(),
        "or-profile"
    );
    assert_eq!(
        configured_opencode_go_key(&cfg_with_key("og-profile")).unwrap(),
        "og-profile"
    );
}

#[test]
fn configured_key_trims_whitespace() {
    assert_eq!(
        configured_openai_key(&cfg_with_key("  sk-padded  ")).unwrap(),
        "sk-padded"
    );
}
