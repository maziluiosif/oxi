//! Cmd/Ctrl+F in the chat: Sublime-style incremental find over the transcript. Typing narrows the
//! matches at once, Enter / Shift+Enter (or the arrows) step through them, Escape closes. The
//! current match is scrolled into view and its message gets a soft accent backdrop.
//!
//! Matching works per transcript unit (a prompt, or one agent reply with all its steps): that
//! is what the transcript can scroll to and highlight without laying out off-screen messages.

use std::sync::Arc;

use eframe::egui::{self, Align2, TextEdit, Ui};

use crate::model::{AssistantBlock, ChatMessage, MsgRole};
use crate::theme::*;

use super::super::OxiApp;

const FIELD_ID: &str = "chat_find_field";
const BAR_W: f32 = 340.0;
/// Room kept above a revealed match for the bar floating over the transcript top.
pub(super) const BAR_CLEARANCE: f32 = 56.0;

/// Open find bar state, kept on the transcript.
#[derive(Default)]
pub(crate) struct ChatFind {
    pub query: String,
    /// Index into the matches of the current query.
    pub current: usize,
    pub focus_field: bool,
    /// Scroll the current match into view on the next transcript pass.
    pub reveal: bool,
    cache: Option<MatchCache>,
}

/// Matching unit starts, for the query, chat and transcript fingerprint they were found in.
struct MatchCache {
    query: String,
    chat: (usize, usize),
    fingerprint: u64,
    matches: Arc<Vec<usize>>,
}

/// The text a unit is searched by: prompts and the agent's answers. Tool output and reasoning
/// stay out: they are collapsed, and they would match nearly every query in a coding chat.
fn unit_matches(messages: &[ChatMessage], needle: &str) -> bool {
    messages.iter().any(|message| {
        match message.role {
        MsgRole::Assistant => message.blocks.iter().any(|block| {
            matches!(block, AssistantBlock::Answer(text) if contains_ignore_case(text, needle))
        }),
        _ => contains_ignore_case(&message.text, needle),
    }
    })
}

fn contains_ignore_case(haystack: &str, needle_lower: &str) -> bool {
    if haystack.is_ascii() && needle_lower.is_ascii() {
        let needle = needle_lower.as_bytes();
        return haystack
            .as_bytes()
            .windows(needle.len())
            .any(|window| window.eq_ignore_ascii_case(needle));
    }
    haystack.to_lowercase().contains(needle_lower)
}

/// Starts of the units of `units` that contain `query`, in transcript order.
pub(super) fn matching_units(
    messages: &[ChatMessage],
    units: &[(usize, usize)],
    query: &str,
) -> Vec<usize> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return Vec::new();
    }
    units
        .iter()
        .filter(|&&(start, end)| unit_matches(&messages[start..end], &needle))
        .map(|&(start, _)| start)
        .collect()
}

impl OxiApp {
    pub(crate) fn open_chat_find(&mut self) {
        let find = self
            .conv
            .transcript
            .find
            .get_or_insert_with(Default::default);
        find.focus_field = true;
        find.reveal = true;
    }

    /// Open the find bar on `query`, to show where a search result matched.
    pub(crate) fn find_in_chat(&mut self, query: &str) {
        self.open_chat_find();
        if let Some(find) = self.conv.transcript.find.as_mut() {
            find.query = query.to_owned();
            find.current = 0;
        }
    }

    pub(crate) fn close_chat_find(&mut self) {
        self.conv.transcript.find = None;
        self.focus_active_view_next_frame();
    }

    /// The matches of the open find bar, cached until the query or the transcript changes.
    pub(super) fn chat_find_matches(&mut self, units: &[(usize, usize)]) -> Arc<Vec<usize>> {
        let wi = self.conv.active_workspace;
        let si = self.conv.workspaces[wi].active;
        let messages = &self.conv.workspaces[wi].sessions[si].messages;
        let Some(find) = self.conv.transcript.find.as_mut() else {
            return Arc::default();
        };
        let fingerprint = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            messages.len().hash(&mut hasher);
            if let Some(&(start, end)) = units.last() {
                super::transcript_unit_fingerprint(&messages[start..end]).hash(&mut hasher);
            }
            hasher.finish()
        };
        if let Some(cache) = &find.cache
            && cache.query == find.query
            && cache.chat == (wi, si)
            && cache.fingerprint == fingerprint
        {
            return Arc::clone(&cache.matches);
        }
        let matches = Arc::new(matching_units(messages, units, &find.query));
        find.current = find.current.min(matches.len().saturating_sub(1));
        find.cache = Some(MatchCache {
            query: find.query.clone(),
            chat: (wi, si),
            fingerprint,
            matches: Arc::clone(&matches),
        });
        matches
    }

    /// The floating bar at the top right of the chat column.
    pub(super) fn render_chat_find_bar(
        &mut self,
        ctx: &egui::Context,
        anchor: egui::Pos2,
        matches: usize,
    ) {
        let Some(find) = self.conv.transcript.find.as_mut() else {
            return;
        };
        let mut close = false;
        let mut step: Option<bool> = None;
        let previous_query = find.query.clone();
        egui::Area::new(egui::Id::new("chat_find_bar"))
            .order(egui::Order::Foreground)
            .pivot(Align2::RIGHT_TOP)
            .fixed_pos(anchor)
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(c_bg_elevated())
                    .stroke(egui::Stroke::new(1.0, c_border()))
                    .corner_radius(RADIUS_CARD)
                    .shadow(ui.visuals().popup_shadow)
                    .inner_margin(egui::Margin::symmetric(8, 6))
                    .show(ui, |ui| {
                        ui.set_width(BAR_W);
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = SPACE_XS;
                            let status = if find.query.trim().is_empty() {
                                String::new()
                            } else if matches == 0 {
                                "No matches".to_string()
                            } else {
                                format!("{} of {matches}", find.current + 1)
                            };
                            let field_w = BAR_W - 3.0 * 26.0 - 3.0 * SPACE_XS;
                            let field = ui.add(
                                TextEdit::singleline(&mut find.query)
                                    .id(egui::Id::new(FIELD_ID))
                                    .hint_text("Find in chat")
                                    .font(egui::FontId::proportional(FS_SMALL))
                                    .margin(egui::Margin {
                                        left: 8,
                                        right: 76,
                                        top: 4,
                                        bottom: 4,
                                    })
                                    .desired_width(field_w),
                            );
                            if !status.is_empty() {
                                ui.painter().text(
                                    egui::pos2(field.rect.right() - 8.0, field.rect.center().y),
                                    Align2::RIGHT_CENTER,
                                    status,
                                    egui::FontId::proportional(FS_TINY),
                                    if matches == 0 {
                                        c_error_fg()
                                    } else {
                                        c_text_faint()
                                    },
                                );
                            }
                            if std::mem::take(&mut find.focus_field) {
                                field.request_focus();
                            }
                            // A single-line field drops focus on Enter, so check `lost_focus` too.
                            let (enter, shift, escape) = ui.input(|i| {
                                (
                                    i.key_pressed(egui::Key::Enter),
                                    i.modifiers.shift,
                                    i.key_pressed(egui::Key::Escape),
                                )
                            });
                            if (field.has_focus() || field.lost_focus()) && enter {
                                step = Some(!shift);
                                find.focus_field = true;
                            }
                            if (field.has_focus() || field.lost_focus()) && escape {
                                close = true;
                            }
                            let up = crate::ui::chrome::icon_button_plain(
                                ui,
                                ICON_ANGLE_UP,
                                26.0,
                                false,
                            )
                            .on_hover_text("Previous match  (Shift+Enter)");
                            if up.clicked() {
                                step = Some(false);
                            }
                            let down = crate::ui::chrome::icon_button_plain(
                                ui,
                                ICON_ANGLE_DOWN,
                                26.0,
                                false,
                            )
                            .on_hover_text("Next match  (Enter)");
                            if down.clicked() {
                                step = Some(true);
                            }
                            let x =
                                crate::ui::chrome::icon_button_plain(ui, ICON_CLOSE, 26.0, false)
                                    .on_hover_text("Close  (Esc)");
                            if x.clicked() {
                                close = true;
                            }
                        });
                    });
            });
        if close {
            self.close_chat_find();
            return;
        }
        let Some(find) = self.conv.transcript.find.as_mut() else {
            return;
        };
        if find.query != previous_query {
            // A new query starts from its first match, like Sublime's incremental find.
            find.current = 0;
            find.reveal = true;
        }
        if let Some(forward) = step
            && matches > 0
        {
            find.current = if forward {
                (find.current + 1) % matches
            } else {
                (find.current + matches - 1) % matches
            };
            find.reveal = true;
        }
    }
}

/// Paint the backdrop of the current match under the unit at `rect`.
pub(super) fn paint_match(ui: &Ui, slot: egui::layers::ShapeIdx, rect: egui::Rect) {
    ui.painter().set(
        slot,
        egui::Shape::rect_filled(
            rect.expand2(egui::vec2(8.0, 4.0)),
            RADIUS_CARD,
            c_accent().gamma_multiply(0.12),
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(text: &str) -> ChatMessage {
        ChatMessage::user(text, &[])
    }

    fn reply(answer: &str, thinking: &str) -> ChatMessage {
        let mut msg = ChatMessage::streaming_assistant(None);
        msg.blocks = vec![
            AssistantBlock::Thinking(thinking.into()),
            AssistantBlock::Answer(answer.into()),
        ];
        msg.finish_streaming();
        msg
    }

    #[test]
    fn finds_prompts_and_answers_case_insensitively_in_order() {
        let messages = vec![
            user("Add CSV export"),
            reply("Done: export.csv is written by `write_csv`.", "plan"),
            user("Now the median"),
            reply("Fixed the Median.", "csv is unrelated here"),
        ];
        let units = [(0, 1), (1, 2), (2, 3), (3, 4)];
        assert_eq!(matching_units(&messages, &units, "csv"), vec![0, 1]);
        assert_eq!(matching_units(&messages, &units, "MEDIAN"), vec![2, 3]);
        assert!(
            matching_units(&messages, &units, "unrelated").is_empty(),
            "reasoning is not searched"
        );
        assert!(matching_units(&messages, &units, "  ").is_empty());
        assert_eq!(matching_units(&messages, &units, "ăș"), Vec::<usize>::new());
    }

    #[test]
    fn non_ascii_text_matches_ignoring_case() {
        let messages = vec![user("Rezolvă Ștergerea ultimei litere")];
        assert_eq!(matching_units(&messages, &[(0, 1)], "ștergerea"), vec![0]);
    }
}
