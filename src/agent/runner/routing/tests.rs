use super::*;

const ADAPTER: &str = r#"
import json, sys
from pathlib import Path
mode, log = sys.argv[1:]
prompt_id = None
def send(v):
    print(json.dumps(dict(jsonrpc='2.0', **v)), flush=True)
def update(v):
    send(dict(method='session/update', params=dict(sessionId='fake-session', update=v)))
for line in sys.stdin:
    m = json.loads(line)
    method = m.get('method')
    if method == 'initialize':
        send(dict(id=m['id'], result=dict(protocolVersion=1, agentCapabilities={})))
    elif method == 'session/new':
        send(dict(id=m['id'], result=dict(sessionId='fake-session')))
    elif method == 'session/prompt':
        prompt_id = m['id']
        Path(log).write_text(json.dumps(m['params']))
        if mode == 'quotaonce':
            marker = Path(log + '.ran')
            mode = 'success' if marker.exists() else 'quota'
            marker.write_text('1')
        if mode == 'auth':
            send(dict(id=prompt_id, error=dict(code=-32000,message='not authenticated')))
        elif mode in ['quota', 'pending', 'error']:
            update(dict(sessionUpdate='agent_message_chunk', content=dict(type='text', text='Parser updated.')))
            update(dict(sessionUpdate='tool_call', toolCallId='edit-1', kind='edit', status='in_progress' if mode == 'pending' else 'completed', rawInput=dict(path='parser.rs'), content=[dict(type='content', content=dict(type='text',text='Applied parser patch; tests passed.'))]))
            send(dict(id=prompt_id, error=dict(code=-32000,message='model not found' if mode == 'error' else 'usage limit reached')))
        elif mode == 'wait':
            update(dict(sessionUpdate='agent_message_chunk', content=dict(type='text', text='Waiting.')))
        else:
            send(dict(id=901, method='session/request_permission', params=dict(sessionId='fake-session', toolCall=dict(toolCallId='edit-1',kind='edit',rawInput=dict(path='next.rs')), options=[dict(optionId='yes',kind='allow_once'),dict(optionId='no',kind='reject_once')])))
    elif m.get('id') == 901 and 'result' in m:
        option = m['result']['outcome'].get('optionId')
        update(dict(sessionUpdate='agent_message_chunk', content=dict(type='text',text='Permission '+str(option))))
        send(dict(id=prompt_id,result=dict(stopReason='end_turn')))
    elif method == 'session/cancel':
        send(dict(id=prompt_id,result=dict(stopReason='cancelled')))
    elif 'id' in m:
        send(dict(id=m['id'], result={}))
"#;

fn user(text: &str) -> ChatMessage {
    ChatMessage {
        role: MsgRole::User,
        text: text.into(),
        is_summary: false,
        attachments: vec![],
        blocks: vec![],
        streaming: false,
        started_at: None,
        worked_duration: None,
        route: None,
        changes: None,
        evidence: None,
    }
}

async fn run_case(
    mode: &str,
    exclusive: bool,
    plan: bool,
    stop: bool,
) -> (Vec<AgentEvent>, String) {
    crate::secrets::use_mock_store();
    for kind in [
        LlmProviderKind::CodexAcp,
        LlmProviderKind::ClaudeCodeAcp,
        LlmProviderKind::CursorAcp,
    ] {
        crate::router::quota::clear_cooldown(kind);
    }
    let root = std::env::temp_dir().join(format!(
        "oxi-routing-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let script = root.join("adapter.py");
    std::fs::write(&script, ADAPTER).unwrap();
    let mut settings = AppSettings {
        active_provider: if matches!(
            mode,
            "pinned" | "direct" | "directwait" | "authwait" | "chain"
        ) {
            LlmProviderKind::CodexAcp
        } else {
            LlmProviderKind::Router
        },
        ..Default::default()
    };
    settings.router.use_jev = false;
    settings.router.wait_for_quota = matches!(
        mode,
        "directwait" | "authwait" | "initialwait" | "initialcancel"
    );
    if matches!(
        mode,
        "directwait" | "authwait" | "initialwait" | "initialcancel"
    ) {
        // Codex's 5h window is used up and resets in a few seconds.
        crate::router::quota::set_snapshot_for_tests(
            LlmProviderKind::CodexAcp,
            crate::router::quota::QuotaSnapshot {
                windows: vec![crate::router::quota::UsageWindow {
                    label: "5h".into(),
                    used_pct: 100.0,
                    resets_at: Some(crate::router::quota::now_secs() + 4),
                    model_scope: None,
                }],
                ..Default::default()
            },
        );
    }
    settings.require_write_edit_approval = true;
    for kind in LlmProviderKind::ALL {
        settings.router.prefs_mut(kind).enabled = false;
    }
    let server = if matches!(mode, "http" | "tohttp") {
        let server = wiremock::MockServer::start().await;
        let response = if mode == "http" {
            wiremock::ResponseTemplate::new(402).set_body_string("insufficient credits")
        } else {
            wiremock::ResponseTemplate::new(200).insert_header("content-type", "text/event-stream")
                .set_body_string("data: {\"choices\":[{\"delta\":{\"content\":\"Continued through API.\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n")
        };
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/v1/chat/completions"))
            .respond_with(response)
            .mount(&server)
            .await;
        settings.router.prefs_mut(LlmProviderKind::OpenAi).enabled = true;
        let cfg = settings.provider_mut(LlmProviderKind::OpenAi);
        cfg.api_key = "fixture-key".into();
        cfg.base_url = format!("{}/v1", server.uri());
        cfg.model_id = "gpt-5-mini".into();
        Some(server)
    } else {
        None
    };
    let python = crate::agent::test_python_executable();
    for (kind, adapter_mode, log) in [
        (
            LlmProviderKind::CodexAcp,
            if matches!(mode, "directwait" | "authwait") {
                "quotaonce"
            } else if matches!(mode, "initialwait" | "initialcancel") {
                "success"
            } else if matches!(mode, "exhaust" | "tohttp" | "pinned" | "direct" | "chain") {
                "quota"
            } else {
                mode
            },
            root.join("first.json"),
        ),
        (
            LlmProviderKind::ClaudeCodeAcp,
            if mode == "authwait" {
                "auth"
            } else if matches!(mode, "exhaust" | "directwait" | "chain") {
                "quota"
            } else {
                "success"
            },
            root.join("second.json"),
        ),
    ] {
        settings.router.prefs_mut(kind).enabled = true;
        let cfg = settings.provider_mut(kind);
        cfg.model_id = "default".into();
        cfg.acp_command = format!(
            "\"{}\" -u \"{}\" {adapter_mode} \"{}\"",
            python.display(),
            script.display(),
            log.display()
        );
    }
    if mode == "http" {
        settings.router.prefs_mut(LlmProviderKind::CodexAcp).enabled = false;
    }
    if matches!(mode, "tohttp" | "initialwait" | "initialcancel") {
        settings
            .router
            .prefs_mut(LlmProviderKind::ClaudeCodeAcp)
            .enabled = false;
    }
    if matches!(mode, "exhaust" | "chain") {
        settings
            .router
            .prefs_mut(LlmProviderKind::CursorAcp)
            .enabled = true;
        let cfg = settings.provider_mut(LlmProviderKind::CursorAcp);
        cfg.model_id = "default".into();
        cfg.acp_command = format!(
            "\"{}\" -u \"{}\" quota \"{}\"",
            python.display(),
            script.display(),
            root.join("third.json").display()
        );
    }
    if mode == "chain" {
        let cfg = settings.provider_mut(LlmProviderKind::CursorAcp);
        cfg.acp_command = cfg.acp_command.replace(" quota ", " success ");
    }
    // Any optional quota probes stay on loopback in this test process.
    settings.provider_mut(LlmProviderKind::OpenRouter).base_url = "http://127.0.0.1:1/v1".into();
    let (tx, rx) = mpsc::channel();
    let (approval_tx, approval_rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let acp = crate::agent::acp::AcpManager::spawn();
    let session_key = format!("routing-test-{}", root.display());
    let executor = AgentExecutor::new().unwrap();
    let handle = spawn_agent_run(
        &executor,
        AgentRunRequest {
            settings,
            tunnels: crate::compute::TunnelManager::spawn(),
            acp: acp.clone(),
            mcp: crate::agent::mcp::McpManager::new(),
            acp_session_key: session_key.clone(),
            cwd: root.clone(),
            chat_for_history: vec![user(if mode == "http" {
                "Use OpenAI API for this task"
            } else if mode == "image" {
                "Generează o imagine cu o pisică pe lună"
            } else if mode == "pinned" {
                "foloseste claude care sunt stirile de azi?"
            } else if matches!(
                mode,
                "direct" | "directwait" | "authwait" | "initialwait" | "initialcancel" | "chain"
            ) {
                "Fix the parser"
            } else if exclusive {
                "Folosește doar Codex pentru asta"
            } else {
                "Folosește Codex pentru asta"
            })],
            approval_rx,
            cancel: cancel.clone(),
            wire_candidate: None,
            chars_per_token: 4.0,
            plan_mode: plan,
            evidence_mode: false.into(),
            undo_journal: Arc::new(std::sync::Mutex::new(Default::default())),
            steer: Default::default(),
        },
        tx,
    );
    let mut events = Vec::new();
    let timeout = tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            while let Ok(event) = rx.try_recv() {
                if matches!(event, AgentEvent::ApprovalRequest { .. }) {
                    let _ = approval_tx.send(ApprovalDecision::Approve);
                }
                if stop && matches!(event, AgentEvent::TextDelta(_)) {
                    cancel.store(true, Ordering::SeqCst);
                }
                let done = matches!(event, AgentEvent::Finished(_));
                events.push(event);
                if done {
                    return;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
    if timeout.is_err() {
        cancel.store(true, Ordering::SeqCst);
        handle.abort();
    }
    timeout.expect("router fixture timed out");
    handle.await.unwrap();
    acp.close(&session_key);
    let replay = if mode == "tohttp" {
        let requests = server.as_ref().unwrap().received_requests().await.unwrap();
        String::from_utf8(requests.last().unwrap().body.clone()).unwrap()
    } else {
        std::fs::read_to_string(root.join("second.json")).unwrap_or_default()
    };
    let _ = std::fs::remove_dir_all(root);
    for kind in [
        LlmProviderKind::CodexAcp,
        LlmProviderKind::ClaudeCodeAcp,
        LlmProviderKind::CursorAcp,
    ] {
        crate::router::quota::clear_cooldown(kind);
    }
    (events, replay)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acp_failover_replays_progress_preserves_permissions_and_stops_safely() {
    let (events, replay) = run_case("quota", false, false, false).await;
    let routes: Vec<_> = events
        .iter()
        .filter_map(|e| {
            if let AgentEvent::Routed(note) = e {
                Some(note.provider)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        routes,
        [LlmProviderKind::CodexAcp, LlmProviderKind::ClaudeCodeAcp]
    );
    assert!(
        replay.contains("Parser updated.")
            && replay.contains("tests passed")
            && replay.contains("Original request")
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ApprovalRequest { .. }))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::TextDelta(t) if t == "Permission yes"))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolUpdate(t) if t.tool_call_id == "edit-1"))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolUpdate(t) if t.tool_call_id == "route-2:edit-1"))
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::Finished(_)))
            .count(),
        1
    );
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Finished(AgentOutcome::Success { .. }))
    ));

    let (events, _) = run_case("quota", false, true, false).await;
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::ApprovalRequest { .. }))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::TextDelta(t) if t == "Permission no"))
    );
    for (mode, exclusive) in [("quota", true), ("error", false), ("pending", false)] {
        let (events, replay) = run_case(mode, exclusive, false, false).await;
        assert!(
            replay.is_empty(),
            "must not switch for {mode}, exclusive={exclusive}"
        );
        assert!(matches!(
            events.last(),
            Some(AgentEvent::Finished(AgentOutcome::Failed { .. }))
        ));
    }
    // Image generation goes to Codex even when another agent would score higher.
    let (events, replay) = run_case("image", false, false, false).await;
    let routes: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Routed(n) => Some((n.provider, n.reason.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(routes.len(), 1, "{events:?}");
    assert_eq!(routes[0].0, LlmProviderKind::CodexAcp);
    assert!(routes[0].1.starts_with("Image generation needs Codex."));
    assert!(replay.is_empty());
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Finished(AgentOutcome::Success { .. }))
    ));
    // A chat pinned to Codex still moves to the agent named in the message, without
    // ever starting Codex (its adapter would fail with a quota error).
    let (events, replay) = run_case("pinned", false, false, false).await;
    let routes: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Routed(n) => Some(n.provider),
            _ => None,
        })
        .collect();
    assert_eq!(routes, [LlmProviderKind::ClaudeCodeAcp], "{events:?}");
    assert!(replay.contains("stirile de azi"));
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Finished(AgentOutcome::Success { .. }))
    ));
    for mode in ["http", "tohttp"] {
        crate::router::quota::clear_cooldown(LlmProviderKind::OpenAi);
        let (events, replay) = run_case(mode, false, false, false).await;
        let routes: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::Routed(n) => Some(n.provider),
                _ => None,
            })
            .collect();
        assert_eq!(routes.len(), 2, "{mode}: {events:?}");
        assert!(routes.contains(&LlmProviderKind::OpenAi));
        assert!(
            matches!(
                events.last(),
                Some(AgentEvent::Finished(AgentOutcome::Success { .. }))
            ),
            "{mode}: {events:?}"
        );
        if mode == "tohttp" {
            assert!(replay.contains("tests passed"));
        }
    }
    crate::router::quota::clear_cooldown(LlmProviderKind::OpenAi);
    let (events, _) = run_case("exhaust", false, false, false).await;
    let routes: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Routed(n) => Some(n.provider),
            _ => None,
        })
        .collect();
    assert_eq!(routes.len(), 3);
    assert_eq!(
        routes
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        3
    );
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Finished(AgentOutcome::Failed { .. }))
    ));
    let (events, replay) = run_case("wait", false, false, true).await;
    assert!(replay.is_empty());
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Finished(AgentOutcome::Cancelled))
    ));

    // An agent picked in the composer hands the turn to another ACP agent when its quota
    // runs out, keeping that agent's own model.
    let (events, replay) = run_case("direct", false, false, false).await;
    let routes: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Routed(n) => Some((**n).clone()),
            _ => None,
        })
        .collect();
    assert_eq!(routes.len(), 1, "{events:?}");
    assert_eq!(routes[0].provider, LlmProviderKind::ClaudeCodeAcp);
    assert!(routes[0].direct);
    assert!(
        routes[0]
            .failover_from
            .as_deref()
            .is_some_and(|f| f.contains("usage limit"))
    );
    assert!(replay.contains("Parser updated.") && replay.contains("Fix the parser"));
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Finished(AgentOutcome::Success { .. }))
    ));
    let (events, _) = run_case("chain", false, false, false).await;
    let final_route = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Routed(note) => Some(note),
            _ => None,
        })
        .next_back()
        .unwrap();
    assert_eq!(final_route.provider, LlmProviderKind::CursorAcp);
    assert_eq!(
        final_route.interrupted_providers,
        [LlmProviderKind::CodexAcp, LlmProviderKind::ClaudeCodeAcp]
    );
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Finished(AgentOutcome::Success { .. }))
    ));
    let (events, _) = run_case("initialcancel", false, false, true).await;
    crate::router::quota::set_snapshot_for_tests(LlmProviderKind::CodexAcp, Default::default());
    assert!(
        events.iter().any(
            |e| matches!(e, AgentEvent::TextDelta(t) if t.contains("Continuing automatically"))
        )
    );
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Finished(AgentOutcome::Cancelled))
    ));
    assert!(!events.iter().any(|e| matches!(e, AgentEvent::Routed(_))));
    // A setup failure on the replacement must not prevent waiting for the first agent.
    // Initial Router selection must also wait when no provider has quota yet.
    for mode in ["authwait", "initialwait"] {
        let (events, _) = run_case(mode, false, false, false).await;
        crate::router::quota::set_snapshot_for_tests(LlmProviderKind::CodexAcp, Default::default());
        assert!(
            events.iter().any(
                |e| matches!(e, AgentEvent::TextDelta(t) if t.contains("Continuing automatically"))
            ),
            "{mode}: {events:?}"
        );
        assert!(
            matches!(
                events.last(),
                Some(AgentEvent::Finished(AgentOutcome::Success { .. }))
            ),
            "{mode}: {events:?}"
        );
        let routes: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::Routed(note) => Some(note.provider),
                _ => None,
            })
            .collect();
        if mode == "authwait" {
            assert_eq!(
                routes,
                [LlmProviderKind::ClaudeCodeAcp, LlmProviderKind::CodexAcp]
            );
        } else {
            assert_eq!(routes, [LlmProviderKind::CodexAcp]);
        }
    }
    // With every agent out of quota the turn waits for the first reset, then continues on
    // the picked agent.
    let (events, _) = run_case("directwait", false, false, false).await;
    crate::router::quota::set_snapshot_for_tests(LlmProviderKind::CodexAcp, Default::default());
    assert!(
        events.iter().any(
            |e| matches!(e, AgentEvent::TextDelta(t) if t.contains("Every agent is out of quota"))
        ),
        "{events:?}"
    );
    let routes: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Routed(n) => Some(n.provider),
            _ => None,
        })
        .collect();
    assert_eq!(
        routes,
        [LlmProviderKind::ClaudeCodeAcp, LlmProviderKind::CodexAcp]
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::TextDelta(t) if t == "Permission yes"))
    );
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Finished(AgentOutcome::Success { .. }))
    ));
}

#[test]
fn composer_pick_falls_back_to_agents_the_chat_used_first() {
    let mut reply = user("");
    reply.role = MsgRole::Assistant;
    reply.route = Some(Box::new(RouteNote::direct(
        LlmProviderKind::CopilotAcp,
        "m",
        "",
    )));
    let order = acp_fallback_order(LlmProviderKind::CodexAcp, &[user("hi"), reply]);
    assert_eq!(
        order[..2],
        [LlmProviderKind::CodexAcp, LlmProviderKind::CopilotAcp]
    );
    assert!(order.iter().all(|k| k.is_acp()));
    assert_eq!(
        order.len(),
        LlmProviderKind::ALL.iter().filter(|k| k.is_acp()).count()
    );
}
