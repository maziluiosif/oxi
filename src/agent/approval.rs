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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalDecision {
    /// Run this tool call.
    Approve,
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
        match name {
            "bash" => self.bash,
            "write" | "edit" | "delete" | "move" | "mkdir" => self.write_edit,
            // MCP servers are third-party processes and their tools may mutate files, external
            // systems, or credentials. Unknown capabilities must fail closed rather than being
            // treated like built-in read-only tools.
            name if crate::agent::mcp::McpManager::is_mcp_tool(name) => true,
            _ => false,
        }
    }
}

/// Mediates user approval for mutating tool calls within a single agent run.
pub struct ApprovalGate {
    policy: ApprovalPolicy,
    auto_approve: bool,
    rx: Receiver<ApprovalDecision>,
}

impl ApprovalGate {
    pub fn new(policy: ApprovalPolicy, rx: Receiver<ApprovalDecision>) -> Self {
        Self {
            policy,
            auto_approve: false,
            rx,
        }
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
        if self.auto_approve || !self.policy.requires_approval(name) {
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
