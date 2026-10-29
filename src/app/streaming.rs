use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::agent::events::AgentEvent;
use crate::agent::runner::AgentRunRequest;
use crate::agent::spawn_agent_run;
use crate::model::{
    AssistantBlock, ChatMessage, MsgRole, UserAttachment, make_session_title,
    set_tool_output_on_blocks,
};
use eframe::egui;

use super::{OxiApp, SessionKey};
use crate::session_store;

impl OxiApp {
    pub(crate) fn send_message(&mut self) {
        if self.conv.composer.editing_last_prompt.is_some() {
            self.submit_edited_last_prompt();
        } else {
            let mirror = self.split_mirror_payload();
            self.send_message_opts(false);
            if let Some(payload) = mirror {
                self.send_to_split_partner(payload);
            }
        }
    }

    pub(crate) fn begin_edit_last_prompt(&mut self) {
        let key = self.active_session_key();
        if self.active_waiting_response() || self.compaction_active_for(key) {
            self.notify_composer(
                "Wait for the current operation to finish before editing the prompt.",
            );
            return;
        }
        let Some(journal) = self.run_state(key).and_then(|run| run.undo_journal.clone()) else {
            self.notify_composer("This older response has no restorable file checkpoint.");
            return;
        };
        if let Some(reason) = journal
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .unavailable_reason()
        {
            self.notify_composer(format!("Edit & retry is unavailable: {reason}"));
            return;
        }
        let Some(user) = self
            .active_session()
            .messages
            .iter()
            .rev()
            .find(|m| m.role == MsgRole::User && !m.is_summary)
            .cloned()
        else {
            return;
        };
        let previous_input = std::mem::take(&mut self.conv.composer.input);
        let previous_images = std::mem::take(&mut self.conv.composer.pending_images);
        let previous_texts = std::mem::take(&mut self.conv.composer.pending_texts);
        self.conv.composer.pending_texts = user
            .attachments
            .iter()
            .filter(|a| matches!(a, UserAttachment::Text { .. }))
            .cloned()
            .collect();
        self.conv.composer.input = self
            .run_state(key)
            .and_then(|run| run.last_user_prompt.clone())
            .unwrap_or(user.text);
        self.conv.composer.pending_images = user
            .attachments
            .into_iter()
            .filter_map(|a| match a {
                UserAttachment::Image { mime, data } => Some((mime, data)),
                _ => None,
            })
            .collect();
        self.conv.composer.editing_last_prompt = Some(super::state::PromptEditState {
            previous_input,
            previous_images,
            previous_texts,
        });
        self.conv.composer.focus_next_frame = true;
    }

    pub(crate) fn cancel_edit_last_prompt(&mut self) {
        if let Some(edit) = self.conv.composer.editing_last_prompt.take() {
            self.conv.composer.input = edit.previous_input;
            self.conv.composer.pending_images = edit.previous_images;
            self.conv.composer.pending_texts = edit.previous_texts;
        }
    }

    fn submit_edited_last_prompt(&mut self) {
        let key = self.active_session_key();
        if self.conv.composer.input.trim().is_empty()
            && self.conv.composer.pending_images.is_empty()
            && self.conv.composer.pending_texts.is_empty()
        {
            return;
        }
        let journal = self.run_state(key).and_then(|r| r.undo_journal.clone());
        if let Some(journal) = journal {
            let root = PathBuf::from(&self.conv.workspaces[key.workspace_idx].root_path);
            if let Err(e) = journal
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .restore(&root)
            {
                self.notify_composer(format!("Cannot restore this response: {e}"));
                return;
            }
        }
        let Some(user_idx) = self
            .active_session()
            .messages
            .iter()
            .rposition(|m| m.role == MsgRole::User && !m.is_summary)
        else {
            return;
        };
        self.active_session_mut().messages.truncate(user_idx);
        self.invalidate_wire_cache(key);
        self.reset_acp_session(key);
        self.conv.composer.editing_last_prompt = None;
        self.send_message_opts(true);
    }

    /// Index of the most recent real (non-summary) user message in the active chat.
    pub(crate) fn last_user_prompt_index(&self) -> Option<usize> {
        self.active_session()
            .messages
            .iter()
            .rposition(|m| m.role == MsgRole::User && !m.is_summary)
    }

    /// Re-sends the last prompt after a failed run: drops that prompt and everything after it,
    /// then submits it again. Whatever is in the composer is kept as a draft.
    pub(crate) fn retry_last_prompt(&mut self) {
        let key = self.active_session_key();
        if self.active_waiting_response() || self.compaction_active_for(key) {
            return;
        }
        let Some(user_idx) = self.last_user_prompt_index() else {
            return;
        };
        let user = self.active_session().messages[user_idx].clone();
        let text = self
            .run_state(key)
            .and_then(|run| run.last_user_prompt.clone())
            .unwrap_or(user.text);
        let draft_input = std::mem::replace(&mut self.conv.composer.input, text);
        let draft_texts = std::mem::replace(
            &mut self.conv.composer.pending_texts,
            user.attachments
                .iter()
                .filter(|a| matches!(a, UserAttachment::Text { .. }))
                .cloned()
                .collect(),
        );
        let draft_images = std::mem::replace(
            &mut self.conv.composer.pending_images,
            user.attachments
                .into_iter()
                .filter_map(|a| match a {
                    UserAttachment::Image { mime, data } => Some((mime, data)),
                    _ => None,
                })
                .collect(),
        );
        self.active_session_mut().messages.truncate(user_idx);
        self.invalidate_wire_cache(key);
        self.reset_acp_session(key);
        self.run_state_mut(key).stream_error = None;
        self.send_message_opts(false);
        if self.conv.composer.input.is_empty()
            && self.conv.composer.pending_images.is_empty()
            && self.conv.composer.pending_texts.is_empty()
        {
            self.conv.composer.input = draft_input;
            self.conv.composer.pending_images = draft_images;
            self.conv.composer.pending_texts = draft_texts;
        }
    }

    /// The most recent other agent that answered in this chat, if any.
    pub(crate) fn other_agent_in_chat(
        &mut self,
        key: SessionKey,
    ) -> Option<crate::settings::LlmProviderKind> {
        let current = self.ensure_session_config(key).provider;
        self.session_by_key(key)
            .messages
            .iter()
            .rev()
            .filter_map(|m| m.route.as_deref())
            .map(|r| r.provider)
            .find(|p| *p != current && *p != crate::settings::LlmProviderKind::Router)
    }

    /// After a failed turn, switch the chat to `kind` and ask it to finish the request. Unlike
    /// Retry, the failed turn stays: the new agent gets it as context, partial work included.
    pub(crate) fn continue_with_agent(
        &mut self,
        kind: crate::settings::LlmProviderKind,
        error: &str,
    ) {
        let key = self.active_session_key();
        if self.active_waiting_response() || self.compaction_active_for(key) {
            return;
        }
        let failed = self.ensure_session_config(key).provider;
        self.set_active_session_provider(kind);
        self.save_settings_quietly();
        self.run_state_mut(key).stream_error = None;
        let reason = error.lines().next().unwrap_or("").trim();
        let text = format!(
            "Continue my last request. {} stopped with an error before finishing ({reason}). Check what it already changed, then carry on from there.",
            crate::app::composer::composer_provider_label(failed)
        );
        let draft_input = std::mem::replace(&mut self.conv.composer.input, text);
        let draft_texts = std::mem::take(&mut self.conv.composer.pending_texts);
        let draft_images = std::mem::take(&mut self.conv.composer.pending_images);
        self.send_message_opts(false);
        self.conv.composer.input = draft_input;
        self.conv.composer.pending_texts = draft_texts;
        self.conv.composer.pending_images = draft_images;
    }

    /// `skip_autocompact` is set when an auto-compaction has just finished and is replaying
    /// the deferred message — it must not re-trigger the threshold check (loop guard).
    pub(crate) fn send_message_opts(&mut self, skip_autocompact: bool) {
        let key = self.active_session_key();
        self.send_message_for(key, skip_autocompact);
    }

    /// Send the composer contents into `key`'s chat. While that chat is still answering, the
    /// message is queued instead and goes out when the response finishes.
    pub(super) fn send_message_for(&mut self, key: SessionKey, skip_autocompact: bool) {
        let text = self.conv.composer.input.trim().to_string();
        let has_attachments = !self.conv.composer.pending_images.is_empty()
            || !self.conv.composer.pending_texts.is_empty();
        if text.is_empty() && !has_attachments {
            return;
        }
        // A turn run from an editor (the headless daemon) a moment ago must not be overwritten.
        self.reload_session_if_changed(key);
        let live_evidence_toggle = !has_attachments
            && matches!(
                super::compaction::parse_slash_command(&text),
                Some(super::compaction::SlashCommand::Evidence(None))
            );
        if !live_evidence_toggle
            && self
                .run_state(key)
                .is_some_and(|state| state.waiting_response)
        {
            self.push_input_history(&text);
            self.conv.composer.history_index = None;
            self.conv.composer.history_draft.clear();
            let queued = self.take_composer_payload();
            self.run_state_mut(key).queued.push_back(queued);
            return;
        }

        // Slash commands: `/new` and `/compact` only exact, argument-free and with no attachments.
        // An ACP agent's own command of the same name wins (except `/new` and `/plan`): oxi's
        // compaction can't shrink the context the agent keeps in its subprocess.
        let mut text = text;
        match super::compaction::parse_slash_command(&text) {
            Some(super::compaction::SlashCommand::Plan(task)) => {
                let run = self.run_state_mut(key);
                match task {
                    // `/plan <task>`: plan mode on, then send the task as a normal message.
                    Some(task) => {
                        run.plan_mode = true;
                        text = task;
                        self.save_settings_quietly();
                    }
                    None if !has_attachments => {
                        run.plan_mode = !run.plan_mode;
                        self.save_settings_quietly();
                        self.push_input_history(&text);
                        self.conv.composer.history_index = None;
                        self.conv.composer.history_draft.clear();
                        self.conv.composer.input.clear();
                        return;
                    }
                    // Bare `/plan` with attachments: plan them.
                    None => {
                        run.plan_mode = true;
                        text.clear();
                        self.save_settings_quietly();
                    }
                }
            }
            Some(super::compaction::SlashCommand::Evidence(task)) => {
                let run = self.run_state_mut(key);
                match task {
                    Some(task) => {
                        run.evidence_mode.set(true);
                        text = task;
                    }
                    None => {
                        run.evidence_mode.toggle();
                        let on = run.evidence_mode.enabled();
                        self.push_input_history(&text);
                        self.conv.composer.history_index = None;
                        self.conv.composer.history_draft.clear();
                        self.conv.composer.input.clear();
                        self.notify_composer(if on {
                            "Evidence mode on: changes are checked at the end when proof is needed, including the current response."
                        } else {
                            "Evidence mode off."
                        });
                        return;
                    }
                }
            }
            Some(cmd)
                if !has_attachments
                    && (cmd == super::compaction::SlashCommand::New
                        || !self
                            .acp_commands_for(key.workspace_idx)
                            .iter()
                            .any(|c| c.name == "compact")) =>
            {
                self.push_input_history(&text);
                self.conv.composer.history_index = None;
                self.conv.composer.history_draft.clear();
                self.conv.composer.input.clear();
                match cmd {
                    super::compaction::SlashCommand::Compact => {
                        self.start_compaction(key, None);
                    }
                    _ => self.new_chat(),
                }
                return;
            }
            _ => {}
        }

        self.ensure_session_config(key);
        // Don't send into a session whose history is mid-compaction.
        if self.compaction_active_for(key) {
            self.notify_composer("Context is being compacted — try again in a moment.");
            return;
        }

        // Auto-compaction: if the context is near full, summarize first and defer this send.
        // ACP agents compact their own context, which oxi's transcript summary can't shrink.
        if !skip_autocompact
            && self.conv.compaction.is_none()
            && !self.ensure_session_config(key).provider.is_acp()
        {
            let config = self.ensure_session_config(key);
            let mut provider = self.conv.settings.provider(config.provider).clone();
            provider.model_id = config.model_id;
            provider.effort = config.effort;
            provider.context_window = config.context_window;
            let max_tokens =
                provider.effective_context_window(self.conv.settings.context_window_default);
            let est_tokens = self.estimated_session_context_tokens(key);
            if max_tokens > 0
                && est_tokens as f32
                    >= super::compaction::AUTO_COMPACT_THRESHOLD * max_tokens as f32
                && self.compactable_turns(key) > super::compaction::COMPACT_KEEP_RECENT_TURNS
            {
                let images = std::mem::take(&mut self.conv.composer.pending_images);
                let texts = std::mem::take(&mut self.conv.composer.pending_texts);
                self.conv.composer.input.clear();
                self.notify_composer(
                    "Context is almost full — compacting first; your message will be sent \
                     automatically.",
                );
                self.start_compaction(
                    key,
                    Some(super::compaction::QueuedSend {
                        text,
                        images,
                        texts,
                    }),
                );
                return;
            }
        }

        if self.session_by_key(key).messages.is_empty()
            && self.session_by_key(key).session_file.is_none()
        {
            self.session_mut_by_key(key).title = if !text.is_empty() {
                make_session_title(&text)
            } else {
                if !self.conv.composer.pending_texts.is_empty() {
                    "Pasted text"
                } else {
                    "Image"
                }
                .to_string()
            };
        }

        self.push_input_history(&text);
        self.conv.composer.history_index = None;
        self.conv.composer.history_draft.clear();

        let pending = std::mem::take(&mut self.conv.composer.pending_images);
        let mut user_attachments: Vec<UserAttachment> = pending
            .iter()
            .map(|(mime, data)| UserAttachment::Image {
                mime: mime.clone(),
                data: data.clone(),
            })
            .collect();
        user_attachments.append(&mut self.conv.composer.pending_texts);
        self.conv.composer.input.clear();
        self.conv.transcript.scroll_to_bottom_once = true;

        {
            let run = self.run_state_mut(key);
            run.begin_waiting_response();
            run.stream_error = None;
        }

        // Expand @path mentions into attached file/folder context for the agent.
        let cwd = PathBuf::from(&self.conv.workspaces[key.workspace_idx].root_path);
        let remote = self.is_remote_key(key);
        // A remote chat's files are on its host, whose server expands the mentions.
        let expanded = if remote {
            text.clone()
        } else {
            super::mentions::expand_at_mentions(&text, &cwd)
        };
        self.run_state_mut(key).last_user_prompt = Some(text.clone());
        // The previous cache remains only a candidate for this run. Removing it before the
        // initial save prevents a crash from persisting an advanced transcript with stale wire.
        let wire_candidate = self.session_mut_by_key(key).wire_cache.take();
        self.materialize_prompt(key, &expanded, &user_attachments);
        let root_path = self.conv.workspaces[key.workspace_idx].root_path.clone();
        if remote {
            // The server saves the chat when the turn starts.
        } else if let Err(e) =
            session_store::save_session_messages(&root_path, self.session_mut_by_key(key))
        {
            self.run_state_mut(key).stream_error = Some(format!("Save session: {e}"));
        } else if key == self.active_session_key() {
            self.persist_active_session_selection();
        }

        if let Err(e) = self.send_prompt_payload(key, wire_candidate) {
            self.invalidate_wire_cache(key);
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
        sess.messages.push(ChatMessage::user(text, attachments));
        sess.messages.push(ChatMessage::streaming_assistant(None));
    }

    pub(crate) fn last_assistant_mut(&mut self, key: SessionKey) -> Option<&mut ChatMessage> {
        self.session_mut_by_key(key)
            .messages
            .iter_mut()
            .rev()
            .find(|m| m.role == MsgRole::Assistant)
    }

    pub(crate) fn on_text_block_start(&mut self, key: SessionKey) {
        if let Some(m) = self.last_assistant_mut(key) {
            m.begin_text_block();
        }
    }

    pub(crate) fn append_text_delta(&mut self, key: SessionKey, delta: &str) {
        if let Some(m) = self.last_assistant_mut(key) {
            m.append_answer(delta);
        }
    }

    pub(crate) fn append_thinking_delta(&mut self, key: SessionKey, delta: &str) {
        if let Some(m) = self.last_assistant_mut(key) {
            m.append_thinking(delta);
        }
    }

    /// See [`ChatMessage::reset_streaming_tail`].
    pub(crate) fn reset_streaming_tail(&mut self, key: SessionKey) {
        if let Some(m) = self.last_assistant_mut(key) {
            m.reset_streaming_tail();
        }
    }

    pub(crate) fn append_assistant_answer(&mut self, key: SessionKey, s: &str) {
        if let Some(m) = self.last_assistant_mut(key) {
            m.append_answer(s);
        }
    }

    pub(crate) fn start_tool_block(
        &mut self,
        key: SessionKey,
        name: &str,
        tool_call_id: Option<&str>,
        args: Option<&serde_json::Value>,
    ) {
        if let Some(m) = self.last_assistant_mut(key) {
            m.start_tool(name, tool_call_id, args);
        }
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
        if let Some(m) = self.last_assistant_mut(key) {
            m.finish_tool(tool_call_id, is_error, full_output_path, diff);
        }
    }

    pub(crate) fn send_prompt_payload(
        &mut self,
        key: SessionKey,
        wire_candidate: Option<crate::model::WireCache>,
    ) -> Result<(), String> {
        if self.is_remote_key(key) {
            return self.start_remote_turn(key);
        }
        let cwd = PathBuf::from(self.conv.workspaces[key.workspace_idx].root_path.trim());
        let chat = {
            let s = self.session_mut_by_key(key);
            if s.messages.len() < 2 {
                return Err("internal: expected user and assistant messages".into());
            }
            s.messages[..s.messages.len() - 1].to_vec()
        };
        let mut settings = self.conv.settings.clone();
        let session_config = self.ensure_session_config(key);
        session_config.apply_to_settings(&mut settings);
        // Under the Router the real target arrives with `AgentEvent::Routed`; a directly chosen
        // model is recorded on the reply now.
        let direct =
            (session_config.provider != crate::settings::LlmProviderKind::Router).then(|| {
                crate::model::RouteNote::direct(
                    session_config.provider,
                    &session_config.model_id,
                    &session_config.effort,
                )
            });
        self.run_state_mut(key).usage_target = direct
            .as_ref()
            .map(|note| (note.provider, note.model.clone()));
        if let Some(note) = direct
            && let Some(reply) = self.last_assistant_mut(key)
        {
            reply.route = Some(Box::new(note));
        }
        let session_file = self.conv.workspaces[key.workspace_idx].sessions[key.session_idx]
            .session_file
            .clone();
        let chars_per_token = self.calibrated_chars_per_token(key);
        // Stable key for the per-session ACP subprocess: prefer the session file, falling back
        // to a synthetic id for an as-yet-unsaved chat. Once a chat has been saved it switches
        // from the `mem:` key (used while warming a brand-new chat) to its file path, so retire
        // the now-orphaned warm subprocess to avoid leaking an idle Claude Code process.
        if session_file.is_some() {
            self.acp
                .close(&format!("mem:{}:{}", key.workspace_idx, key.session_idx));
        }
        let acp_session_key = self.acp_session_key(key);
        let (tx, rx) = relay_waking_ui(self.conv.git_ctx.clone());
        let (approval_tx, approval_rx) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let undo_journal = Arc::new(std::sync::Mutex::new(
            crate::agent::tools::TurnUndoJournal::default(),
        ));
        self.run_state_mut(key).undo_journal = Some(undo_journal.clone());
        let plan_mode = self.run_state(key).is_some_and(|r| r.plan_mode);
        let evidence_mode = self.run_state_mut(key).evidence_mode.clone();
        self.run_state_mut(key).last_turn_planned = plan_mode;
        let request = AgentRunRequest {
            settings,
            tunnels: self.tunnels.clone(),
            acp: self.acp.clone(),
            mcp: self.mcp.clone(),
            acp_session_key,
            cwd,
            chat_for_history: chat,
            approval_rx,
            cancel: cancel.clone(),
            wire_candidate,
            chars_per_token,
            plan_mode,
            evidence_mode,
            undo_journal,
        };
        let _join = spawn_agent_run(&self.agent_executor, request, tx);
        let run = self.run_state_mut(key);
        run.agent_rx = Some(rx);
        run.approval_tx = Some(approval_tx);
        run.pending_approval = None;
        run.cancel_agent = Some(cancel);
        Ok(())
    }

    /// Show the turns ACP agents started on their own (a background command finished, a
    /// scheduled wake-up fired) in their chats, as soon as each chat is free.
    pub(crate) fn attach_autonomous_turns(&mut self, ctx: &egui::Context) {
        let waker = ctx.clone();
        crate::agent::acp::set_autonomous_waker(move || waker.request_repaint());
        self.apply_agent_titles(ctx);
        for (acp_key, root, params) in crate::agent::acp::take_delegated() {
            if let Some(key) = self.session_key_for_acp(&acp_key) {
                self.ensure_session_loaded(key);
                let delegated = &mut self.session_mut_by_key(key).delegated;
                if delegated.apply(&root, &params) {
                    delegated.mark_unsaved();
                }
            }
        }
        self.save_delegated_chats(ctx);
        for acp_key in crate::agent::acp::pending_autonomous() {
            let Some(key) = self.session_key_for_acp(&acp_key) else {
                crate::agent::acp::claim_autonomous(&acp_key);
                continue;
            };
            // A turn in flight shows it itself; one that is ending leaves it to the next frame.
            if self
                .run_state(key)
                .is_some_and(|run| run.waiting_response || run.agent_rx.is_some())
            {
                continue;
            }
            crate::agent::acp::claim_autonomous(&acp_key);
            self.start_autonomous_turn(key, acp_key);
        }
    }

    /// Write chats whose subagent transcripts changed between turns, at most once per
    /// [`crate::model::delegated::SAVE_DEBOUNCE`]: a running workflow reports every second, and
    /// each save rewrites the whole transcript on this thread.
    fn save_delegated_chats(&mut self, ctx: &egui::Context) {
        if self.conn.no_session {
            return;
        }
        let mut due = Vec::new();
        for (wi, workspace) in self.conv.workspaces.iter().enumerate() {
            for (si, session) in workspace.sessions.iter().enumerate() {
                match session.delegated.save_wait() {
                    Some(wait) if wait.is_zero() => due.push(self.session_key(wi, si)),
                    Some(wait) => ctx.request_repaint_after(wait),
                    None => {}
                }
            }
        }
        for key in due {
            if self.is_remote_key(key) {
                // The host's server saves a remote chat.
                self.session_mut_by_key(key).delegated.unsaved_since = None;
                continue;
            }
            let root = self.conv.workspaces[key.workspace_idx].root_path.clone();
            if let Err(e) =
                session_store::save_session_messages(&root, self.session_mut_by_key(key))
            {
                log::warn!("saving subagent transcript: {e}");
                self.session_mut_by_key(key).delegated.unsaved_since = None;
            }
        }
    }

    /// The chat an ACP session key belongs to.
    pub(crate) fn session_key_for_acp(&self, acp_key: &str) -> Option<SessionKey> {
        self.conv.workspaces.iter().enumerate().find_map(|(w, ws)| {
            (0..ws.sessions.len())
                .map(|s| SessionKey {
                    workspace_idx: w,
                    session_idx: s,
                })
                .find(|key| self.acp_session_key(*key) == acp_key)
        })
    }

    fn start_autonomous_turn(&mut self, key: SessionKey, acp_session_key: String) {
        let session = self.session_by_key(key);
        let disk_mtime = session
            .session_file
            .as_deref()
            .and_then(|f| std::fs::metadata(f).and_then(|m| m.modified()).ok());
        if !session.messages_loaded
            && let Some(file) = session.session_file.clone()
            && let Some((loaded, wire)) = session_store::load_session_messages_with_wire(&file)
        {
            let session = self.session_mut_by_key(key);
            session.disk_mtime = disk_mtime;
            session.delegated = loaded.delegated;
            session.messages = loaded.messages;
            session.config = loaded.config;
            session.wire_cache = wire;
        }
        let session = self.session_mut_by_key(key);
        session.messages_loaded = true;
        // The agent that ran last is the one still attached to the chat.
        let route = session
            .messages
            .iter()
            .rev()
            .find_map(|m| m.route.clone())
            .filter(|r| r.provider.is_acp());
        session
            .messages
            .push(ChatMessage::streaming_assistant(route));
        if key == self.active_session_key() {
            self.conv.transcript.scroll_to_bottom_once = true;
        }

        let cwd = PathBuf::from(self.conv.workspaces[key.workspace_idx].root_path.trim());
        let mut settings = self.conv.settings.clone();
        let session_config = self.ensure_session_config(key);
        session_config.apply_to_settings(&mut settings);
        let (tx, rx) = relay_waking_ui(self.conv.git_ctx.clone());
        let (approval_tx, approval_rx) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let undo_journal = Arc::new(std::sync::Mutex::new(
            crate::agent::tools::TurnUndoJournal::default(),
        ));
        let plan_mode = self.run_state(key).is_some_and(|r| r.plan_mode);
        let run = crate::agent::AcpAttachRun {
            settings,
            acp: self.acp.clone(),
            acp_session_key,
            cwd,
            approval_rx,
            cancel: cancel.clone(),
            plan_mode,
            undo_journal: undo_journal.clone(),
        };
        let _join = crate::agent::spawn_acp_attach(&self.agent_executor, run, tx);
        let run = self.run_state_mut(key);
        run.begin_waiting_response();
        run.stream_error = None;
        run.last_turn_planned = plan_mode;
        run.undo_journal = Some(undo_journal);
        run.agent_rx = Some(rx);
        run.approval_tx = Some(approval_tx);
        run.pending_approval = None;
        run.cancel_agent = Some(cancel);
    }

    /// Key of the chat's ACP agent subprocess: its session file, or a synthetic id while unsaved.
    pub(crate) fn acp_session_key(&self, key: SessionKey) -> String {
        match &self.session_by_key(key).session_file {
            Some(path) => path.clone(),
            None => format!("mem:{}:{}", key.workspace_idx, key.session_idx),
        }
    }

    /// Drop the ACP agent's own memory of this chat. Used when oxi rewrites the transcript
    /// (rewind, retry, fork): the next prompt starts a fresh agent session that is handed oxi's
    /// transcript instead.
    pub(crate) fn reset_acp_session(&mut self, key: SessionKey) {
        // Every agent the chat used keeps a session, not only the selected one.
        let session_key = self.acp_session_key(key);
        self.acp.close(&session_key);
    }

    pub(crate) fn invalidate_wire_cache(&mut self, key: SessionKey) {
        self.session_mut_by_key(key).wire_cache = None;
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
        // Do not force the transcript back to the bottom when streaming ends. The user may
        // have scrolled up to read earlier content while the response was being generated.
        let root_path = self.conv.workspaces[key.workspace_idx].root_path.clone();
        if self.is_remote_key(key) {
            // Saved by the server; take its version of the reply.
            self.remote_reconcile(key);
        } else if let Err(e) =
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
        let run = self.run_state_mut(key);
        if let Some(usage) = completed_turn_usage {
            run.last_turn_usage = usage;
        }
        run.end_waiting_response();
        run.cancel_agent = None;
        run.agent_rx = None;
        run.approval_tx = None;
        run.pending_approval = None;
    }
}

/// Agent event channel whose sends also wake the UI.
///
/// The worker only pushes into a plain channel, and the UI drains it when it paints. Without a
/// wake-up, streamed text reached the screen on the 50 ms fallback tick in bursts (20 updates a
/// second) instead of on the next frame. A relay thread forwards each event and requests a
/// repaint; egui coalesces those into at most one frame per vsync. When the worker hangs up, the
/// relay drops its sender too, so the UI still sees `Disconnected` exactly as before.
fn relay_waking_ui(
    ctx: egui::Context,
) -> (
    std::sync::mpsc::Sender<AgentEvent>,
    std::sync::mpsc::Receiver<AgentEvent>,
) {
    let (worker_tx, worker_rx) = std::sync::mpsc::channel::<AgentEvent>();
    let (ui_tx, ui_rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("oxi-agent-events".into())
        .spawn(move || {
            for event in worker_rx {
                if ui_tx.send(event).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
        });
    match spawned {
        Ok(_) => (worker_tx, ui_rx),
        // No relay thread: fall back to a direct channel drained on the fallback tick.
        Err(_) => std::sync::mpsc::channel(),
    }
}

#[cfg(test)]
mod relay_tests {
    use super::*;

    #[test]
    fn relay_forwards_in_order_wakes_the_ui_and_reports_hangup() {
        let ctx = egui::Context::default();
        let woken = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&woken);
        ctx.set_request_repaint_callback(move |_| {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        let (tx, rx) = relay_waking_ui(ctx);
        for delta in ["a", "b", "c"] {
            tx.send(AgentEvent::TextDelta(delta.into())).unwrap();
        }
        drop(tx);
        let timeout = std::time::Duration::from_secs(5);
        let received: Vec<String> = (0..3)
            .map(|_| match rx.recv_timeout(timeout).unwrap() {
                AgentEvent::TextDelta(delta) => delta,
                _ => panic!("unexpected event"),
            })
            .collect();
        assert_eq!(received, ["a", "b", "c"]);
        assert_eq!(
            rx.recv_timeout(timeout).unwrap_err(),
            std::sync::mpsc::RecvTimeoutError::Disconnected
        );
        assert!(woken.load(std::sync::atomic::Ordering::SeqCst) >= 1);
    }
}
