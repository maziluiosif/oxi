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

#[tokio::test]
async fn router_cancellation_drops_a_pending_request_promptly() {
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    let trigger = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        flag.store(true, Ordering::SeqCst);
    });
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        tokio::select! {
            _ = std::future::pending::<()>() => panic!("pending request completed"),
            _ = wait_for_router_cancel(&cancel) => {}
        }
    })
    .await
    .expect("routing cancellation must stay responsive");
    trigger.await.unwrap();
}

fn session_header(headers: &[(String, String)]) -> &str {
    headers
        .iter()
        .find(|(k, _)| k == "x-opencode-session")
        .map(|(_, v)| v.as_str())
        .expect("OpenCode Go requests must carry x-opencode-session")
}

#[test]
fn opencode_go_session_is_stable_per_chat_and_hides_the_path() {
    let chat = "/Users/someone/.oxi/sessions/chat-1.json";
    let a = opencode_go_headers(Some(chat));
    let b = opencode_go_headers(Some(chat));
    let other = opencode_go_headers(Some("/Users/someone/.oxi/sessions/chat-2.json"));
    assert_eq!(session_header(&a), session_header(&b));
    assert_ne!(session_header(&a), session_header(&other));
    assert!(!session_header(&a).contains("someone"));
    assert!(
        a.iter()
            .any(|(k, v)| k == "user-agent" && v.starts_with("oxi/"))
    );
}

#[test]
fn opencode_go_session_falls_back_to_a_fresh_id() {
    let a = opencode_go_headers(None);
    let b = opencode_go_headers(Some("  "));
    assert!(session_header(&a).starts_with("oxi-"));
    assert_ne!(session_header(&a), session_header(&b));
    for (k, v) in &a {
        reqwest::header::HeaderName::from_bytes(k.as_bytes()).unwrap();
        reqwest::header::HeaderValue::from_str(v).unwrap();
    }
}
