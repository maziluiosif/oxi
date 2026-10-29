use super::*;
use crate::agent::approval::{ApprovalGate, ApprovalPolicy};
use crate::agent::tools::ToolEnv;
use crate::settings::{ALL_TOOL_NAMES, LlmProviderKind, ProviderConfig};
use serde_json::json;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

#[derive(Clone, Copy)]
enum Protocol {
    Chat,
    Anthropic,
    Codex,
}

struct Rounds {
    protocol: Protocol,
    round: AtomicUsize,
    missing: bool,
}

impl Respond for Rounds {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let round = self.round.fetch_add(1, Ordering::SeqCst);
        let call = if self.missing {
            None
        } else {
            match round {
                1 => Some((
                    "evidence_run",
                    json!({"label":"Tests", "command":"echo verified"}),
                )),
                2 => Some((
                    "evidence_submit",
                    json!({"summary":"Verified", "claims":[
                        {"text":"The check passed", "evidence":["e1"]}
                    ]}),
                )),
                _ => None,
            }
        };
        let id = format!("call_{round}");
        response(self.protocol, &id, call)
    }
}

fn response(protocol: Protocol, id: &str, call: Option<(&str, Value)>) -> ResponseTemplate {
    let events = match protocol {
        Protocol::Chat => match call {
            Some((name, args)) => vec![json!({"choices":[{"delta":{"tool_calls":[{
                    "index":0, "id":id, "type":"function", "function":{"name":name,"arguments":args.to_string()}
                }]}, "finish_reason":"tool_calls"}]})],
            None => {
                vec![json!({"choices":[{"delta":{"content":"Done"},"finish_reason":"stop"}]})]
            }
        },
        Protocol::Anthropic => match call {
            Some((name, args)) => vec![
                json!({"type":"content_block_start", "index":0, "content_block":{"type":"tool_use", "id":id,"name":name,"input":{}}}),
                json!({"type":"content_block_delta", "index":0, "delta":{"type":"input_json_delta", "partial_json":args.to_string()}}),
                json!({"type":"message_delta", "delta":{"stop_reason":"tool_use"}}),
            ],
            None => vec![
                json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Done"}}),
                json!({"type":"message_delta","delta":{"stop_reason":"end_turn"}}),
            ],
        },
        Protocol::Codex => match call {
            Some((name, args)) => vec![
                json!({"type":"response.output_item.added", "item":{"type":"function_call","call_id":id,"name":name}}),
                json!({"type":"response.output_item.done", "item":{"type":"function_call","call_id":id,"name":name,"arguments":args.to_string()}}),
                json!({"type":"response.completed","response":{}}),
            ],
            None => vec![
                json!({"type":"response.output_text.delta","delta":"Done"}),
                json!({"type":"response.completed","response":{}}),
            ],
        },
    };
    let body: String = events
        .iter()
        .map(|event| match protocol {
            Protocol::Anthropic => format!(
                "event: {}\ndata: {event}\n\n",
                event["type"].as_str().unwrap()
            ),
            _ => format!("data: {event}\n\n"),
        })
        .collect();
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(body)
}

fn fixture() -> (std::path::PathBuf, ToolEnv) {
    let cwd = std::env::temp_dir().join(format!("oxi-native-evidence-{}", rand::random::<u64>()));
    std::fs::create_dir_all(&cwd).unwrap();
    let context = Context {
        dir: cwd.with_extension("proof"),
        execution: Default::default(),
        policy: None,
    };
    evidence::begin_turn(
        &context.dir,
        &TurnMarker {
            baseline: None,
            turn: "turn".into(),
            started_ms: 0,
            cwd: cwd.to_string_lossy().into_owned(),
            repo: None,
            base_tree: None,
        },
    )
    .unwrap();
    let env = ToolEnv {
        enabled: vec![true; ALL_TOOL_NAMES.len()],
        web_search_url: String::new(),
        web_search_backend: Default::default(),
        bash_timeout_cap_secs: 30,
        mcp: None,
        subagent: None,
        undo_journal: None,
        evidence: Some(context),
    };
    (cwd, env)
}

async fn exercise(kind: LlmProviderKind, protocol: Protocol, missing: bool, max_rounds: u32) {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(Rounds {
            protocol,
            round: AtomicUsize::new(0),
            missing,
        })
        .mount(&server)
        .await;
    let (cwd, env) = fixture();
    let mut cfg = ProviderConfig::new(kind);
    cfg.base_url = server.uri();
    cfg.api_key = "fixture-key".into();
    cfg.model_id = "test-model".into();
    let client = reqwest::Client::new();
    let (tx, rx) = mpsc::channel();
    let (_approvals, approval_rx) = mpsc::channel();
    let mut gate = ApprovalGate::new(ApprovalPolicy::disabled(), approval_rx);
    let cancel = Arc::new(AtomicBool::new(false));
    let tools = evidence::tools::native_definitions();
    let mut messages = vec![
        json!({"role":"system","content":evidence::PROMPT_PREFIX}),
        json!({"role":"user","content":"Prove this check passes"}),
    ];
    run_provider_loop(
        DispatchParams {
            cfg: &cfg,
            client: &client,
            tunnels: None,
            cwd: &cwd,
            env: &env,
            tx: &tx,
            cancel: &cancel,
            gate: &mut gate,
            max_rounds,
            effort_override: None,
            context_char_budget: usize::MAX,
            tools_chars: 0,
            steer: None,
        },
        &mut messages,
        &tools,
    )
    .await
    .unwrap();
    let events: Vec<_> = rx.try_iter().collect();
    let report = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::Evidence(r) => Some(r),
            _ => None,
        })
        .unwrap();
    let followups = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::TextDelta(s) if s.contains("missing evidence")))
        .count();
    let requests = server.received_requests().await.unwrap();
    if missing {
        let expected = if max_rounds == 0 {
            3
        } else {
            max_rounds.min(3) as usize
        };
        assert_eq!(requests.len(), expected);
        assert_eq!(followups, expected - 1);
        assert!(report.requested && !report.submitted);
    } else {
        assert_eq!(requests.len(), 4);
        assert_eq!(followups, 1);
        assert_eq!(report.tally(), (1, 0, 0));
        assert!(report.problems().is_empty());
        assert!(messages.iter().any(|m| {
            m["role"] == "tool"
                && m["content"]
                    .as_str()
                    .is_some_and(|s| s.contains("verified"))
        }));
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert!(
            body["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["name"] == "evidence_run" || t["function"]["name"] == "evidence_run")
        );
    }
    let _ = std::fs::remove_dir_all(cwd);
}

#[tokio::test]
async fn openai_evidence_requests_missing_proof_and_collects_the_report() {
    exercise(LlmProviderKind::OpenAi, Protocol::Chat, false, 10).await;
}

#[tokio::test]
async fn anthropic_evidence_uses_the_same_tools_and_followups() {
    exercise(
        LlmProviderKind::CustomAnthropic,
        Protocol::Anthropic,
        false,
        10,
    )
    .await;
}

#[tokio::test]
async fn local_provider_evidence_stops_after_two_followups() {
    exercise(LlmProviderKind::Ollama, Protocol::Chat, true, 10).await;
}

#[tokio::test]
async fn evidence_followups_respect_the_round_budget_including_unlimited() {
    for rounds in [0, 1, 2] {
        exercise(LlmProviderKind::Ollama, Protocol::Chat, true, rounds).await;
    }
}

#[tokio::test]
async fn codex_responses_executes_native_evidence_tools() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(Rounds {
            protocol: Protocol::Codex,
            round: AtomicUsize::new(1),
            missing: false,
        })
        .mount(&server)
        .await;
    let (cwd, env) = fixture();
    let client = reqwest::Client::new();
    let base = server.uri();
    let (tx, _rx) = mpsc::channel();
    let (_approvals, approval_rx) = mpsc::channel();
    let mut gate = ApprovalGate::new(ApprovalPolicy::disabled(), approval_rx);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut ctx = crate::agent::loop_ctx::LoopCtx {
        client: &client,
        base_url: &base,
        model: "test-model",
        cwd: &cwd,
        env: &env,
        tx: &tx,
        cancel: &cancel,
        gate: &mut gate,
        max_rounds: 10,
        effort_override: None,
        context_char_budget: usize::MAX,
        tools_chars: 0,
        steer: None,
    };
    let mut messages = vec![json!({"role":"user","content":"Prove the check"})];
    crate::agent::codex_responses::run_codex_responses_loop(
        &mut ctx,
        "fixture-token",
        "fixture-account",
        &mut messages,
        &evidence::tools::native_definitions(),
    )
    .await
    .unwrap();
    let report = evidence::collect(&env.evidence.unwrap().dir, "turn", true);
    assert_eq!(report.tally(), (1, 0, 0));
    assert!(report.problems().is_empty());
    let _ = std::fs::remove_dir_all(cwd);
}

#[tokio::test]
async fn native_runner_enables_evidence_only_outside_plan_mode() {
    use crate::agent::runner::{AgentExecutor, AgentRunRequest, spawn_agent_attempt};
    use crate::settings::AppSettings;
    for (enabled, plan, cancelled) in [
        (false, false, false),
        (true, false, false),
        (true, true, false),
        (true, false, true),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(Rounds {
                protocol: Protocol::Chat,
                round: AtomicUsize::new(0),
                missing: true,
            })
            .mount(&server)
            .await;
        let (cwd, _env) = fixture();
        let mut settings = AppSettings {
            active_provider: LlmProviderKind::OpenAi,
            ..Default::default()
        };
        settings.mcp_servers.clear();
        let cfg = settings.provider_mut(LlmProviderKind::OpenAi);
        cfg.base_url = server.uri();
        cfg.api_key = "fixture-key".into();
        cfg.model_id = "test-model".into();
        let (tx, rx) = mpsc::channel();
        let (_approval_tx, approval_rx) = mpsc::channel();
        let executor = AgentExecutor::new().unwrap();
        let handle = spawn_agent_attempt(
            &executor,
            AgentRunRequest {
                settings,
                tunnels: crate::compute::TunnelManager::spawn(),
                acp: crate::agent::acp::AcpManager::spawn(),
                mcp: crate::agent::mcp::McpManager::new(),
                acp_session_key: "fixture".into(),
                cwd: cwd.clone(),
                chat_for_history: vec![crate::model::ChatMessage::user("Prove the check", &[])],
                approval_rx,
                cancel: Arc::new(AtomicBool::new(cancelled)),
                wire_candidate: None,
                chars_per_token: 4.0,
                plan_mode: plan,
                evidence_mode: enabled.into(),
                undo_journal: Arc::new(std::sync::Mutex::new(Default::default())),
                steer: Default::default(),
            },
            tx,
        );
        tokio::time::timeout(Duration::from_secs(20), handle)
            .await
            .unwrap()
            .unwrap();
        let events: Vec<_> = rx.try_iter().collect();
        if cancelled {
            assert!(matches!(
                events.last(),
                Some(AgentEvent::Finished(
                    crate::agent::events::AgentOutcome::Cancelled
                ))
            ));
            assert!(server.received_requests().await.unwrap().is_empty());
            let _ = std::fs::remove_dir_all(cwd);
            continue;
        }

        assert!(matches!(
            events.last(),
            Some(AgentEvent::Finished(
                crate::agent::events::AgentOutcome::Success { .. }
            ))
        ));
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, AgentEvent::Evidence(_)))
                .count(),
            0
        );
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        let offered = body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["function"]["name"] == "evidence_run");
        assert_eq!(offered, enabled && !plan);
        assert_eq!(
            body["messages"][0]["content"]
                .as_str()
                .unwrap()
                .contains("[Evidence mode]"),
            enabled && !plan
        );
        let _ = std::fs::remove_dir_all(cwd);
    }
}

#[tokio::test]
async fn native_baseline_keeps_the_original_working_tree() {
    let (cwd, _env) = fixture();
    let repo = cwd.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git2::Repository::init(&repo).unwrap();
    std::fs::write(repo.join("file.txt"), "before").unwrap();
    let context = begin(
        &repo,
        Arc::new(AtomicBool::new(false)),
        30,
        Default::default(),
    )
    .await
    .unwrap();
    std::fs::write(repo.join("file.txt"), "after").unwrap();
    context.call("evidence_base_checkout", &json!({})).unwrap();
    assert_eq!(
        std::fs::read_to_string(context.dir.join("turn/base/file.txt")).unwrap(),
        "before"
    );
    assert_eq!(
        std::fs::read_to_string(repo.join("file.txt")).unwrap(),
        "after"
    );
    let _ = std::fs::remove_dir_all(context.dir);
    let _ = std::fs::remove_dir_all(cwd);
}

#[tokio::test]
async fn partial_evidence_survives_provider_errors_and_cancellation() {
    for cancelled in [false, true] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(400).set_body_string("fixture error"))
            .mount(&server)
            .await;
        let (cwd, env) = fixture();
        env.evidence
            .as_ref()
            .unwrap()
            .call(
                "evidence_run",
                &json!({"label":"Earlier check","command":"echo verified"}),
            )
            .unwrap();
        let mut cfg = ProviderConfig::new(LlmProviderKind::OpenAi);
        cfg.api_key = "fixture-key".into();
        cfg.model_id = "test-model".into();
        cfg.base_url = server.uri();
        let client = reqwest::Client::new();
        let (tx, rx) = mpsc::channel();
        let (_approval_tx, approval_rx) = mpsc::channel();
        let mut gate = ApprovalGate::new(ApprovalPolicy::disabled(), approval_rx);
        let cancel = Arc::new(AtomicBool::new(cancelled));
        let mut messages = vec![json!({"role":"user","content":"Finish"})];
        let result = run_provider_loop(
            DispatchParams {
                cfg: &cfg,
                client: &client,
                tunnels: None,
                cwd: &cwd,
                env: &env,
                tx: &tx,
                cancel: &cancel,
                gate: &mut gate,
                max_rounds: 10,
                effort_override: None,
                context_char_budget: usize::MAX,
                tools_chars: 0,
                steer: None,
            },
            &mut messages,
            &evidence::tools::native_definitions(),
        )
        .await;
        assert_eq!(result.is_err(), !cancelled);
        let report = rx
            .try_iter()
            .find_map(|event| match event {
                AgentEvent::Evidence(report) => Some(report),
                _ => None,
            })
            .unwrap();
        assert_eq!(report.items.len(), 1);
        assert!(report.items[0].passed());
        assert!(!report.submitted);
        let _ = std::fs::remove_dir_all(cwd);
    }
}

struct LiveRounds {
    protocol: Protocol,
    mode: evidence::Mode,
    enable: bool,
    first: AtomicBool,
    rest: Rounds,
}

impl Respond for LiveRounds {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        if !self.first.swap(true, Ordering::SeqCst) {
            return response(
                self.protocol,
                "edit",
                Some((
                    "write",
                    json!({"path":"changed.txt","content":"changed during the response"}),
                )),
            );
        }
        self.mode.set(self.enable);
        self.rest.respond(request)
    }
}

#[tokio::test]
async fn live_toggle_checks_this_response_with_tools_added_on_the_next_request() {
    for (kind, protocol) in [
        (LlmProviderKind::OpenAi, Protocol::Chat),
        (LlmProviderKind::CustomAnthropic, Protocol::Anthropic),
        (LlmProviderKind::GptCodex, Protocol::Codex),
    ] {
        for enable in [true, false] {
            let mode: evidence::Mode = (!enable).into();
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(LiveRounds {
                    protocol,
                    mode: mode.clone(),
                    enable,
                    first: AtomicBool::new(false),
                    rest: Rounds {
                        protocol,
                        round: AtomicUsize::new(0),
                        missing: !enable,
                    },
                })
                .mount(&server)
                .await;
            let (cwd, mut env) = fixture();
            let context = env.evidence.as_mut().unwrap();
            let mut marker = evidence::current_turn(&context.dir).unwrap();
            marker.baseline = Some(evidence::WorkspaceState::capture(&cwd).unwrap());
            evidence::begin_turn(&context.dir, &marker).unwrap();
            context.policy = Some(evidence::Policy {
                mode,
                ..Default::default()
            });
            let mut cfg = ProviderConfig::new(kind);
            cfg.api_key = "fixture-key".into();
            cfg.base_url = server.uri();
            cfg.model_id = "test-model".into();
            let client = reqwest::Client::new();
            let (tx, rx) = mpsc::channel();
            let (_atx, arx) = mpsc::channel();
            let mut gate = ApprovalGate::new(ApprovalPolicy::disabled(), arx);
            let cancel = Arc::new(AtomicBool::new(false));
            let mut messages = vec![json!({"role":"user","content":"Update changed.txt"})];
            let tools = crate::agent::tools::tool_definitions_json(&env.enabled, 30);
            if matches!(protocol, Protocol::Codex) {
                let base = server.uri();
                let mut ctx = crate::agent::loop_ctx::LoopCtx {
                    client: &client,
                    base_url: &base,
                    model: "test-model",
                    cwd: &cwd,
                    env: &env,
                    tx: &tx,
                    cancel: &cancel,
                    gate: &mut gate,
                    max_rounds: 10,
                    effort_override: None,
                    context_char_budget: usize::MAX,
                    tools_chars: 0,
                    steer: None,
                };
                crate::agent::codex_responses::run_codex_responses_loop(
                    &mut ctx,
                    "fixture-token",
                    "fixture-account",
                    &mut messages,
                    &tools,
                )
                .await
                .unwrap();
                let context = env.evidence.as_ref().unwrap();
                let report = context
                    .policy
                    .as_ref()
                    .unwrap()
                    .inspect(&context.dir, "turn")
                    .report;
                if report.requested || !report.is_empty() {
                    tx.send(AgentEvent::Evidence(Box::new(report))).unwrap();
                }
            } else {
                run_provider_loop(
                    DispatchParams {
                        cfg: &cfg,
                        client: &client,
                        tunnels: None,
                        cwd: &cwd,
                        env: &env,
                        tx: &tx,
                        cancel: &cancel,
                        gate: &mut gate,
                        max_rounds: 10,
                        effort_override: None,
                        context_char_budget: usize::MAX,
                        tools_chars: 0,
                        steer: None,
                    },
                    &mut messages,
                    &tools,
                )
                .await
                .unwrap();
            }
            assert_eq!(
                std::fs::read_to_string(cwd.join("changed.txt")).unwrap(),
                "changed during the response"
            );
            let requests = server.received_requests().await.unwrap();
            assert_eq!(requests.len(), if enable { 5 } else { 2 });
            let offered =
                |index: usize| {
                    let body: Value = serde_json::from_slice(&requests[index].body).unwrap();
                    body["tools"].as_array().unwrap().iter().any(|t| {
                        t["function"]["name"] == "evidence_run" || t["name"] == "evidence_run"
                    })
                };
            assert_eq!(offered(0), !enable);
            if enable {
                assert!(
                    offered(2),
                    "tools must become available after enabling during the response"
                );
                let report = rx
                    .try_iter()
                    .find_map(|e| match e {
                        AgentEvent::Evidence(r) => Some(r),
                        _ => None,
                    })
                    .unwrap();
                assert!(report.requested);
                assert_eq!(report.tally(), (1, 0, 0));
                assert!(!report.stale);
            } else {
                assert!(!rx.try_iter().any(|e| matches!(e, AgentEvent::Evidence(_))));
            }
            let _ = std::fs::remove_dir_all(cwd);
        }
    }
}
