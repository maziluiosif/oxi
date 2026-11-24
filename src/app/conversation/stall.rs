//! A turn that stops producing events still shows the sidebar spinner and "Working", so the chat
//! looks busy forever. After a quiet spell the transcript tail says so and offers a way out.
//! It replaces the "Working" line (see [`crate::ui::messages::set_turn_stalled`]).

use std::time::{Duration, Instant};

use eframe::egui::{self, Response, RichText, Ui};

use crate::model::{AssistantBlock, ChatMessage, ToolStatus};
use crate::theme::*;

use super::super::compaction::QueuedSend;
use super::super::{OxiApp, SessionKey};

/// Quiet time before the notice while the agent is thinking or writing.
const QUIET: Duration = Duration::from_secs(90);
/// A running tool (a build, a test suite) may legitimately say nothing for much longer.
const QUIET_TOOL: Duration = Duration::from_secs(300);
/// Sent by "Stop and continue": the agent keeps its session, so a short nudge is enough.
const CONTINUE_PROMPT: &str = "Continue where you left off.";

/// How long the running turn has been silent, once that is long enough to mention.
pub(super) fn quiet_for(
    last_event: Instant,
    now: Instant,
    reply: Option<&ChatMessage>,
) -> Option<Duration> {
    let quiet = now.saturating_duration_since(last_event);
    let limit = if reply.is_some_and(tool_in_flight) {
        QUIET_TOOL
    } else {
        QUIET
    };
    (quiet >= limit).then_some(quiet)
}

fn tool_in_flight(reply: &ChatMessage) -> bool {
    reply.blocks.iter().any(|block| {
        matches!(
            block,
            AssistantBlock::Tool { is_error, metadata, .. }
                if metadata.as_ref().map_or(is_error.is_none(), |m| {
                    matches!(m.status, ToolStatus::Pending | ToolStatus::InProgress)
                })
        )
    })
}

impl OxiApp {
    /// How long the active chat's running turn has been silent, once worth mentioning.
    pub(super) fn active_stall(&self) -> Option<Duration> {
        self.stall_for(self.active_session_key())
    }

    /// The same for any chat: the sidebar shows it on chats running in the background.
    pub(crate) fn stall_for(&self, key: SessionKey) -> Option<Duration> {
        let run = self.run_state(key)?;
        // An approval prompt is the agent waiting on the user, not the other way round.
        if !run.waiting_response || run.pending_approval.is_some() {
            return None;
        }
        quiet_for(
            run.last_event_at?,
            Instant::now(),
            self.session_by_key(key).messages.last(),
        )
    }

    /// "No updates from the agent for 2m 10s  Stop  Continue", in place of "Working".
    pub(super) fn render_stall_notice(&mut self, ui: &mut Ui, quiet: Duration) {
        // The label counts seconds.
        ui.ctx().request_repaint_after(Duration::from_secs(1));
        let mut stop = false;
        let mut nudge = false;
        // The reply above already ends with its own spacing.
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = SPACE_MD;
            ui.label(
                RichText::new(format!(
                    "No updates from the agent for {}",
                    format_stream_elapsed(quiet)
                ))
                .size(FS_SMALL)
                .color(c_text_muted()),
            );
            stop = quiet_action(ui, "Stop")
                .on_hover_text("End this turn and keep what it produced")
                .clicked();
            nudge = quiet_action(ui, "Continue")
                .on_hover_text(
                    "End this turn and ask the agent to pick up where it left off, in the same session",
                )
                .clicked();
        });
        ui.add_space(SPACE_SM);
        let key = self.active_session_key();
        if stop || nudge {
            self.stop_agent_run(key);
        }
        if nudge {
            self.send_queued(
                key,
                QueuedSend {
                    text: CONTINUE_PROMPT.into(),
                    images: Vec::new(),
                    texts: Vec::new(),
                },
                false,
            );
        }
    }

    /// Shown below the transcript when an ACP agent ended its turn while still waiting on one or
    /// more background commands (e.g. Antigravity `run_command` past `WaitMsBeforeAsync`).
    pub(super) fn render_background_wait_notice(
        &mut self,
        ui: &mut Ui,
        tools: &[crate::agent::acp::BackgroundTool],
    ) {
        let Some(first) = tools.first() else {
            return;
        };
        let elapsed = format_stream_elapsed(first.started_at.elapsed());
        let heading = if tools.len() == 1 {
            format!("Waiting for background command ({elapsed})")
        } else {
            format!(
                "Waiting for {} background commands ({elapsed})",
                tools.len()
            )
        };
        let detail = if tools.len() == 1 {
            first.label.clone()
        } else {
            let names: Vec<&str> = tools
                .iter()
                .map(|t| t.label.as_str())
                .filter(|s| !s.is_empty())
                .take(2)
                .collect();
            names.join(", ")
        };
        let mut stop = false;
        ui.add_space(SPACE_SM);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = SPACE_MD;
            animated_status_label(ui, &heading, FS_SMALL);
            if !detail.is_empty() {
                ui.label(
                    RichText::new(format!("· {detail}"))
                        .size(FS_SMALL)
                        .color(c_text_faint()),
                );
            }
            stop = quiet_action(ui, "Stop")
                .on_hover_text(
                    "Stop the agent subprocess and its background command (the session stays resumable)",
                )
                .clicked();
        });
        ui.add_space(SPACE_SM);
        if stop {
            let key = self.active_session_key();
            let acp_key = self.acp_session_key(key);
            self.acp.stop(&acp_key);
        }
    }
}

/// A text-only action that reads as part of the status line until hovered.
fn quiet_action(ui: &mut Ui, label: &str) -> Response {
    let response = ui
        .add(
            egui::Label::new(RichText::new(label).size(FS_SMALL).color(c_accent()))
                .sense(egui::Sense::click())
                .selectable(false),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
    });
    if response.hovered() || response.has_focus() {
        let y = response.rect.bottom() + 1.0;
        ui.painter().hline(
            response.rect.x_range(),
            y,
            egui::Stroke::new(1.0, c_accent()),
        );
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(blocks: Vec<AssistantBlock>) -> ChatMessage {
        let mut msg = ChatMessage::streaming_assistant(None);
        msg.blocks = blocks;
        msg
    }

    fn running_bash() -> AssistantBlock {
        AssistantBlock::Tool {
            tool_call_id: "1".into(),
            name: "bash".into(),
            args_summary: None,
            output: String::new(),
            diff: None,
            is_error: None,
            full_output_path: None,
            output_truncated: false,
            metadata: None,
        }
    }

    #[test]
    fn quiet_turns_are_reported_after_the_limit() {
        let start = Instant::now();
        let text = reply(vec![AssistantBlock::Answer("Checking.".into())]);
        assert_eq!(quiet_for(start, start + QUIET / 2, Some(&text)), None);
        assert_eq!(
            quiet_for(start, start + QUIET, Some(&text)),
            Some(QUIET),
            "thinking or writing goes quiet after the short limit"
        );
        assert_eq!(quiet_for(start, start + QUIET, None), Some(QUIET));
    }

    #[test]
    fn a_running_tool_gets_the_longer_limit() {
        let start = Instant::now();
        let tool = reply(vec![running_bash()]);
        assert_eq!(quiet_for(start, start + QUIET * 2, Some(&tool)), None);
        assert!(quiet_for(start, start + QUIET_TOOL, Some(&tool)).is_some());

        let mut done = running_bash();
        if let AssistantBlock::Tool { is_error, .. } = &mut done {
            *is_error = Some(false);
        }
        let finished = reply(vec![done]);
        assert!(
            quiet_for(start, start + QUIET, Some(&finished)).is_some(),
            "a finished tool no longer explains the silence"
        );

        let with_note = reply(vec![
            running_bash(),
            AssistantBlock::Answer("Waiting for CI…".into()),
        ]);
        assert_eq!(
            quiet_for(start, start + QUIET * 2, Some(&with_note)),
            None,
            "a progress note after an in-flight tool still gets the tool quiet limit"
        );
    }
}
