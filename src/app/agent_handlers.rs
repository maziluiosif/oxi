//! Consume local [`AgentEvent`](crate::agent::AgentEvent) stream.

use eframe::egui;

use crate::agent::AgentEvent;
use crate::oauth::OAuthUiMsg;

use super::{OxiApp, SessionKey};

impl OxiApp {
    pub(crate) fn drain_agent(&mut self, ctx: &egui::Context) {
        let keys: Vec<SessionKey> = self.flow.sessions.keys().copied().collect();
        let mut repainted = false;

        for key in keys {
            let Some(rx) = self
                .flow
                .sessions
                .get_mut(&key)
                .and_then(|state| state.agent_rx.take())
            else {
                continue;
            };

            loop {
                match rx.try_recv() {
                    Ok(ev) => {
                        self.apply_agent_event(key, ev);
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
                        repainted = true;
                        break;
                    }
                }
            }
        }

        self.flow.sessions.retain(|_, state| {
            state.agent_rx.is_some() || state.waiting_response || state.stream_error.is_some()
        });

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
                    OAuthUiMsg::GitHubDevice { url, user_code } => {
                        self.conv.oauth_device_copilot = Some((url, user_code));
                    }
                    OAuthUiMsg::GitHubDone(r) => {
                        self.conv.oauth_busy = false;
                        self.conv.oauth_device_copilot = None;
                        self.conv.oauth_last_message = Some(match r {
                            Ok(()) => "GitHub Copilot: signed in.".to_string(),
                            Err(e) => format!("GitHub Copilot OAuth: {e}"),
                        });
                    }
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
                    self.conv.oauth_busy = false;
                    break;
                }
            }
        }
        ctx.request_repaint();
    }

    fn apply_agent_event(&mut self, key: SessionKey, ev: AgentEvent) {
        match ev {
            AgentEvent::AgentStart => {
                self.run_state_mut(key).agent_ack = true;
            }
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
            AgentEvent::StreamError(reason) => {
                self.append_assistant_answer(key, &format!("\n[Error] {reason}\n"));
                self.finish_assistant_stream(key);
            }
            AgentEvent::AssistantMessageDone => {}
            AgentEvent::AgentEnd => {
                self.finish_assistant_stream(key);
            }
        }
    }
}
