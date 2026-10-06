//! ACP `session/request_permission` handling: map the agent's tool call onto oxi's approval
//! policy, ask the UI when needed, and answer with the matching permission option.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver as StdReceiver, Sender as StdSender, TryRecvError};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::process::ChildStdin;
use tokio::sync::Mutex as AsyncMutex;

use super::write_line;
use crate::agent::approval::{ApprovalDecision, ApprovalPolicy};
use crate::agent::events::AgentEvent;

/// ACP tool kinds that cannot change anything, so plan mode lets their permission requests
/// through to the normal policy.
fn acp_kind_is_read_only(kind: &str) -> bool {
    matches!(kind, "read" | "search" | "think" | "fetch")
}

/// Claude Code's plan mode writes the plan to `~/.claude/plans/`; that edit is part of planning.
pub(super) fn is_plan_file_edit(tool: &Value) -> bool {
    let Some(dir) = dirs::home_dir().map(|h| h.join(".claude").join("plans")) else {
        return false;
    };
    let mut paths: Vec<&str> = tool["locations"]
        .as_array()
        .map(|l| l.iter().filter_map(|x| x["path"].as_str()).collect())
        .unwrap_or_default();
    if paths.is_empty() {
        paths.extend(tool["rawInput"]["file_path"].as_str());
    }
    tool["kind"].as_str() == Some("edit")
        && !paths.is_empty()
        && paths
            .iter()
            .all(|p| std::path::Path::new(p).starts_with(&dir))
}

/// A `session/request_permission` request forwarded from the reader task to the prompt task.
pub(super) struct PermReq {
    pub(super) id: Value,
    pub(super) params: Value,
}

/// Resolve one forwarded `session/request_permission` request: ask the UI (unless approval is
/// disabled or already auto-approved), then answer the agent with the selected option.
#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_permission(
    stdin: &Arc<AsyncMutex<ChildStdin>>,
    event_tx: StdSender<AgentEvent>,
    approval_rx: &mut StdReceiver<ApprovalDecision>,
    approval_policy: ApprovalPolicy,
    bash_allowlist: &mut Vec<String>,
    plan_mode: bool,
    cancel: &Arc<AtomicBool>,
    auto_approve: &mut bool,
    pr: PermReq,
) {
    let (name, args) = permission_name_args(&pr.params["toolCall"]);
    let options = pr.params["options"].as_array().cloned().unwrap_or_default();
    let kind = pr.params["toolCall"]["kind"].as_str().unwrap_or("");

    // oxi's own checklist tool only updates the UI.
    let decision = if super::todo_mcp::is_todo_tool(&pr.params["toolCall"])
        || plan_mode && is_plan_file_edit(&pr.params["toolCall"])
    {
        Some(ApprovalDecision::Approve)
    } else if plan_mode && !acp_kind_is_read_only(kind) {
        Some(ApprovalDecision::Deny)
    } else if *auto_approve
        || !approval_policy.requires_approval(&name)
        || (name == "bash"
            && args
                .as_ref()
                .and_then(|a| a.get("command"))
                .and_then(Value::as_str)
                .is_some_and(|c| {
                    crate::agent::approval::bash_command_allowlisted(c, bash_allowlist)
                }))
    {
        Some(ApprovalDecision::Approve)
    } else {
        let _ = event_tx.send(AgentEvent::ApprovalRequest { name, args });
        wait_decision(approval_rx, cancel).await
    };

    let outcome = match decision {
        None => json!({ "outcome": "cancelled" }),
        Some(d) => {
            let wanted: &[&str] = match &d {
                ApprovalDecision::Approve | ApprovalDecision::AllowPrefix(_) => {
                    &["allow_once", "allow"]
                }
                ApprovalDecision::ApproveRest => &["allow_always", "allow_once", "allow"],
                ApprovalDecision::Deny => &["reject_once", "reject"],
            };
            match pick_option(&options, wanted) {
                Some(option_id) => {
                    match d {
                        ApprovalDecision::ApproveRest => *auto_approve = true,
                        ApprovalDecision::AllowPrefix(prefix) => bash_allowlist.push(prefix),
                        ApprovalDecision::Approve | ApprovalDecision::Deny => {}
                    }
                    json!({ "outcome": "selected", "optionId": option_id })
                }
                None => json!({ "outcome": "cancelled" }),
            }
        }
    };
    let _ = write_line(
        stdin,
        &json!({"jsonrpc":"2.0","id": pr.id, "result": { "outcome": outcome }}),
    )
    .await;
}

/// Poll the approval back-channel until a decision arrives or the turn is cancelled.
async fn wait_decision(
    rx: &mut StdReceiver<ApprovalDecision>,
    cancel: &Arc<AtomicBool>,
) -> Option<ApprovalDecision> {
    loop {
        if cancel.load(Ordering::SeqCst) {
            return None;
        }
        match rx.try_recv() {
            Ok(d) => return Some(d),
            Err(TryRecvError::Empty) => tokio::time::sleep(Duration::from_millis(80)).await,
            Err(TryRecvError::Disconnected) => return None,
        }
    }
}

/// Choose a permission `optionId` from the offered options, preferring the given option `kind`s
/// in order, then any matching allow/reject option. Never select the opposite decision.
pub(super) fn pick_option(options: &[Value], wanted_kinds: &[&str]) -> Option<String> {
    for want in wanted_kinds {
        if let Some(id) = options
            .iter()
            .filter(|o| o.get("kind").and_then(|k| k.as_str()) == Some(*want))
            .find_map(|o| {
                o.get("optionId")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
            })
        {
            return Some(id.to_string());
        }
    }
    // Fall back to any option whose kind starts with the same allow/reject prefix.
    let prefix = if wanted_kinds.iter().any(|k| k.starts_with("allow")) {
        "allow"
    } else {
        "reject"
    };
    options
        .iter()
        .filter(|o| {
            o.get("kind")
                .and_then(|k| k.as_str())
                .is_some_and(|k| k.starts_with(prefix))
        })
        .find_map(|o| {
            o.get("optionId")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
        })
        .map(str::to_owned)
}

/// Map an ACP `toolCall` object to the (name, args) shape oxi's approval UI expects.
pub(super) fn permission_name_args(tool: &Value) -> (String, Option<Value>) {
    let kind = tool.get("kind").and_then(|k| k.as_str()).unwrap_or("");
    let title = tool.get("title").and_then(|t| t.as_str()).unwrap_or("");
    let name = match kind {
        "execute" => "bash".to_string(),
        "edit" | "delete" | "move" => "edit".to_string(),
        "" => {
            if title.is_empty() {
                "tool".to_string()
            } else {
                title.to_string()
            }
        }
        other => other.to_string(),
    };
    (name, tool.get("rawInput").cloned())
}
