//! Consume local [`AgentEvent`](crate::agent::AgentEvent) stream.

use eframe::egui;
use serde_json::Value;

use crate::agent::{AgentEvent, AgentOutcome};
use crate::oauth::OAuthUiMsg;

use super::{OxiApp, PendingApproval, SessionKey};

/// Build a short human-readable summary of a pending tool call for the approval prompt.
fn approval_summary(name: &str, args: &Option<Value>) -> String {
    let Some(args) = args else {
        return name.to_string();
    };
    let field = match name {
        "bash" => "command",
        "write" | "edit" | "delete" | "mkdir" => "path",
        "move" => "from",
        _ => "",
    };
    args.get(field)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

impl OxiApp {
    pub(crate) fn drain_agent(&mut self, ctx: &egui::Context) {
        let keys: Vec<SessionKey> = self.flow.sessions.keys().copied().collect();
        let mut repainted = false;
        let mut disconnected: Vec<SessionKey> = Vec::new();

        for key in keys {
            let Some(rx) = self
                .flow
                .sessions
                .get_mut(&key)
                .and_then(|state| state.agent_rx.take())
            else {
                continue;
            };

            const MAX_AGENT_EVENTS_PER_FRAME: usize = 512;
            let mut processed = 0usize;
            loop {
                if processed >= MAX_AGENT_EVENTS_PER_FRAME {
                    if let Some(state) = self.flow.sessions.get_mut(&key) {
                        state.agent_rx = Some(rx);
                    }
                    repainted = true;
                    break;
                }
                match rx.try_recv() {
                    Ok(ev) => {
                        self.apply_agent_event(key, ev);
                        processed += 1;
                        repainted = true;
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        if let Some(state) = self.flow.sessions.get_mut(&key) {
                            state.agent_rx = Some(rx);
                        }
                        break;
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        if let Some(state) = self.flow.sessions.get_mut(&key) {
                            state.cancel_agent = None;
                        }
                        disconnected.push(key);
                        repainted = true;
                        break;
                    }
                }
            }
        }

        // A disconnect while still waiting means the worker died without a terminal
        // event (e.g. a panic): close out the stream so the session doesn't hang.
        // After a normal Finished event, waiting is already off and this is a no-op.
        for key in disconnected {
            if self
                .run_state(key)
                .is_some_and(|state| state.waiting_response)
            {
                self.invalidate_wire_cache(key);
                self.append_assistant_answer(key, "\n[Error] Agent stopped unexpectedly.\n");
                self.finish_assistant_stream(key);
            }
        }

        self.flow.sessions.retain(|_, state| {
            state.agent_rx.is_some()
                || state.waiting_response
                || state.stream_error.is_some()
                // Keep completed usage metadata around for the header "Ready" chip.
                || !state.turn_usage.is_zero()
                || !state.last_turn_usage.is_zero()
                || !state.session_usage.is_zero()
        });

        if repainted {
            ctx.request_repaint();
        }
    }

    pub(crate) fn drain_models(&mut self, ctx: &egui::Context) {
        if self.conv.model_rxs.is_empty() {
            return;
        }
        let mut repainted = false;
        let mut pending = Vec::with_capacity(self.conv.model_rxs.len());
        for rx in self.conv.model_rxs.drain(..) {
            let mut keep_rx = true;
            loop {
                match rx.try_recv() {
                    Ok(msg) => {
                        let entry = self.conv.fetched_models.entry(msg.provider).or_default();
                        entry.loading = false;
                        match msg.result {
                            Ok(models) => {
                                entry.models = models;
                                entry.error = None;
                            }
                            Err(e) => {
                                entry.error = Some(e);
                            }
                        }
                        repainted = true;
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        break;
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        keep_rx = false;
                        break;
                    }
                }
            }
            if keep_rx {
                pending.push(rx);
            }
        }
        self.conv.model_rxs = pending;
        if repainted {
            ctx.request_repaint();
        }
    }

    pub(crate) fn drain_oauth(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.conn.oauth_rx.take() else {
            return;
        };
        loop {
            match rx.try_recv() {
                Ok(msg) => match msg {
                    OAuthUiMsg::CodexOpenBrowser { url } => {
                        self.conv.oauth_last_message = Some(format!(
                            "Complete sign-in in the browser (or check port 1455). {url}"
                        ));
                    }
                    OAuthUiMsg::CodexDone(r) => {
                        self.conv.oauth_busy = false;
                        self.conv.oauth_last_message = Some(match r {
                            Ok(()) => "ChatGPT (Codex): signed in.".to_string(),
                            Err(e) => format!("Codex OAuth: {e}"),
                        });
                    }
                },
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    self.conn.oauth_rx = Some(rx);
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    // Worker ended without a terminal message (normal after CodexDone;
                    // abnormal mid-flow) — make sure the UI doesn't stay silent.
                    if self.conv.oauth_busy {
                        self.conv.oauth_last_message =
                            Some("Sign-in was interrupted — try again.".to_string());
                    }
                    self.conv.oauth_busy = false;
                    break;
                }
            }
        }
        ctx.request_repaint();
    }

    fn apply_agent_event(&mut self, key: SessionKey, ev: AgentEvent) {
        // Any event other than another retry means the stream is flowing again.
        if !matches!(ev, AgentEvent::StreamRetry { .. }) {
            self.run_state_mut(key).stream_retrying = None;
        }
        match ev {
            AgentEvent::AgentStart => {}
            AgentEvent::TextStart => {
                self.on_text_block_start(key);
            }
            AgentEvent::TextDelta(d) => {
                self.append_text_delta(key, &d);
            }
            AgentEvent::ThinkingDelta(d) => {
                self.append_thinking_delta(key, &d);
            }
            AgentEvent::ToolStart {
                name,
                tool_call_id,
                args,
            } => {
                let id = if tool_call_id.is_empty() {
                    None
                } else {
                    Some(tool_call_id.as_str())
                };
                self.start_tool_block(key, &name, id, args.as_ref());
            }
            AgentEvent::ApprovalRequest { name, args } => {
                let summary = approval_summary(&name, &args);
                self.run_state_mut(key).pending_approval = Some(PendingApproval { name, summary });
            }
            AgentEvent::ToolOutput {
                tool_call_id,
                text,
                truncated,
            } => {
                let id = if tool_call_id.is_empty() {
                    None
                } else {
                    Some(tool_call_id.as_str())
                };
                self.set_tool_output(key, id, &text, truncated);
            }
            AgentEvent::ToolEnd {
                tool_call_id,
                is_error,
                full_output_path,
                diff,
            } => {
                let id = if tool_call_id.is_empty() {
                    None
                } else {
                    Some(tool_call_id.as_str())
                };
                self.finalize_tool_run(key, id, is_error, full_output_path, diff);
            }
            AgentEvent::StreamRetry { attempt, reason } => {
                eprintln!("[oxi] stream retry (attempt {attempt}): {reason}");
                self.run_state_mut(key).stream_retrying =
                    Some(format!("Connection lost (attempt {attempt}): {reason}"));
                self.reset_streaming_tail(key);
            }
            AgentEvent::AssistantMessageDone => {}
            AgentEvent::Usage(usage) => {
                {
                    let run = self.run_state_mut(key);
                    run.turn_usage.add(&usage);
                    run.session_usage.add(&usage);
                }
                // Calibrate chars-per-token from the real prompt size the provider reported.
                // The estimate is taken against the current in-UI transcript, which can run a
                // little ahead of what this round's prompt contained (in-flight tool output),
                // so the ratio is clamped and simply overwritten each round (latest wins).
                let prompt_tokens = usage.total_input();
                if prompt_tokens > 500 {
                    let est = self.estimated_session_context_chars(key);
                    let cpt = crate::agent::calibrate_chars_per_token(est, prompt_tokens);
                    self.session_mut_by_key(key).chars_per_token = Some(cpt);
                }
            }
            AgentEvent::Finished(outcome) => {
                match outcome {
                    AgentOutcome::Success { wire_cache } => {
                        self.session_mut_by_key(key).wire_cache = wire_cache;
                    }
                    AgentOutcome::Failed { error } => {
                        self.invalidate_wire_cache(key);
                        self.append_assistant_answer(key, &format!("\n[Error] {error}\n"));
                    }
                    AgentOutcome::Cancelled => {
                        self.invalidate_wire_cache(key);
                    }
                }
                self.finish_assistant_stream(key);
            }
        }
    }
}
