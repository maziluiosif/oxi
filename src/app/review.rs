//! Cross-agent review of one response's changes: "Review with Codex" on the "N files changed"
//! card sends the turn's diff to another ACP agent in the same chat, and "Apply fixes with
//! Claude" under that review hands the findings back to the agent that made the changes.

use eframe::egui::Ui;

use super::{OxiApp, SessionKey};
use crate::model::{ChatMessage, MsgRole, TurnChanges, UserAttachment};
use crate::settings::LlmProviderKind;
use crate::theme::*;

/// Name of the diff attached to a review request; it also marks the prompt as one.
pub(crate) const REVIEW_ATTACHMENT: &str = "changes-to-review.diff";
/// First line of the attached diff, followed by the turn's `before..after` snapshot trees: it
/// ties the review to the response that made the changes.
const TURN_LINE: &str = "# oxi turn ";
/// Diff budget for the attachment; bigger files are listed for the reviewer to read itself.
const REVIEW_DIFF_BYTES: usize = 120_000;
/// Reviewers offered after the other agent already in the chat, best known first.
const REVIEWERS: [LlmProviderKind; 6] = [
    LlmProviderKind::CodexAcp,
    LlmProviderKind::ClaudeCodeAcp,
    LlmProviderKind::AntigravityAcp,
    LlmProviderKind::CursorAcp,
    LlmProviderKind::CopilotAcp,
    LlmProviderKind::MuseCodeAcp,
];

/// A finished review at the end of the chat: who wrote the changes and who reviewed them.
#[derive(Clone, Copy)]
pub(crate) struct ReviewFollowup {
    pub author: LlmProviderKind,
    pub reviewer: LlmProviderKind,
}

fn label(kind: LlmProviderKind) -> &'static str {
    super::composer::composer_provider_label(kind)
}

impl OxiApp {
    /// Agents that can review `author`'s changes, the default first: the other agent already
    /// working in this chat, then [`REVIEWERS`] in order.
    pub(crate) fn review_candidates(
        &mut self,
        key: SessionKey,
        author: Option<LlmProviderKind>,
    ) -> Vec<LlmProviderKind> {
        let author = author.unwrap_or_else(|| self.ensure_session_config(key).provider);
        let in_chat = self
            .session_by_key(key)
            .messages
            .iter()
            .rev()
            .filter_map(|m| m.route.as_deref())
            .map(|r| r.provider)
            .find(|p| p.is_acp() && *p != author);
        in_chat
            .into_iter()
            .chain(REVIEWERS)
            .filter(|p| *p != author)
            .fold(Vec::new(), |mut list, p| {
                if !list.contains(&p) {
                    list.push(p);
                }
                list
            })
    }

    /// Switch the active chat to `reviewer` and send it `changes` to review.
    pub(crate) fn review_turn_changes(
        &mut self,
        changes: &TurnChanges,
        author: Option<LlmProviderKind>,
        reviewer: LlmProviderKind,
    ) {
        let key = self.active_session_key();
        if self.active_waiting_response() || self.compaction_active_for(key) {
            return;
        }
        let diff = match crate::git::checkpoint::patch(
            &self.turn_repo(&changes.repo),
            &changes.before,
            &changes.after,
            REVIEW_DIFF_BYTES,
        ) {
            Ok(diff) if !diff.trim().is_empty() => diff,
            Ok(_) => return self.notify_composer("Nothing left to review in this response."),
            Err(e) => return self.notify_composer(format!("Cannot read the changes: {e}")),
        };
        let (before, after) = (&changes.before, &changes.after);
        let repo = changes.repo.replace('\'', "'\\''");
        // The snapshots are trees in the repository's object database, so git can still show
        // what was left out of the attachment, deleted files included.
        let diff = format!(
            "{TURN_LINE}{before}..{after}\n\
             # Full diff: git -C '{repo}' diff {before} {after}\n\
             # A file before the change: git -C '{repo}' show {before}:<path>\n\n{diff}"
        );
        let whose = match author {
            Some(author) => format!("{} made", label(author)),
            None => "made".to_string(),
        };
        let text = format!(
            "Review the changes {whose} in that response (diff attached; the files are in the workspace). Look \
             for bugs, regressions, missed cases and anything that does not do what I asked. \
             Do not edit files. List findings most severe first, each with file:line, what goes \
             wrong and a suggested fix. If it all looks right, say so."
        );
        let diff = UserAttachment::Text {
            name: REVIEW_ATTACHMENT.to_string(),
            text: diff,
        };
        self.send_with_agent(reviewer, text, vec![diff]);
    }

    /// Hand the review at the end of the chat back to the agent that made the changes.
    pub(crate) fn apply_review_fixes(&mut self, followup: ReviewFollowup) {
        let key = self.active_session_key();
        if self.active_waiting_response() || self.compaction_active_for(key) {
            return;
        }
        let text = format!(
            "Apply the fixes from {}'s review above. Skip any finding you think is wrong and \
             say why.",
            label(followup.reviewer)
        );
        self.send_with_agent(followup.author, text, Vec::new());
    }

    /// Send `text` to `kind` in the active chat, leaving the composer draft untouched.
    fn send_with_agent(
        &mut self,
        kind: LlmProviderKind,
        text: String,
        attachments: Vec<UserAttachment>,
    ) {
        self.set_active_session_provider(kind);
        self.save_settings_quietly();
        let draft_input = std::mem::replace(&mut self.conv.composer.input, text);
        let draft_texts = std::mem::replace(&mut self.conv.composer.pending_texts, attachments);
        let draft_images = std::mem::take(&mut self.conv.composer.pending_images);
        self.send_message_opts(false);
        self.conv.composer.input = draft_input;
        self.conv.composer.pending_texts = draft_texts;
        self.conv.composer.pending_images = draft_images;
    }

    /// "Apply fixes with X" under a finished review at the end of the chat.
    pub(crate) fn render_review_followup(&mut self, ui: &mut Ui, followup: ReviewFollowup) {
        let mut apply = false;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            apply = crate::ui::chrome::mini_button_icon_enabled(
                ui,
                ICON_MAGIC,
                &format!("Apply fixes with {}", label(followup.author)),
                true,
            )
            .on_hover_text(format!(
                "Send this review back to {} to fix what it found",
                label(followup.author)
            ))
            .clicked();
        });
        ui.add_space(12.0);
        if apply {
            self.apply_review_fixes(followup);
        }
    }
}

/// The review follow-up for the assistant run starting at `start`, if that run is the end of
/// the chat, finished, and answers a review request.
pub(crate) fn review_followup(messages: &[ChatMessage], start: usize) -> Option<ReviewFollowup> {
    let request = messages.get(start.checked_sub(1)?)?;
    let is_request = request.role == MsgRole::User
        && request
            .attachments
            .iter()
            .any(|a| matches!(a, UserAttachment::Text { name, .. } if name == REVIEW_ATTACHMENT));
    let run = &messages[start..];
    if !is_request
        || run.is_empty()
        || run
            .iter()
            .any(|m| m.role != MsgRole::Assistant || m.streaming)
    {
        return None;
    }
    // A failed review ends on an error note, and one with no answer has nothing to apply.
    let answered = crate::model::last_answer(run).is_some() && !crate::model::reply_failed(run);
    if !answered {
        return None;
    }
    let reviewer = run.iter().rev().find_map(|m| m.route.as_deref())?.provider;
    // The author is whoever made the reviewed turn, named by the attachment's first line.
    let (before, after) = request.attachments.iter().find_map(|a| match a {
        UserAttachment::Text { name, text } if name == REVIEW_ATTACHMENT => text
            .lines()
            .next()?
            .strip_prefix(TURN_LINE)?
            .split_once(".."),
        _ => None,
    })?;
    let author = messages[..start - 1]
        .iter()
        .rev()
        .find(|m| {
            m.changes
                .as_deref()
                .is_some_and(|c| c.before == before && c.after == after)
        })?
        .route
        .as_deref()?
        .provider;
    (author != reviewer).then_some(ReviewFollowup { author, reviewer })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::RouteNote;

    fn reply(kind: LlmProviderKind, answer: &str) -> ChatMessage {
        let mut m =
            ChatMessage::streaming_assistant(Some(Box::new(RouteNote::direct(kind, "m", ""))));
        m.append_answer(answer);
        m.finish_streaming();
        m
    }

    fn turn(kind: LlmProviderKind, before: &str, after: &str) -> ChatMessage {
        let mut m = reply(kind, "done");
        m.changes = Some(Box::new(TurnChanges {
            repo: "/r".into(),
            before: before.into(),
            after: after.into(),
            files: None,
        }));
        m
    }

    fn review_request(before: &str, after: &str) -> ChatMessage {
        ChatMessage::user(
            "Review",
            &[UserAttachment::Text {
                name: REVIEW_ATTACHMENT.into(),
                text: format!("{TURN_LINE}{before}..{after}\n# Full diff: ...\n\ndiff"),
            }],
        )
    }

    #[test]
    fn followup_points_back_at_the_reviewed_turns_author() {
        // Claude's turn is reviewed after Cursor answered too: fixes still go to Claude.
        let messages = vec![
            ChatMessage::user("do it", &[]),
            turn(LlmProviderKind::ClaudeCodeAcp, "b1", "a1"),
            ChatMessage::user("and this", &[]),
            turn(LlmProviderKind::CursorAcp, "b2", "a2"),
            review_request("b1", "a1"),
            reply(LlmProviderKind::CodexAcp, "1. a.rs:3 breaks"),
        ];
        let f = review_followup(&messages, 5).unwrap();
        assert_eq!(f.author, LlmProviderKind::ClaudeCodeAcp);
        assert_eq!(f.reviewer, LlmProviderKind::CodexAcp);
        // A plain prompt is not a review.
        let mut plain = messages.clone();
        plain[4] = ChatMessage::user("Review", &[]);
        assert!(review_followup(&plain, 5).is_none());
        // Still answering: no button yet.
        let mut streaming = messages.clone();
        streaming[5].streaming = true;
        assert!(review_followup(&streaming, 5).is_none());
        // A review that failed or never answered has nothing to apply.
        let mut failed = messages.clone();
        failed[5] = reply(LlmProviderKind::CodexAcp, "Looking\n[Error] quota\n");
        assert!(review_followup(&failed, 5).is_none());
        let mut silent = messages;
        silent[5] = reply(LlmProviderKind::CodexAcp, "  ");
        assert!(review_followup(&silent, 5).is_none());
    }
}
