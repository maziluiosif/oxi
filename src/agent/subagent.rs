//! Sub-agents behind the `task` tool: a fresh, read-only agent loop on the same provider and
//! model, with its own short context. The main agent delegates self-contained investigations
//! ("find where X is configured and summarize") and gets back only the final report, which keeps
//! its own context small. Several `task` calls in one turn run in parallel, like other read-only
//! tools.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

use serde_json::{Value, json};

use super::approval::{ApprovalGate, ApprovalPolicy};
use super::dispatch::{DispatchParams, run_provider_loop, streaming_client};
use super::events::{AgentEvent, TokenUsage};
use super::tools::{
    ToolEnv, ToolOutputCallback, ToolSideEffect, tool_definitions_json, tool_side_effect,
};
use crate::settings::{ALL_TOOL_NAMES, ProviderConfig};

/// Tool rounds one sub-agent may use before it must answer.
const MAX_SUBAGENT_ROUNDS: u32 = 30;

const SUBAGENT_SYSTEM_PROMPT: &str = "You are a research sub-agent of oxi, a coding agent. You were given one self-contained task by the main agent. Investigate it in the workspace with your read-only tools ({tools_list}) and reply with a concise, factual report: what you found, with file paths and line numbers, and anything you could not determine. You cannot modify files or run commands, and the user does not see your report directly, so do not ask questions; state assumptions instead.";

pub struct SubagentRunner {
    cfg: ProviderConfig,
    tunnels: crate::compute::TunnelManager,
    cwd: PathBuf,
    env: ToolEnv,
    handle: tokio::runtime::Handle,
    cancel: Arc<AtomicBool>,
    context_char_budget: usize,
    usage_tx: Option<mpsc::Sender<AgentEvent>>,
}

impl std::fmt::Debug for SubagentRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubagentRunner")
            .field("provider", &self.cfg.provider)
            .field("model", &self.cfg.model_id)
            .finish()
    }
}

impl SubagentRunner {
    /// `parent` is the main run's tool environment; the sub-agent keeps only its enabled
    /// read-only built-ins (no MCP, no nested `task`, no checklist).
    pub fn new(
        cfg: ProviderConfig,
        tunnels: crate::compute::TunnelManager,
        cwd: PathBuf,
        parent: &ToolEnv,
        handle: tokio::runtime::Handle,
        cancel: Arc<AtomicBool>,
        context_char_budget: usize,
    ) -> Self {
        let enabled = ALL_TOOL_NAMES
            .iter()
            .zip(&parent.enabled)
            .map(|(name, on)| {
                *on && tool_side_effect(name) == ToolSideEffect::ReadOnly
                    && !matches!(*name, "task" | "todo_write")
            })
            .collect();
        let env = ToolEnv {
            enabled,
            mcp: None,
            undo_journal: None,
            subagent: None,
            ..parent.clone()
        };
        Self {
            cfg,
            tunnels,
            cwd,
            env,
            handle,
            cancel,
            context_char_budget,
            usage_tx: None,
        }
    }

    /// Account for sub-agent requests in the parent run without calibrating its context.
    pub fn with_usage_sender(mut self, tx: mpsc::Sender<AgentEvent>) -> Self {
        self.usage_tx = Some(tx);
        self
    }

    /// Run one sub-agent to completion. Blocking: call from a blocking thread (read-only tools
    /// run on `spawn_blocking`), never from inside an async task. `on_progress` receives a live
    /// log of the sub-agent's tool calls so the parent's `task` pill is not silent for minutes.
    pub fn run(
        &self,
        args: &Value,
        on_progress: Option<ToolOutputCallback>,
    ) -> Result<String, String> {
        let prompt = args
            .get("prompt")
            .and_then(|p| p.as_str())
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .ok_or("missing prompt")?;
        let tools = tool_definitions_json(&self.env.enabled, self.env.bash_timeout_cap_secs);
        let tools_list = ALL_TOOL_NAMES
            .iter()
            .zip(&self.env.enabled)
            .filter(|(_, on)| **on)
            .map(|(n, _)| *n)
            .collect::<Vec<_>>()
            .join(", ");
        let tools_chars = tools.iter().map(|v| v.to_string().len()).sum();
        let mut messages = vec![
            json!({
                "role": "system",
                "content": format!(
                    "{}\n\nCurrent working directory: {}",
                    SUBAGENT_SYSTEM_PROMPT.replace("{tools_list}", &tools_list),
                    self.cwd.to_string_lossy().replace('\\', "/")
                ),
            }),
            json!({ "role": "user", "content": prompt }),
        ];
        let client = streaming_client(&self.cfg, 180)?;
        let (tx, rx) = mpsc::channel();
        let (_approval_tx, approval_rx) = mpsc::channel();
        // Only read-only tools are offered, which never need approval.
        let mut gate = ApprovalGate::new(ApprovalPolicy::disabled(), approval_rx);
        let effort = self.cfg.effort.trim();
        // Drain events while the loop runs so progress reaches the UI as it happens.
        let collector = std::thread::spawn(move || collect_report(rx.into_iter(), on_progress));
        let result = self.handle.block_on(run_provider_loop(
            DispatchParams {
                cfg: &self.cfg,
                client: &client,
                tunnels: Some(&self.tunnels),
                cwd: &self.cwd,
                env: &self.env,
                tx: &tx,
                cancel: &self.cancel,
                gate: &mut gate,
                max_rounds: MAX_SUBAGENT_ROUNDS,
                effort_override: (!effort.is_empty()).then_some(effort),
                context_char_budget: self.context_char_budget,
                tools_chars,
            },
            &mut messages,
            &tools,
        ));
        drop(tx);
        let (answer, tool_counts, usage) = collector
            .join()
            .map_err(|_| "Sub-agent report collector panicked.".to_string())?;
        // Failed/cancelled investigations still consumed the usage reported before stopping.
        if !usage.is_zero()
            && let Some(tx) = &self.usage_tx
        {
            let _ = tx.send(AgentEvent::SubagentUsage(usage));
        }
        if self.cancel.load(Ordering::SeqCst) {
            return Err("Cancelled.".into());
        }
        let footer = tool_footer(&tool_counts);
        match result {
            Ok(()) if answer.trim().is_empty() => {
                Err(format!("The sub-agent finished without a report.{footer}"))
            }
            Ok(()) => Ok(format!("{}{footer}", answer.trim())),
            Err(e) if answer.trim().is_empty() => Err(format!("Sub-agent failed: {e}{footer}")),
            Err(e) => Err(format!(
                "Sub-agent stopped early ({e}). Partial report:\n{}{footer}",
                answer.trim()
            )),
        }
    }
}

/// The last round's text (earlier rounds are narration before tool calls) and a count of the
/// tools the sub-agent used, plus usage across every round. Each tool call is also published to
/// `on_progress` as a cumulative log, one line per call.
fn collect_report(
    events: impl Iterator<Item = AgentEvent>,
    on_progress: Option<ToolOutputCallback>,
) -> (String, BTreeMap<String, usize>, TokenUsage) {
    let mut answer = String::new();
    let mut tools = BTreeMap::new();
    let mut usage = TokenUsage::default();
    // Loops announce a call early (name + id) and again once its arguments are complete, so a
    // call's log line is refined in place rather than appended twice.
    let mut lines: Vec<(String, String)> = Vec::new();
    for event in events {
        match event {
            AgentEvent::TextStart => answer.clear(),
            AgentEvent::TextDelta(delta) => answer.push_str(&delta),
            AgentEvent::Usage(round) => usage.add(&round),
            AgentEvent::ToolStart {
                name,
                tool_call_id,
                args,
            } => {
                let line = progress_line(&name, args.as_ref());
                match lines.iter_mut().find(|(id, _)| *id == tool_call_id) {
                    Some((_, existing)) if *existing == line => continue,
                    Some((_, existing)) => *existing = line,
                    None => {
                        *tools.entry(name).or_insert(0) += 1;
                        lines.push((tool_call_id, line));
                    }
                }
                if let Some(cb) = &on_progress {
                    let log: Vec<&str> = lines.iter().map(|(_, l)| l.as_str()).collect();
                    cb(log.join("\n"));
                }
            }
            _ => {}
        }
    }
    (answer, tools, usage)
}

/// `grep TODO`, `read src/a.rs`: the tool and its main argument, on one short line.
fn progress_line(name: &str, args: Option<&Value>) -> String {
    let target = args
        .and_then(|a| {
            ["path", "pattern", "query", "url", "command"]
                .iter()
                .find_map(|k| a.get(*k).and_then(Value::as_str))
        })
        .map(|t| t.split_whitespace().collect::<Vec<_>>().join(" "))
        .unwrap_or_default();
    let target: String = if target.chars().count() > 80 {
        target.chars().take(79).chain(['…']).collect()
    } else {
        target
    };
    format!("{name} {target}").trim_end().to_string()
}

fn tool_footer(tools: &BTreeMap<String, usize>) -> String {
    if tools.is_empty() {
        return String::new();
    }
    let total: usize = tools.values().sum();
    let parts: Vec<String> = tools.iter().map(|(n, c)| format!("{n}×{c}")).collect();
    format!(
        "\n\n[sub-agent used {total} tool call(s): {}]",
        parts.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_keeps_only_the_final_round_text() {
        let events = vec![
            AgentEvent::TextStart,
            AgentEvent::TextDelta("Let me look.".into()),
            AgentEvent::ToolStart {
                name: "grep".into(),
                tool_call_id: "1".into(),
                args: None,
            },
            AgentEvent::ToolStart {
                name: "read".into(),
                tool_call_id: "2".into(),
                args: None,
            },
            AgentEvent::ToolStart {
                name: "read".into(),
                tool_call_id: "3".into(),
                args: None,
            },
            AgentEvent::TextStart,
            AgentEvent::TextDelta("Found it in ".into()),
            AgentEvent::TextDelta("src/a.rs:3.".into()),
        ];
        let (answer, tools, _) = collect_report(events.into_iter(), None);
        assert_eq!(answer, "Found it in src/a.rs:3.");
        assert_eq!(
            tool_footer(&tools),
            "\n\n[sub-agent used 3 tool call(s): grep×1, read×2]"
        );
    }

    #[test]
    fn report_accumulates_usage_across_rounds() {
        let events = vec![
            AgentEvent::TextStart,
            AgentEvent::Usage(TokenUsage {
                input_tokens: 20,
                output_tokens: 10,
                cache_read_input_tokens: 30,
                cache_creation_input_tokens: 40,
                timed_output_tokens: 10,
                generation_ms: 200,
            }),
            AgentEvent::TextStart,
            AgentEvent::TextDelta("Final report.".into()),
            AgentEvent::Usage(TokenUsage {
                input_tokens: 50,
                output_tokens: 15,
                ..Default::default()
            }),
        ];
        let (answer, _, usage) = collect_report(events.into_iter(), None);
        assert_eq!(answer, "Final report.");
        assert_eq!(usage.total_input(), 140);
        assert_eq!(usage.output_tokens, 25);
        assert_eq!(usage.cache_read_input_tokens, 30);
        assert_eq!(usage.cache_creation_input_tokens, 40);
        assert_eq!(usage.timed_output_tokens, 10);
        assert_eq!(usage.generation_ms, 200);
    }

    #[test]
    fn subagent_env_keeps_only_read_only_builtins() {
        let parent = ToolEnv {
            enabled: vec![true; ALL_TOOL_NAMES.len()],
            web_search_url: String::new(),
            web_search_backend: Default::default(),
            bash_timeout_cap_secs: 60,
            mcp: None,
            undo_journal: None,
            subagent: None,
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let runner = SubagentRunner::new(
            ProviderConfig::default(),
            crate::compute::TunnelManager::spawn(),
            PathBuf::from("."),
            &parent,
            rt.handle().clone(),
            Arc::new(AtomicBool::new(false)),
            100_000,
        );
        let on: Vec<&str> = ALL_TOOL_NAMES
            .iter()
            .zip(&runner.env.enabled)
            .filter(|(_, on)| **on)
            .map(|(n, _)| *n)
            .collect();
        assert!(on.contains(&"read") && on.contains(&"grep"));
        for denied in [
            "write",
            "edit",
            "bash",
            "delete",
            "task",
            "todo_write",
            "diagnostics",
        ] {
            assert!(!on.contains(&denied), "{denied} must be off");
        }
    }

    /// A scripted OpenAI-compatible server: round 1 asks to `read` notes.txt, round 2 reports.
    /// The sub-agent must run the read itself and return only the final report.
    struct Scripted(std::sync::atomic::AtomicUsize);

    impl wiremock::Respond for Scripted {
        fn respond(&self, req: &wiremock::Request) -> wiremock::ResponseTemplate {
            let n = self.0.fetch_add(1, Ordering::SeqCst);
            let body: Value = serde_json::from_slice(&req.body).unwrap();
            let chunks = if n == 0 {
                // Only read-only tools may be offered to a sub-agent.
                let names: Vec<&str> = body["tools"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(|t| t["function"]["name"].as_str())
                    .collect();
                assert!(names.contains(&"read"));
                assert!(!names.contains(&"write") && !names.contains(&"task"));
                vec![
                    json!({"choices": [{"index": 0, "delta": {"content": "Checking."}}]}),
                    json!({"choices": [{"index": 0, "delta": {"tool_calls": [
                        {"index": 0, "id": "c1", "type": "function",
                         "function": {"name": "read", "arguments": "{\"path\":\"notes.txt\"}"}}
                    ]}}]}),
                    json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]}),
                ]
            } else {
                let tool_result = body["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|m| m["role"] == "tool")
                    .and_then(|m| m["content"].as_str())
                    .unwrap_or_default()
                    .to_string();
                assert!(tool_result.contains("secret sauce"), "{tool_result}");
                vec![
                    json!({"choices": [{"index": 0, "delta": {"content": "Report: notes mention the secret sauce."}}]}),
                    json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]}),
                ]
            };
            let mut sse = String::new();
            for c in chunks {
                sse.push_str(&format!("data: {c}\n\n"));
            }
            let usage = json!({"choices":[], "usage":{
                "prompt_tokens":20 * (n + 1), "completion_tokens":10 * (n + 1)
            }});
            sse.push_str(&format!("data: {usage}\n\n"));
            sse.push_str("data: [DONE]\n\n");
            wiremock::ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse)
        }
    }

    #[test]
    fn subagent_runs_its_own_loop_and_returns_the_report() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let server = rt.block_on(async {
            let server = wiremock::MockServer::start().await;
            wiremock::Mock::given(wiremock::matchers::method("POST"))
                .respond_with(Scripted(std::sync::atomic::AtomicUsize::new(0)))
                .mount(&server)
                .await;
            server
        });
        let cwd = std::env::temp_dir().join(format!("oxi-subagent-{}", std::process::id()));
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::write(cwd.join("notes.txt"), "the secret sauce is here\n").unwrap();
        let mut cfg = ProviderConfig::new(crate::settings::LlmProviderKind::OpenAi);
        cfg.base_url = server.uri();
        cfg.api_key = "test".into();
        let parent = ToolEnv {
            enabled: vec![true; ALL_TOOL_NAMES.len()],
            web_search_url: String::new(),
            web_search_backend: Default::default(),
            bash_timeout_cap_secs: 60,
            mcp: None,
            undo_journal: None,
            subagent: None,
        };
        let (usage_tx, usage_rx) = mpsc::channel();
        let runner = SubagentRunner::new(
            cfg,
            crate::compute::TunnelManager::spawn(),
            cwd,
            &parent,
            rt.handle().clone(),
            Arc::new(AtomicBool::new(false)),
            100_000,
        )
        .with_usage_sender(usage_tx);
        let progress = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let sink = progress.clone();
        let report = runner
            .run(
                &json!({"description": "read notes", "prompt": "What do the notes say?"}),
                Some(Arc::new(move |text| sink.lock().unwrap().push(text))),
            )
            .unwrap();
        assert_eq!(progress.lock().unwrap().last().unwrap(), "read notes.txt");
        assert!(
            report.starts_with("Report: notes mention the secret sauce."),
            "{report}"
        );
        assert!(
            report.ends_with("[sub-agent used 1 tool call(s): read×1]"),
            "{report}"
        );
        let AgentEvent::SubagentUsage(usage) = usage_rx.try_recv().unwrap() else {
            panic!("expected delegated usage")
        };
        assert_eq!(usage.total_input(), 60);
        assert_eq!(usage.output_tokens, 30);
        assert!(usage_rx.try_recv().is_err(), "usage must be counted once");
    }
}
