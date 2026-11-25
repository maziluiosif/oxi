use super::*;
use crate::agent::approval::{ApprovalDecision, ApprovalGate, ApprovalPolicy};
use crate::agent::events::AgentEvent;
use crate::evidence::{self, TurnMarker};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;

fn environment() -> (PathBuf, ToolEnv) {
    let root = temp_workspace("evidence");
    let context = evidence::tools::Context {
        dir: root.with_extension("proof"),
        execution: Default::default(),
        policy: None,
    };
    evidence::begin_turn(
        &context.dir,
        &TurnMarker {
            baseline: None,
            turn: "turn".into(),
            started_ms: 0,
            cwd: root.to_string_lossy().into_owned(),
            repo: None,
            base_tree: None,
        },
    )
    .unwrap();
    let mut env = all_enabled();
    env.evidence = Some(context);
    (root, env)
}

#[test]
fn evidence_commands_obey_shell_approvals_even_when_the_tool_is_allowlisted() {
    let (root, env) = environment();
    let (tx, rx) = mpsc::channel();
    let (approvals, approval_rx) = mpsc::channel();
    approvals.send(ApprovalDecision::Deny).unwrap();
    let mut gate = ApprovalGate::new(ApprovalPolicy::new(false, true), approval_rx)
        .with_allowed_tools(vec!["evidence_run".into()]);
    let result = crate::agent::dispatch::run_gated_tool(
        &mut gate,
        &tx,
        &Arc::new(AtomicBool::new(false)),
        &root,
        "call",
        "evidence_run",
        &json!({"label":"Check", "command":"echo forbidden > leaked.txt"}),
        &env,
    );
    assert!(result.is_error);
    assert!(!root.join("leaked.txt").exists());
    assert!(rx.try_iter().any(
        |e| matches!(e, AgentEvent::ApprovalRequest { name, args: Some(args) }
        if name == "bash" && args["command"] == "echo forbidden > leaked.txt")
    ));
    assert!(
        evidence::collect(&env.evidence.unwrap().dir, "turn", true)
            .items
            .is_empty()
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn evidence_capture_drivers_cannot_bypass_approval_or_plan_mode() {
    let (root, env) = environment();
    let (tx, rx) = mpsc::channel();
    let (approvals, approval_rx) = mpsc::channel();
    approvals.send(ApprovalDecision::Deny).unwrap();
    let mut gate = ApprovalGate::new(ApprovalPolicy::new(false, true), approval_rx)
        .with_bash_allowlist(vec!["echo".into()]);
    let args = json!({"phase":"after", "label":"App", "screen":true,
        "command":"echo first", "during":"echo second"});
    assert!(
        gate.request(
            &tx,
            &Arc::new(AtomicBool::new(false)),
            "evidence_capture",
            &args
        )
        .is_err()
    );
    assert!(rx.try_iter().any(
        |e| matches!(e, AgentEvent::ApprovalRequest { args: Some(args), .. }
        if args["command"] == "echo first && echo second")
    ));
    let mut gate = gate.with_plan_mode(true);
    let result = crate::agent::dispatch::run_gated_tool(
        &mut gate,
        &tx,
        &Arc::new(AtomicBool::new(false)),
        &root,
        "call",
        "evidence_submit",
        &json!({"summary":"Forbidden", "claims":[]}),
        &env,
    );
    assert!(result.is_error && result.output.contains("Plan mode"));
    assert!(!evidence::collect(&env.evidence.unwrap().dir, "turn", true).submitted);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn evidence_requires_run_context_and_respects_disabled_and_risky_shell_commands() {
    let (root, mut env) = environment();
    let args = json!({"label":"Check", "command":"echo hello"});
    let i = ALL_TOOL_NAMES.iter().position(|n| *n == "bash").unwrap();
    env.enabled[i] = false;
    let disabled = run_tool(&root, "evidence_run", &args, &env);
    assert!(disabled.is_error && disabled.output.contains("disabled"));
    env.enabled[i] = true;
    let risky = run_tool(
        &root,
        "evidence_run",
        &json!({"label":"Risky", "command":"sudo echo hi"}),
        &env,
    );
    assert!(risky.is_error && risky.output.contains("Refusing risky"));
    env.evidence = None;
    assert!(run_tool(&root, "evidence_run", &args, &env).is_error);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn simultaneous_runs_keep_their_evidence_separate() {
    let workers: Vec<_> = ["First", "Second"]
        .into_iter()
        .map(|label| {
            std::thread::spawn(move || {
                let (root, env) = environment();
                let result = run_tool(
                    &root,
                    "evidence_run",
                    &json!({"label":label, "command":"echo verified"}),
                    &env,
                );
                assert!(!result.is_error, "{}", result.output);
                assert!(result.output.starts_with("e1:"));
                let submitted = run_tool(
                    &root,
                    "evidence_submit",
                    &json!({"summary":label, "claims":[
                        {"text":label, "evidence":["e1"]}
                    ]}),
                    &env,
                );
                assert!(!submitted.is_error);
                let report = evidence::collect(&env.evidence.unwrap().dir, "turn", true);
                assert_eq!(report.items.len(), 1);
                assert_eq!(report.items[0].label, label);
                assert_eq!(report.summary, label);
                assert_eq!(report.tally(), (1, 0, 0));
                let _ = fs::remove_dir_all(root);
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
}

#[test]
fn project_subprocesses_cannot_inherit_the_active_mcp_evidence_store() {
    let mut command = std::process::Command::new(crate::agent::test_python_executable());
    command
        .args([
            "-c",
            "import os; print(os.environ.get('OXI_EVIDENCE_DIR', 'isolated'))",
        ])
        .env(crate::evidence::DIR_ENV, "another-conversation");
    crate::agent::tools::sanitize_bash_env(&mut command);
    let output = command.output().unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "isolated");
}
