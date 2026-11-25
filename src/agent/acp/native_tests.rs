//! Unit and mock-subprocess integration tests for the official native Codex (`codex app-server`)
//! and Claude Code (`claude --output-format stream-json`) backends.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::channel;

use serde_json::{Value, json};

use super::claude_native;
use super::codex_native;
#[cfg(unix)]
use super::config_options::{self, AcpConfigKind};
#[cfg(unix)]
use super::import::{AcpImport, list_sessions, load_session};
use super::{AcpManager, AcpPrompt, AcpWarm};
#[cfg(unix)]
use super::{AcpSetOption, commands, session_info};
use crate::agent::approval::{ApprovalDecision, ApprovalPolicy};
use crate::agent::events::AgentEvent;
use crate::agent::steer::SteerInbox;
#[cfg(unix)]
use crate::model::ToolStatus;
use crate::model::{AssistantBlock, MsgRole};
use crate::settings::{McpServerConfig, McpTransport};

struct TempDir(PathBuf);

impl TempDir {
    fn new(prefix: &str) -> Self {
        let path = crate::fsutil::unique_temp_path(&std::env::temp_dir(), prefix, "d");
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(unix)]
fn write_executable(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

// ── Unit tests (all platforms) ──────────────────────────────────────────────

#[test]
fn codex_args_inject_oxi_and_user_mcp_servers() {
    let dir = TempDir::new("oxi-codex-args");
    let servers = vec![
        McpServerConfig {
            name: "my-fs".into(),
            command: "npx".into(),
            args: vec!["-y".into(), "server-fs".into()],
            env: "API_KEY=secret\n".into(),
            ..Default::default()
        },
        McpServerConfig {
            name: "remote".into(),
            transport: McpTransport::Http,
            url: "https://example.com/mcp".into(),
            bearer_token: "tok123".into(),
            ..Default::default()
        },
    ];
    let args = codex_native::build_codex_args(
        &[
            "app-server".into(),
            "-c".into(),
            "model_verbosity=low".into(),
        ],
        &servers,
        Some(dir.path()),
        None,
    );
    assert_eq!(args[0], "app-server");
    assert!(args.contains(&"--stdio".to_string()));
    assert!(args.iter().any(|a| a == "model_verbosity=low"));
    assert!(
        args.iter()
            .any(|a| a.starts_with("mcp_servers.oxi.command="))
    );
    assert!(args.iter().any(|a| a.contains("acp-todo-mcp")));
    assert!(
        args.iter()
            .any(|a| a.starts_with("mcp_servers.my-fs.command="))
    );
    assert!(args.iter().any(|a| a.contains("API_KEY=\"secret\"")));
    assert!(
        args.iter()
            .any(|a| a == "mcp_servers.remote.url=\"https://example.com/mcp\"")
    );
    assert!(
        args.iter()
            .any(|a| a == "mcp_servers.remote.http_headers={Authorization=\"Bearer tok123\"}")
    );
}

#[test]
fn codex_questions_roundtrip_through_elicitation() {
    let questions = vec![
        json!({
            "id": "q1",
            "header": "Target",
            "question": "Which branch?",
            "isOther": true,
            "isSecret": false,
            "options": [
                { "label": "main", "description": "Default branch" },
                { "label": "dev", "description": "Development branch" }
            ]
        }),
        json!({
            "id": "q2",
            "header": "Note",
            "question": "Release note?",
            "isOther": false,
            "isSecret": false,
            "options": null
        }),
    ];
    let params = codex_native::codex_questions_to_elicitation(&questions);
    let parsed = crate::agent::elicitation::parse(&params);
    assert!(matches!(
        parsed.kind,
        crate::agent::elicitation::ElicitationKind::Form(ref fields) if fields.len() == 3
    ));
    let decision = Some(ApprovalDecision::Elicitation(json!({
        "action": "accept",
        "content": {
            "question_0": "main",
            "question_0_custom": "release/v2",
            "question_1": "Native clients"
        }
    })));
    let answers = codex_native::elicitation_to_codex_answers(&questions, decision);
    assert_eq!(answers["q1"]["answers"], json!(["release/v2"]));
    assert_eq!(answers["q2"]["answers"], json!(["Native clients"]));

    // Cancel produces an empty answer map.
    let cancelled = codex_native::elicitation_to_codex_answers(&questions, None);
    assert_eq!(cancelled, json!({}));
}

#[test]
fn codex_thread_to_messages_converts_turns_and_strips_preambles() {
    let thread = json!({
        "id": "th-1",
        "turns": [{
            "id": "turn-1",
            "items": [
                {
                    "type": "userMessage",
                    "id": "u1",
                    "content": [{ "type": "text", "text": "<conversation_history>\nold\n</conversation_history>\n\nFix tests" }]
                },
                { "type": "reasoning", "id": "r1", "summary": ["Checking tests"], "content": [] },
                {
                    "type": "commandExecution",
                    "id": "c1",
                    "command": "cargo test",
                    "status": "completed",
                    "aggregatedOutput": "ok"
                },
                {
                    "type": "fileChange",
                    "id": "f1",
                    "status": "completed",
                    "changes": [{ "path": "src/lib.rs", "diff": "@@ -1 +1 @@\n-a\n+b\n" }]
                },
                { "type": "agentMessage", "id": "a1", "text": "Fixed." }
            ]
        }]
    });
    let msgs = codex_native::thread_to_messages(&thread);
    assert_eq!(msgs.len(), 2);
    assert!(msgs[0].role == MsgRole::User);
    assert_eq!(msgs[0].text, "Fix tests");
    assert!(msgs[1].role == MsgRole::Assistant);
    assert!(
        msgs[1]
            .blocks
            .iter()
            .any(|b| matches!(b, AssistantBlock::Thinking(t) if t == "Checking tests"))
    );
    assert!(
        msgs[1]
            .blocks
            .iter()
            .any(|b| matches!(b, AssistantBlock::Tool { name, .. } if name == "bash"))
    );
    assert!(
        msgs[1].blocks.iter().any(
            |b| matches!(b, AssistantBlock::Tool { name, diff: Some(_), .. } if name == "edit")
        )
    );
    assert!(
        msgs[1]
            .blocks
            .iter()
            .any(|b| matches!(b, AssistantBlock::Answer(t) if t == "Fixed."))
    );
}

#[test]
fn claude_args_and_mcp_config_include_oxi_and_user_servers() {
    let dir = TempDir::new("oxi-claude-args");
    let servers = vec![
        McpServerConfig {
            name: "docs".into(),
            transport: McpTransport::Http,
            url: "https://docs.example/mcp".into(),
            bearer_token: "abc".into(),
            ..Default::default()
        },
        McpServerConfig {
            name: "fs".into(),
            command: "npx".into(),
            args: vec!["-y".into(), "mcp-fs".into()],
            env: "FOO=bar\n".into(),
            ..Default::default()
        },
    ];
    let path = claude_native::write_claude_mcp_config(dir.path(), &servers, Some(dir.path()), None)
        .unwrap();
    let cfg: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(cfg["mcpServers"]["oxi"]["args"], json!(["acp-todo-mcp"]));
    assert_eq!(cfg["mcpServers"]["fs"]["env"]["FOO"], "bar");
    assert_eq!(
        cfg["mcpServers"]["docs"]["headers"]["Authorization"],
        "Bearer abc"
    );

    let args = claude_native::build_claude_args(
        &["--chrome".into(), "--verbose".into()],
        &path,
        Some("sess-42"),
        "opus",
        "high",
    );
    assert!(
        args.windows(2)
            .any(|w| w == ["--output-format", "stream-json"])
    );
    assert!(
        args.windows(2)
            .any(|w| w == ["--input-format", "stream-json"])
    );
    assert!(
        args.windows(2)
            .any(|w| w == ["--permission-prompt-tool", "stdio"])
    );
    assert!(args.windows(2).any(|w| w == ["--resume", "sess-42"]));
    assert!(args.windows(2).any(|w| w == ["--model", "opus"]));
    assert!(args.windows(2).any(|w| w == ["--effort", "high"]));
    assert!(args.contains(&"--chrome".to_string()));
    assert_eq!(args.iter().filter(|a| *a == "--verbose").count(), 1);
}

#[test]
fn claude_ask_user_question_roundtrip_through_elicitation() {
    let questions = vec![
        json!({
            "header": "Database",
            "question": "Which DB?",
            "multiSelect": false,
            "options": [
                { "label": "SQLite", "description": "Embedded", "preview": "file.db" },
                { "label": "Postgres", "description": "Server" }
            ]
        }),
        json!({
            "header": "Features",
            "question": "Enable features?",
            "multiSelect": true,
            "options": [
                { "label": "Auth", "description": "OAuth" },
                { "label": "Metrics", "description": "Prometheus" }
            ]
        }),
    ];
    let params = claude_native::claude_questions_to_elicitation(&questions);
    let parsed = crate::agent::elicitation::parse(&params);
    assert!(matches!(
        parsed.kind,
        crate::agent::elicitation::ElicitationKind::Form(ref fields) if fields.len() == 4
    ));
    let decision = Some(ApprovalDecision::Elicitation(json!({
        "action": "accept",
        "content": {
            "question_0": "SQLite",
            "question_1": ["Auth", "Metrics"]
        }
    })));
    let answers = claude_native::elicitation_to_claude_answers(&questions, decision).unwrap();
    assert_eq!(answers["Which DB?"], "SQLite");
    assert_eq!(answers["Enable features?"], "Auth, Metrics");
    assert!(claude_native::elicitation_to_claude_answers(&questions, None).is_none());
}

#[test]
fn claude_jsonl_list_and_load_external_sessions() {
    let dir = TempDir::new("oxi-claude-jsonl");
    let jsonl = dir.path().join("sess-1.jsonl");
    std::fs::write(
        &jsonl,
        concat!(
            r#"{"type":"user","isMeta":false,"message":{"role":"user","content":"<conversation_history>\nold\n</conversation_history>\n\nBuild native client"}}"#, "\n",
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"Inspecting code"},{"type":"tool_use","id":"t1","name":"Edit","input":{"file_path":"src/a.rs","old_string":"old","new_string":"new"}},{"type":"text","text":"Done."}]}}"#, "\n",
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}"#, "\n",
            r#"{"type":"custom-title","customTitle":"Native Client Upgrade"}"#, "\n"
        ),
    )
    .unwrap();
    // Subagent transcript and already-known sessions are skipped.
    std::fs::write(dir.path().join("agent-sub.jsonl"), "{}\n").unwrap();
    std::fs::write(
        dir.path().join("known-sess.jsonl"),
        r#"{"type":"user","message":{"role":"user","content":"Already in oxi"}}"#,
    )
    .unwrap();

    let known = HashSet::from(["known-sess".to_owned()]);
    let listed = claude_native::list_sessions_from_project_dir(dir.path(), &known).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].session_id, "sess-1");
    assert_eq!(listed[0].title, "Native Client Upgrade");

    let msgs = claude_native::load_session_from_jsonl(&jsonl).unwrap();
    assert_eq!(msgs.len(), 2);
    assert!(msgs[0].role == MsgRole::User);
    assert_eq!(msgs[0].text, "Build native client");
    assert!(
        msgs[1]
            .blocks
            .iter()
            .any(|b| matches!(b, AssistantBlock::Thinking(t) if t == "Inspecting code"))
    );
    assert!(msgs[1].blocks.iter().any(|b| matches!(b, AssistantBlock::Tool { name, diff: Some(_), output, .. } if name == "edit" && output == "ok")));
    assert!(
        msgs[1]
            .blocks
            .iter()
            .any(|b| matches!(b, AssistantBlock::Answer(t) if t == "Done."))
    );
}

#[test]
fn claude_jsonl_import_skips_bad_lines_and_hidden_tool_results() {
    let dir = TempDir::new("oxi-claude-jsonl-bad");
    let jsonl = dir.path().join("sess-2.jsonl");
    let mut bytes = Vec::new();
    bytes.extend_from_slice(
        br#"{"type":"user","message":{"role":"user","content":"Find the tool"}}"#,
    );
    bytes.push(b'\n');
    // Invalid UTF-8 mid-file (e.g. a torn write) must not abort the whole import.
    bytes.extend_from_slice(b"{\"type\":\"user\",\"message\":\"\xff\xfe\"}\n");
    bytes.extend_from_slice(br#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"ts1","name":"ToolSearch","input":{"query":"x"}},{"type":"text","text":"Found it."}]}}"#);
    bytes.push(b'\n');
    bytes.extend_from_slice(br#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"ts1","content":"tool list"}]}}"#);
    // Half-written last line of a live session, cut inside a multi-byte character.
    bytes.extend_from_slice(b"\n{\"type\":\"assistant\",\"message\":\"\xc3");
    std::fs::write(&jsonl, bytes).unwrap();

    let msgs = claude_native::load_session_from_jsonl(&jsonl).unwrap();
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[0].text, "Find the tool");
    assert!(
        !msgs[1]
            .blocks
            .iter()
            .any(|b| matches!(b, AssistantBlock::Tool { .. })),
        "ToolSearch result leaked into the import"
    );
    assert!(
        msgs[1]
            .blocks
            .iter()
            .any(|b| matches!(b, AssistantBlock::Answer(t) if t == "Found it."))
    );
}

#[test]
fn codex_allow_for_this_turn_never_grants_the_session() {
    use codex_native::{Grant, apply_decision, permissions_result};

    let mut auto = false;
    let mut bash = Vec::new();
    let mut tools = Vec::new();
    let requested = json!({ "fileSystem": { "write": ["/work"] } });

    let rest = apply_decision(
        Some(ApprovalDecision::ApproveRest),
        &mut auto,
        &mut bash,
        &mut tools,
    );
    assert!(auto, "the rest of this turn is auto-approved by oxi");
    assert_eq!(rest.wire(), "accept");
    assert_eq!(
        permissions_result(rest, &requested),
        json!({ "permissions": requested, "scope": "turn" })
    );

    let prefix = apply_decision(
        Some(ApprovalDecision::AllowPrefix("cargo".into())),
        &mut auto,
        &mut bash,
        &mut tools,
    );
    assert_eq!(prefix, Grant::Accept);
    assert_eq!(bash, vec!["cargo".to_string()]);

    let denied = apply_decision(
        Some(ApprovalDecision::Deny),
        &mut auto,
        &mut bash,
        &mut tools,
    );
    assert_eq!(denied.wire(), "decline");
    assert_eq!(
        permissions_result(denied, &requested),
        json!({ "permissions": {}, "scope": "turn" })
    );
    assert_eq!(
        apply_decision(None, &mut auto, &mut bash, &mut tools).wire(),
        "cancel"
    );
}

#[test]
fn codex_plan_mode_maps_to_native_collaboration_mode() {
    use codex_native::collaboration_mode;

    let plan = collaboration_mode(true, false, "gpt-6.2-sol", "high").unwrap();
    assert_eq!(plan["mode"], "plan");
    assert_eq!(plan["settings"]["model"], "gpt-6.2-sol");
    assert_eq!(plan["settings"]["reasoning_effort"], "high");
    assert!(plan["settings"]["developer_instructions"].is_null());
    // Leaving Plan mode switches the thread back once; afterwards the mode is left alone.
    let back = collaboration_mode(false, true, "gpt-6.2-sol", "default").unwrap();
    assert_eq!(back["mode"], "default");
    assert!(back["settings"]["reasoning_effort"].is_null());
    assert!(collaboration_mode(false, false, "gpt-6.2-sol", "high").is_none());
    // The mode requires a model id; without one, keep the plain turn.
    assert!(collaboration_mode(true, false, "", "high").is_none());
}

#[cfg(unix)]
#[test]
fn codex_plan_mode_switches_the_thread_into_and_out_of_native_plan() {
    let dir = TempDir::new("oxi-codex-plan");
    let log_file = dir.path().join("codex.log");
    let script = format!(
        r#"#!/usr/bin/env python3
import sys, json
log_path = {log_path:?}
def log(msg):
    with open(log_path, "a") as f:
        f.write(json.dumps(msg) + "\n")
def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()
n = 0
for line in sys.stdin:
    if not line.strip():
        continue
    msg = json.loads(line)
    method, rid, params = msg.get("method"), msg.get("id"), msg.get("params", {{}})
    if method == "initialize":
        send({{"id": rid, "result": {{"userAgent": "codex-mock/1.0"}}}})
    elif method == "model/list":
        send({{"id": rid, "result": {{"data": [{{"id": "gpt-6.2-sol", "displayName": "Sol", "isDefault": True, "supportedReasoningEfforts": [], "inputModalities": ["text"]}}]}}}})
    elif method == "thread/start":
        send({{"id": rid, "result": {{"thread": {{"id": "thr-plan"}}, "model": "gpt-6.2-sol"}}}})
    elif method == "turn/start":
        log(params)
        n += 1
        tid = "turn-%d" % n
        send({{"id": rid, "result": {{"turn": {{"id": tid, "status": "inProgress"}}}}}})
        send({{"method": "turn/completed", "params": {{"threadId": params["threadId"], "turn": {{"id": tid, "status": "completed", "error": None}}}}}})
    elif rid is not None and method:
        send({{"id": rid, "result": {{"data": []}}}})
"#,
        log_path = log_file.to_string_lossy()
    );
    let bin = write_executable(dir.path(), "codex", &script);
    let cmd_line = format!("{} app-server", bin.display());
    let session_key = format!("mem:codex-plan:{}", rand::random::<u64>());
    let rt = crate::runtime::runtime().unwrap();
    let mgr = AcpManager::spawn();
    for plan_mode in [true, false, false] {
        let (ev_tx, _ev_rx) = channel();
        let (_ap_tx, ap_rx) = channel();
        rt.block_on(mgr.prompt(AcpPrompt {
            session_key: session_key.clone(),
            cwd: dir.path().to_path_buf(),
            command_line: cmd_line.clone(),
            env: Vec::new(),
            model: "default".into(),
            effort: String::new(),
            mcp_servers: Vec::new(),
            text: "plan the change".into(),
            history: String::new(),
            missed: String::new(),
            archive: Vec::new(),
            images: Vec::new(),
            resources: Vec::new(),
            event_tx: ev_tx,
            approval_rx: ap_rx,
            approval_policy: ApprovalPolicy::disabled(),
            bash_allowlist: Vec::new(),
            allowed_tools: Vec::new(),
            cancel: Arc::new(AtomicBool::new(false)),
            plan_mode,
            steer: None,
            evidence_policy: crate::evidence::Policy::default(),
            evidence_base: None,
        }))
        .unwrap();
    }
    let starts: Vec<Value> = std::fs::read_to_string(&log_file)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(starts.len(), 3);
    assert_eq!(starts[0]["collaborationMode"]["mode"], "plan");
    assert_eq!(
        starts[0]["collaborationMode"]["settings"]["model"],
        "gpt-6.2-sol"
    );
    assert_eq!(starts[0]["sandboxPolicy"]["type"], "readOnly");
    assert_eq!(starts[1]["collaborationMode"]["mode"], "default");
    assert!(starts[2].get("collaborationMode").is_none());
    mgr.close(&session_key);
}

/// Runs one prompt on its own thread and flips `cancel` shortly after it starts. Returns
/// `None` if the turn did not finish within `limit` (the Stop button would hang).
#[cfg(unix)]
fn prompt_then_stop(
    mgr: &AcpManager,
    session_key: &str,
    cwd: &Path,
    command_line: &str,
    limit: std::time::Duration,
) -> Option<Result<(), String>> {
    let cancel = Arc::new(AtomicBool::new(false));
    let (ev_tx, ev_rx) = channel();
    let (_ap_tx, ap_rx) = channel();
    let req = AcpPrompt {
        session_key: session_key.to_owned(),
        cwd: cwd.to_path_buf(),
        command_line: command_line.to_owned(),
        env: Vec::new(),
        model: "default".into(),
        effort: String::new(),
        mcp_servers: Vec::new(),
        text: "long task".into(),
        history: String::new(),
        missed: String::new(),
        archive: Vec::new(),
        images: Vec::new(),
        resources: Vec::new(),
        event_tx: ev_tx,
        approval_rx: ap_rx,
        approval_policy: ApprovalPolicy::disabled(),
        bash_allowlist: Vec::new(),
        allowed_tools: Vec::new(),
        cancel: cancel.clone(),
        plan_mode: false,
        steer: Some(SteerInbox::default()),
        evidence_policy: crate::evidence::Policy::default(),
        evidence_base: None,
    };
    let mgr = mgr.clone();
    let (done_tx, done_rx) = channel();
    std::thread::spawn(move || {
        let rt = crate::runtime::runtime().unwrap();
        let _ = done_tx.send(rt.block_on(mgr.prompt(req)).map(|_| ()));
    });
    // Press Stop only once the turn is streaming, so the interrupt path is what runs.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        match ev_rx.recv_timeout(left) {
            Ok(AgentEvent::TextDelta(_)) => break,
            Ok(_) => {}
            Err(_) => panic!("the turn never started streaming"),
        }
    }
    cancel.store(true, std::sync::atomic::Ordering::SeqCst);
    done_rx.recv_timeout(limit).ok()
}

#[cfg(unix)]
#[test]
fn codex_stop_finishes_even_if_interrupt_is_never_acknowledged() {
    let dir = TempDir::new("oxi-codex-stop");
    let log_file = dir.path().join("codex.log");
    let script = format!(
        r#"#!/usr/bin/env python3
import sys, json
log_path = {log_path:?}
def log(msg):
    with open(log_path, "a") as f:
        f.write(json.dumps(msg) + "\n")
def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()
for line in sys.stdin:
    if not line.strip():
        continue
    msg = json.loads(line)
    log(msg)
    method, rid, params = msg.get("method"), msg.get("id"), msg.get("params", {{}})
    if method == "initialize":
        send({{"id": rid, "result": {{"userAgent": "codex-mock/1.0"}}}})
    elif method == "model/list":
        send({{"id": rid, "result": {{"data": [{{"id": "m", "displayName": "M", "isDefault": True, "supportedReasoningEfforts": [], "inputModalities": ["text"]}}]}}}})
    elif method == "skills/list":
        send({{"id": rid, "result": {{"data": []}}}})
    elif method == "thread/start":
        send({{"id": rid, "result": {{"thread": {{"id": "thr-stop"}}, "model": "m"}}}})
    elif method == "turn/start":
        send({{"id": rid, "result": {{"turn": {{"id": "turn-stop", "status": "inProgress"}}}}}})
        send({{"method": "item/agentMessage/delta", "params": {{"threadId": params["threadId"], "turnId": "turn-stop", "itemId": "m1", "delta": "working"}}}})
    elif method == "turn/interrupt":
        # Never acknowledge the request; only end the turn.
        send({{"method": "turn/completed", "params": {{"threadId": params["threadId"], "turn": {{"id": "turn-stop", "status": "interrupted", "error": None}}}}}})
    elif rid is not None and method:
        send({{"id": rid, "result": {{}}}})
"#,
        log_path = log_file.to_string_lossy()
    );
    let bin = write_executable(dir.path(), "codex", &script);
    let cmd_line = format!("{} app-server", bin.display());
    let session_key = format!("mem:codex-stop:{}", rand::random::<u64>());
    let mgr = AcpManager::spawn();

    let outcome = prompt_then_stop(
        &mgr,
        &session_key,
        dir.path(),
        &cmd_line,
        std::time::Duration::from_secs(8),
    );
    assert!(
        outcome.is_some(),
        "Stop hung waiting for the turn/interrupt response"
    );
    let log = std::fs::read_to_string(&log_file).unwrap();
    assert!(
        log.contains("turn/interrupt"),
        "outcome={outcome:?}\nlog={log}"
    );
    mgr.close(&session_key);
}

#[cfg(unix)]
#[test]
fn claude_stop_finishes_even_if_interrupt_is_never_acknowledged() {
    let dir = TempDir::new("oxi-claude-stop");
    let log_file = dir.path().join("claude.log");
    let script = format!(
        r#"#!/usr/bin/env python3
import sys, json
log_path = {log_path:?}
def log(msg):
    with open(log_path, "a") as f:
        f.write(json.dumps(msg) + "\n")
def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()
sid = "claude-stop-1"
for line in sys.stdin:
    if not line.strip():
        continue
    msg = json.loads(line)
    log(msg)
    if msg.get("type") == "control_request":
        rid, sub = msg["request_id"], msg["request"]["subtype"]
        if sub == "initialize":
            send({{"type": "system", "subtype": "init", "session_id": sid}})
            send({{"type": "control_response", "response": {{"subtype": "success", "request_id": rid, "response": {{
                "models": [{{"value": "default", "displayName": "Default", "description": "", "supportedEffortLevels": []}}],
                "commands": [], "current_permission_mode": "default"
            }}}}}})
        elif sub == "interrupt":
            # Never acknowledge the request; only end the turn.
            send({{"type": "result", "subtype": "error_during_execution", "is_error": True, "session_id": sid, "usage": {{}}, "modelUsage": {{}}}})
        else:
            send({{"type": "control_response", "response": {{"subtype": "success", "request_id": rid, "response": {{}}}}}})
    elif msg.get("type") == "user":
        send({{"type": "stream_event", "parent_tool_use_id": None, "event": {{"type": "message_start", "message": {{}}}}}})
        send({{"type": "stream_event", "parent_tool_use_id": None, "event": {{"type": "content_block_delta", "index": 0, "delta": {{"type": "text_delta", "text": "working"}}}}}})
"#,
        log_path = log_file.to_string_lossy()
    );
    let bin = write_executable(dir.path(), "claude", &script);
    let cmd_line = bin.display().to_string();
    let session_key = format!("mem:claude-stop:{}", rand::random::<u64>());
    let mgr = AcpManager::spawn();

    let outcome = prompt_then_stop(
        &mgr,
        &session_key,
        dir.path(),
        &cmd_line,
        std::time::Duration::from_secs(8),
    );
    assert!(
        outcome.is_some(),
        "Stop hung waiting for the interrupt control_response"
    );
    let log = std::fs::read_to_string(&log_file).unwrap();
    assert!(
        log.contains("\"interrupt\""),
        "outcome={outcome:?}\nlog={log}"
    );
    mgr.close(&session_key);
}

// ── End-to-end mock-subprocess integration tests (Unix, uses python3) ───────

#[cfg(unix)]
#[test]
fn codex_native_e2e_turn_approvals_steer_options_and_import() {
    let dir = TempDir::new("oxi-codex-e2e");
    let log_file = dir.path().join("codex.log");
    let script = format!(
        r#"#!/usr/bin/env python3
import sys, json

log_path = {log_path:?}
def log(msg):
    with open(log_path, "a") as f:
        f.write(json.dumps(msg) + "\n")

def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    log(msg)
    method = msg.get("method")
    rid = msg.get("id")
    params = msg.get("params", {{}})
    if method == "initialize":
        send({{"id": rid, "result": {{"userAgent": "codex-mock/1.0"}}}})
    elif method == "initialized":
        continue
    elif method == "model/list":
        send({{"id": rid, "result": {{"data": [
            {{
                "id": "gpt-6.2-sol",
                "displayName": "GPT-6.2-Sol",
                "description": "Fast coding model",
                "hidden": False,
                "isDefault": True,
                "supportedReasoningEfforts": [
                    {{"reasoningEffort": "low", "description": "Light"}},
                    {{"reasoningEffort": "high", "description": "Deep"}}
                ],
                "defaultReasoningEffort": "high",
                "inputModalities": ["text", "image"],
                "serviceTiers": [{{"id": "priority", "name": "Fast"}}]
            }},
            {{
                "id": "gpt-6.2-astra",
                "displayName": "GPT-6.2-Astra",
                "description": "Complex coding",
                "hidden": False,
                "isDefault": False,
                "supportedReasoningEfforts": [{{"reasoningEffort": "max", "description": "Max"}}],
                "defaultReasoningEffort": "max",
                "inputModalities": ["text", "image"],
                "serviceTiers": []
            }}
        ]}}}})
    elif method == "skills/list":
        send({{"id": rid, "result": {{"data": [{{"skills": [{{"name": "audit", "description": "Run audit"}}]}}]}}}})
    elif method == "thread/start":
        send({{"id": rid, "result": {{"thread": {{"id": "thr-native-1"}}, "model": "gpt-6.2-sol", "reasoningEffort": "high"}}}})
    elif method == "thread/list":
        send({{"id": rid, "result": {{"data": [{{"id": "ext-1", "name": "External Codex Chat", "updatedAt": 1760000000}}]}}}})
    elif method == "thread/read":
        send({{"id": rid, "result": {{"thread": {{"id": params.get("threadId"), "turns": [{{
            "id": "t1",
            "items": [
                {{"type": "userMessage", "id": "u1", "content": [{{"type": "text", "text": "Hello external"}}]}},
                {{"type": "agentMessage", "id": "a1", "text": "Hi from Codex"}}
            ]
        }}]}}}}}})
    elif method == "turn/start":
        tid = params["threadId"]
        turn_id = "turn-1"
        send({{"id": rid, "result": {{"turn": {{"id": turn_id, "status": "inProgress"}}}}}})
        send({{"method": "item/reasoning/summaryTextDelta", "params": {{"threadId": tid, "turnId": turn_id, "itemId": "r1", "delta": "Thinking carefully"}}}})
        send({{"method": "turn/plan/updated", "params": {{"threadId": tid, "turnId": turn_id, "plan": [{{"step": "Run cargo test", "status": "inProgress"}}]}}}})
        # Ask command approval
        send({{"id": 900, "method": "item/commandExecution/requestApproval", "params": {{"threadId": tid, "turnId": turn_id, "itemId": "cmd1", "command": "cargo test", "cwd": "/tmp"}}}})
        # Wait for approval response AND optional steer
        approved = False
        while not approved:
            nxt = sys.stdin.readline()
            if not nxt: break
            resp = json.loads(nxt)
            log(resp)
            if resp.get("id") == 900:
                approved = True
            elif resp.get("method") == "turn/steer":
                send({{"id": resp["id"], "result": {{"turnId": turn_id}}}})
        send({{"method": "item/started", "params": {{"threadId": tid, "turnId": turn_id, "item": {{"type": "commandExecution", "id": "cmd1", "command": "cargo test", "cwd": "/tmp", "status": "inProgress"}}}}}})
        send({{"method": "item/commandExecution/outputDelta", "params": {{"threadId": tid, "turnId": turn_id, "itemId": "cmd1", "delta": "test ok\n"}}}})
        send({{"method": "item/completed", "params": {{"threadId": tid, "turnId": turn_id, "item": {{"type": "commandExecution", "id": "cmd1", "command": "cargo test", "cwd": "/tmp", "status": "completed", "aggregatedOutput": "test ok\n", "exitCode": 0}}}}}})
        send({{"method": "item/completed", "params": {{"threadId": tid, "turnId": turn_id, "item": {{"type": "fileChange", "id": "fc1", "status": "completed", "changes": [{{"path": "src/main.rs", "diff": "@@ -1 +1 @@\n-old\n+new\n"}}]}}}}}})
        send({{"method": "item/agentMessage/delta", "params": {{"threadId": tid, "turnId": turn_id, "itemId": "m1", "delta": "All green."}}}})
        send({{"method": "thread/tokenUsage/updated", "params": {{"threadId": tid, "tokenUsage": {{"modelContextWindow": 200000, "last": {{"totalTokens": 12000}}, "total": {{"inputTokens": 10000, "cachedInputTokens": 4000, "outputTokens": 500}}}}}}}})
        send({{"method": "thread/name/updated", "params": {{"threadId": tid, "threadName": "Cargo test pass"}}}})
        send({{"method": "turn/completed", "params": {{"threadId": tid, "turn": {{"id": turn_id, "status": "completed", "error": None}}}}}})
    elif method == "turn/steer":
        send({{"id": rid, "result": {{"turnId": params.get("expectedTurnId")}}}})
    elif method == "turn/interrupt":
        send({{"id": rid, "result": {{}}}})
"#,
        log_path = log_file.to_string_lossy()
    );
    let codex_bin = write_executable(dir.path(), "codex", &script);
    let cmd_line = format!("{} app-server", codex_bin.display());
    let session_key = format!("mem:codex-e2e:{}", rand::random::<u64>());

    let rt = crate::runtime::runtime().unwrap();
    let mgr = AcpManager::spawn();

    // 1) Warm discovers models, options, and skills.
    let models = rt
        .block_on(mgr.warm(AcpWarm {
            session_key: session_key.clone(),
            cwd: dir.path().to_path_buf(),
            command_line: cmd_line.clone(),
            env: Vec::new(),
            model: "default".into(),
            effort: String::new(),
            mcp_servers: Vec::new(),
        }))
        .unwrap();
    assert_eq!(models, vec!["gpt-6.2-sol", "gpt-6.2-astra"]);
    let opts = config_options::options(&session_key, &cmd_line).unwrap();
    assert!(opts.iter().any(|o| o.id == "mode"));
    assert!(opts.iter().any(|o| o.id == "model"));
    assert!(opts.iter().any(|o| o.id == "reasoning_effort"));
    assert!(opts.iter().any(|o| o.id == "fast"));
    assert!(
        commands::available(&cmd_line, dir.path())
            .iter()
            .any(|c| c.name == "audit")
    );

    // 2) Live option update without restarting subprocess.
    rt.block_on(mgr.set_option(AcpSetOption {
        session_key: session_key.clone(),
        command_line: cmd_line.clone(),
        config_id: "fast".into(),
        value: json!(true),
        model: None,
        effort: None,
    }))
    .unwrap();
    let opts = config_options::options(&session_key, &cmd_line).unwrap();
    assert!(
        opts.iter()
            .any(|o| o.id == "fast" && o.kind == AcpConfigKind::Boolean(true))
    );

    // 3) Run full prompt turn with approval + steer + streaming + usage + title.
    let (ev_tx, ev_rx) = channel();
    let (ap_tx, ap_rx) = channel();
    let steer = SteerInbox::default();
    let steer_clone = steer.clone();
    let ev_thread = std::thread::spawn(move || {
        let mut events = Vec::new();
        while let Ok(ev) = ev_rx.recv() {
            if let AgentEvent::ApprovalRequest { name, .. } = &ev {
                assert_eq!(name, "bash");
                steer_clone.push("also check clippy".into());
                let _ = ap_tx.send(ApprovalDecision::ApproveRest);
            }
            events.push(ev);
        }
        events
    });

    rt.block_on(mgr.prompt(AcpPrompt {
        session_key: session_key.clone(),
        cwd: dir.path().to_path_buf(),
        command_line: cmd_line.clone(),
        env: Vec::new(),
        model: "gpt-6.2-sol".into(),
        effort: "high".into(),
        mcp_servers: Vec::new(),
        text: "run tests".into(),
        history: String::new(),
        missed: String::new(),
        archive: Vec::new(),
        images: Vec::new(),
        resources: Vec::new(),
        event_tx: ev_tx,
        approval_rx: ap_rx,
        approval_policy: ApprovalPolicy {
            write_edit: true,
            bash: true,
        },
        bash_allowlist: Vec::new(),
        allowed_tools: Vec::new(),
        cancel: Arc::new(AtomicBool::new(false)),
        plan_mode: false,
        steer: Some(steer),
        evidence_policy: crate::evidence::Policy::default(),
        evidence_base: None,
    }))
    .unwrap();
    let events = ev_thread.join().unwrap();
    let requests = std::fs::read_to_string(&log_file).unwrap();
    let approval = requests
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|request| request["id"] == 900)
        .unwrap();
    assert_eq!(approval["result"]["decision"], "accept");

    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::SteerAvailable))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ThinkingDelta(t) if t == "Thinking carefully"))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::TextDelta(t) if t == "All green."))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolUpdate(u) if u.name == "todo_write"))
    );
    assert!(events.iter().any(|e| matches!(e, AgentEvent::ToolUpdate(u) if u.name == "bash" && u.metadata.status == ToolStatus::Completed && u.output.contains("test ok"))));
    assert!(events.iter().any(|e| matches!(e, AgentEvent::ToolUpdate(u) if u.name == "edit" && u.diff.as_deref().is_some_and(|d| d.contains("+new")))));
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::ContextUsage {
            used: 12000,
            size: 200000
        }
    )));
    assert!(events.iter().any(|e| matches!(e, AgentEvent::ExternalUsage(u) if u.input_tokens == 6000 && u.cache_read_input_tokens == 4000 && u.output_tokens == 500)));
    assert!(
        session_info::take_titles()
            .iter()
            .any(|(k, t)| k == &session_key && t == "Cargo test pass")
    );

    // 4) Verify external session import (`thread/list` + `thread/read`).
    let import_req = AcpImport {
        command_line: cmd_line.clone(),
        cwd: dir.path().to_path_buf(),
        env: Vec::new(),
    };
    let listed = rt.block_on(list_sessions(&import_req)).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].session_id, "ext-1");
    assert_eq!(listed[0].title, "External Codex Chat");
    let loaded = rt.block_on(load_session(&import_req, "ext-1")).unwrap();
    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded[0].text, "Hello external");

    mgr.close(&session_key);
}

#[cfg(unix)]
#[test]
fn claude_native_e2e_turn_approvals_questions_options_and_plan_mode() {
    let dir = TempDir::new("oxi-claude-e2e");
    let log_file = dir.path().join("claude.log");
    let script = format!(
        r#"#!/usr/bin/env python3
import sys, json

log_path = {log_path:?}
def log(msg):
    with open(log_path, "a") as f:
        f.write(json.dumps(msg) + "\n")

def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()

sid = "claude-sess-native-1"

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    log(msg)
    mtype = msg.get("type")
    if mtype == "control_request":
        rid = msg["request_id"]
        sub = msg["request"]["subtype"]
        if sub == "initialize":
            send({{"type": "system", "subtype": "init", "session_id": sid}})
            send({{"type": "control_response", "response": {{
                "subtype": "success",
                "request_id": rid,
                "response": {{
                    "models": [
                        {{"value": "default", "displayName": "Default", "description": "Opus", "supportedEffortLevels": ["low", "high"], "supportsFastMode": True, "supportsAutoMode": True}},
                        {{"value": "sonnet", "displayName": "Sonnet", "description": "Fast", "supportedEffortLevels": ["low", "high"], "supportsFastMode": False, "supportsAutoMode": True}}
                    ],
                    "commands": [{{"name": "review", "description": "Review code", "argumentHint": "<branch>"}}],
                    "current_permission_mode": "default",
                    "fast_mode_state": "off"
                }}
            }}}})
        elif sub in ("set_model", "set_permission_mode", "apply_flag_settings", "interrupt"):
            send({{"type": "control_response", "response": {{"subtype": "success", "request_id": rid, "response": {{}}}}}})
    elif mtype == "user":
        text = ""
        for b in msg["message"]["content"]:
            if b.get("type") == "text":
                text += b.get("text", "")
        # Ask tool permission for Edit in plan mode vs normal mode
        if "plan this" in text:
            send({{"type": "control_request", "request_id": "perm_plan", "request": {{
                "subtype": "can_use_tool",
                "tool_name": "Edit",
                "tool_use_id": "tu_plan",
                "input": {{"file_path": "/tmp/src/a.rs", "old_string": "a", "new_string": "b"}}
            }}}})
            resp = json.loads(sys.stdin.readline())
            log(resp)
            behavior = resp["response"]["response"]["behavior"]
            send({{"type": "assistant", "session_id": sid, "parent_tool_use_id": None, "message": {{"content": [{{"type": "text", "text": f"plan_edit={{behavior}}"}}]}}}})
            send({{"type": "result", "subtype": "success", "is_error": False, "session_id": sid, "total_cost_usd": 0.01, "usage": {{"input_tokens": 10, "output_tokens": 5}}, "modelUsage": {{}}}})
            continue

        # Stream thinking + tool_use + text
        send({{"type": "stream_event", "parent_tool_use_id": None, "event": {{"type": "message_start", "message": {{"usage": {{"input_tokens": 2, "cache_read_input_tokens": 3000, "cache_creation_input_tokens": 200, "output_tokens": 1}}}}}}}})
        send({{"type": "stream_event", "parent_tool_use_id": None, "event": {{"type": "content_block_delta", "index": 0, "delta": {{"type": "thinking_delta", "thinking": "Planning fix"}}}}}})
        send({{"type": "stream_event", "parent_tool_use_id": None, "event": {{"type": "content_block_start", "index": 1, "content_block": {{"type": "tool_use", "id": "tu_edit", "name": "Edit", "input": {{}}}}}}}})
        send({{"type": "stream_event", "parent_tool_use_id": None, "event": {{"type": "content_block_delta", "index": 1, "delta": {{"type": "input_json_delta", "partial_json": "{{\"file_path\":\"src/app.rs\",\"old_string\":\"old\",\"new_string\":\"new\"}}"}}}}}})
        send({{"type": "stream_event", "parent_tool_use_id": None, "event": {{"type": "content_block_stop", "index": 1}}}})

        # Request Bash permission from host
        send({{"type": "control_request", "request_id": "perm_bash", "request": {{
            "subtype": "can_use_tool",
            "tool_name": "Bash",
            "tool_use_id": "tu_bash",
            "input": {{"command": "cargo check"}}
        }}}})
        perm_resp = json.loads(sys.stdin.readline())
        log(perm_resp)
        assert perm_resp["response"]["response"]["behavior"] == "allow"

        # Request AskUserQuestion from host
        send({{"type": "control_request", "request_id": "perm_q", "request": {{
            "subtype": "can_use_tool",
            "tool_name": "AskUserQuestion",
            "tool_use_id": "tu_q",
            "input": {{"questions": [{{"header": "Mode", "question": "Ship it?", "multiSelect": False, "options": [{{"label": "Yes", "description": "Go"}}, {{"label": "No", "description": "Stop"}}]}}]}}
        }}}})
        q_resp = json.loads(sys.stdin.readline())
        log(q_resp)
        ans = q_resp["response"]["response"]["updatedInput"]["answers"]["Ship it?"]

        send({{"type": "user", "session_id": sid, "parent_tool_use_id": None, "message": {{"content": [{{"type": "tool_result", "tool_use_id": "tu_edit", "content": "Edited ok"}}]}}}})
        # Second API call of the turn; a subagent's call must not count.
        send({{"type": "stream_event", "parent_tool_use_id": "tu_task", "event": {{"type": "message_start", "message": {{"usage": {{"input_tokens": 9, "cache_read_input_tokens": 90000}}}}}}}})
        send({{"type": "stream_event", "parent_tool_use_id": None, "event": {{"type": "message_start", "message": {{"usage": {{"input_tokens": 3, "cache_read_input_tokens": 3200, "cache_creation_input_tokens": 100, "output_tokens": 1}}}}}}}})
        send({{"type": "stream_event", "parent_tool_use_id": None, "event": {{"type": "content_block_delta", "index": 2, "delta": {{"type": "text_delta", "text": f"Shipped ({{ans}})."}}}}}})
        # Duplicate full assistant message must NOT double-emit text!
        send({{"type": "assistant", "session_id": sid, "parent_tool_use_id": None, "message": {{"content": [
            {{"type": "text", "text": f"Shipped ({{ans}})."}},
            {{"type": "tool_use", "id": "tu_todo", "name": "mcp__oxi__todo_write", "input": {{"todos": [{{"content": "Verify", "status": "completed"}}]}}}},
            {{"type": "tool_use", "id": "tu_ts", "name": "ToolSearch", "input": {{"query": "foo"}}}}
        ]}}}})
        send({{"type": "stream_event", "parent_tool_use_id": None, "event": {{"type": "message_delta", "delta": {{"stop_reason": "end_turn"}}, "usage": {{"output_tokens": 40}}}}}})
        send({{"type": "result", "subtype": "success", "is_error": False, "session_id": sid, "total_cost_usd": 0.42, "usage": {{"input_tokens": 1000, "output_tokens": 250, "cache_read_input_tokens": 5000, "cache_creation_input_tokens": 300}}, "modelUsage": {{"claude-opus-5-5": {{"inputTokens": 1000, "cacheReadInputTokens": 5000, "cacheCreationInputTokens": 300, "contextWindow": 200000}}}}}})
"#,
        log_path = log_file.to_string_lossy()
    );
    let claude_bin = write_executable(dir.path(), "claude", &script);
    let cmd_line = claude_bin.display().to_string();
    let session_key = format!("mem:claude-e2e:{}", rand::random::<u64>());

    let rt = crate::runtime::runtime().unwrap();
    let mgr = AcpManager::spawn();

    // 1) Warm discovers models, commands, and options.
    let models = rt
        .block_on(mgr.warm(AcpWarm {
            session_key: session_key.clone(),
            cwd: dir.path().to_path_buf(),
            command_line: cmd_line.clone(),
            env: Vec::new(),
            model: "default".into(),
            effort: String::new(),
            mcp_servers: Vec::new(),
        }))
        .unwrap();
    assert_eq!(models, vec!["default", "sonnet"]);
    assert!(
        commands::available(&cmd_line, dir.path())
            .iter()
            .any(|c| c.name == "review")
    );

    // 2) Live set_option sends control_requests without restarting.
    rt.block_on(mgr.set_option(AcpSetOption {
        session_key: session_key.clone(),
        command_line: cmd_line.clone(),
        config_id: "model".into(),
        value: json!("sonnet"),
        model: Some("sonnet".into()),
        effort: None,
    }))
    .unwrap();
    let opts = config_options::options(&session_key, &cmd_line).unwrap();
    assert!(opts.iter().any(|o| o.id == "model"
        && matches!(&o.kind, AcpConfigKind::Select { current, .. } if current == "sonnet")));

    // 3) Full prompt turn with streaming, diff, approval, AskUserQuestion, checklist, usage, cost.
    let (ev_tx, ev_rx) = channel();
    let (ap_tx, ap_rx) = channel();
    let ev_thread = std::thread::spawn(move || {
        let mut events = Vec::new();
        while let Ok(ev) = ev_rx.recv() {
            match &ev {
                AgentEvent::ApprovalRequest { name, .. } => {
                    assert_eq!(name, "bash");
                    let _ = ap_tx.send(ApprovalDecision::Approve);
                }
                AgentEvent::ElicitationRequest { .. } => {
                    let _ = ap_tx.send(ApprovalDecision::Elicitation(json!({
                        "action": "accept",
                        "content": { "question_0": "Yes" }
                    })));
                }
                _ => {}
            }
            events.push(ev);
        }
        events
    });

    rt.block_on(mgr.prompt(AcpPrompt {
        session_key: session_key.clone(),
        cwd: dir.path().to_path_buf(),
        command_line: cmd_line.clone(),
        env: Vec::new(),
        model: "sonnet".into(),
        effort: String::new(),
        mcp_servers: Vec::new(),
        text: "implement feature".into(),
        history: String::new(),
        missed: String::new(),
        archive: Vec::new(),
        images: Vec::new(),
        resources: Vec::new(),
        event_tx: ev_tx,
        approval_rx: ap_rx,
        approval_policy: ApprovalPolicy {
            write_edit: true,
            bash: true,
        },
        bash_allowlist: Vec::new(),
        allowed_tools: Vec::new(),
        cancel: Arc::new(AtomicBool::new(false)),
        plan_mode: false,
        steer: Some(SteerInbox::default()),
        evidence_policy: crate::evidence::Policy::default(),
        evidence_base: None,
    }))
    .unwrap();
    let events = ev_thread.join().unwrap();

    let answer: String = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::TextDelta(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    // Streamed once, never duplicated by the subsequent `assistant` frame.
    assert_eq!(answer, "Shipped (Yes).");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ThinkingDelta(t) if t == "Planning fix"))
    );
    assert!(events.iter().any(|e| matches!(e, AgentEvent::ToolUpdate(u) if u.name == "edit" && u.diff.as_deref().is_some_and(|d| d.contains("+new")) && u.output == "Edited ok")));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolUpdate(u) if u.name == "todo_write"))
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolUpdate(u) if u.name == "ToolSearch"))
    );
    // Context is the last main-agent call (3 + 3200 + 100 in, 40 out), not the turn's
    // `modelUsage` total (6300) that adds up both calls.
    let context: Vec<(u64, u64)> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ContextUsage { used, size } => Some((*used, *size)),
            _ => None,
        })
        .collect();
    assert_eq!(context.last(), Some(&(3343, 200000)), "{context:?}");
    assert!(context.iter().all(|(used, _)| *used < 6300), "{context:?}");
    assert!(events.iter().any(|e| matches!(e, AgentEvent::ExternalUsage(u) if u.input_tokens == 1000 && u.cache_read_input_tokens == 5000 && u.cache_creation_input_tokens == 300 && u.output_tokens == 250)));
    let (cost, cur) = session_info::reported_cost(&session_key).unwrap();
    assert!((cost - 0.42).abs() < 1e-6 && cur == "USD");

    // 4) Plan mode switches Claude into plan mode and denies non-plan Edit automatically.
    let (ev_tx2, ev_rx2) = channel();
    let (_ap_tx2, ap_rx2) = channel();
    rt.block_on(mgr.prompt(AcpPrompt {
        session_key: session_key.clone(),
        cwd: dir.path().to_path_buf(),
        command_line: cmd_line.clone(),
        env: Vec::new(),
        model: "sonnet".into(),
        effort: String::new(),
        mcp_servers: Vec::new(),
        text: "plan this".into(),
        history: String::new(),
        missed: String::new(),
        archive: Vec::new(),
        images: Vec::new(),
        resources: Vec::new(),
        event_tx: ev_tx2,
        approval_rx: ap_rx2,
        approval_policy: ApprovalPolicy::disabled(),
        bash_allowlist: Vec::new(),
        allowed_tools: Vec::new(),
        cancel: Arc::new(AtomicBool::new(false)),
        plan_mode: true,
        steer: Some(SteerInbox::default()),
        evidence_policy: crate::evidence::Policy::default(),
        evidence_base: None,
    }))
    .unwrap();
    let plan_answer: String = ev_rx2
        .try_iter()
        .filter_map(|e| match e {
            AgentEvent::TextDelta(t) => Some(t),
            _ => None,
        })
        .collect();
    assert_eq!(plan_answer, "plan_edit=deny");

    mgr.close(&session_key);
}

// ── Live smoke tests against real installed `codex` and `claude` CLIs ───────

/// Real upgrade path: a session created by the `codex-acp` adapter (as older oxi builds did)
/// resumes in `codex app-server` through the legacy binding instead of starting a new thread.
/// Needs `codex-acp` on PATH.
#[test]
#[ignore]
fn codex_acp_session_resumes_in_native_app_server() {
    use crate::settings::ProviderConfig;

    let dir = TempDir::new("oxi-codex-acp-upgrade");
    let key = dir.path().join("chat.json").to_string_lossy().into_owned();
    let rt = crate::runtime::runtime().unwrap();
    let mgr = AcpManager::spawn();
    let prompt = |command: &str, text: &str| {
        let (ev_tx, ev_rx) = channel();
        let (_ap_tx, ap_rx) = channel();
        rt.block_on(mgr.prompt(AcpPrompt {
            session_key: key.clone(),
            cwd: dir.path().to_path_buf(),
            command_line: command.into(),
            env: Vec::new(),
            model: "default".into(),
            effort: "low".into(),
            mcp_servers: Vec::new(),
            text: text.into(),
            history: String::new(),
            missed: String::new(),
            archive: Vec::new(),
            images: Vec::new(),
            resources: Vec::new(),
            event_tx: ev_tx,
            approval_rx: ap_rx,
            approval_policy: ApprovalPolicy::disabled(),
            bash_allowlist: Vec::new(),
            allowed_tools: Vec::new(),
            cancel: Arc::new(AtomicBool::new(false)),
            plan_mode: false,
            steer: None,
            evidence_policy: crate::evidence::Policy::default(),
            evidence_base: None,
        }))
        .unwrap_or_else(|e| panic!("{command}: {e}"));
        ev_rx
            .try_iter()
            .filter_map(|e| match e {
                AgentEvent::TextDelta(t) => Some(t),
                _ => None,
            })
            .collect::<String>()
    };

    prompt(
        "codex-acp",
        "Remember the code word PELICAN. Do not use any tools. Reply with only OK.",
    );
    let acp_id = super::sessions::lookup(&key, "codex-acp", dir.path()).expect("codex-acp session");
    mgr.stop(&key);
    // Older builds stored the binding under the default npx command line.
    super::sessions::remember(
        &key,
        ProviderConfig::LEGACY_CODEX_ACP_COMMAND,
        dir.path(),
        &acp_id,
    );

    let native = ProviderConfig::DEFAULT_CODEX_ACP_COMMAND;
    let answer = prompt(
        native,
        "What was the code word? Do not use any tools. Reply with only the word.",
    );
    assert_eq!(
        super::sessions::lookup(&key, native, dir.path()),
        Some(acp_id),
        "app-server started a new thread instead of resuming the codex-acp session"
    );
    assert!(
        answer.to_uppercase().contains("PELICAN"),
        "answer={answer:?}"
    );
    mgr.close(&key);
}

/// Real `codex app-server`: a Plan mode turn (native `collaborationMode: plan`) and the turn
/// after it (switch back to `default`) are both accepted and answer.
#[test]
#[ignore]
fn codex_native_live_plan_mode() {
    let dir = TempDir::new("oxi-codex-live-plan");
    let session_key = format!("mem:codex-live-plan:{}", rand::random::<u64>());
    let command = crate::settings::ProviderConfig::DEFAULT_CODEX_ACP_COMMAND;
    let rt = crate::runtime::runtime().unwrap();
    let mgr = AcpManager::spawn();
    let models = rt
        .block_on(mgr.warm(AcpWarm {
            session_key: session_key.clone(),
            cwd: dir.path().to_path_buf(),
            command_line: command.into(),
            env: Vec::new(),
            model: "default".into(),
            effort: "low".into(),
            mcp_servers: Vec::new(),
        }))
        .expect("codex warm");
    for plan_mode in [true, false] {
        let (ev_tx, ev_rx) = channel();
        let (_ap_tx, ap_rx) = channel();
        rt.block_on(mgr.prompt(AcpPrompt {
            session_key: session_key.clone(),
            cwd: dir.path().to_path_buf(),
            command_line: command.into(),
            env: Vec::new(),
            model: models[0].clone(),
            effort: "low".into(),
            mcp_servers: Vec::new(),
            text: "Do not use any tools. Reply with only the word OK.".into(),
            history: String::new(),
            missed: String::new(),
            archive: Vec::new(),
            images: Vec::new(),
            resources: Vec::new(),
            event_tx: ev_tx,
            approval_rx: ap_rx,
            approval_policy: ApprovalPolicy::disabled(),
            bash_allowlist: Vec::new(),
            allowed_tools: Vec::new(),
            cancel: Arc::new(AtomicBool::new(false)),
            plan_mode,
            steer: Some(SteerInbox::default()),
            evidence_policy: crate::evidence::Policy::default(),
            evidence_base: None,
        }))
        .unwrap_or_else(|e| panic!("plan_mode={plan_mode}: {e}"));
        let answer: String = ev_rx
            .try_iter()
            .filter_map(|e| match e {
                AgentEvent::TextDelta(t) => Some(t),
                _ => None,
            })
            .collect();
        assert!(
            answer.to_uppercase().contains("OK"),
            "plan_mode={plan_mode} answer={answer:?}"
        );
    }
    mgr.close(&session_key);
}

#[test]
#[ignore]
fn codex_native_live_smoke() {
    use crate::settings::ProviderConfig;

    let dir = TempDir::new("oxi-codex-live");
    let session_key = dir
        .path()
        .join("native.json")
        .to_string_lossy()
        .into_owned();
    let command = ProviderConfig::DEFAULT_CODEX_ACP_COMMAND;
    let rt = crate::runtime::runtime().unwrap();
    let mgr = AcpManager::spawn();
    let models = rt
        .block_on(mgr.warm(AcpWarm {
            session_key: session_key.clone(),
            cwd: dir.path().to_path_buf(),
            command_line: command.into(),
            env: Vec::new(),
            model: "default".into(),
            effort: "low".into(),
            mcp_servers: Vec::new(),
        }))
        .expect("codex warm");
    assert!(!models.is_empty(), "expected codex models");
    let (ev_tx, ev_rx) = channel();
    let (_ap_tx, ap_rx) = channel();
    rt.block_on(mgr.prompt(AcpPrompt {
        session_key: session_key.clone(),
        cwd: dir.path().to_path_buf(),
        command_line: command.into(),
        env: Vec::new(),
        model: models[0].clone(),
        effort: "low".into(),
        mcp_servers: Vec::new(),
        text: "Reply with exactly the single word OK and nothing else.".into(),
        history: String::new(),
        missed: String::new(),
        archive: Vec::new(),
        images: Vec::new(),
        resources: Vec::new(),
        event_tx: ev_tx,
        approval_rx: ap_rx,
        approval_policy: ApprovalPolicy::disabled(),
        bash_allowlist: Vec::new(),
        allowed_tools: Vec::new(),
        cancel: Arc::new(AtomicBool::new(false)),
        plan_mode: false,
        steer: Some(SteerInbox::default()),
        evidence_policy: crate::evidence::Policy::default(),
        evidence_base: None,
    }))
    .expect("codex prompt");
    let answer: String = ev_rx
        .try_iter()
        .filter_map(|e| match e {
            AgentEvent::TextDelta(t) => Some(t),
            _ => None,
        })
        .collect();
    let word: String = answer.chars().filter(|c| c.is_alphanumeric()).collect();
    assert_eq!(word.to_uppercase(), "OK", "answer={answer:?}");

    let thread_id = super::sessions::lookup(&session_key, command, dir.path()).unwrap();
    mgr.close(&session_key);

    let legacy_key = dir
        .path()
        .join("legacy.json")
        .to_string_lossy()
        .into_owned();
    super::sessions::remember(
        &legacy_key,
        ProviderConfig::LEGACY_CODEX_ACP_COMMAND,
        dir.path(),
        &thread_id,
    );
    rt.block_on(mgr.warm(AcpWarm {
        session_key: legacy_key.clone(),
        cwd: dir.path().to_path_buf(),
        command_line: command.into(),
        env: Vec::new(),
        model: "default".into(),
        effort: "low".into(),
        mcp_servers: Vec::new(),
    }))
    .expect("codex resume from legacy binding");
    assert_eq!(
        super::sessions::lookup(&legacy_key, command, dir.path()),
        Some(thread_id)
    );
    mgr.close(&legacy_key);
}

#[test]
#[ignore]
fn claude_native_live_smoke() {
    let dir = TempDir::new("oxi-claude-live");
    let session_key = format!("mem:claude-live:{}", rand::random::<u64>());
    let rt = crate::runtime::runtime().unwrap();
    let mgr = AcpManager::spawn();
    let models = rt
        .block_on(mgr.warm(AcpWarm {
            session_key: session_key.clone(),
            cwd: dir.path().to_path_buf(),
            command_line: crate::settings::ProviderConfig::DEFAULT_ACP_COMMAND.into(),
            env: Vec::new(),
            model: "haiku".into(),
            effort: "low".into(),
            mcp_servers: Vec::new(),
        }))
        .expect("claude warm");
    assert!(!models.is_empty(), "expected claude models");
    let (ev_tx, ev_rx) = channel();
    let (_ap_tx, ap_rx) = channel();
    rt.block_on(mgr.prompt(AcpPrompt {
        session_key: session_key.clone(),
        cwd: dir.path().to_path_buf(),
        command_line: crate::settings::ProviderConfig::DEFAULT_ACP_COMMAND.into(),
        env: Vec::new(),
        model: "haiku".into(),
        effort: "low".into(),
        mcp_servers: Vec::new(),
        text: "Reply with exactly the single word OK and nothing else.".into(),
        history: String::new(),
        missed: String::new(),
        archive: Vec::new(),
        images: Vec::new(),
        resources: Vec::new(),
        event_tx: ev_tx,
        approval_rx: ap_rx,
        approval_policy: ApprovalPolicy::disabled(),
        bash_allowlist: Vec::new(),
        allowed_tools: Vec::new(),
        cancel: Arc::new(AtomicBool::new(false)),
        plan_mode: false,
        steer: Some(SteerInbox::default()),
        evidence_policy: crate::evidence::Policy::default(),
        evidence_base: None,
    }))
    .expect("claude prompt");
    let answer: String = ev_rx
        .try_iter()
        .filter_map(|e| match e {
            AgentEvent::TextDelta(t) => Some(t),
            _ => None,
        })
        .collect();
    assert!(answer.to_uppercase().contains("OK"), "answer={answer:?}");
    mgr.close(&session_key);
}

/// Claude Code ends the turn while a `run_in_background` command keeps going, then starts a
/// turn of its own when the command finishes. The command stays shown as running in between,
/// and the later turn is held until the UI attaches to show it.
#[cfg(unix)]
#[test]
fn claude_background_command_stays_running_and_its_followup_turn_is_attached() {
    let dir = TempDir::new("oxi-claude-bg");
    let script = r#"#!/usr/bin/env python3
import sys, json, threading, time
lock = threading.Lock()
def send(obj):
    with lock:
        sys.stdout.write(json.dumps(obj) + "\n")
        sys.stdout.flush()
sid = "claude-bg-1"
def finish_later():
    time.sleep(1.0)
    send({"type": "system", "subtype": "task_updated", "task_id": "bg1", "patch": {"status": "completed"}, "session_id": sid})
    send({"type": "system", "subtype": "task_notification", "task_id": "bg1", "tool_use_id": "toolu_bg", "status": "completed", "summary": "Background command \"ci\" completed (exit code 0)", "session_id": sid})
    send({"type": "system", "subtype": "background_tasks_changed", "tasks": [], "session_id": sid})
    send({"type": "system", "subtype": "init", "session_id": sid})
    send({"type": "assistant", "parent_tool_use_id": None, "session_id": sid, "message": {"content": [{"type": "text", "text": "CI is green"}]}})
    send({"type": "result", "subtype": "success", "is_error": False, "session_id": sid, "usage": {}, "modelUsage": {}})
for line in sys.stdin:
    if not line.strip():
        continue
    msg = json.loads(line)
    if msg.get("type") == "control_request":
        rid, sub = msg["request_id"], msg["request"]["subtype"]
        if sub == "initialize":
            send({"type": "control_response", "response": {"subtype": "success", "request_id": rid, "response": {
                "models": [{"value": "default", "displayName": "Default", "description": "", "supportedEffortLevels": []}],
                "commands": [], "current_permission_mode": "default"
            }}})
        else:
            send({"type": "control_response", "response": {"subtype": "success", "request_id": rid, "response": {}}})
    elif msg.get("type") == "user":
        send({"type": "system", "subtype": "init", "session_id": sid})
        send({"type": "assistant", "parent_tool_use_id": None, "session_id": sid, "message": {"content": [
            {"type": "tool_use", "id": "toolu_bg", "name": "Bash", "input": {"command": "gh pr checks --watch", "description": "ci", "run_in_background": True}}
        ]}})
        send({"type": "system", "subtype": "background_tasks_changed", "tasks": [{"task_id": "bg1"}], "session_id": sid})
        send({"type": "system", "subtype": "task_started", "task_id": "bg1", "tool_use_id": "toolu_bg", "description": "ci", "is_backgrounded": True, "task_type": "local_bash", "session_id": sid})
        send({"type": "user", "parent_tool_use_id": None, "session_id": sid,
              "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_bg", "content": "Command running in background with ID: bg1."}]},
              "tool_use_result": {"stdout": "", "backgroundTaskId": "bg1"}})
        send({"type": "assistant", "parent_tool_use_id": None, "session_id": sid, "message": {"content": [{"type": "text", "text": "waiting"}]}})
        send({"type": "result", "subtype": "success", "is_error": False, "session_id": sid, "usage": {}, "modelUsage": {}})
        threading.Thread(target=finish_later).start()
"#;
    let bin = write_executable(dir.path(), "claude", script);
    let cmd_line = bin.display().to_string();
    let session_key = format!("mem:claude-bg:{}", rand::random::<u64>());
    let mgr = AcpManager::spawn();
    let rt = crate::runtime::runtime().unwrap();

    let tool_status = |events: &[AgentEvent]| {
        events.iter().rev().find_map(|ev| match ev {
            AgentEvent::ToolUpdate(u) if u.tool_call_id == "toolu_bg" => Some(u.metadata.status),
            _ => None,
        })
    };

    let (ev_tx, ev_rx) = channel();
    let (_ap_tx, ap_rx) = channel();
    let req = AcpPrompt {
        session_key: session_key.clone(),
        cwd: dir.path().to_path_buf(),
        command_line: cmd_line.clone(),
        env: Vec::new(),
        model: "default".into(),
        effort: String::new(),
        mcp_servers: Vec::new(),
        text: "watch CI".into(),
        history: String::new(),
        missed: String::new(),
        archive: Vec::new(),
        images: Vec::new(),
        resources: Vec::new(),
        event_tx: ev_tx,
        approval_rx: ap_rx,
        approval_policy: ApprovalPolicy::disabled(),
        bash_allowlist: Vec::new(),
        allowed_tools: Vec::new(),
        cancel: Arc::new(AtomicBool::new(false)),
        plan_mode: false,
        steer: None,
        evidence_policy: crate::evidence::Policy::default(),
        evidence_base: None,
    };
    rt.block_on(mgr.prompt(req)).unwrap();
    let events: Vec<AgentEvent> = ev_rx.try_iter().collect();
    assert_eq!(tool_status(&events), Some(ToolStatus::InProgress));
    let running = super::background_tools(&session_key);
    assert_eq!(
        running
            .iter()
            .map(|t| t.tool_call_id.as_str())
            .collect::<Vec<_>>(),
        ["toolu_bg"]
    );
    assert_eq!(running[0].label, "ci");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !super::pending_autonomous().contains(&session_key) {
        assert!(
            std::time::Instant::now() < deadline,
            "the follow-up turn was never announced"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    super::claim_autonomous(&session_key);

    let (ev_tx, ev_rx) = channel();
    let (_ap_tx, ap_rx) = channel();
    let attached = rt.block_on(async {
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            mgr.attach(super::AcpAttach {
                session_key: session_key.clone(),
                cwd: dir.path().to_path_buf(),
                event_tx: ev_tx,
                approval_rx: ap_rx,
                approval_policy: ApprovalPolicy::disabled(),
                bash_allowlist: Vec::new(),
                allowed_tools: Vec::new(),
                cancel: Arc::new(AtomicBool::new(false)),
                plan_mode: false,
            }),
        )
        .await
    });
    assert!(matches!(attached, Ok(Ok(()))), "{attached:?}");
    let events: Vec<AgentEvent> = ev_rx.try_iter().collect();
    assert_eq!(tool_status(&events), Some(ToolStatus::Completed));
    let text: String = events
        .iter()
        .filter_map(|ev| match ev {
            AgentEvent::TextDelta(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    assert!(text.contains("CI is green"), "{text:?}");
    assert!(super::background_tools(&session_key).is_empty());
    mgr.close(&session_key);
}
