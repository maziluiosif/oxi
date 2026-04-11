//! Consume local [`AgentEvent`](crate::agent::AgentEvent) stream.

use eframe::egui;

use crate::agent::AgentEvent;
use crate::oauth::OAuthUiMsg;

use super::PiChatApp;

impl PiChatApp {
    pub(crate) fn drain_agent(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.conn.agent_rx.take() else {
            return;
        };
        let mut repainted = false;
        loop {
            match rx.try_recv() {
                Ok(ev) => {
                    self.apply_agent_event(ev);
                    repainted = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    self.conn.agent_rx = Some(rx);
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.conn.cancel_agent = None;
                    repainted = true;
                    break;
                }
            }
        }
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

    fn apply_agent_event(&mut self, ev: AgentEvent) {
        match ev {
            AgentEvent::AgentStart => {
                self.flow.agent_ack = true;
            }
            AgentEvent::TextStart => {
                self.on_text_block_start();
            }
            AgentEvent::TextDelta(d) => {
                self.append_text_delta(&d);
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
                self.start_tool_block(&name, id, args.as_ref());
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
                self.set_tool_output(id, &text, truncated);
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
                self.finalize_tool_run(id, is_error, full_output_path, diff);
            }
            AgentEvent::StreamError(reason) => {
                self.append_assistant_answer(&format!("\n[Error] {reason}\n"));
                self.finish_assistant_stream();
            }
            AgentEvent::AssistantMessageDone => {}
            AgentEvent::AgentEnd => {
                self.finish_assistant_stream();
            }
        }
    }
}
