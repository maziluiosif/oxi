//! Bring a session started outside oxi (Claude Code in a terminal, another editor) into an oxi
//! chat: list the agent's sessions for the workspace (`session/list`), replay one
//! (`session/load`) into oxi's transcript, and bind the chat to it so the next prompt resumes
//! the agent's own session instead of starting over.
//!
//! Each call runs a short-lived agent of its own, so no chat's agent is disturbed.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::channel;
use std::time::Duration;

use serde_json::{Value, json};

use super::update_events::{UpdateState, content_block_text};
use super::{AgentEvent, ConnHandles, LaunchSpec, PromptCtx, request, sessions, spawn_conn};
use crate::model::ChatMessage;

/// The agent and workspace to list or import sessions from.
pub struct AcpImport {
    pub command_line: String,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
}

/// One of the agent's sessions, as `session/list` describes it.
#[derive(Clone, Debug, PartialEq)]
pub struct ExternalSession {
    pub session_id: String,
    pub title: String,
    /// RFC 3339, when the agent reports it.
    pub updated_at: Option<String>,
}

/// Pages of `session/list` read at most; the newest sessions come first.
const MAX_PAGES: usize = 5;

/// The agent's sessions in `req.cwd`, newest first, without the ones an oxi chat already uses.
pub async fn list_sessions(req: &AcpImport) -> Result<Vec<ExternalSession>, String> {
    let (conn, key) = launch(req).await?;
    let result = list_with(&conn.handles, req).await;
    drop(conn);
    super::config_options::forget(&key);
    result
}

async fn list_with(h: &ConnHandles, req: &AcpImport) -> Result<Vec<ExternalSession>, String> {
    if !h.caps["sessionCapabilities"]["list"].is_object() {
        return Err("This agent can't list its sessions.".into());
    }
    let known: HashSet<String> = sessions::known_ids();
    let mut out = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut params = json!({ "cwd": req.cwd.to_string_lossy() });
        if let Some(cursor) = &cursor {
            params["cursor"] = json!(cursor);
        }
        let page = tokio::time::timeout(
            Duration::from_secs(30),
            request(&h.stdin, &h.next_id, &h.pending, "session/list", params),
        )
        .await
        .map_err(|_| "The agent took too long to list its sessions.".to_string())?
        .map_err(|e| format!("The agent could not list its sessions: {e}"))?;
        out.extend(parse_sessions(&page, &known));
        cursor = page["nextCursor"].as_str().map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    Ok(out)
}

fn parse_sessions(page: &Value, known: &HashSet<String>) -> Vec<ExternalSession> {
    page["sessions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| {
            let session_id = s["sessionId"].as_str()?.to_owned();
            if known.contains(&session_id) {
                return None;
            }
            let title = s["title"]
                .as_str()
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .unwrap_or("Untitled session")
                .to_owned();
            Some(ExternalSession {
                session_id,
                title,
                updated_at: s["updatedAt"].as_str().map(str::to_owned),
            })
        })
        .collect()
}

/// Replay `session_id` into chat messages.
pub async fn load_session(req: &AcpImport, session_id: &str) -> Result<Vec<ChatMessage>, String> {
    let (conn, key) = launch(req).await?;
    let result = load_with(&conn.handles, req, session_id).await;
    drop(conn);
    super::config_options::forget(&key);
    result
}

async fn load_with(
    h: &ConnHandles,
    req: &AcpImport,
    session_id: &str,
) -> Result<Vec<ChatMessage>, String> {
    if h.caps["loadSession"].as_bool() != Some(true) {
        return Err("This agent can't replay its sessions, so they can't be imported.".into());
    }
    let (replay_tx, replay_rx) = channel();
    let (event_tx, _) = channel();
    let (perm_tx, _) = tokio::sync::mpsc::unbounded_channel();
    *h.prompt_ctx.lock().await = Some(PromptCtx {
        session_id: session_id.to_owned(),
        cwd: req.cwd.clone(),
        // Nothing the replay asks for may change anything.
        plan_mode: true,
        updates: UpdateState::default(),
        children: Default::default(),
        event_tx,
        perm_tx,
        cycle_end: None,
        last_update: std::time::Instant::now(),
        replay: Some(replay_tx),
    });
    let params = json!({
        "sessionId": session_id,
        "cwd": req.cwd.to_string_lossy(),
        "mcpServers": []
    });
    let loaded = tokio::time::timeout(
        Duration::from_secs(120),
        request(&h.stdin, &h.next_id, &h.pending, "session/load", params),
    )
    .await;
    // The replay arrives before the response, so it is all in by now.
    *h.prompt_ctx.lock().await = None;
    loaded
        .map_err(|_| "The agent took too long to load the session.".to_string())?
        .map_err(|e| format!("The agent could not load the session: {e}"))?;
    let updates: Vec<Value> = replay_rx.try_iter().collect();
    let messages = transcript(&updates);
    if messages.is_empty() {
        return Err("The session has no messages to import.".into());
    }
    Ok(messages)
}

/// A short-lived agent for listing or loading sessions, under a key no chat uses.
async fn launch(req: &AcpImport) -> Result<(super::Conn, String), String> {
    let key = format!("mem:import:{}", rand::random::<u64>());
    let spec = LaunchSpec {
        session_key: &key,
        command_line: &req.command_line,
        cwd: &req.cwd,
        env: &req.env,
        model: "",
        effort: "",
        mcp_servers: &[],
    };
    Ok((spawn_conn(&spec, Vec::new()).await?, key))
}

/// Make the chat `chat_key` continue the agent's session `session_id` (see [`sessions`]).
pub fn bind_session(chat_key: &str, command_line: &str, cwd: &std::path::Path, session_id: &str) {
    sessions::remember(chat_key, command_line.trim(), cwd, session_id);
}

/// Oxi messages from a replayed session: user chunks become user messages, everything the
/// agent said or did in between one assistant message each.
fn transcript(updates: &[Value]) -> Vec<ChatMessage> {
    let (tx, rx) = channel();
    let mut state = UpdateState::default();
    let mut messages: Vec<ChatMessage> = Vec::new();
    // Consecutive user chunks are one message.
    let mut in_user = false;
    for update in updates {
        if update["sessionUpdate"].as_str() == Some("user_message_chunk") {
            let Some(text) = update.get("content").and_then(content_block_text) else {
                continue;
            };
            match messages.last_mut() {
                Some(last) if in_user => last.text.push_str(&text),
                _ => messages.push(ChatMessage::user(&text, &[])),
            }
            in_user = true;
            continue;
        }
        state.emit_update(update, &tx);
        for event in rx.try_iter() {
            let needs_assistant = messages
                .last()
                .is_none_or(|m| m.role != crate::model::MsgRole::Assistant);
            let shows = matches!(
                event,
                AgentEvent::TextDelta(_) | AgentEvent::ThinkingDelta(_) | AgentEvent::ToolUpdate(_)
            );
            if !shows {
                continue;
            }
            in_user = false;
            if needs_assistant {
                messages.push(ChatMessage::streaming_assistant(None));
            }
            let Some(message) = messages.last_mut() else {
                continue;
            };
            match event {
                AgentEvent::TextDelta(text) => message.append_answer(&text),
                AgentEvent::ThinkingDelta(text) => message.append_thinking(&text),
                AgentEvent::ToolUpdate(update) => {
                    crate::model::apply_tool_update(&mut message.blocks, *update)
                }
                _ => {}
            }
        }
    }
    for message in &mut messages {
        if message.role == crate::model::MsgRole::Assistant {
            message.finish_streaming();
        }
        message.text = without_oxi_preamble(&message.text).trim().to_owned();
    }
    messages
}

/// What the user typed, without what oxi put in front of it when it sent the message (a
/// session that started in oxi and was deleted there).
fn without_oxi_preamble(text: &str) -> &str {
    let mut text = text.trim_start();
    for tag in ["conversation_history", "conversation_update"] {
        if text.starts_with(&format!("<{tag}>"))
            && let Some(end) = text.find(&format!("</{tag}>"))
        {
            text = text[end + tag.len() + 3..].trim_start();
        }
    }
    text.strip_prefix(super::ACP_PLAN_MODE_PREFIX)
        .unwrap_or(text)
        .trim_start()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AssistantBlock, MsgRole};

    fn chunk(kind: &str, text: &str) -> Value {
        json!({ "sessionUpdate": kind, "content": { "type": "text", "text": text } })
    }

    #[test]
    fn a_replay_becomes_alternating_user_and_assistant_messages() {
        let messages = transcript(&[
            chunk("user_message_chunk", "Fix the "),
            chunk("user_message_chunk", "login bug"),
            chunk("agent_thought_chunk", "Look at auth.rs"),
            json!({ "sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Read auth.rs",
                    "kind": "read", "status": "completed" }),
            chunk("agent_message_chunk", "Fixed."),
            json!({ "sessionUpdate": "usage_update", "used": 1, "size": 2 }),
            chunk("user_message_chunk", "Thanks"),
            chunk("agent_message_chunk", "You're welcome."),
        ]);
        let roles: Vec<MsgRole> = messages.iter().map(|m| m.role).collect();
        assert!(
            roles
                == [
                    MsgRole::User,
                    MsgRole::Assistant,
                    MsgRole::User,
                    MsgRole::Assistant
                ]
        );
        assert_eq!(messages[0].text, "Fix the login bug");
        let blocks = &messages[1].blocks;
        assert!(matches!(&blocks[0], AssistantBlock::Thinking(t) if t == "Look at auth.rs"));
        assert!(
            matches!(&blocks[1], AssistantBlock::Tool { tool_call_id, .. } if tool_call_id == "t1")
        );
        assert!(matches!(&blocks[2], AssistantBlock::Answer(t) if t == "Fixed."));
        assert!(!messages[1].streaming);
        assert_eq!(messages[2].text, "Thanks");
    }

    #[test]
    fn oxi_preambles_are_dropped_from_replayed_user_messages() {
        let text = format!(
            "<conversation_history>\nearlier\n</conversation_history>\n\n{}\n\nPlan the fix",
            super::super::ACP_PLAN_MODE_PREFIX
        );
        assert_eq!(without_oxi_preamble(&text), "Plan the fix");
        assert_eq!(
            without_oxi_preamble("plain <conversation_update>"),
            "plain <conversation_update>"
        );
    }

    #[test]
    fn listed_sessions_skip_the_ones_oxi_chats_use() {
        let page = json!({ "sessions": [
            { "sessionId": "a", "cwd": "/w", "title": "Refactor", "updatedAt": "2026-10-20T10:00:00Z" },
            { "sessionId": "b", "cwd": "/w", "title": "  " },
            { "sessionId": "c", "cwd": "/w", "title": "In oxi" },
            { "cwd": "/w" }
        ]});
        let known = HashSet::from(["c".to_owned()]);
        let sessions = parse_sessions(&page, &known);
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].title, "Refactor");
        assert_eq!(
            sessions[0].updated_at.as_deref(),
            Some("2026-10-20T10:00:00Z")
        );
        assert_eq!(sessions[1].title, "Untitled session");
    }
}
