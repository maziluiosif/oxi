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
    UpdateState::default().emit_update(
        &json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hi"}}),
        &tx,
    );
    UpdateState::default().emit_update(
        &json!({"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"hmm"}}),
        &tx,
    );
    let evs = drain(&rx);
    assert!(matches!(&evs[0], AgentEvent::TextDelta(s) if s == "hi"));
    assert!(matches!(&evs[1], AgentEvent::ThinkingDelta(s) if s == "hmm"));
}

#[test]
fn tool_call_emits_complete_snapshot() {
    let (tx, rx) = channel();
    UpdateState::default().emit_update(
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
    let AgentEvent::ToolUpdate(tool) = &evs[0] else {
        panic!("expected tool snapshot")
    };
    assert_eq!(tool.name, "read");
    assert_eq!(tool.tool_call_id, "c1");
    assert_eq!(tool.output, "file body");
    assert_eq!(tool.metadata.status, crate::model::ToolStatus::Completed);
    assert_eq!(evs.len(), 1);
}

#[test]
fn failed_tool_update_marks_error() {
    let (tx, rx) = channel();
    UpdateState::default().emit_update(
        &json!({"sessionUpdate":"tool_call_update","toolCallId":"c2","status":"failed"}),
        &tx,
    );
    let evs = drain(&rx);
    let AgentEvent::ToolUpdate(tool) = &evs[0] else {
        panic!("expected snapshot")
    };
    assert_eq!(tool.metadata.status, crate::model::ToolStatus::Failed);
}

#[test]
fn diff_content_becomes_unified_diff() {
    let (tx, rx) = channel();
    UpdateState::default().emit_update(
        &json!({
            "sessionUpdate":"tool_call_update", "toolCallId":"c3", "status":"completed",
            "content":[{"type":"diff","path":"f.rs","oldText":"a\n","newText":"b\n"}]
        }),
        &tx,
    );
    let evs = drain(&rx);
    let AgentEvent::ToolUpdate(tool) = &evs[0] else {
        panic!("expected snapshot")
    };
    let diff = tool.diff.as_deref().unwrap();
    assert!(diff.contains("-a"));
    assert!(diff.contains("+b"));
    assert!(diff.contains("f.rs"));
    assert!(diff.contains("@@"));
}

#[test]
fn pick_option_prefers_requested_kind() {
    let options = json!([
        {"optionId":"a","kind":"allow_once"},
        {"optionId":"b","kind":"allow_always"},
        {"optionId":"c","kind":"reject_once"}
    ]);
    let opts = options.as_array().unwrap();
    assert_eq!(
        pick_option(opts, &["allow_always", "allow_once"]).as_deref(),
        Some("b")
    );
    assert_eq!(
        pick_option(opts, &["reject_once", "reject"]).as_deref(),
        Some("c")
    );
}

#[test]
fn pick_option_never_falls_back_to_the_opposite_decision() {
    let options = json!([{"optionId":"x","kind":"allow_once"}]);
    let opts = options.as_array().unwrap();
    assert_eq!(pick_option(opts, &["reject_once", "reject"]), None);
    let options = json!([{"optionId":"no","kind":"reject_always"}]);
    assert_eq!(
        pick_option(options.as_array().unwrap(), &["allow_once"]),
        None
    );
    assert_eq!(
        pick_option(options.as_array().unwrap(), &["reject_once"]).as_deref(),
        Some("no")
    );
    assert_eq!(pick_option(&[], &["reject_once"]), None);
}

#[test]
fn pick_option_skips_missing_and_empty_ids() {
    let options = json!([
        {"kind":"reject_once"},
        {"optionId":"","kind":"reject_once"},
        {"optionId":"no","kind":"reject_once"}
    ]);
    assert_eq!(
        pick_option(options.as_array().unwrap(), &["reject_once"]).as_deref(),
        Some("no")
    );
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
        history: String::new(),
        images: Vec::new(),
        event_tx: ev_tx,
        approval_rx: appr_rx,
        approval_policy: ApprovalPolicy::disabled(),
        cancel,
        plan_mode: false,
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

#[test]
fn generated_image_becomes_inline_markdown_image() {
    let dir = std::env::temp_dir().join("oxi acp image test");
    std::fs::create_dir_all(&dir).unwrap();
    let saved = dir.join("ig_1.png");
    std::fs::write(&saved, [0x89, b'P', b'N', b'G']).unwrap();
    let (tx, rx) = channel();
    UpdateState::default().emit_update(
        &json!({
            "sessionUpdate":"tool_call_update",
            "toolCallId":"ig_1",
            "status":"completed",
            "content":[
                {"type":"content","content":{"type":"text","text":"Revised prompt: a cat"}},
                {"type":"content","content":{
                    "type":"image","mimeType":"image/png","data":"iVBORw==",
                    "uri": saved.to_string_lossy()
                }}
            ]
        }),
        &tx,
    );
    let evs = drain(&rx);
    assert!(
        matches!(&evs[0], AgentEvent::ToolUpdate(tool) if tool.output == "Revised prompt: a cat")
    );
    let AgentEvent::TextDelta(md) = &evs[1] else {
        panic!("expected image markdown");
    };
    let dest = pulldown_cmark::Parser::new(md)
        .find_map(|ev| match ev {
            pulldown_cmark::Event::Start(pulldown_cmark::Tag::Image { dest_url, .. }) => {
                Some(dest_url.to_string())
            }
            _ => None,
        })
        .expect("markdown image");
    assert!(dest.starts_with("file://"));
    assert_eq!(
        super::update_events::file_uri_to_path(&dest),
        std::path::PathBuf::from(saved.to_string_lossy().replace('\\', "/"))
    );
    assert_eq!(evs.len(), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

fn replay_updates(updates: &[Value]) -> Vec<crate::model::AssistantBlock> {
    let (tx, rx) = channel();
    let mut state = UpdateState::default();
    let mut blocks = Vec::new();
    for update in updates {
        state.emit_update(update, &tx);
        for event in drain(&rx) {
            if let AgentEvent::ToolUpdate(tool) = event {
                crate::model::apply_tool_update(&mut blocks, *tool);
            }
        }
    }
    blocks
}

#[test]
fn partial_updates_preserve_live_diff_and_late_metadata_through_completion() {
    use crate::model::{AssistantBlock, ToolStatus};
    let initial = json!({"sessionUpdate":"tool_call", "toolCallId":"edit", "kind":"edit", "title":"Preparing edit"});
    let content = json!({"sessionUpdate":"tool_call_update", "toolCallId":"edit", "status":"in_progress",
    "content":[
        {"type":"diff","path":"a.rs","oldText":"same\nold\n","newText":"same\nnew\n"},
        {"type":"diff","path":"b.rs","oldText":null,"newText":"nou 🦀\n"}
    ]});
    let live = replay_updates(&[initial.clone(), content.clone()]);
    assert!(matches!(
        &live[0],
        AssistantBlock::Tool {
            diff: Some(_),
            is_error: None,
            ..
        }
    ));
    let blocks = replay_updates(&[
        initial,
        content,
        json!({"sessionUpdate":"tool_call_update", "toolCallId":"edit", "title":"Update two files", "rawInput":{"file_path":"a.rs"}, "locations":[{"path":"a.rs","line":2},{"path":"b.rs"}]}),
        json!({"sessionUpdate":"tool_call_update", "toolCallId":"edit", "status":"completed"}),
        json!({"sessionUpdate":"tool_call_update", "toolCallId":"edit", "status":"completed", "content":null}),
    ]);
    assert_eq!(blocks.len(), 1);
    let AssistantBlock::Tool {
        diff: Some(diff),
        metadata: Some(meta),
        args_summary: Some(args),
        is_error,
        ..
    } = &blocks[0]
    else {
        panic!("complete tool")
    };
    assert!(diff.contains("a.rs") && diff.contains("b.rs") && diff.contains("+nou 🦀"));
    assert!(!diff.contains("-same"));
    assert_eq!(meta.title, "Update two files");
    assert_eq!(meta.locations.len(), 2);
    assert_eq!(meta.status, ToolStatus::Completed);
    assert_eq!(*is_error, Some(false));
    assert_eq!(
        serde_json::from_str::<Value>(args).unwrap()["file_path"],
        "a.rs"
    );
}

#[test]
fn interleaved_orphan_updates_and_collection_replacements_keep_tool_identity() {
    use crate::model::AssistantBlock;
    let blocks = replay_updates(&[
        json!({"sessionUpdate":"tool_call", "toolCallId":"a", "kind":"read"}),
        json!({"sessionUpdate":"tool_call_update", "toolCallId":"b", "rawOutput":{"message":"denied"}, "status":"failed"}),
        json!({"sessionUpdate":"tool_call", "toolCallId":"b", "kind":"execute", "title":"Run checks"}),
        json!({"sessionUpdate":"tool_call_update", "toolCallId":"a", "content":[{"type":"content","content":{"type":"text","text":"old output"}},{"type":"diff","path":"a","oldText":"a","newText":"b"}],"locations":[{"path":"a"}]}),
        json!({"sessionUpdate":"tool_call_update", "toolCallId":"a", "content":[], "locations":[]}),
        json!({"sessionUpdate":"tool_call_update", "toolCallId":"", "rawOutput":"invalid"}),
    ]);
    assert_eq!(blocks.len(), 2);
    let AssistantBlock::Tool {
        output,
        diff,
        metadata: Some(meta),
        ..
    } = &blocks[0]
    else {
        panic!()
    };
    assert!(output.is_empty() && diff.is_none() && meta.locations.is_empty());
    let AssistantBlock::Tool {
        name,
        output,
        is_error,
        metadata: Some(meta),
        ..
    } = &blocks[1]
    else {
        panic!()
    };
    assert_eq!(name, "bash");
    assert!(output.contains("denied"));
    assert_eq!(*is_error, Some(true));
    assert_eq!(meta.title, "Run checks");
}

#[test]
fn acp_metadata_diff_and_valid_long_args_survive_session_roundtrip() {
    use crate::model::{AssistantBlock, ChatMessage, MsgRole, ToolStatus};
    let blocks = replay_updates(&[
        json!({"sessionUpdate":"tool_call", "toolCallId":"a", "kind":"edit", "title":"Update Unicode", "status":"in_progress", "rawInput":{"content":"🦀".repeat(1200)}, "locations":[{"path":"a.rs","line":2}], "content":[{"type":"diff","path":"a.rs","oldText":"old","newText":"new"}]}),
    ]);
    let mut message = ChatMessage {
        role: MsgRole::Assistant,
        text: String::new(),
        is_summary: false,
        attachments: vec![],
        blocks,
        streaming: true,
        started_at: None,
        worked_duration: None,
    };
    message.finish_streaming();
    let entries = crate::session_store::chat_message_to_json_entries(&message);
    let restored = crate::hydrate::messages_from_get_messages(&json!({"messages":entries}));
    let AssistantBlock::Tool {
        metadata: Some(meta),
        diff: Some(diff),
        args_summary: Some(args),
        is_error,
        ..
    } = &restored[0].blocks[0]
    else {
        panic!("roundtrip tool")
    };
    assert_eq!(meta.status, ToolStatus::Interrupted);
    assert_eq!(meta.title, "Update Unicode");
    assert_eq!(meta.locations[0].line, Some(2));
    assert!(diff.contains("+new"));
    assert_eq!(
        serde_json::from_str::<Value>(args).unwrap()["content"],
        "🦀".repeat(1200)
    );
    assert_eq!(*is_error, None);
    assert!(!crate::model::assistant_is_effectively_empty(
        &restored[0].blocks,
        false
    ));
}

#[test]
fn status_only_tools_remain_visible_and_large_output_is_bounded() {
    use crate::model::AssistantBlock;
    let blocks = replay_updates(&[
        json!({"sessionUpdate":"tool_call", "toolCallId":"empty", "status":"completed", "title":"Done"}),
    ]);
    assert!(!crate::model::assistant_is_effectively_empty(
        &blocks, false
    ));
    let blocks = replay_updates(&[
        json!({"sessionUpdate":"tool_call", "toolCallId":"large", "rawOutput":"🦀".repeat(41000)}),
    ]);
    assert!(
        matches!(&blocks[0],AssistantBlock::Tool {output,output_truncated:true,..} if output.chars().count()==40000)
    );
}

#[test]
fn notifications_are_scoped_to_the_active_session() {
    let (tx, rx) = channel();
    let (perm_tx, _) = mpsc::unbounded_channel();
    let mut ctx = PromptCtx {
        session_id: "active".into(),
        plan_mode: false,
        updates: UpdateState::default(),
        event_tx: tx,
        perm_tx,
    };
    let update = json!({"sessionUpdate":"tool_call","toolCallId":"one","title":"Read"});
    ctx.emit_notification(&json!({"sessionId":"stale","update":update}));
    ctx.emit_notification(&json!({"update":update}));
    assert!(drain(&rx).is_empty());
    ctx.emit_notification(&json!({"sessionId":"active","update":update}));
    assert_eq!(drain(&rx).len(), 1);
}

#[test]
fn client_fs_writes_respect_plan_mode_and_session_identity() {
    let dir = std::env::temp_dir().join(format!("oxi-acp-write-{}", rand::random::<u64>()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("target.txt");
    std::fs::write(&path, "original").unwrap();
    let (tx, _rx) = channel();
    let (perm_tx, _) = mpsc::unbounded_channel();
    let mut ctx = PromptCtx {
        session_id: "active".into(),
        plan_mode: true,
        updates: UpdateState::default(),
        event_tx: tx,
        perm_tx,
    };
    let mut args = json!({"sessionId":"active", "path":path, "content":"changed"});
    assert_eq!(ctx.write_text_file(&args), Err(PLAN_MODE_REFUSAL.into()));
    assert_eq!(fs_read_text(&args).unwrap(), "original");
    // Plan mode must refuse creation as well as overwriting an existing file.
    args["path"] = json!(dir.join("new.txt"));
    assert!(ctx.write_text_file(&args).is_err());
    assert!(!dir.join("new.txt").exists());
    args["path"] = json!(path);
    ctx.plan_mode = false;
    for session in [json!("stale"), Value::Null] {
        args["sessionId"] = session;
        assert!(ctx.write_text_file(&args).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "original");
    }
    args["sessionId"] = json!("active");
    ctx.write_text_file(&args).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "changed");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn permission_details_merge_into_the_same_tool() {
    let (tx, rx) = channel();
    let mut state = UpdateState::default();
    state.emit_tool(&json!({"toolCallId":"edit","title":"Approve change","kind":"edit","content":[{"type":"diff","path":"a","oldText":"old","newText":"new"}]}),&tx);
    state.emit_update(
        &json!({"sessionUpdate":"tool_call","toolCallId":"edit","status":"in_progress"}),
        &tx,
    );
    let mut blocks = Vec::new();
    for event in drain(&rx) {
        if let AgentEvent::ToolUpdate(tool) = event {
            crate::model::apply_tool_update(&mut blocks, *tool);
        }
    }
    assert_eq!(blocks.len(), 1);
    assert!(
        matches!(&blocks[0],crate::model::AssistantBlock::Tool{diff:Some(_),metadata:Some(meta),..} if meta.title=="Approve change")
    );
}

#[test]
fn empty_new_file_and_missing_locations_have_a_visible_target() {
    let blocks = replay_updates(&[
        json!({"sessionUpdate":"tool_call","toolCallId":"empty","kind":"edit","content":[{"type":"diff","path":"empty.rs","oldText":null,"newText":""}]}),
    ]);
    let crate::model::AssistantBlock::Tool {
        output,
        metadata: Some(meta),
        ..
    } = &blocks[0]
    else {
        panic!()
    };
    assert!(output.contains("New empty file: empty.rs"));
    assert_eq!(meta.locations[0].path, "empty.rs");
}
