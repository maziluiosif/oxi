use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::agent::spawn_agent_run;
use crate::model::{
    make_session_title, set_tool_output_on_blocks, AssistantBlock, ChatMessage, MsgRole,
    UserAttachment,
};

use super::PiChatApp;
use crate::session_store;

impl PiChatApp {
    pub(crate) fn send_message(&mut self) {
        let text = self.conv.input.trim().to_string();
        let has_images = !self.conv.pending_images.is_empty();
        if (text.is_empty() && !has_images) || self.flow.waiting_response {
            return;
        }
        let session_idx = self.active_workspace().active;
        if self.active_session().messages.is_empty() && self.active_session().session_file.is_none()
        {
            self.active_session_mut().title = if !text.is_empty() {
                make_session_title(&text)
            } else {
                "Image".to_string()
            };
        }

        self.push_input_history(&text);
        self.conv.input_history_index = None;
        self.conv.input_history_draft.clear();

        let pending = std::mem::take(&mut self.conv.pending_images);
        let user_attachments: Vec<UserAttachment> = pending
            .iter()
            .map(|(mime, data)| UserAttachment::Image {
                mime: mime.clone(),
                data: data.clone(),
            })
            .collect();
        self.conv.input.clear();
        self.conv.scroll_to_bottom_once = true;

        self.flow.agent_ack = false;
        self.begin_waiting_response();
        self.flow.stream_session_idx = Some(session_idx);
        self.flow.stream_error = None;

        self.materialize_prompt(session_idx, &text, &user_attachments);
        let root_path = self.active_workspace().root_path.clone();
        if let Err(e) =
            session_store::save_session_messages(&root_path, self.session_mut(session_idx))
        {
            self.flow.stream_error = Some(format!("Save session: {e}"));
        }

        if let Err(e) = self.send_prompt_payload(&text, &user_attachments) {
            self.flow.stream_error = Some(e.clone());
            let sess = self.stream_session_mut();
            if let Some(last) = sess.messages.last_mut() {
                if last.role == MsgRole::Assistant {
                    last.streaming = false;
                    last.blocks = vec![AssistantBlock::Answer(format!("[Send error] {e}"))];
                }
            }
            self.end_waiting_response();
            self.flow.agent_ack = false;
            self.flow.stream_session_idx = None;
        }
    }

    pub(crate) fn materialize_prompt(
        &mut self,
        session_idx: usize,
        text: &str,
        attachments: &[UserAttachment],
    ) {
        let sess = self.session_mut(session_idx);
        sess.messages_loaded = true;
        sess.messages.push(ChatMessage {
            role: MsgRole::User,
            text: text.to_string(),
            attachments: attachments.to_vec(),
            blocks: vec![],
            streaming: false,
        });
        sess.messages.push(ChatMessage {
            role: MsgRole::Assistant,
            text: String::new(),
            attachments: vec![],
            blocks: vec![],
            streaming: true,
        });
    }

    pub(crate) fn last_assistant_mut(&mut self) -> Option<&mut ChatMessage> {
        self.stream_session_mut()
            .messages
            .iter_mut()
            .rev()
            .find(|m| m.role == MsgRole::Assistant)
    }

    pub(crate) fn on_text_block_start(&mut self) {
        let Some(m) = self.last_assistant_mut() else {
            return;
        };
        if let Some(AssistantBlock::Answer(s)) = m.blocks.last() {
            if s.is_empty() {
                return;
            }
        }
        m.blocks.push(AssistantBlock::Answer(String::new()));
    }

    pub(crate) fn append_text_delta(&mut self, delta: &str) {
        let Some(m) = self.last_assistant_mut() else {
            return;
        };
        match m.blocks.last_mut() {
            Some(AssistantBlock::Answer(s)) => s.push_str(delta),
            _ => m.blocks.push(AssistantBlock::Answer(delta.to_string())),
        }
    }

    /// Error lines and similar go into the answer stream (Markdown).
    pub(crate) fn append_assistant_answer(&mut self, s: &str) {
        let Some(m) = self.last_assistant_mut() else {
            return;
        };
        match m.blocks.last_mut() {
            Some(AssistantBlock::Answer(a)) => a.push_str(s),
            _ => m.blocks.push(AssistantBlock::Answer(s.to_string())),
        }
    }

    pub(crate) fn start_tool_block(
        &mut self,
        name: &str,
        tool_call_id: Option<&str>,
        args: Option<&serde_json::Value>,
    ) {
        let Some(m) = self.last_assistant_mut() else {
            return;
        };
        let id = tool_call_id.unwrap_or("").to_string();
        if !id.is_empty() {
            let dup = m
                .blocks
                .iter()
                .any(|b| matches!(b, AssistantBlock::Tool { tool_call_id: tid, .. } if tid == &id));
            if dup {
                return;
            }
        } else if let Some(AssistantBlock::Tool {
            name: n, output, ..
        }) = m.blocks.last()
        {
            if n == name && output.is_empty() {
                return;
            }
        }
        let args_summary = args.map(|a| {
            let s = a.to_string();
            s.chars().take(800).collect::<String>()
        });
        m.blocks.push(AssistantBlock::Tool {
            tool_call_id: id,
            name: name.to_string(),
            args_summary,
            output: String::new(),
            diff: None,
            is_error: None,
            full_output_path: None,
            output_truncated: false,
        });
    }

    /// `partialResult` / final `result` are cumulative; route by `toolCallId` when present.
    pub(crate) fn set_tool_output(
        &mut self,
        tool_call_id: Option<&str>,
        text: &str,
        truncated: bool,
    ) {
        let Some(m) = self.last_assistant_mut() else {
            return;
        };
        set_tool_output_on_blocks(&mut m.blocks, tool_call_id, text, truncated);
    }

    pub(crate) fn finalize_tool_run(
        &mut self,
        tool_call_id: Option<&str>,
        is_error: Option<bool>,
        full_output_path: Option<String>,
        diff: Option<String>,
    ) {
        let Some(m) = self.last_assistant_mut() else {
            return;
        };
        let tid = tool_call_id.unwrap_or("");
        for b in m.blocks.iter_mut().rev() {
            if let AssistantBlock::Tool {
                tool_call_id: id,
                diff: d,
                is_error: ie,
                full_output_path: fp,
                ..
            } = b
            {
                if tid.is_empty() || id == tid {
                    *d = diff;
                    *ie = is_error;
                    *fp = full_output_path;
                    return;
                }
            }
        }
    }

    pub(crate) fn send_prompt_payload(
        &mut self,
        _text: &str,
        attachments: &[UserAttachment],
    ) -> Result<(), String> {
        let session_idx = self.stream_session_index();
        let cwd = PathBuf::from(self.active_workspace().root_path.trim());
        let chat = {
            let s = self.session_mut(session_idx);
            if s.messages.len() < 2 {
                return Err("internal: expected user and assistant messages".into());
            }
            s.messages[..s.messages.len() - 1].to_vec()
        };
        let settings = self.conv.settings.clone();
        let _has_attachments = !attachments.is_empty();
        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let _join = spawn_agent_run(settings, cwd, chat, tx, cancel.clone());
        self.conn.agent_rx = Some(rx);
        self.conn.cancel_agent = Some(cancel);
        Ok(())
    }

    pub(crate) fn finish_assistant_stream(&mut self) {
        if let Some(stream_idx) = self.flow.stream_session_idx {
            let widx = self.conv.active_workspace;
            if let Some(last) = self.conv.workspaces[widx].sessions[stream_idx]
                .messages
                .iter_mut()
                .rev()
                .find(|m| m.role == MsgRole::Assistant)
            {
                last.streaming = false;
            }
            let root_path = self.active_workspace().root_path.clone();
            if let Err(e) =
                session_store::save_session_messages(&root_path, self.session_mut(stream_idx))
            {
                self.flow.stream_error = Some(format!("Save session: {e}"));
            }
        }
        self.end_waiting_response();
        self.flow.agent_ack = false;
        self.flow.stream_session_idx = None;
    }
}
