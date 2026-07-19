//! Context compaction: replace older conversation turns with a single LLM-generated
//! summary so long chats keep fitting the model's context window. Triggered manually with
//! the `/compact` slash command or automatically just before sending when the estimated
//! context exceeds [`AUTO_COMPACT_THRESHOLD`].
//!
//! The summary is stored as a synthetic [`MsgRole::User`] message with `is_summary = true`
//! (see [`crate::model::ChatMessage`]); this round-trips through session persistence and
//! degrades to a plain user message on builds that predate the flag.

use eframe::egui;

use crate::agent::{CompleteEvent, CompleteRequest, spawn_completion};
use crate::model::{ChatMessage, MsgRole};

use super::{OxiApp, SessionKey};

/// Keep this many most-recent user turns verbatim; everything older is summarized.
pub(crate) const COMPACT_KEEP_RECENT_TURNS: usize = 4;
/// Auto-compact when estimated context reaches this fraction of the window.
pub(crate) const AUTO_COMPACT_THRESHOLD: f32 = 0.85;
/// Hard cap on the transcript handed to the summarizer; older text is dropped (tail kept).
const COMPACT_TRANSCRIPT_CAP_CHARS: usize = 300_000;

/// A user message deferred while an auto-compaction runs; sent once compaction completes.
pub(crate) struct QueuedSend {
    pub text: String,
    pub images: Vec<(String, Vec<u8>)>,
}

/// State for the one in-flight compaction (there is at most one app-wide).
pub(crate) struct ActiveCompaction {
    pub key: SessionKey,
    /// Identity of the target session at kickoff; the result is discarded if the session
    /// was replaced (switched/deleted) meanwhile.
    pub session_file: Option<String>,
    pub rx: std::sync::mpsc::Receiver<CompleteEvent>,
    /// `messages[..split_len]` are being summarized; `messages[split_len..]` are kept.
    pub split_len: usize,
    /// Message to auto-send after an auto-triggered compaction finishes.
    pub queued_send: Option<QueuedSend>,
}

/// The two slash commands understood by the composer.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SlashCommand {
    New,
    Compact,
}

/// Recognize an exact, argument-free `/new` or `/compact`. Anything else (including
/// `/newfoo`) is `None` and is sent as a normal message.
pub(crate) fn parse_slash_command(text: &str) -> Option<SlashCommand> {
    match text.trim() {
        "/new" => Some(SlashCommand::New),
        "/compact" => Some(SlashCommand::Compact),
        _ => None,
    }
}

/// Indices of user messages — the start of each conversational turn.
fn user_turn_starts(messages: &[ChatMessage]) -> Vec<usize> {
    messages
        .iter()
        .enumerate()
        .filter(|(_, m)| matches!(m.role, MsgRole::User))
        .map(|(i, _)| i)
        .collect()
}

/// Split index: `messages[..split]` get summarized, `messages[split..]` are kept verbatim.
/// `None` when there aren't more than `keep_recent` user turns (nothing worth compacting).
pub(crate) fn compaction_split_len(messages: &[ChatMessage], keep_recent: usize) -> Option<usize> {
    let starts = user_turn_starts(messages);
    if starts.len() <= keep_recent {
        return None;
    }
    Some(starts[starts.len() - keep_recent])
}

/// Render `messages` into a plain-text transcript for the summarizer. Assistant blocks are
/// flattened (tool output already truncated) and the whole thing is tail-capped.
pub(crate) fn compaction_transcript(messages: &[ChatMessage]) -> String {
    let mut s = String::new();
    for m in messages {
        match m.role {
            MsgRole::User => {
                s.push_str(if m.is_summary {
                    "[Earlier summary]:\n"
                } else {
                    "User:\n"
                });
                s.push_str(&m.text);
                s.push_str("\n\n");
            }
            MsgRole::Assistant => {
                let flat = crate::agent::flatten_assistant(m);
                if !flat.trim().is_empty() {
                    s.push_str("Assistant:\n");
                    s.push_str(&flat);
                    s.push_str("\n\n");
                }
            }
        }
    }
    if s.len() > COMPACT_TRANSCRIPT_CAP_CHARS {
        let start = s.len() - COMPACT_TRANSCRIPT_CAP_CHARS;
        let cut = crate::agent::tools::floor_char_boundary(&s, start);
        s = format!("[oldest part of the conversation omitted]\n\n{}", &s[cut..]);
    }
    s
}

pub(crate) const COMPACTION_SYSTEM_PROMPT: &str = "You are summarizing a conversation between a user and a coding agent so the \
conversation can continue with less context. Write a dense, factual summary in Markdown \
with these sections, omitting any that are empty:\n\n\
## Goal\nWhat the user is trying to accomplish overall.\n\
## Current state\nWhat has been done so far; what works and what is broken.\n\
## Key decisions\nChoices made and their reasons (designs, APIs, tradeoffs, rejected alternatives).\n\
## Files touched\nBullet list: `path` — what was changed or learned about it.\n\
## Important details\nExact identifiers worth preserving: function/struct names, commands run, \
error messages, version numbers, URLs, config values.\n\
## Pending work\nRemaining tasks, next steps, open questions, anything the user asked for that is not done yet.\n\n\
Rules: be specific and concrete; prefer file paths and symbol names over prose; never invent \
details not present in the transcript; no praise, apologies, or filler; at most ~600 words. \
Output only the summary, nothing else.";

impl OxiApp {
    /// True while a compaction targeting `key` is running (blocks sending for that session).
    pub(crate) fn compaction_active_for(&self, key: SessionKey) -> bool {
        self.conv.compaction.as_ref().is_some_and(|c| c.key == key)
    }

    /// How many user turns in `key` could be summarized right now.
    pub(crate) fn compactable_turns(&self, key: SessionKey) -> usize {
        user_turn_starts(&self.session_by_key(key).messages).len()
    }

    /// Kick off a summarization of the older turns of `key`. No-op (with an inline hint) if a
    /// compaction is already running, the session is mid-run, or there's too little to compact.
    /// `queued_send` is auto-sent when an auto-triggered compaction completes.
    pub(crate) fn start_compaction(&mut self, key: SessionKey, queued_send: Option<QueuedSend>) {
        if self.conv.compaction.is_some() {
            return;
        }
        if self.run_state(key).is_some_and(|s| s.waiting_response) {
            self.run_state_mut(key).stream_error =
                Some("Can't compact while a response is streaming.".to_string());
            return;
        }
        let messages = &self.session_by_key(key).messages;
        let Some(split_len) = compaction_split_len(messages, COMPACT_KEEP_RECENT_TURNS) else {
            self.run_state_mut(key).stream_error =
                Some("Not enough conversation to compact yet.".to_string());
            return;
        };
        let transcript = compaction_transcript(&messages[..split_len]);
        let session_file = self.session_by_key(key).session_file.clone();
        let config = self.conv.settings.active_config().clone();
        let (rx, _handle) = spawn_completion(CompleteRequest {
            config,
            system_prompt: COMPACTION_SYSTEM_PROMPT.to_string(),
            user_prompt: transcript,
            max_chars: Some(8_000),
            effort_override: Some("low".to_string()),
        });
        self.run_state_mut(key).stream_error = None;
        self.conv.compaction = Some(ActiveCompaction {
            key,
            session_file,
            rx,
            split_len,
            queued_send,
        });
    }

    /// Drain the in-flight compaction each frame. On success, replace the summarized prefix
    /// with a single summary message, persist, invalidate the wire cache, and auto-send any
    /// queued message.
    pub(crate) fn drain_compaction(&mut self, ctx: &egui::Context) {
        let Some(active) = self.conv.compaction.as_ref() else {
            return;
        };
        let key = active.key;
        // Collect a terminal result without holding the borrow across the mutations below.
        let result: Result<String, String> = loop {
            match active.rx.try_recv() {
                Ok(CompleteEvent::Delta(_)) => {}
                Ok(CompleteEvent::Done(r)) => break r,
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    break Err("summarizer disconnected".to_string());
                }
            }
        };
        let active = self.conv.compaction.take().expect("compaction present");
        ctx.request_repaint();

        // The target session must still be the one we started on.
        let same_session = key.workspace_idx < self.conv.workspaces.len()
            && key.session_idx < self.conv.workspaces[key.workspace_idx].sessions.len()
            && self.session_by_key(key).session_file == active.session_file;
        if !same_session {
            self.restore_queued_send(active.queued_send);
            return;
        }

        match result {
            Ok(summary) => {
                let summary = summary.trim().to_string();
                if summary.is_empty() {
                    self.run_state_mut(key).stream_error =
                        Some("Compaction produced an empty summary; nothing changed.".to_string());
                    self.restore_queued_send(active.queued_send);
                    return;
                }
                {
                    let sess = self.session_mut_by_key(key);
                    let split = active.split_len.min(sess.messages.len());
                    sess.messages.drain(..split);
                    sess.messages.insert(
                        0,
                        ChatMessage {
                            role: MsgRole::User,
                            text: summary,
                            is_summary: true,
                            attachments: vec![],
                            blocks: vec![],
                            streaming: false,
                            started_at: None,
                            worked_duration: None,
                        },
                    );
                }
                // Invalidate before saving so restart cannot resurrect the un-compacted cache.
                self.invalidate_wire_cache(key);
                let root_path = self.conv.workspaces[key.workspace_idx].root_path.clone();
                if let Err(e) = crate::session_store::save_session_messages(
                    &root_path,
                    self.session_mut_by_key(key),
                ) {
                    self.run_state_mut(key).stream_error = Some(format!("Save session: {e}"));
                }
                if let Some(queued) = active.queued_send {
                    self.conv.input = queued.text;
                    self.conv.pending_images = queued.images;
                    self.send_message_opts(true);
                }
            }
            Err(e) => {
                self.run_state_mut(key).stream_error = Some(format!("Compaction failed: {e}"));
                self.restore_queued_send(active.queued_send);
            }
        }
    }

    /// Put a deferred message back into the composer so an aborted auto-compaction loses nothing.
    fn restore_queued_send(&mut self, queued: Option<QueuedSend>) {
        if let Some(q) = queued {
            self.conv.input = q.text;
            self.conv.pending_images = q.images;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(text: &str) -> ChatMessage {
        ChatMessage {
            role: MsgRole::User,
            text: text.to_string(),
            is_summary: false,
            attachments: vec![],
            blocks: vec![],
            streaming: false,
            started_at: None,
            worked_duration: None,
        }
    }

    fn assistant(answer: &str) -> ChatMessage {
        ChatMessage {
            role: MsgRole::Assistant,
            text: String::new(),
            is_summary: false,
            attachments: vec![],
            blocks: vec![crate::model::AssistantBlock::Answer(answer.to_string())],
            streaming: false,
            started_at: None,
            worked_duration: None,
        }
    }

    fn convo(turns: usize) -> Vec<ChatMessage> {
        let mut v = Vec::new();
        for i in 0..turns {
            v.push(user(&format!("q{i}")));
            v.push(assistant(&format!("a{i}")));
        }
        v
    }

    #[test]
    fn parse_slash_command_matches_exact_only() {
        assert_eq!(parse_slash_command("/new"), Some(SlashCommand::New));
        assert_eq!(
            parse_slash_command("  /compact  "),
            Some(SlashCommand::Compact)
        );
        assert_eq!(parse_slash_command("/newfoo"), None);
        assert_eq!(parse_slash_command("/compact now"), None);
        assert_eq!(parse_slash_command("hello"), None);
    }

    #[test]
    fn split_none_when_at_or_below_keep() {
        assert_eq!(compaction_split_len(&convo(4), 4), None);
        assert_eq!(compaction_split_len(&convo(3), 4), None);
    }

    #[test]
    fn split_keeps_last_n_turns() {
        // 6 turns, keep 4 → summarize the first 2 turns (messages 0..4).
        let msgs = convo(6);
        let split = compaction_split_len(&msgs, 4).unwrap();
        assert_eq!(split, 4);
        // The kept tail starts at a user message.
        assert!(matches!(msgs[split].role, MsgRole::User));
    }

    #[test]
    fn transcript_tail_capped() {
        let big = user(&"x".repeat(COMPACT_TRANSCRIPT_CAP_CHARS + 5_000));
        let out = compaction_transcript(std::slice::from_ref(&big));
        assert!(out.len() <= COMPACT_TRANSCRIPT_CAP_CHARS + 64);
        assert!(out.starts_with("[oldest part"));
    }
}
