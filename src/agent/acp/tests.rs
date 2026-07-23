use super::*;
use std::sync::mpsc::channel;

fn drain(rx: &StdReceiver<AgentEvent>) -> Vec<AgentEvent> {
    let mut out = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        out.push(ev);
    }
    out
}

#[test]
fn maps_message_and_thought_chunks() {
    let (tx, rx) = channel();
    emit_update(
        &json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hi"}}),
        &tx,
    );
    emit_update(
        &json!({"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"hmm"}}),
        &tx,
    );
    let evs = drain(&rx);
    assert!(matches!(&evs[0], AgentEvent::TextDelta(s) if s == "hi"));
    assert!(matches!(&evs[1], AgentEvent::ThinkingDelta(s) if s == "hmm"));
}

#[test]
fn tool_call_emits_start_output_and_end() {
    let (tx, rx) = channel();
    emit_update(
        &json!({
            "sessionUpdate":"tool_call",
            "toolCallId":"c1",
            "kind":"read",
            "status":"completed",
            "rawInput":{"path":"a.txt"},
            "content":[{"type":"content","content":{"type":"text","text":"file body"}}]
        }),
        &tx,
    );
    let evs = drain(&rx);
    assert!(
        matches!(&evs[0], AgentEvent::ToolStart { name, tool_call_id, .. }
            if name == "read" && tool_call_id == "c1")
    );
    assert!(matches!(&evs[1], AgentEvent::ToolOutput { text, .. } if text == "file body"));
    assert!(matches!(
        &evs[2],
        AgentEvent::ToolEnd {
            is_error: Some(false),
            ..
        }
    ));
}

#[test]
fn failed_tool_update_marks_error() {
    let (tx, rx) = channel();
    emit_update(
        &json!({"sessionUpdate":"tool_call_update","toolCallId":"c2","status":"failed"}),
        &tx,
    );
    let evs = drain(&rx);
    assert!(
        matches!(&evs[0], AgentEvent::ToolEnd { is_error: Some(true), tool_call_id, .. }
            if tool_call_id == "c2")
    );
}

#[test]
fn diff_content_becomes_unified_diff() {
    let (tx, rx) = channel();
    emit_update(
        &json!({
            "sessionUpdate":"tool_call_update",
            "toolCallId":"c3",
            "status":"completed",
            "content":[{"type":"diff","path":"f.rs","oldText":"a\n","newText":"b\n"}]
        }),
        &tx,
    );
    let evs = drain(&rx);
    match evs.last().unwrap() {
        AgentEvent::ToolEnd { diff: Some(d), .. } => {
            assert!(d.contains("-a"));
            assert!(d.contains("+b"));
            assert!(d.contains("f.rs"));
        }
        other => panic!("expected ToolEnd with diff, got {other:?}"),
    }
}

#[test]
fn pick_option_prefers_requested_kind() {
    let options = json!([
        {"optionId":"a","kind":"allow_once"},
        {"optionId":"b","kind":"allow_always"},
        {"optionId":"c","kind":"reject_once"}
    ]);
    let opts = options.as_array().unwrap();
    assert_eq!(pick_option(opts, &["allow_always", "allow_once"]), "b");
    assert_eq!(pick_option(opts, &["reject_once", "reject"]), "c");
}

#[test]
fn pick_option_falls_back_to_prefix_then_first() {
    let options = json!([{"optionId":"x","kind":"allow_once"}]);
    let opts = options.as_array().unwrap();
    // No exact "reject" kind: reject falls back to the first option.
    assert_eq!(pick_option(opts, &["reject_once", "reject"]), "x");
}

#[test]
fn permission_name_maps_kind() {
    let (name, args) =
        permission_name_args(&json!({"kind":"execute","rawInput":{"command":"ls"},"title":"Run"}));
    assert_eq!(name, "bash");
    assert_eq!(args.unwrap()["command"], "ls");
    let (name, _) = permission_name_args(&json!({"kind":"edit"}));
    assert_eq!(name, "edit");
}

/// End-to-end smoke test against the real adapter. Ignored by default (spawns `npx`, needs a
/// logged-in Claude Code, and calls the API). Run with:
///   cargo test acp_end_to_end_applies_model -- --ignored --nocapture
#[test]
#[ignore]
fn acp_end_to_end_applies_model() {
    let mgr = AcpManager::spawn();
    let (ev_tx, ev_rx) = channel::<AgentEvent>();
    let (_appr_tx, appr_rx) = channel::<ApprovalDecision>();
    let cancel = Arc::new(AtomicBool::new(false));
    let req = AcpPrompt {
        session_key: "test-e2e".to_string(),
        cwd: std::env::temp_dir(),
        command_line: "npx @agentclientprotocol/claude-agent-acp".to_string(),
        env: Vec::new(),
        model: "haiku".to_string(),
        effort: "low".to_string(),
        text: "Reply with ONLY one word naming your model family: Opus, Sonnet, or Haiku."
            .to_string(),
        images: Vec::new(),
        event_tx: ev_tx,
        approval_rx: appr_rx,
        approval_policy: ApprovalPolicy::disabled(),
        cancel,
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let r = rt.block_on(mgr.prompt(req));
    eprintln!("prompt result: {r:?}");
    let mut text = String::new();
    while let Ok(ev) = ev_rx.try_recv() {
        if let AgentEvent::TextDelta(d) = ev {
            text.push_str(&d);
        }
    }
    eprintln!("ANSWER: {text:?}");
    assert!(
        text.to_lowercase().contains("haiku"),
        "expected the model set via session/set_model (haiku) to answer, got: {text:?}"
    );
}

#[test]
fn parse_models_from_config_options() {
    // Current adapter shape: models live under configOptions -> the `model` select option.
    let res = json!({
        "sessionId": "s",
        "configOptions": [
            {"id": "mode", "options": [{"value": "auto"}]},
            {"id": "model", "type": "select", "options": [
                {"value": "default"}, {"value": "sonnet"}, {"value": "opus"}
            ]}
        ]
    });
    assert_eq!(
        parse_available_models(&res),
        vec!["default", "sonnet", "opus"]
    );
}

#[test]
fn parse_models_from_legacy_shape() {
    // Older @zed-industries/claude-code-acp shape.
    let res = json!({
        "sessionId": "s",
        "models": {"availableModels": [{"modelId": "sonnet"}, {"modelId": "haiku"}]}
    });
    assert_eq!(parse_available_models(&res), vec!["sonnet", "haiku"]);
}

#[test]
fn parse_models_empty_when_absent() {
    assert!(parse_available_models(&json!({"sessionId": "s"})).is_empty());
}

#[test]
fn build_prompt_blocks_includes_text_and_image() {
    let blocks = build_prompt_blocks("hello", &[("image/png".to_string(), vec![1, 2, 3])]);
    let arr = blocks.as_array().unwrap();
    assert_eq!(arr[0]["type"], "text");
    assert_eq!(arr[0]["text"], "hello");
    assert_eq!(arr[1]["type"], "image");
    assert_eq!(arr[1]["mimeType"], "image/png");
}
