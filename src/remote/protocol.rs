//! What the app and a remote oxi server send each other on top of the headless protocol
//! (`src/headless`): agent events, chat transcripts and approval decisions as JSON.
//!
//! Both ends run the same oxi build (the hello checks [`FEATURE`]), so the shapes only need
//! to round-trip between this function pair and the next, not to be stable over versions.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::agent::{AgentEvent, AgentOutcome, ApprovalDecision};
use crate::model::ChatMessage;

/// Announced by servers that speak this module's additions; the app checks for it.
pub const FEATURE: &str = "remote-app-1";

/// An agent event as sent to the app. `None` for events that only make sense in the process
/// running the agent (a command PTY).
pub fn event_to_json(event: &AgentEvent) -> Option<Value> {
    Some(match event {
        AgentEvent::Delegated { root, params } => {
            json!({"type": "delegated", "root": root, "params": params})
        }
        AgentEvent::AgentStart => json!({"type": "agent_start"}),
        AgentEvent::AcpTerminal(_) => return None,
        AgentEvent::TextStart => json!({"type": "text_start"}),
        AgentEvent::TextDelta(d) => json!({"type": "text_delta", "d": d}),
        AgentEvent::ThinkingDelta(d) => json!({"type": "thinking_delta", "d": d}),
        AgentEvent::ToolUpdate(update) => json!({"type": "tool_update", "update": update}),
        AgentEvent::ToolStart {
            name,
            tool_call_id,
            args,
        } => json!({"type": "tool_start", "name": name, "id": tool_call_id, "args": args}),
        AgentEvent::ApprovalResolved => json!({"type":"approval_resolved"}),
        AgentEvent::ApprovalRequest { name, args } => {
            json!({"type": "approval_request", "name": name, "args": args})
        }
        AgentEvent::ToolOutput {
            tool_call_id,
            text,
            truncated,
        } => {
            json!({"type": "tool_output", "id": tool_call_id, "text": text, "truncated": truncated})
        }
        AgentEvent::ToolEnd {
            tool_call_id,
            is_error,
            full_output_path,
            diff,
        } => json!({
            "type": "tool_end",
            "id": tool_call_id,
            "is_error": is_error,
            "full_output_path": full_output_path,
            "diff": diff,
        }),
        AgentEvent::StreamRetry { attempt, reason } => {
            json!({"type": "stream_retry", "attempt": attempt, "reason": reason})
        }
        AgentEvent::AssistantMessageDone => json!({"type": "assistant_message_done"}),
        AgentEvent::Usage(u) => json!({"type": "usage", "usage": u}),
        AgentEvent::SubagentUsage(u) => json!({"type": "subagent_usage", "usage": u}),
        AgentEvent::ExternalUsage(u) => json!({"type": "external_usage", "usage": u}),
        AgentEvent::ContextUsage { used, size } => {
            json!({"type": "context_usage", "used": used, "size": size})
        }
        AgentEvent::Routed(note) => json!({"type": "routed", "note": note}),
        AgentEvent::TurnChanges(changes) => json!({"type": "turn_changes", "changes": changes}),
        AgentEvent::Evidence(report) => json!({"type": "evidence", "report": report}),
        AgentEvent::Finished(outcome) => match outcome {
            AgentOutcome::Success { .. } => json!({"type": "finished", "outcome": "success"}),
            AgentOutcome::Failed { error } => {
                json!({"type": "finished", "outcome": "failed", "error": error})
            }
            AgentOutcome::Cancelled => json!({"type": "finished", "outcome": "cancelled"}),
        },
    })
}

/// The event [`event_to_json`] sent. The wire cache of a finished turn stays on the server.
pub fn event_from_json(v: &Value) -> Option<AgentEvent> {
    fn s(v: &Value, key: &str) -> String {
        v[key].as_str().unwrap_or_default().to_string()
    }
    fn opt_s(v: &Value, key: &str) -> Option<String> {
        v[key].as_str().map(str::to_string)
    }
    fn args(v: &Value) -> Option<Value> {
        (!v["args"].is_null()).then(|| v["args"].clone())
    }
    fn de<T: serde::de::DeserializeOwned>(v: &Value, key: &str) -> Option<T> {
        serde_json::from_value(v[key].clone()).ok()
    }
    Some(match v["type"].as_str()? {
        "delegated" => AgentEvent::Delegated {
            root: s(v, "root"),
            params: v["params"].clone(),
        },
        "agent_start" => AgentEvent::AgentStart,
        "text_start" => AgentEvent::TextStart,
        "text_delta" => AgentEvent::TextDelta(s(v, "d")),
        "thinking_delta" => AgentEvent::ThinkingDelta(s(v, "d")),
        "tool_update" => AgentEvent::ToolUpdate(Box::new(de(v, "update")?)),
        "tool_start" => AgentEvent::ToolStart {
            name: s(v, "name"),
            tool_call_id: s(v, "id"),
            args: args(v),
        },
        "approval_resolved" => AgentEvent::ApprovalResolved,
        "approval_request" => AgentEvent::ApprovalRequest {
            name: s(v, "name"),
            args: args(v),
        },
        "tool_output" => AgentEvent::ToolOutput {
            tool_call_id: s(v, "id"),
            text: s(v, "text"),
            truncated: v["truncated"].as_bool().unwrap_or(false),
        },
        "tool_end" => AgentEvent::ToolEnd {
            tool_call_id: s(v, "id"),
            is_error: v["is_error"].as_bool(),
            full_output_path: opt_s(v, "full_output_path"),
            diff: opt_s(v, "diff"),
        },
        "stream_retry" => AgentEvent::StreamRetry {
            attempt: v["attempt"].as_u64().unwrap_or(0) as u32,
            reason: s(v, "reason"),
        },
        "assistant_message_done" => AgentEvent::AssistantMessageDone,
        "usage" => AgentEvent::Usage(de(v, "usage")?),
        "subagent_usage" => AgentEvent::SubagentUsage(de(v, "usage")?),
        "external_usage" => AgentEvent::ExternalUsage(de(v, "usage")?),
        "context_usage" => AgentEvent::ContextUsage {
            used: v["used"].as_u64().unwrap_or(0),
            size: v["size"].as_u64().unwrap_or(0),
        },
        "routed" => AgentEvent::Routed(Box::new(de(v, "note")?)),
        "turn_changes" => AgentEvent::TurnChanges(Box::new(de(v, "changes")?)),
        "evidence" => AgentEvent::Evidence(Box::new(de(v, "report")?)),
        "finished" => AgentEvent::Finished(match v["outcome"].as_str()? {
            "success" => AgentOutcome::Success { wire_cache: None },
            "failed" => AgentOutcome::Failed {
                error: s(v, "error"),
            },
            _ => AgentOutcome::Cancelled,
        }),
        _ => return None,
    })
}

/// A transcript in the session file's entry format, which keeps everything a reload keeps.
pub fn messages_to_json(messages: &[ChatMessage]) -> Value {
    Value::Array(
        messages
            .iter()
            .flat_map(crate::session_store::chat_message_to_json_entries)
            .collect(),
    )
}

pub fn messages_from_json(entries: &Value) -> Vec<ChatMessage> {
    crate::hydrate::messages_from_get_messages(&json!({ "messages": entries }))
}

/// Fingerprint of a transcript, so the app sends its history only when the server's differs.
/// The work duration is left out: each end times the turn itself.
pub fn messages_hash(messages: &[ChatMessage]) -> String {
    let mut hasher = Sha256::new();
    for mut entry in messages
        .iter()
        .flat_map(crate::session_store::chat_message_to_json_entries)
    {
        if let Some(entry) = entry.as_object_mut() {
            entry.remove("workedSecs");
        }
        hasher.update(entry.to_string().as_bytes());
        hasher.update(b"\n");
    }
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn decision_to_json(decision: &ApprovalDecision) -> Value {
    match decision {
        ApprovalDecision::Approve => json!({"decision": "approve"}),
        ApprovalDecision::ApproveRest => json!({"decision": "approve_rest"}),
        ApprovalDecision::Deny => json!({"decision": "deny"}),
        ApprovalDecision::AllowPrefix(prefix) => {
            json!({"decision": "allow_prefix", "prefix": prefix})
        }
        ApprovalDecision::AllowTool(_) => json!({"decision": "allow_tool"}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AssistantBlock, ToolMetadata, ToolStatus, ToolUpdate};

    fn round_trip(event: AgentEvent) -> AgentEvent {
        event_from_json(&event_to_json(&event).expect("sent")).expect("parsed")
    }

    #[test]
    fn events_round_trip() {
        assert!(matches!(
            round_trip(AgentEvent::TextDelta("hi".into())),
            AgentEvent::TextDelta(d) if d == "hi"
        ));
        let update = ToolUpdate {
            tool_call_id: "t1".into(),
            name: "Read".into(),
            args: Some(json!({"path": "a.rs"})),
            output: "x".into(),
            diff: None,
            output_truncated: false,
            metadata: ToolMetadata {
                status: ToolStatus::Completed,
                ..Default::default()
            },
        };
        match round_trip(AgentEvent::ToolUpdate(Box::new(update))) {
            AgentEvent::ToolUpdate(u) => {
                assert_eq!(u.tool_call_id, "t1");
                assert_eq!(u.metadata.status, ToolStatus::Completed);
            }
            other => panic!("unexpected {other:?}"),
        }
        match round_trip(AgentEvent::ToolEnd {
            tool_call_id: "t1".into(),
            is_error: Some(true),
            full_output_path: None,
            diff: Some("@@".into()),
        }) {
            AgentEvent::ToolEnd { is_error, diff, .. } => {
                assert_eq!(is_error, Some(true));
                assert_eq!(diff.as_deref(), Some("@@"));
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(matches!(
            round_trip(AgentEvent::Finished(AgentOutcome::Failed {
                error: "boom".into()
            })),
            AgentEvent::Finished(AgentOutcome::Failed { error }) if error == "boom"
        ));
        let usage = crate::agent::TokenUsage {
            input_tokens: 7,
            ..Default::default()
        };
        assert!(matches!(
            round_trip(AgentEvent::Usage(usage)),
            AgentEvent::Usage(u) if u.input_tokens == 7
        ));
    }

    #[test]
    fn transcripts_round_trip_with_a_stable_hash() {
        let mut reply = ChatMessage::streaming_assistant(None);
        reply.append_thinking("hmm");
        reply.append_answer("done");
        reply.finish_streaming();
        let messages = vec![ChatMessage::user("hello", &[]), reply];
        let back = messages_from_json(&messages_to_json(&messages));
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].text, "hello");
        assert!(matches!(&back[1].blocks[1], AssistantBlock::Answer(a) if a == "done"));
        assert_eq!(messages_hash(&back), messages_hash(&messages));
    }
}
