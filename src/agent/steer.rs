//! Messages the user sends into a running turn of oxi's own agent ("steering"). They wait here
//! until the agent loop reaches its next step (after the tools of a round ran, or once the model
//! answered), then join the turn's history as user messages, so the model reads them before it
//! continues. Agents that run their own loop (ACP) get no inbox: their chats queue the message,
//! or stop the turn and send it, instead.

use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::events::AgentEvent;

/// Shared by the chat (which adds messages) and its running turn (which takes them).
#[derive(Clone, Debug, Default)]
pub struct SteerInbox(Arc<Mutex<Vec<String>>>);

impl SteerInbox {
    pub fn push(&self, text: String) {
        self.lock().push(text);
    }

    /// Take back pending message `index` before the agent reads it.
    pub fn remove(&self, index: usize) -> Option<String> {
        let mut pending = self.lock();
        (index < pending.len()).then(|| pending.remove(index))
    }

    /// Take every pending message.
    pub fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.lock())
    }

    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<String>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Move the pending messages into `messages` (the provider-neutral wire history) and announce
/// each with [`AgentEvent::Steered`]. Returns whether there were any.
pub(crate) fn deliver(
    inbox: Option<&SteerInbox>,
    messages: &mut Vec<Value>,
    tx: &Sender<AgentEvent>,
) -> bool {
    let Some(inbox) = inbox else {
        return false;
    };
    let texts = inbox.take();
    for text in &texts {
        messages.push(json!({ "role": "user", "content": text }));
        let _ = tx.send(AgentEvent::Steered(text.clone()));
    }
    !texts.is_empty()
}

/// Whether messages are waiting for the turn.
pub(crate) fn waiting(inbox: Option<&SteerInbox>) -> bool {
    inbox.is_some_and(|inbox| !inbox.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delivered_messages_join_the_history_once_and_are_announced() {
        let inbox = SteerInbox::default();
        inbox.push("use the v2 API".into());
        inbox.push("and keep the old one".into());
        assert_eq!(inbox.remove(1).as_deref(), Some("and keep the old one"));
        assert!(waiting(Some(&inbox)));
        let (tx, rx) = std::sync::mpsc::channel();
        let mut messages = vec![json!({"role": "tool", "tool_call_id": "a", "content": "ok"})];
        assert!(deliver(Some(&inbox), &mut messages, &tx));
        assert_eq!(
            messages[1],
            json!({"role": "user", "content": "use the v2 API"})
        );
        assert!(matches!(rx.try_recv(), Ok(AgentEvent::Steered(t)) if t == "use the v2 API"));
        assert!(!deliver(Some(&inbox), &mut messages, &tx), "taken once");
        assert!(!deliver(None, &mut messages, &tx));
        assert_eq!(messages.len(), 2);
    }
}

/// Against a real model: `MODEL_API_KEY=… cargo test --lib live_steer -- --ignored --nocapture`
/// (Meta Muse; any other OpenAI-compatible provider works the same way).
#[cfg(test)]
mod live_tests {
    use super::*;

    #[test]
    #[ignore = "calls a real model; needs MODEL_API_KEY for Meta Muse"]
    fn live_steer_reaches_the_model_within_the_turn() {
        use crate::settings::{AppSettings, LlmProviderKind};
        let cfg = AppSettings::default()
            .provider(LlmProviderKind::MetaMuse)
            .clone();
        let dir = crate::fsutil::unique_temp_path(&std::env::temp_dir(), "oxi-live-steer", "d");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("notes.txt"), "alpha\n").unwrap();
        let mut enabled = vec![false; crate::settings::ALL_TOOL_NAMES.len()];
        if let Some(i) = crate::settings::ALL_TOOL_NAMES
            .iter()
            .position(|n| *n == "read")
        {
            enabled[i] = true;
        }
        let env = crate::agent::tools::ToolEnv {
            enabled: enabled.clone(),
            web_search_url: String::new(),
            web_search_backend: Default::default(),
            bash_timeout_cap_secs: 30,
            mcp: None,
            undo_journal: None,
            subagent: None,
            evidence: None,
            delegation: None,
        };
        let tools = crate::agent::tools::tool_definitions_json(&enabled, 30);
        let inbox = SteerInbox::default();
        inbox.push("Also end your answer with the word BANANA.".into());
        let (tx, rx) = std::sync::mpsc::channel();
        let (_approval_tx, approval_rx) = std::sync::mpsc::channel();
        let mut gate = crate::agent::approval::ApprovalGate::new(
            crate::agent::approval::ApprovalPolicy::disabled(),
            approval_rx,
        );
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let client = crate::agent::dispatch::streaming_client(&cfg, 120).unwrap();
        let mut messages = vec![
            json!({"role": "system", "content": format!("You are a coding agent. Working directory: {}", dir.display())}),
            json!({"role": "user", "content": "Read notes.txt with the read tool and tell me what it says."}),
        ];
        let result =
            crate::runtime::runtime()
                .unwrap()
                .block_on(crate::agent::dispatch::run_provider_loop(
                    crate::agent::dispatch::DispatchParams {
                        cfg: &cfg,
                        client: &client,
                        tunnels: None,
                        cwd: &dir,
                        env: &env,
                        tx: &tx,
                        cancel: &cancel,
                        gate: &mut gate,
                        max_rounds: 8,
                        effort_override: None,
                        context_char_budget: usize::MAX,
                        tools_chars: 0,
                        steer: Some(&inbox),
                    },
                    &mut messages,
                    &tools,
                ));
        drop(tx);
        let events: Vec<AgentEvent> = rx.into_iter().collect();
        let text: String = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::TextDelta(d) => Some(d.as_str()),
                _ => None,
            })
            .collect();
        println!("answer: {text}");
        result.unwrap();
        assert!(events.iter().any(|e| matches!(e, AgentEvent::Steered(_))));
        assert!(text.to_uppercase().contains("BANANA"), "{text}");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
