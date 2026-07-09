use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::agent::runner::wire_fingerprint_for;
use crate::agent::spawn_agent_run;
use crate::model::{
    AssistantBlock, ChatMessage, MsgRole, UserAttachment, make_session_title,
    set_tool_output_on_blocks,
};

use super::{OxiApp, SessionKey};
use crate::session_store;

impl OxiApp {
    pub(crate) fn send_message(&mut self) {
        self.send_message_opts(false);
    }

    /// `skip_autocompact` is set when an auto-compaction has just finished and is replaying
    /// the deferred message — it must not re-trigger the threshold check (loop guard).
    pub(crate) fn send_message_opts(&mut self, skip_autocompact: bool) {
        let text = self.conv.input.trim().to_string();
        let has_images = !self.conv.pending_images.is_empty();
        if text.is_empty() && !has_images {
            return;
        }

        // Slash commands: only an exact, argument-free `/new` or `/compact` with no images.
        if !has_images && let Some(cmd) = super::compaction::parse_slash_command(&text) {
            self.push_input_history(&text);
            self.conv.input_history_index = None;
            self.conv.input_history_draft.clear();
            self.conv.input.clear();
            match cmd {
                super::compaction::SlashCommand::New => self.new_chat(),
                super::compaction::SlashCommand::Compact => {
                    let key = self.active_session_key();
                    self.start_compaction(key, None);
                }
            }
            return;
        }

        let key = self.active_session_key();
        if self
            .run_state(key)
            .is_some_and(|state| state.waiting_response)
        {
            return;
        }
        // Don't send into a session whose history is mid-compaction.
        if self.compaction_active_for(key) {
            return;
        }

        // Auto-compaction: if the context is near full, summarize first and defer this send.
        if !skip_autocompact && self.conv.compaction.is_none() {
            let max_tokens = self
                .conv
                .settings
                .active_config()
                .effective_context_window(self.conv.settings.context_window_default);
            let est_tokens = self.estimated_session_context_tokens(key);
            if max_tokens > 0
                && est_tokens as f32
                    >= super::compaction::AUTO_COMPACT_THRESHOLD * max_tokens as f32
                && self.compactable_turns(key) > super::compaction::COMPACT_KEEP_RECENT_TURNS
            {
                let images = std::mem::take(&mut self.conv.pending_images);
                self.conv.input.clear();
                self.start_compaction(key, Some(super::compaction::QueuedSend { text, images }));
                return;
            }
        }

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

        {
            let run = self.run_state_mut(key);
            run.agent_ack = false;
            run.begin_waiting_response();
            run.stream_error = None;
        }

        // Expand @path mentions into attached file/folder context for the agent.
        let cwd = PathBuf::from(&self.conv.workspaces[key.workspace_idx].root_path);
        let expanded = super::mentions::expand_at_mentions(&text, &cwd);
        self.materialize_prompt(key, &expanded, &user_attachments);
        let root_path = self.conv.workspaces[key.workspace_idx].root_path.clone();
        if let Err(e) =
            session_store::save_session_messages(&root_path, self.session_mut_by_key(key))
        {
            self.run_state_mut(key).stream_error = Some(format!("Save session: {e}"));
        } else if key == self.active_session_key() {
            self.persist_active_session_selection();
        }

        if let Err(e) = self.send_prompt_payload(key) {
            self.run_state_mut(key).stream_error = Some(e.clone());
            let sess = self.session_mut_by_key(key);
            if let Some(last) = sess.messages.last_mut()
                && last.role == MsgRole::Assistant
            {
                last.finish_streaming();
                last.blocks = vec![AssistantBlock::Answer(format!("[Send error] {e}"))];
            }
            let run = self.run_state_mut(key);
            run.end_waiting_response();
            run.agent_ack = false;
        }
    }

    pub(crate) fn materialize_prompt(
        &mut self,
        key: SessionKey,
        text: &str,
        attachments: &[UserAttachment],
    ) {
        let sess = self.session_mut_by_key(key);
        sess.messages_loaded = true;
        sess.messages.push(ChatMessage {
            role: MsgRole::User,
            text: text.to_string(),
            is_summary: false,
            attachments: attachments.to_vec(),
            blocks: vec![],
            streaming: false,
            started_at: None,
            worked_duration: None,
        });
        sess.messages.push(ChatMessage {
            role: MsgRole::Assistant,
            text: String::new(),
            is_summary: false,
            attachments: vec![],
            blocks: vec![],
            streaming: true,
            started_at: Some(std::time::Instant::now()),
            worked_duration: None,
        });
    }

    pub(crate) fn last_assistant_mut(&mut self, key: SessionKey) -> Option<&mut ChatMessage> {
        self.session_mut_by_key(key)
            .messages
            .iter_mut()
            .rev()
            .find(|m| m.role == MsgRole::Assistant)
    }

    pub(crate) fn on_text_block_start(&mut self, key: SessionKey) {
        let Some(m) = self.last_assistant_mut(key) else {
            return;
        };
        if let Some(AssistantBlock::Answer(s)) = m.blocks.last()
            && s.is_empty()
        {
            return;
        }
        m.blocks.push(AssistantBlock::Answer(String::new()));
    }

    pub(crate) fn append_text_delta(&mut self, key: SessionKey, delta: &str) {
        let Some(m) = self.last_assistant_mut(key) else {
            return;
        };
        match m.blocks.last_mut() {
            Some(AssistantBlock::Answer(s)) => s.push_str(delta),
            _ => m.blocks.push(AssistantBlock::Answer(delta.to_string())),
        }
    }

    pub(crate) fn append_thinking_delta(&mut self, key: SessionKey, delta: &str) {
        let Some(m) = self.last_assistant_mut(key) else {
            return;
        };
        // TextStart creates an empty Answer before any SSE deltas; reasoning often streams first.
        // Keep Thinking before that placeholder so the collapsible block is not hidden after an empty answer.
        if let Some(AssistantBlock::Thinking(s)) = m.blocks.last_mut() {
            s.push_str(delta);
            return;
        }
        if matches!(m.blocks.last(), Some(AssistantBlock::Answer(s)) if s.is_empty()) {
            let n = m.blocks.len();
            if n >= 2 {
                let i = n - 2;
                if let AssistantBlock::Thinking(s) = &mut m.blocks[i] {
                    s.push_str(delta);
                    return;
                }
            }
            let empty = m.blocks.pop().unwrap();
            m.blocks.push(AssistantBlock::Thinking(delta.to_string()));
            m.blocks.push(empty);
            return;
        }
        m.blocks.push(AssistantBlock::Thinking(delta.to_string()));
    }

    /// Drop the partial text/thinking of the round being retried after a mid-stream
    /// failure, so the regenerated round is not shown twice. Tool blocks from
    /// completed rounds are kept; in-flight tools (early `ToolStart` before args
    /// finished / execution began) are dropped so they don't linger as ghost pills.
    pub(crate) fn reset_streaming_tail(&mut self, key: SessionKey) {
        let Some(m) = self.last_assistant_mut(key) else {
            return;
        };
        if !m.streaming {
            return;
        }
        while matches!(
            m.blocks.last(),
            Some(
                AssistantBlock::Answer(_)
                    | AssistantBlock::Thinking(_)
                    | AssistantBlock::Tool { is_error: None, .. }
            )
        ) {
            m.blocks.pop();
        }
    }

    pub(crate) fn append_assistant_answer(&mut self, key: SessionKey, s: &str) {
        let Some(m) = self.last_assistant_mut(key) else {
            return;
        };
        match m.blocks.last_mut() {
            Some(AssistantBlock::Answer(a)) => a.push_str(s),
            _ => m.blocks.push(AssistantBlock::Answer(s.to_string())),
        }
    }

    pub(crate) fn start_tool_block(
        &mut self,
        key: SessionKey,
        name: &str,
        tool_call_id: Option<&str>,
        args: Option<&serde_json::Value>,
    ) {
        let Some(m) = self.last_assistant_mut(key) else {
            return;
        };
        let id = tool_call_id.unwrap_or("").to_string();
        let args_summary = args.map(|a| {
            let s = a.to_string();
            s.chars().take(800).collect::<String>()
        });
        // Providers may emit an early ToolStart (name+id, args still streaming) and a
        // second ToolStart with full args right before execution. Update the existing
        // block's summary instead of creating a duplicate pill.
        if !id.is_empty() {
            for b in m.blocks.iter_mut().rev() {
                if let AssistantBlock::Tool {
                    tool_call_id: tid,
                    args_summary: existing,
                    ..
                } = b
                    && tid == &id
                {
                    if args_summary.is_some() {
                        *existing = args_summary;
                    }
                    return;
                }
            }
        } else if let Some(AssistantBlock::Tool {
            name: n,
            output,
            args_summary: existing,
            ..
        }) = m.blocks.last_mut()
            && n == name
            && output.is_empty()
        {
            if args_summary.is_some() {
                *existing = args_summary;
            }
            return;
        }
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

    pub(crate) fn set_tool_output(
        &mut self,
        key: SessionKey,
        tool_call_id: Option<&str>,
        text: &str,
        truncated: bool,
    ) {
        let Some(m) = self.last_assistant_mut(key) else {
            return;
        };
        set_tool_output_on_blocks(&mut m.blocks, tool_call_id, text, truncated);
    }

    pub(crate) fn finalize_tool_run(
        &mut self,
        key: SessionKey,
        tool_call_id: Option<&str>,
        is_error: Option<bool>,
        full_output_path: Option<String>,
        diff: Option<String>,
    ) {
        let Some(m) = self.last_assistant_mut(key) else {
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
                && (tid.is_empty() || id == tid)
            {
                *d = diff;
                *ie = is_error;
                *fp = full_output_path;
                return;
            }
        }
    }

    pub(crate) fn send_prompt_payload(&mut self, key: SessionKey) -> Result<(), String> {
        let cwd = PathBuf::from(self.conv.workspaces[key.workspace_idx].root_path.trim());
        let chat = {
            let s = self.session_mut_by_key(key);
            if s.messages.len() < 2 {
                return Err("internal: expected user and assistant messages".into());
            }
            s.messages[..s.messages.len() - 1].to_vec()
        };
        let settings = self.conv.settings.clone();
        let system = crate::agent::prompt::build_system_prompt_for_workspace(&settings, &cwd);
        let tools = crate::agent::tools::tool_definitions_json(
            &settings.tools_enabled,
            settings.bash_timeout_cap_secs,
        );
        let wire_fingerprint = wire_fingerprint_for(&settings, &system, &tools);
        let session_file = self.conv.workspaces[key.workspace_idx].sessions[key.session_idx]
            .session_file
            .clone();
        let prior_wire = self.run_state(key).and_then(|run| {
            (run.wire_session_file == session_file && run.wire_fingerprint == wire_fingerprint)
                .then(|| run.wire_history.clone())
                .flatten()
        });
        let used_prior_wire = prior_wire.is_some();
        let chars_per_token = self.calibrated_chars_per_token(key);
        // Stable key for the per-session ACP subprocess: prefer the session file, falling back
        // to a synthetic id for an as-yet-unsaved chat. Once a chat has been saved it switches
        // from the `mem:` key (used while warming a brand-new chat) to its file path, so retire
        // the now-orphaned warm subprocess to avoid leaking an idle Claude Code process.
        let acp_session_key = match &session_file {
            Some(path) => {
                self.acp
                    .close(&format!("mem:{}:{}", key.workspace_idx, key.session_idx));
                path.clone()
            }
            None => format!("mem:{}:{}", key.workspace_idx, key.session_idx),
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let (approval_tx, approval_rx) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let _join = spawn_agent_run(
            settings,
            self.tunnels.clone(),
            self.acp.clone(),
            acp_session_key,
            cwd,
            chat,
            tx,
            approval_rx,
            cancel.clone(),
            prior_wire,
            chars_per_token,
        );
        let existing_wire = self.run_state(key).and_then(|run| run.wire_history.clone());
        let run = self.run_state_mut(key);
        run.agent_rx = Some(rx);
        run.approval_tx = Some(approval_tx);
        run.pending_approval = None;
        run.cancel_agent = Some(cancel);
        run.wire_fingerprint = wire_fingerprint;
        run.wire_session_file = session_file;
        if !used_prior_wire && existing_wire.is_some() {
            run.wire_history = None;
        }
        Ok(())
    }

    pub(crate) fn finish_assistant_stream(&mut self, key: SessionKey) {
        if let Some(last) = self.conv.workspaces[key.workspace_idx].sessions[key.session_idx]
            .messages
            .iter_mut()
            .rev()
            .find(|m| m.role == MsgRole::Assistant)
        {
            last.finish_streaming();
        }
        if key == self.active_session_key() {
            self.conv.stick_bottom_hold_frames = 3;
        }
        let root_path = self.conv.workspaces[key.workspace_idx].root_path.clone();
        if let Err(e) =
            session_store::save_session_messages(&root_path, self.session_mut_by_key(key))
        {
            self.run_state_mut(key).stream_error = Some(format!("Save session: {e}"));
        } else if key == self.active_session_key() {
            self.persist_active_session_selection();
        }
        let completed_turn_usage = self
            .run_state(key)
            .map(|run| run.turn_usage)
            .filter(|usage| !usage.is_zero());
        let session_file = self.conv.workspaces[key.workspace_idx].sessions[key.session_idx]
            .session_file
            .clone();
        let run = self.run_state_mut(key);
        if let Some(usage) = completed_turn_usage {
            run.last_turn_usage = usage;
        }
        run.wire_session_file = session_file;
        run.end_waiting_response();
        run.agent_ack = false;
        run.cancel_agent = None;
        run.agent_rx = None;
        run.approval_tx = None;
        run.pending_approval = None;
    }
}
