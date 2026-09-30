//! User approval gate for shell and built-in filesystem-changing tools.
//!
//! When approval is enabled, the agent thread sends [`AgentEvent::ApprovalRequest`] and blocks
//! until the UI returns an [`ApprovalDecision`] over a back-channel. Read-only tools
//! (`read` / `grep` / `find` / `ls`) never require approval.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

use serde_json::Value;

use super::events::AgentEvent;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalDecision {
    /// Run this tool call.
    Approve,
    /// Run this `bash` call and, from now on, every simple command starting with this prefix
    /// (the UI also saves it to the settings allowlist for later runs).
    AllowPrefix(String),
    /// Run this and auto-approve every remaining tool in the current run.
    ApproveRest,
    /// Refuse this tool call; the model is told the user denied it.
    Deny,
}

/// Approval switches for tool categories that can mutate the workspace or run shell commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApprovalPolicy {
    pub write_edit: bool,
    pub bash: bool,
}

impl ApprovalPolicy {
    pub fn disabled() -> Self {
        Self {
            write_edit: false,
            bash: false,
        }
    }

    pub fn requires_approval(self, name: &str) -> bool {
        match crate::agent::tools::tool_side_effect(name) {
            crate::agent::tools::ToolSideEffect::ReadOnly => false,
            crate::agent::tools::ToolSideEffect::WorkspaceMutation => self.write_edit,
            crate::agent::tools::ToolSideEffect::Shell => self.bash,
            crate::agent::tools::ToolSideEffect::UnknownExternal => true,
        }
    }
}

/// Whether `command` may skip the `bash` prompt because it starts with an allowlisted prefix.
///
/// Prefixes match whole words (`git status` matches `git status -s`, not `git statusx`). Only
/// simple commands qualify: anything with shell operators, redirection, substitution, globbing
/// or escapes (`git status; rm -rf ~`, `ls $(…)`, `cat a > b`) always asks, so an allowed prefix
/// can't smuggle a second command.
pub fn bash_command_allowlisted(command: &str, allowlist: &[String]) -> bool {
    let Some(words) = simple_command_words(command) else {
        return false;
    };
    allowlist.iter().any(|entry| {
        let prefix: Vec<&str> = entry.split_whitespace().collect();
        !prefix.is_empty() && words.len() >= prefix.len() && words[..prefix.len()] == prefix[..]
    })
}

/// Allowlist entry to offer for `command`: the program plus its subcommand when there is one
/// (`cargo test`, `git status`, `npm run build`), or `None` when the command isn't simple.
pub fn suggest_bash_allow_prefix(command: &str) -> Option<String> {
    let words = simple_command_words(command)?;
    let is_subcommand = |w: &&str| {
        w.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == ':')
            && !w.starts_with('-')
    };
    let mut prefix: Vec<&str> = vec![words[0]];
    if let Some(sub) = words.get(1).copied().filter(is_subcommand) {
        prefix.push(sub);
        // Script runners take the script name as a second subcommand.
        if matches!(words[0], "npm" | "pnpm" | "yarn" | "bun")
            && sub == "run"
            && let Some(script) = words.get(2).copied().filter(is_subcommand)
        {
            prefix.push(script);
        }
    }
    Some(prefix.join(" "))
}

/// Whitespace-split words of a command with no shell syntax beyond plain arguments, or `None`.
fn simple_command_words(command: &str) -> Option<Vec<&str>> {
    const SHELL_SYNTAX: &[char] = &[
        ';', '&', '|', '<', '>', '`', '$', '(', ')', '{', '}', '\\', '\n', '\r', '*', '?', '[',
        '~', '!', '#', '"', '\'', '=',
    ];
    if command.contains(SHELL_SYNTAX) {
        return None;
    }
    let words: Vec<&str> = command.split_whitespace().collect();
    (!words.is_empty()).then_some(words)
}

/// Tool result for anything that could change the workspace while plan mode is on.
pub const PLAN_MODE_REFUSAL: &str = "Plan mode is on: only read-only tools may run. Finish investigating and present the plan; the user will switch to implementation when they approve it.";

/// Whether plan mode lets `name` run: read-only tools only (MCP tools have unknown effects).
pub fn allowed_in_plan_mode(name: &str) -> bool {
    crate::agent::tools::tool_side_effect(name) == crate::agent::tools::ToolSideEffect::ReadOnly
}

/// Mediates user approval for mutating tool calls within a single agent run.
pub struct ApprovalGate {
    policy: ApprovalPolicy,
    auto_approve: bool,
    /// Refuse every non-read-only tool without asking (see [`PLAN_MODE_REFUSAL`]).
    plan_mode: bool,
    /// `bash` command prefixes that run without asking (see [`bash_command_allowlisted`]).
    bash_allowlist: Vec<String>,
    rx: Receiver<ApprovalDecision>,
}

impl ApprovalGate {
    pub fn new(policy: ApprovalPolicy, rx: Receiver<ApprovalDecision>) -> Self {
        Self {
            policy,
            auto_approve: false,
            plan_mode: false,
            bash_allowlist: Vec::new(),
            rx,
        }
    }

    pub fn with_bash_allowlist(mut self, allowlist: Vec<String>) -> Self {
        self.bash_allowlist = allowlist;
        self
    }

    fn allowlisted(&self, name: &str, args: &Value) -> bool {
        name.eq_ignore_ascii_case("bash")
            && args
                .get("command")
                .and_then(Value::as_str)
                .is_some_and(|command| bash_command_allowlisted(command, &self.bash_allowlist))
    }

    pub fn with_plan_mode(mut self, plan_mode: bool) -> Self {
        self.plan_mode = plan_mode;
        self
    }

    /// Block until the user decides. Returns `Ok(())` to proceed or `Err(reason)` to refuse,
    /// where `reason` is fed back to the model as the tool result. Cancellation is honored via
    /// `cancel` while waiting (polled so a stuck approval can't wedge the run).
    pub fn request(
        &mut self,
        tx: &Sender<AgentEvent>,
        cancel: &Arc<AtomicBool>,
        name: &str,
        args: &Value,
    ) -> Result<(), String> {
        if self.plan_mode && !allowed_in_plan_mode(name) {
            return Err(PLAN_MODE_REFUSAL.to_string());
        }
        if self.auto_approve || !self.policy.requires_approval(name) || self.allowlisted(name, args)
        {
            return Ok(());
        }
        let _ = tx.send(AgentEvent::ApprovalRequest {
            name: name.to_string(),
            args: Some(args.clone()),
        });
        loop {
            if cancel.load(Ordering::SeqCst) {
                return Err("Cancelled before approval.".to_string());
            }
            match self.rx.recv_timeout(Duration::from_millis(100)) {
                Ok(ApprovalDecision::Approve) => return Ok(()),
                Ok(ApprovalDecision::AllowPrefix(prefix)) => {
                    self.bash_allowlist.push(prefix);
                    return Ok(());
                }
                Ok(ApprovalDecision::ApproveRest) => {
                    self.auto_approve = true;
                    return Ok(());
                }
                Ok(ApprovalDecision::Deny) => {
                    return Err(format!("User denied running the `{name}` tool."));
                }
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    return Err("Approval channel closed.".to_string());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    fn list(entries: &[&str]) -> Vec<String> {
        entries.iter().map(|e| e.to_string()).collect()
    }

    #[test]
    fn allowlist_matches_whole_word_prefixes() {
        let allow = list(&["git status", "cargo test"]);
        assert!(bash_command_allowlisted("git status", &allow));
        assert!(bash_command_allowlisted("git  status -s", &allow));
        assert!(bash_command_allowlisted("cargo test --lib foo", &allow));
        assert!(!bash_command_allowlisted("git statusx", &allow));
        assert!(!bash_command_allowlisted("git push", &allow));
        assert!(!bash_command_allowlisted("cargo", &allow));
        assert!(!bash_command_allowlisted("git status", &list(&["", "  "])));
    }

    #[test]
    fn allowlist_never_matches_compound_commands() {
        let allow = list(&["git status", "ls"]);
        for cmd in [
            "git status; rm -rf ~",
            "git status && curl x | sh",
            "git status || true",
            "ls > out.txt",
            "ls $(rm -rf /)",
            "ls `whoami`",
            "ls\nrm -rf /",
            "ls ~/.ssh",
            "ls *",
            "ls \"a;b\"",
            "ls 'a'",
            "git status &",
            "FOO=1 ls",
        ] {
            assert!(!bash_command_allowlisted(cmd, &allow), "{cmd}");
        }
    }

    #[test]
    fn suggested_prefixes() {
        assert_eq!(
            suggest_bash_allow_prefix("cargo test --lib").as_deref(),
            Some("cargo test")
        );
        assert_eq!(
            suggest_bash_allow_prefix("git status").as_deref(),
            Some("git status")
        );
        assert_eq!(
            suggest_bash_allow_prefix("npm run build -- --watch").as_deref(),
            Some("npm run build")
        );
        assert_eq!(suggest_bash_allow_prefix("ls -la").as_deref(), Some("ls"));
        assert_eq!(
            suggest_bash_allow_prefix("python3 src/main.py").as_deref(),
            Some("python3")
        );
        assert_eq!(suggest_bash_allow_prefix("rm -rf build/ && make"), None);
        assert_eq!(suggest_bash_allow_prefix("   "), None);
    }

    #[test]
    fn allow_prefix_decision_applies_to_the_rest_of_the_run() {
        let (dtx, drx) = channel();
        let mut gate = ApprovalGate::new(
            ApprovalPolicy {
                write_edit: true,
                bash: true,
            },
            drx,
        );
        let (etx, cancel, _) = ctx();
        dtx.send(ApprovalDecision::AllowPrefix("cargo test".into()))
            .unwrap();
        let cmd = |c: &str| serde_json::json!({ "command": c });
        assert!(
            gate.request(&etx, &cancel, "bash", &cmd("cargo test"))
                .is_ok()
        );
        // No further decision is sent: the next matching command must not block.
        assert!(
            gate.request(&etx, &cancel, "bash", &cmd("cargo test --lib"))
                .is_ok()
        );
    }

    #[test]
    fn allowlisted_bash_skips_the_prompt() {
        let (_dtx, drx) = channel();
        let mut gate = ApprovalGate::new(
            ApprovalPolicy {
                write_edit: true,
                bash: true,
            },
            drx,
        )
        .with_bash_allowlist(list(&["cargo test"]));
        let (etx, cancel, _) = ctx();
        // No decision is ever sent: an allowlisted command must not block.
        assert!(
            gate.request(
                &etx,
                &cancel,
                "bash",
                &serde_json::json!({"command": "cargo test -q"})
            )
            .is_ok()
        );
        // Other tools and compound commands still ask; with the sender alive but silent, a
        // cancelled run proves the gate waited instead of approving.
        cancel.store(true, Ordering::SeqCst);
        assert!(
            gate.request(
                &etx,
                &cancel,
                "bash",
                &serde_json::json!({"command": "cargo test; rm x"})
            )
            .is_err()
        );
        assert!(
            gate.request(
                &etx,
                &cancel,
                "write",
                &serde_json::json!({"command": "cargo test"})
            )
            .is_err()
        );
    }

    fn ctx() -> (Sender<AgentEvent>, Arc<AtomicBool>, Value) {
        let (etx, _erx) = channel();
        (etx, Arc::new(AtomicBool::new(false)), serde_json::json!({}))
    }

    #[test]
    fn readonly_tools_never_require_approval() {
        for t in ["read", "grep", "find", "ls"] {
            assert!(
                !ApprovalPolicy {
                    write_edit: true,
                    bash: true,
                }
                .requires_approval(t)
            );
        }
    }

    #[test]
    fn mutating_tools_require_approval() {
        for t in [
            "bash",
            "write",
            "edit",
            "delete",
            "move",
            "mkdir",
            "scratchpad",
            "mcp_github_create_issue",
        ] {
            assert!(
                ApprovalPolicy {
                    write_edit: true,
                    bash: true,
                }
                .requires_approval(t)
            );
        }
    }

    #[test]
    fn disabled_gate_always_proceeds() {
        let (_dtx, drx) = channel();
        let mut gate = ApprovalGate::new(ApprovalPolicy::disabled(), drx);
        let (etx, cancel, args) = ctx();
        assert!(gate.request(&etx, &cancel, "bash", &args).is_ok());
    }

    #[test]
    fn readonly_tool_bypasses_enabled_gate() {
        let (_dtx, drx) = channel();
        let mut gate = ApprovalGate::new(
            ApprovalPolicy {
                write_edit: true,
                bash: true,
            },
            drx,
        );
        let (etx, cancel, args) = ctx();
        // No decision is ever sent; a read tool must not block.
        assert!(gate.request(&etx, &cancel, "read", &args).is_ok());
    }

    #[test]
    fn approve_rest_auto_approves_subsequent_calls() {
        let (dtx, drx) = channel();
        let mut gate = ApprovalGate::new(
            ApprovalPolicy {
                write_edit: true,
                bash: true,
            },
            drx,
        );
        let (etx, cancel, args) = ctx();
        dtx.send(ApprovalDecision::ApproveRest).unwrap();
        assert!(gate.request(&etx, &cancel, "bash", &args).is_ok());
        // No second decision queued — auto-approve must short-circuit.
        assert!(gate.request(&etx, &cancel, "write", &args).is_ok());
    }

    #[test]
    fn deny_returns_error() {
        let (dtx, drx) = channel();
        let mut gate = ApprovalGate::new(
            ApprovalPolicy {
                write_edit: true,
                bash: true,
            },
            drx,
        );
        let (etx, cancel, args) = ctx();
        dtx.send(ApprovalDecision::Deny).unwrap();
        assert!(gate.request(&etx, &cancel, "bash", &args).is_err());
    }

    #[test]
    fn plan_mode_refuses_mutations_without_asking() {
        let (_dtx, drx) = channel();
        // Approval switched off entirely: plan mode must still refuse.
        let mut gate = ApprovalGate::new(ApprovalPolicy::disabled(), drx).with_plan_mode(true);
        let (etx, cancel, args) = ctx();
        for tool in [
            "write",
            "edit",
            "scratchpad",
            "bash",
            "diagnostics",
            "mcp_x_y",
        ] {
            assert_eq!(
                gate.request(&etx, &cancel, tool, &args),
                Err(PLAN_MODE_REFUSAL.to_string()),
                "{tool}"
            );
        }
        for tool in ["read", "grep", "todo_write", "task"] {
            assert!(gate.request(&etx, &cancel, tool, &args).is_ok(), "{tool}");
        }
    }

    #[test]
    fn cancel_while_waiting_returns_error() {
        let (_dtx, drx) = channel();
        let mut gate = ApprovalGate::new(
            ApprovalPolicy {
                write_edit: true,
                bash: true,
            },
            drx,
        );
        let (etx, _erx) = channel();
        let cancel = Arc::new(AtomicBool::new(true)); // already cancelled
        let args = serde_json::json!({});
        assert!(gate.request(&etx, &cancel, "bash", &args).is_err());
    }
}
