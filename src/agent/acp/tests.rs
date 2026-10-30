use super::client_fs::fs_read_text;
use super::permissions::{is_plan_file_edit, permission_name_args, pick_option};
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
    // An MCP tool is named by its tool, not by a title that changes per call.
    let (name, _) = permission_name_args(&json!({"kind":"other","title":"Create issue #12",
        "_meta":{"claudeCode":{"toolName":"mcp__gh__create_issue"}}}));
    assert_eq!(name, "mcp__gh__create_issue");
}

#[test]
fn a_relaunched_agent_gets_back_the_options_the_user_set() {
    let previous = super::config_options::parse(&json!([
        {"id":"mode","category":"mode","type":"select","currentValue":"acceptEdits",
         "options":[{"value":"default"},{"value":"acceptEdits"},{"value":"plan"}]},
        {"id":"fast","type":"boolean","currentValue":true},
        {"id":"model","category":"model","type":"select","currentValue":"opus",
         "options":[{"value":"opus"},{"value":"sonnet"}]},
        {"id":"gone","type":"boolean","currentValue":true}
    ]));
    let fresh = json!([
        {"id":"mode","category":"mode","type":"select","currentValue":"default",
         "options":[{"value":"default"},{"value":"acceptEdits"},{"value":"plan"}]},
        {"id":"fast","type":"boolean","currentValue":false},
        {"id":"model","category":"model","type":"select","currentValue":"sonnet",
         "options":[{"value":"opus"},{"value":"sonnet"}]}
    ]);
    // The model comes from oxi's settings; an option the agent no longer offers is skipped.
    assert_eq!(
        super::options_to_restore(&fresh, &previous),
        vec![
            ("mode".to_string(), json!("acceptEdits")),
            ("fast".to_string(), json!(true))
        ]
    );
    // Plan mode is oxi's to switch per turn, never restored.
    let in_plan = super::config_options::parse(&json!([
        {"id":"mode","category":"mode","type":"select","currentValue":"plan",
         "options":[{"value":"default"},{"value":"plan"}]}
    ]));
    assert!(super::options_to_restore(&fresh, &in_plan).is_empty());
}

#[test]
fn approval_settings_cover_acp_tools_like_oxi_tools() {
    let off = ApprovalPolicy::disabled();
    let on = ApprovalPolicy {
        write_edit: true,
        bash: true,
    };
    for kind in ["read", "search", "fetch", "think"] {
        let (name, _) = permission_name_args(&json!({"kind":kind,"title":"x"}));
        assert!(!on.requires_approval(&name), "{kind} is read-only");
    }
    for kind in ["execute", "edit", "delete", "move"] {
        let (name, _) = permission_name_args(&json!({"kind":kind,"title":"x"}));
        assert!(on.requires_approval(&name), "{kind} asks when switched on");
        assert!(
            !off.requires_approval(&name),
            "{kind} runs when switched off"
        );
    }
    // External tools ask like oxi's own MCP tools, under their own name.
    let (name, _) = permission_name_args(&json!({"kind":"other","title":"mcp__gh__create_issue"}));
    assert_eq!(name, "mcp__gh__create_issue");
    assert!(off.requires_approval(&name));
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
        mcp_servers: Vec::new(),
        text: "Reply with ONLY one word naming your model family: Opus, Sonnet, or Haiku."
            .to_string(),
        history: String::new(),
        missed: String::new(),
        archive: Vec::new(),
        images: Vec::new(),
        resources: Vec::new(),
        event_tx: ev_tx,
        approval_rx: appr_rx,
        approval_policy: ApprovalPolicy::disabled(),
        bash_allowlist: Vec::new(),
        allowed_tools: Vec::new(),
        cancel,
        plan_mode: false,
        evidence_policy: Default::default(),
        evidence_base: None,
        steer: None,
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
    let blocks = build_prompt_blocks("hello", &[("image/png".to_string(), vec![1, 2, 3])], &[]);
    let arr = blocks.as_array().unwrap();
    assert_eq!(arr[0]["type"], "text");
    assert_eq!(arr[0]["text"], "hello");
    assert_eq!(arr[1]["type"], "image");
    assert_eq!(arr[1]["mimeType"], "image/png");
}

#[test]
fn mentioned_paths_become_embedded_or_linked_resources() {
    let dir = std::env::temp_dir().join(format!("oxi-acp-resources-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    let file = dir.join("note.md");
    std::fs::write(&file, "# hi").unwrap();
    let paths = [file.clone(), dir.join("sub")];

    let embedded = resource_blocks(&paths, true);
    assert_eq!(embedded[0]["type"], "resource");
    assert_eq!(embedded[0]["resource"]["text"], "# hi");
    assert!(
        embedded[0]["resource"]["uri"]
            .as_str()
            .unwrap()
            .starts_with("file://")
    );
    assert_eq!(embedded[1]["type"], "resource_link");
    assert_eq!(embedded[1]["name"], "sub");

    let linked = resource_blocks(&paths, false);
    assert_eq!(linked[0]["type"], "resource_link");
    assert_eq!(linked[0]["size"], 4);
    let blocks = build_prompt_blocks("see @note.md", &[], &linked);
    assert_eq!(blocks.as_array().unwrap().len(), 3);
    let _ = std::fs::remove_dir_all(dir);
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
        route: None,
        changes: None,
        evidence: None,
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
        cwd: std::env::temp_dir(),
        plan_mode: false,
        updates: UpdateState::default(),
        children: Default::default(),
        event_tx: tx,
        perm_tx,
        cycle_end: None,
        last_update: std::time::Instant::now(),
        replay: None,
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
        cwd: std::env::temp_dir(),
        plan_mode: true,
        updates: UpdateState::default(),
        children: Default::default(),
        event_tx: tx,
        perm_tx,
        cycle_end: None,
        last_update: std::time::Instant::now(),
        replay: None,
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

#[test]
fn agent_plan_becomes_a_todo_write_checklist() {
    use crate::model::AssistantBlock;
    let blocks = replay_updates(&[
        json!({"sessionUpdate":"plan","entries":[
            {"content":"Read the code","status":"completed","priority":"medium"},
            {"content":"Fix the bug","status":"in_progress","priority":"high"},
            {"content":"  ","status":"pending","priority":"low"}
        ]}),
        json!({"sessionUpdate":"plan","entries":[
            {"content":"Read the code","status":"completed","priority":"medium"},
            {"content":"Fix the bug","status":"completed","priority":"high"}
        ]}),
    ]);
    // Every plan replaces the last one in place.
    assert_eq!(blocks.len(), 1);
    let AssistantBlock::Tool {
        name, args_summary, ..
    } = &blocks[0]
    else {
        panic!("expected a tool block")
    };
    assert_eq!(name, "todo_write");
    let args: Value = serde_json::from_str(args_summary.as_deref().unwrap()).unwrap();
    let todos = crate::agent::tools::parse_todos(&args).unwrap();
    assert_eq!(todos.len(), 2);
    assert!(
        todos
            .iter()
            .all(|t| t.status == crate::agent::tools::TodoStatus::Completed)
    );
}

#[test]
fn oxi_todo_tool_drives_the_checklist_and_tool_search_stays_hidden() {
    use crate::model::AssistantBlock;
    let (tx, rx) = channel();
    let mut state = UpdateState::default();
    let todo_call = |id: &str, status: &str| {
        json!({"sessionUpdate":"tool_call","toolCallId":id,"title":"mcp__oxi__todo_write",
        "kind":"other","status":"completed","rawInput":{"todos":[
            {"content":"Build","status":status}
        ]}})
    };
    let updates = [
        json!({"sessionUpdate":"tool_call","toolCallId":"s1","title":"ToolSearch","kind":"other",
            "status":"completed","rawInput":{"query":"select:mcp__oxi__todo_write"}}),
        json!({"sessionUpdate":"tool_call","toolCallId":"t1","title":"mcp__oxi__todo_write",
            "kind":"other","status":"pending","rawInput":{}}),
        todo_call("t1", "in_progress"),
        todo_call("t1", "in_progress"),
        todo_call("t2", "completed"),
    ];
    let mut events = Vec::new();
    for update in &updates {
        state.emit_update(update, &tx);
        events.extend(drain(&rx));
    }
    // One checklist per distinct list; nothing for ToolSearch or the empty pending call.
    assert_eq!(events.len(), 2);
    let mut blocks = Vec::new();
    for event in events {
        let AgentEvent::ToolUpdate(tool) = event else {
            panic!("expected a tool update")
        };
        crate::model::apply_tool_update(&mut blocks, *tool);
    }
    assert_eq!(blocks.len(), 1);
    let AssistantBlock::Tool {
        name, args_summary, ..
    } = &blocks[0]
    else {
        panic!("expected a tool block")
    };
    assert_eq!(name, "todo_write");
    let args: Value = serde_json::from_str(args_summary.as_deref().unwrap()).unwrap();
    let todos = crate::agent::tools::parse_todos(&args).unwrap();
    assert_eq!(todos[0].status, crate::agent::tools::TodoStatus::Completed);
}

#[test]
fn oxi_tools_are_recognized_in_permission_requests() {
    assert_eq!(
        oxi_mcp::oxi_tool(&json!({"title":"mcp__oxi__todo_write","kind":"other"})),
        Some("todo_write")
    );
    assert_eq!(
        oxi_mcp::oxi_tool(&json!({"title":"Bash","kind":"execute"})),
        None
    );
    // The scratchpad asks under the native tool's name, so its approval settings apply.
    let (name, args) = permission_name_args(&json!({
        "title": "mcp__oxi__scratchpad",
        "kind": "other",
        "rawInput": {"mode": "append", "content": "x"},
    }));
    assert_eq!(name, "scratchpad");
    assert_eq!(args.unwrap()["mode"], "append");
}

#[test]
fn terminal_output_deltas_accumulate_into_the_tool_output() {
    use crate::model::AssistantBlock;
    let blocks = replay_updates(&[
        json!({"sessionUpdate":"tool_call","toolCallId":"sh","kind":"execute","status":"in_progress",
               "rawInput":{"command":"cargo build"},
               "content":[{"type":"terminal","terminalId":"sh"}],
               "_meta":{"terminal_info":{"terminal_id":"sh"}}}),
        json!({"sessionUpdate":"tool_call_update","toolCallId":"sh",
               "_meta":{"terminal_output_delta":{"terminal_id":"sh","data":"Compiling oxi\n"}}}),
        json!({"sessionUpdate":"tool_call_update","toolCallId":"sh",
               "_meta":{"terminal_output_delta":{"terminal_id":"sh","data":"Finished\n"}}}),
        json!({"sessionUpdate":"tool_call_update","toolCallId":"sh","status":"completed",
               "_meta":{"terminal_exit":{"terminal_id":"sh","exit_code":0}}}),
    ]);
    assert!(matches!(
        &blocks[0],
        AssistantBlock::Tool { name, output, is_error: Some(false), .. }
            if name == "bash" && output == "Compiling oxi\nFinished\n"
    ));
}

#[test]
fn exit_plan_mode_plan_is_shown_once_as_text() {
    let (tx, rx) = channel();
    let mut state = UpdateState::default();
    let call = json!({"sessionUpdate":"tool_call","toolCallId":"exit","kind":"switch_mode",
                      "title":"Ready to code?","rawInput":{"plan":"1. Do the thing"}});
    state.emit_update(&call, &tx);
    state.emit_update(
        &json!({"sessionUpdate":"tool_call_update","toolCallId":"exit","status":"failed"}),
        &tx,
    );
    let texts: Vec<String> = drain(&rx)
        .into_iter()
        .filter_map(|e| match e {
            AgentEvent::TextDelta(t) => Some(t),
            _ => None,
        })
        .collect();
    assert_eq!(texts, ["\n\n1. Do the thing\n\n"]);

    // Codex streams the plan as answer text before asking to implement it.
    let mut state = UpdateState::default();
    state.emit_update(
        &json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"1. Do the thing\n"}}),
        &tx,
    );
    state.emit_update(&call, &tx);
    let texts = drain(&rx)
        .into_iter()
        .filter(|e| matches!(e, AgentEvent::TextDelta(_)))
        .count();
    assert_eq!(texts, 1);
}

#[test]
fn usage_updates_and_prompt_usage_are_reported() {
    let (tx, rx) = channel();
    UpdateState::default().emit_update(
        &json!({"sessionUpdate":"usage_update","used":53000,"size":200000}),
        &tx,
    );
    assert!(matches!(
        drain(&rx).as_slice(),
        [AgentEvent::ContextUsage {
            used: 53000,
            size: 200000
        }]
    ));

    // Claude Code: input excludes the cached part.
    let claude = prompt_usage(
        &json!({"inputTokens":10,"outputTokens":5,"cachedReadTokens":100,
                                      "cachedWriteTokens":20,"totalTokens":135}),
    )
    .unwrap();
    assert_eq!(
        (
            claude.input_tokens,
            claude.cache_read_input_tokens,
            claude.cache_creation_input_tokens
        ),
        (10, 100, 20)
    );
    // Codex: input includes the cached part.
    let codex = prompt_usage(
        &json!({"inputTokens":1000,"outputTokens":50,"cachedReadTokens":800,
                                     "totalTokens":1050,"thoughtTokens":20}),
    )
    .unwrap();
    assert_eq!(
        (codex.input_tokens, codex.cache_read_input_tokens),
        (200, 800)
    );
    assert_eq!(codex.total_input(), 1000);
    assert!(prompt_usage(&Value::Null).is_none());
}

#[test]
fn plan_file_edits_are_part_of_planning() {
    let Some(home) = dirs::home_dir() else {
        return;
    };
    let plan = home.join(".claude").join("plans").join("plan.md");
    let plan = plan.to_string_lossy();
    assert!(is_plan_file_edit(
        &json!({"kind":"edit","locations":[{"path": plan}]})
    ));
    assert!(is_plan_file_edit(
        &json!({"kind":"edit","rawInput":{"file_path": plan}})
    ));
    assert!(!is_plan_file_edit(
        &json!({"kind":"edit","locations":[{"path":"/repo/src/main.rs"}]})
    ));
    assert!(!is_plan_file_edit(
        &json!({"kind":"execute","locations":[{"path": plan}]})
    ));
}

/// Live check against a real adapter: `OXI_ACP_E2E_CMD` is the launch line, `OXI_ACP_E2E_PLAN=1`
/// runs the turn in plan mode. Prints every event.
///   OXI_ACP_E2E_CMD="npx -y @agentclientprotocol/codex-acp" cargo test acp_live_features -- --ignored --nocapture
#[test]
#[ignore]
fn acp_live_features() {
    let Ok(command_line) = std::env::var("OXI_ACP_E2E_CMD") else {
        return;
    };
    let plan_mode = std::env::var("OXI_ACP_E2E_PLAN").is_ok();
    let text = std::env::var("OXI_ACP_E2E_TEXT").unwrap_or_else(|_| {
        "Run the shell command `echo oxi-terminal-check` and then reply with just: done".into()
    });
    let cwd = std::env::temp_dir().join(format!("oxi-acp-live-{}", rand::random::<u32>()));
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::write(cwd.join("README.md"), "# demo\n").unwrap();
    let mgr = AcpManager::spawn();
    let (ev_tx, ev_rx) = channel::<AgentEvent>();
    let (_appr_tx, appr_rx) = channel::<ApprovalDecision>();
    let req = AcpPrompt {
        session_key: format!("live-{}", rand::random::<u32>()),
        cwd: cwd.clone(),
        command_line,
        env: Vec::new(),
        model: std::env::var("OXI_ACP_E2E_MODEL").unwrap_or_default(),
        effort: "low".into(),
        mcp_servers: std::env::var("OXI_ACP_E2E_MCP")
            .map(|script| {
                vec![crate::settings::McpServerConfig {
                    name: "probe".into(),
                    command: "python3".into(),
                    args: vec![script],
                    ..Default::default()
                }]
            })
            .unwrap_or_default(),
        text,
        history: String::new(),
        missed: String::new(),
        archive: Vec::new(),
        images: Vec::new(),
        resources: Vec::new(),
        event_tx: ev_tx,
        approval_rx: appr_rx,
        approval_policy: ApprovalPolicy::disabled(),
        bash_allowlist: Vec::new(),
        allowed_tools: Vec::new(),
        cancel: Arc::new(AtomicBool::new(false)),
        plan_mode,
        evidence_policy: Default::default(),
        evidence_base: None,
        steer: None,
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let r = rt.block_on(mgr.prompt(req));
    eprintln!("prompt result: {r:?}");
    let mut text = String::new();
    while let Ok(ev) = ev_rx.try_recv() {
        match ev {
            AgentEvent::TextDelta(d) => text.push_str(&d),
            AgentEvent::ToolUpdate(t) => eprintln!(
                "TOOL {} ({:?}) [{}] {:?} args={:?} output={:?}",
                t.name, t.metadata.title, t.tool_call_id, t.metadata.status, t.args, t.output
            ),
            AgentEvent::ThinkingDelta(_) => {}
            other => eprintln!("EVENT {other:?}"),
        }
    }
    eprintln!("TEXT: {text}");
    eprintln!(
        "README now: {:?}",
        std::fs::read_to_string(cwd.join("README.md"))
    );
}

/// A fake agent that ends its turn at once, then runs one of its own: what Claude Code does when
/// a command it left in the background finishes.
const SELF_STARTING_AGENT: &str = r#"
import json, sys, threading, time
def send(v):
    print(json.dumps(dict(jsonrpc='2.0', **v)), flush=True)
def update(v):
    send(dict(method='session/update', params=dict(sessionId='fake-session', update=v)))
answered = threading.Event()
answer = {}
def own_turn():
    time.sleep(0.3)
    # Asks before saying anything: the request must wait for the turn the client attaches.
    tool = dict(toolCallId='t1', title='Run tests', kind='execute', rawInput=dict(command='make test'))
    send(dict(id=900, method='session/request_permission', params=dict(sessionId='fake-session', toolCall=tool,
        options=[dict(optionId='yes', kind='allow_once', name='Allow'), dict(optionId='no', kind='reject_once', name='Reject')])))
    answered.wait(10)
    outcome = answer.get('outcome', {}).get('optionId', 'cancelled')
    update(dict(sessionUpdate='agent_message_chunk', content=dict(type='text', text='The build finished: ' + outcome)))
    update(dict(sessionUpdate='usage_update', used=1, size=2, _meta={'_claude/origin': dict(kind='task-notification')}))
for line in sys.stdin:
    m = json.loads(line)
    method = m.get('method')
    if method is None:
        answer.update(m.get('result', {}))
        answered.set()
    elif method == 'initialize':
        send(dict(id=m['id'], result=dict(protocolVersion=1, agentCapabilities={})))
    elif method == 'session/new':
        send(dict(id=m['id'], result=dict(sessionId='fake-session')))
    elif method == 'session/prompt':
        update(dict(sessionUpdate='agent_message_chunk', content=dict(type='text', text='Building in the background.')))
        send(dict(id=m['id'], result=dict(stopReason='end_turn')))
        threading.Thread(target=own_turn).start()
    elif 'id' in m:
        send(dict(id=m['id'], result={}))
"#;

fn texts(rx: &StdReceiver<AgentEvent>) -> String {
    drain(rx)
        .into_iter()
        .filter_map(|ev| match ev {
            AgentEvent::TextDelta(text) => Some(text),
            _ => None,
        })
        .collect()
}

#[test]
fn stop_reasons_other_than_finishing_are_explained() {
    assert!(stop_notice("max_tokens").unwrap().contains("token limit"));
    assert!(stop_notice("max_turn_requests").is_some());
    assert!(stop_notice("refusal").is_some());
    assert_eq!(stop_notice("end_turn"), None);
    assert_eq!(stop_notice("cancelled"), None);
    assert_eq!(stop_notice(""), None);
}

#[test]
fn a_turn_the_agent_starts_itself_is_held_until_attached() {
    let dir = std::env::temp_dir().join(format!("oxi-acp-autonomous-{}", rand::random::<u64>()));
    std::fs::create_dir_all(&dir).unwrap();
    let script = dir.join("agent.py");
    std::fs::write(&script, SELF_STARTING_AGENT).unwrap();
    let session_key = format!("autonomous-{}", rand::random::<u64>());
    let mgr = AcpManager::spawn();
    let rt = tokio::runtime::Runtime::new().unwrap();

    let (ev_tx, ev_rx) = channel::<AgentEvent>();
    let (_appr_tx, appr_rx) = channel::<ApprovalDecision>();
    let prompt = AcpPrompt {
        session_key: session_key.clone(),
        cwd: dir.clone(),
        command_line: format!(
            "{} {}",
            crate::agent::test_python_executable().display(),
            script.display()
        ),
        env: Vec::new(),
        model: String::new(),
        effort: String::new(),
        mcp_servers: Vec::new(),
        text: "build it".into(),
        history: String::new(),
        missed: String::new(),
        archive: Vec::new(),
        images: Vec::new(),
        resources: Vec::new(),
        event_tx: ev_tx,
        approval_rx: appr_rx,
        approval_policy: ApprovalPolicy::disabled(),
        bash_allowlist: Vec::new(),
        allowed_tools: Vec::new(),
        cancel: Arc::new(AtomicBool::new(false)),
        plan_mode: false,
        evidence_policy: Default::default(),
        evidence_base: None,
        steer: None,
    };
    rt.block_on(mgr.prompt(prompt)).unwrap();
    assert_eq!(texts(&ev_rx), "Building in the background.");

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !pending_autonomous().contains(&session_key) {
        assert!(
            std::time::Instant::now() < deadline,
            "the agent's own turn was not announced"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    let (ev_tx, ev_rx) = channel::<AgentEvent>();
    let (_appr_tx, appr_rx) = channel::<ApprovalDecision>();
    let attach = AcpAttach {
        session_key: session_key.clone(),
        cwd: dir.clone(),
        event_tx: ev_tx,
        approval_rx: appr_rx,
        approval_policy: ApprovalPolicy::disabled(),
        bash_allowlist: Vec::new(),
        allowed_tools: Vec::new(),
        cancel: Arc::new(AtomicBool::new(false)),
        plan_mode: false,
    };
    // Returns at the usage update that closes the agent's cycle, not at the quiet limit.
    rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(10), mgr.attach(attach))
            .await
            .expect("the attached turn did not end with the agent's cycle")
    })
    .unwrap();
    // The permission request sent between turns waited for the attached turn and was answered.
    assert_eq!(texts(&ev_rx), "The build finished: yes");
    assert!(!pending_autonomous().contains(&session_key));
    mgr.close(&session_key);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn antigravity_authenticates_only_when_required_and_retries_session() {
    const AGENT: &str = r#"
import json, sys
from pathlib import Path
mode = sys.argv[1]
authed = mode == 'existing'
def send(id, **body):
    print(json.dumps(dict(jsonrpc='2.0', id=id, **body)), flush=True)
for line in sys.stdin:
    m = json.loads(line)
    method = m.get('method')
    with Path('calls').open('a') as f:
        f.write(method + '\n')
    if method == 'initialize':
        name = 'other-agent' if mode == 'other' else 'antigravity-acp'
        send(m['id'], result=dict(protocolVersion=1, agentInfo=dict(name=name), agentCapabilities={}))
    elif method == 'session/new':
        if not authed:
            send(m['id'], error=dict(code=-32000, message='Authentication required'))
        else:
            send(m['id'], result=dict(sessionId='authenticated-session', models=dict(availableModels=[dict(modelId='gemini-test', name='Gemini test')])) )
    elif method == 'authenticate':
        assert m['params']['methodId'] == 'gemini-api-key'
        if mode == 'failure':
            send(m['id'], error=dict(code=-32000, message='invalid API key'))
        else:
            authed = True
            send(m['id'], result={})
    else:
        send(m['id'], result={})
"#;
    for mode in ["new", "existing", "other", "failure"] {
        let dir = std::env::temp_dir().join(format!("oxi-acp-auth-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("agent.py");
        std::fs::write(&script, AGENT).unwrap();
        let command = format!(
            "{} {} {mode}",
            install::shell_quote(&crate::agent::test_python_executable()),
            install::shell_quote(&script)
        );
        let env = vec![("GEMINI_API_KEY".into(), "test-key".into())];
        let spec = LaunchSpec {
            session_key: &format!("agy-auth-{}", rand::random::<u64>()),
            command_line: &command,
            cwd: &dir,
            env: &env,
            model: "default",
            effort: "",
            mcp_servers: &[],
        };
        let result = tokio::time::timeout(Duration::from_secs(10), spawn_conn(&spec, vec![]))
            .await
            .unwrap();
        match mode {
            "new" | "existing" => {
                let conn = result.unwrap();
                assert_eq!(conn.handles.session_id, "authenticated-session");
                assert_eq!(conn.handles.available_models, ["gemini-test"]);
                drop(conn);
            }
            _ => {
                let error = result.err().expect("session should fail");
                assert!(error.contains(if mode == "other" {
                    "Authentication required"
                } else {
                    "invalid API key"
                }));
            }
        }
        let calls = std::fs::read_to_string(dir.join("calls")).unwrap();
        assert_eq!(
            calls.lines().filter(|m| *m == "authenticate").count(),
            usize::from(matches!(mode, "new" | "failure"))
        );
        assert_eq!(
            calls.lines().filter(|m| *m == "session/new").count(),
            if mode == "new" { 2 } else { 1 }
        );
        // Windows keeps the dir locked while the agent process is still exiting.
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A fake agent that lists, replays and forks its sessions.
const SESSION_STORE_AGENT: &str = r#"
import json, sys
def send(v):
    print(json.dumps(dict(jsonrpc='2.0', **v)), flush=True)
def update(sid, v):
    send(dict(method='session/update', params=dict(sessionId=sid, update=v)))
def text(kind, t):
    return dict(sessionUpdate=kind, content=dict(type='text', text=t))
for line in sys.stdin:
    m = json.loads(line)
    method = m.get('method')
    with open('calls', 'a') as f:
        f.write(method + ' ' + json.dumps(m.get('params', {}).get('sessionId')) + '\n')
    if method == 'initialize':
        send(dict(id=m['id'], result=dict(protocolVersion=1, agentCapabilities=dict(
            loadSession=True, sessionCapabilities=dict(list={}, fork={})))))
    elif method == 'session/new':
        send(dict(id=m['id'], result=dict(sessionId='blank')))
    elif method == 'session/list':
        if m['params'].get('cursor') is None:
            send(dict(id=m['id'], result=dict(nextCursor='p2', sessions=[
                dict(sessionId='terminal-1', cwd=m['params']['cwd'], title='Fix login', updatedAt='2026-10-20T10:00:00Z')])))
        else:
            send(dict(id=m['id'], result=dict(sessions=[dict(sessionId='terminal-2', cwd=m['params']['cwd'])])))
    elif method == 'session/load':
        sid = m['params']['sessionId']
        update(sid, text('user_message_chunk', 'Fix the login bug'))
        update(sid, text('agent_message_chunk', 'Fixed it.'))
        send(dict(id=m['id'], result={}))
    elif method == 'session/fork':
        send(dict(id=m['id'], result=dict(sessionId=m['params']['sessionId'] + '-fork')))
    elif method == 'session/prompt':
        update(m['params']['sessionId'], text('agent_message_chunk', m['params']['prompt'][0]['text']))
        send(dict(id=m['id'], result=dict(stopReason='end_turn')))
    elif 'id' in m:
        send(dict(id=m['id'], result={}))
"#;

#[tokio::test]
async fn evidence_activated_while_acp_is_running_checks_only_changed_workspaces() {
    const ADAPTER: &str = r#"
import json,sys,time
from pathlib import Path
root=Path(__file__).parent
change=sys.argv[1]=='change'
count=0
def send(v):
    print(json.dumps(dict(jsonrpc='2.0', **v)),flush=True)
for line in sys.stdin:
    m=json.loads(line)
    method=m.get('method')
    if method=='initialize':
        send(dict(id=m['id'],result=dict(protocolVersion=1,agentCapabilities={})))
    elif method=='session/new':
        send(dict(id=m['id'],result=dict(sessionId='live-evidence')))
    elif method=='session/prompt':
        count+=1
        with (root/'prompts').open('a') as log:
            log.write(json.dumps(m['params'])+'\n')
        if count==1:
            if change:
                (root/'work'/'code.txt').write_text('after')
            send(dict(method='session/update',params=dict(sessionId='live-evidence',update=dict(sessionUpdate='agent_message_chunk',content=dict(type='text',text='ready-to-finish')))))
            deadline=time.monotonic()+10
            while not (root/'continue').exists() and time.monotonic()<deadline:
                time.sleep(.01)
        send(dict(id=m['id'],result=dict(stopReason='end_turn')))
    elif 'id' in m:
        send(dict(id=m['id'],result={}))
"#;
    for change in [false, true] {
        let root =
            std::env::temp_dir().join(format!("oxi-acp-live-evidence-{}", rand::random::<u64>()));
        let cwd = root.join("work");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::write(cwd.join("code.txt"), "before").unwrap();
        let script = root.join("adapter.py");
        std::fs::write(&script, ADAPTER).unwrap();
        let mgr = AcpManager::spawn();
        let worker = mgr.clone();
        let (tx, rx) = channel();
        let (_atx, arx) = channel();
        let mode: crate::evidence::Mode = false.into();
        let key = format!("live-evidence-{}", rand::random::<u64>());
        let req = AcpPrompt {
            session_key: key.clone(),
            cwd: cwd.clone(),
            command_line: format!(
                "{} -u {} {}",
                install::shell_quote(&crate::agent::test_python_executable()),
                install::shell_quote(&script),
                if change { "change" } else { "explain" }
            ),
            env: vec![],
            model: String::new(),
            effort: String::new(),
            mcp_servers: vec![],
            text: "Answer the task".into(),
            history: String::new(),
            missed: String::new(),
            archive: Vec::new(),
            images: vec![],
            resources: vec![],
            event_tx: tx,
            approval_rx: arx,
            approval_policy: ApprovalPolicy::disabled(),
            bash_allowlist: vec![],
            allowed_tools: vec![],
            cancel: Arc::new(AtomicBool::new(false)),
            plan_mode: false,
            evidence_policy: crate::evidence::Policy {
                mode: mode.clone(),
                ..Default::default()
            },
            evidence_base: None,
            steer: None,
        };
        let handle = tokio::spawn(async move { worker.prompt(req).await });
        let mut events = vec![];
        tokio::time::timeout(Duration::from_secs(15),async {
            loop {
                while let Ok(event)=rx.try_recv() {
                    let ready=matches!(&event,AgentEvent::TextDelta(text) if text.contains("ready-to-finish"));
                    events.push(event);
                    if ready {return;}
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.unwrap();
        mode.set(true);
        std::fs::write(root.join("continue"), "go").unwrap();
        tokio::time::timeout(Duration::from_secs(15), handle)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        events.extend(rx.try_iter());
        let prompts = std::fs::read_to_string(root.join("prompts")).unwrap();
        assert_eq!(prompts.lines().count(), if change { 3 } else { 1 });
        assert!(!prompts.lines().next().unwrap().contains("[Evidence mode]"));
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e,AgentEvent::Evidence(report) if report.requested))
                .count(),
            usize::from(change)
        );
        mgr.close(&key);
        let _ = std::fs::remove_dir_all(root);
    }
}

#[tokio::test]
async fn sessions_run_outside_oxi_are_listed_imported_and_forked() {
    let dir = std::env::temp_dir().join(format!("oxi-acp-import-{}", rand::random::<u64>()));
    std::fs::create_dir_all(&dir).unwrap();
    let script = dir.join("agent.py");
    std::fs::write(&script, SESSION_STORE_AGENT).unwrap();
    let command = format!(
        "{} {}",
        install::shell_quote(&crate::agent::test_python_executable()),
        install::shell_quote(&script)
    );
    let req = AcpImport {
        command_line: command.clone(),
        cwd: dir.clone(),
        env: Vec::new(),
    };
    let listed = list_sessions(&req).await.unwrap();
    let ids: Vec<&str> = listed.iter().map(|s| s.session_id.as_str()).collect();
    assert_eq!(ids, ["terminal-1", "terminal-2"], "both pages");
    assert_eq!(listed[0].title, "Fix login");

    let messages = load_session(&req, "terminal-1").await.unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].text, "Fix the login bug");
    assert!(matches!(
        &messages[1].blocks[0],
        crate::model::AssistantBlock::Answer(t) if t == "Fixed it."
    ));

    // The imported chat resumes the agent's session; its fork starts with the frozen transcript.
    let chat = dir.join("chat.json").to_string_lossy().into_owned();
    let fork = dir.join("fork.json").to_string_lossy().into_owned();
    bind_session(&chat, &command, &dir, "terminal-1");
    assert!(
        list_sessions(&req)
            .await
            .unwrap()
            .iter()
            .all(|s| s.session_id != "terminal-1"),
        "a session an oxi chat uses is not offered again"
    );
    let spec = LaunchSpec {
        session_key: &fork,
        command_line: &command,
        cwd: &dir,
        env: &[],
        model: "",
        effort: "",
        mcp_servers: &[],
    };
    let conn = spawn_conn(&spec, Vec::new()).await.unwrap();
    assert_eq!(conn.handles.session_id, "blank");
    assert!(conn.handles.needs_history.load(Ordering::SeqCst));
    let (event_tx, event_rx) = channel();
    let (_approval_tx, approval_rx) = channel();
    let (reply_tx, reply_rx) = oneshot::channel();
    let prompt = AcpPrompt {
        session_key: fork.clone(),
        cwd: dir.clone(),
        command_line: command.clone(),
        env: Vec::new(),
        model: String::new(),
        effort: String::new(),
        mcp_servers: Vec::new(),
        text: "Take another direction".into(),
        history: "User: Fix the login bug\nAssistant: Fixed it.".into(),
        missed: String::new(),
        archive: Vec::new(),
        images: Vec::new(),
        resources: Vec::new(),
        event_tx,
        approval_rx,
        approval_policy: ApprovalPolicy::disabled(),
        bash_allowlist: Vec::new(),
        allowed_tools: Vec::new(),
        cancel: Arc::new(AtomicBool::new(false)),
        plan_mode: false,
        evidence_policy: Default::default(),
        evidence_base: None,
        steer: None,
    };
    tokio::time::timeout(
        Duration::from_secs(5),
        run_prompt(conn.handles.clone(), prompt, reply_tx),
    )
    .await
    .unwrap();
    reply_rx.await.unwrap().unwrap();
    let sent = texts(&event_rx);
    assert!(sent.contains("<conversation_history>"));
    assert!(sent.contains("User: Fix the login bug\nAssistant: Fixed it."));
    assert!(sent.ends_with("Take another direction"));
    let calls = std::fs::read_to_string(dir.join("calls")).unwrap();
    assert!(!calls.contains("session/fork"));
    drop(conn);
    assert_eq!(
        sessions::lookup(&fork, &command, &dir).as_deref(),
        Some("blank")
    );
    sessions::forget(&chat);
    sessions::forget(&fork);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn delegated_sessions_route_replay_permissions_cancel_and_late_updates() {
    const ADAPTER: &str = r#"
import json,sys,threading,time
prompt=None
cancelled=False
def send(v): print(json.dumps(dict(jsonrpc='2.0',**v)),flush=True)
def update(s,u): send(dict(method='session/update',params=dict(sessionId=s,update=u)))
def late():
    time.sleep(.15)
    update('background',dict(sessionUpdate='agent_message_chunk',content=dict(type='text',text='late child output')))
    update('root',dict(sessionUpdate='subagent_update',sessionId='background',state=dict(state='idle',outcome='completed')))
for line in sys.stdin:
    m=json.loads(line); method=m.get('method')
    if method=='initialize':
        assert isinstance(m['params']['clientCapabilities'].get('subagents'),dict)
        send(dict(id=m['id'],result=dict(protocolVersion=1,agentCapabilities={})))
    elif method=='session/new':
        update('root',dict(sessionUpdate='subagent_update',sessionId='worker',title='Reviewer',capabilities=dict(cancel={}),state=dict(state='running')))
        update('worker',dict(sessionUpdate='session_message',messageId='launch',recipientSessionId='worker',senderSessionId='root',content=[dict(type='text',text='Review the patch')]))
        update('root',dict(sessionUpdate='subagent_update',sessionId='background',title='Background',state=dict(state='running')))
        send(dict(id=m['id'],result=dict(sessionId='root')))
    elif method=='session/prompt':
        prompt=m['id']
        update('worker',dict(sessionUpdate='agent_message_chunk',content=dict(type='text',text='child-only output')))
        update('unannounced',dict(sessionUpdate='agent_message_chunk',content=dict(type='text',text='foreign output')))
        send(dict(id=77,method='session/request_permission',params=dict(sessionId='worker',toolCall=dict(toolCallId='exec',title='Run tests',kind='execute',rawInput=dict(command='echo child')),options=[dict(optionId='yes',kind='allow_once') ,dict(optionId='no',kind='reject_once') ])))
    elif method=='session/cancel':
        assert m['params']['sessionId']=='worker'
        cancelled=True
    elif method is None and m.get('id')==77:
        assert cancelled and m['result']['outcome']['outcome']=='cancelled'
        update('root',dict(sessionUpdate='subagent_update',sessionId='worker',state=dict(state='idle',outcome='cancelled')))
        update('root',dict(sessionUpdate='agent_message_chunk',content=dict(type='text',text='parent-only output')))
        send(dict(id=prompt,result=dict(stopReason='end_turn')))
        threading.Thread(target=late,daemon=True).start()
    elif 'id' in m: send(dict(id=m['id'],result={}))
"#;
    let dir = crate::fsutil::unique_temp_path(&std::env::temp_dir(), "oxi-acp-delegated", "d");
    std::fs::create_dir_all(&dir).unwrap();
    let script = dir.join("agent.py");
    std::fs::write(&script, ADAPTER).unwrap();
    let key = format!("delegated-{}", rand::random::<u64>());
    let manager = AcpManager::spawn();
    let (tx, rx) = channel();
    let (_approval_tx, approval_rx) = channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let req = AcpPrompt {
        session_key: key.clone(),
        cwd: dir.clone(),
        command_line: format!(
            "{} {}",
            crate::agent::test_python_executable().display(),
            script.display()
        ),
        env: vec![],
        model: String::new(),
        effort: String::new(),
        mcp_servers: vec![],
        text: "Delegate".into(),
        history: String::new(),
        missed: String::new(),
        archive: vec![],
        images: vec![],
        resources: vec![],
        event_tx: tx,
        approval_rx,
        approval_policy: ApprovalPolicy {
            write_edit: true,
            bash: true,
        },
        bash_allowlist: vec![],
        allowed_tools: vec![],
        cancel: cancel.clone(),
        plan_mode: false,
        evidence_policy: Default::default(),
        evidence_base: None,
        steer: None,
    };
    let worker = manager.clone();
    let prompt = tokio::spawn(async move { worker.prompt(req).await });
    let mut chats = crate::model::delegated::DelegatedChats::default();
    let mut parent = String::new();
    let mut resolved = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    let mut asked = false;
    while !prompt.is_finished() {
        assert!(
            std::time::Instant::now() < deadline,
            "subagent permission/cancellation hung"
        );
        while let Ok(event) = rx.try_recv() {
            match event {
                AgentEvent::Delegated { root, params } => {
                    chats.apply(&root, &params);
                }
                AgentEvent::ApprovalRequest { .. } => {
                    asked = true;
                    assert!(
                        manager
                            .cancel_child(key.clone(), "wrong-root".into(), "worker".into(), false)
                            .await
                            .is_err()
                    );
                    manager
                        .cancel_child(key.clone(), "root".into(), "worker".into(), false)
                        .await
                        .unwrap();
                }
                AgentEvent::ApprovalResolved => resolved = true,
                AgentEvent::TextDelta(s) => parent.push_str(&s),
                _ => {}
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    prompt.await.unwrap().unwrap();
    for event in drain(&rx) {
        match event {
            AgentEvent::Delegated { root, params } => {
                chats.apply(&root, &params);
            }
            AgentEvent::TextDelta(s) => parent.push_str(&s),
            AgentEvent::ApprovalResolved => resolved = true,
            _ => {}
        }
    }
    assert!(asked && resolved);
    assert_eq!(parent, "parent-only output");
    assert!(
        !cancel.load(Ordering::SeqCst),
        "stopping a child must not cancel its parent"
    );
    assert_eq!(
        chats
            .chats
            .iter()
            .find(|c| c.id == "worker")
            .unwrap()
            .messages[0]
            .text,
        "Review the patch"
    );
    let mut late = false;
    while !late && std::time::Instant::now() < deadline {
        for (source, root, params) in take_delegated() {
            if source == key {
                late |= params["update"]["content"]["text"] == "late child output";
                chats.apply(&root, &params);
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        late,
        "background child updates must arrive without starting a parent turn"
    );
    assert!(!pending_autonomous().contains(&key));
    manager.close(&key);
    let _ = std::fs::remove_dir_all(dir);
}
