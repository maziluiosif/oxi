//! Rendering for assistant "thinking"/reasoning blocks: the per-group wall-clock timer,
//! a compact collapsible caption row (with a 1-line snippet when folded), a brief live-streaming
//! peek while active, and a bounded scrollable accent-rail body when expanded.

use std::time::{Duration, Instant};

use eframe::egui::text::{LayoutJob, LayoutSection, TextFormat, TextWrapping};
use eframe::egui::{self, CornerRadius, Frame, Id, Margin, RichText, Stroke, Ui};

use crate::model::AssistantBlock;
use crate::theme::*;
use crate::ui::preview_expand::{expand_persist_id, is_expanded, is_expanded_or, set_expanded};

use super::{selectable_layout_job, show_bounded_scroll_box};

/// Max visual rows shown in the live-streaming tail peek so active reasoning stays calm and
/// compact instead of taking over the viewport.
const THINKING_LIVE_PREVIEW_LINES: usize = 4;

/// Height cap (in logical points) for an expanded completed thinking block; longer reasoning
/// scrolls smoothly inside this viewport instead of pushing the whole transcript off screen.
const THINKING_MAX_SCROLL_HEIGHT: f32 = 220.0;

/// Fixed height of the clickable thinking header row.
const THINKING_HEADER_H: f32 = 22.0;

/// Stable id (independent of the live/done state) for a thinking group's wall-clock timer,
/// so the start instant recorded while live can be read back after the group goes done.
fn thinking_timer_id(msg_idx: usize, salt: usize) -> Id {
    Id::new(("thinking_timer", msg_idx, salt))
}

/// Read/refresh the per-group thinking timer. While `live`, returns the running elapsed
/// (recording the start instant on first sight); once `live` flips false, the elapsed is
/// frozen once and reused thereafter. State lives in egui *temp* memory (not persisted):
/// `Instant` isn't serializable and, like the turn-level `started_at`, the timer only
/// matters for the live session that produced it.
///
/// The id is per message position, so every chat shares it: a live group that finds a frozen
/// entry is a new group (another chat's) and restarts, and a frozen time is only shown for the
/// text it was measured on.
fn thinking_elapsed(
    ui: &Ui,
    msg_idx: usize,
    salt: usize,
    text: &str,
    live: bool,
) -> Option<Duration> {
    type Timer = (Option<Instant>, Option<(Duration, u64)>);
    let id = thinking_timer_id(msg_idx, salt);
    let (start, frozen): Timer = ui
        .ctx()
        .data_mut(|d| d.get_temp(id).unwrap_or((None, None)));
    if live {
        let now = Instant::now();
        let start = start.filter(|_| frozen.is_none());
        if start.is_none() {
            ui.ctx()
                .data_mut(|d| d.insert_temp::<Timer>(id, (Some(now), None)));
        }
        Some(now.duration_since(start.unwrap_or(now)))
    } else {
        let hash = egui::util::hash(text);
        match frozen {
            Some((d, frozen_hash)) => (frozen_hash == hash).then_some(d),
            None => {
                // Transition frame: freeze from the recorded start (or nothing if we
                // never saw it live, e.g. a freshly-reloaded done group).
                let d = start.map(|s| s.elapsed());
                if let Some(d) = d {
                    ui.ctx()
                        .data_mut(|dmap| dmap.insert_temp::<Timer>(id, (start, Some((d, hash)))));
                }
                d
            }
        }
    }
}

/// Thinking-block body text: proportional, small, airy — deliberately lighter than the
/// monospace tool-output panels so reasoning reads as a side note, not a code dump.
fn thinking_wrapped_job(text: String, wrap_width: f32) -> LayoutJob {
    LayoutJob {
        sections: vec![LayoutSection {
            leading_space: 0.0,
            byte_range: egui::text::ByteIndex(0)..egui::text::ByteIndex(text.len()),
            format: TextFormat {
                font_id: eframe::egui::FontId::proportional(FS_SMALL),
                color: c_text_muted(),
                line_height: Some(FS_SMALL * 1.45),
                ..Default::default()
            },
        }],
        text,
        wrap: TextWrapping {
            max_width: wrap_width,
            ..Default::default()
        },
        break_on_newline: true,
        ..Default::default()
    }
}

/// Single-line plain-text preview shown inline on a folded thinking row so the user can scan
/// what the model considered without expanding every block.
fn thinking_snippet(text: &str, from_tail: bool) -> String {
    let picked = if from_tail {
        text.lines().rev().find(|l| !l.trim().is_empty())
    } else {
        text.lines().find(|l| !l.trim().is_empty())
    };
    let Some(line) = picked else {
        return String::new();
    };
    let cleaned = line
        .trim()
        .trim_start_matches(['#', '>', '-', '*', ' '])
        .replace("**", "")
        .replace('`', "");
    let mut out = String::new();
    let mut count = 0usize;
    for word in cleaned.split_whitespace() {
        if count > 0 {
            if count + 1 >= 140 {
                break;
            }
            out.push(' ');
            count += 1;
        }
        for ch in word.chars() {
            if count >= 140 {
                break;
            }
            out.push(ch);
            count += 1;
        }
        if count >= 140 {
            break;
        }
    }
    out
}

/// Estimated natural height of `text` wrapped at `wrap_width`. Avoids full galley layout when
/// the text is clearly longer than `max_h`.
fn thinking_body_height(ui: &Ui, text: &str, wrap_width: f32, max_h: f32, line_h: f32) -> f32 {
    if text.is_empty() {
        return line_h;
    }
    let max_lines_guess = (max_h / line_h).ceil() as usize + 6;
    if text.len() > 2400 || text.lines().count() > max_lines_guess {
        return max_h + line_h * 2.0;
    }
    ui.fonts_mut(|fonts| {
        fonts
            .layout_job(thinking_wrapped_job(text.to_string(), wrap_width))
            .size()
            .y
    })
    .max(line_h)
}

/// Frameless thinking body: no card, just a thin accent rail on the left and airy proportional
/// text inside a height-capped scrollable viewport. While `live`, stays pinned to the newest
/// reasoning lines with a soft top fade; once done and unfolded, scrolls smoothly with wheel
/// events and lets the user select text freely without accidental collapse.
fn render_thinking_body(ui: &mut Ui, scroll_id: Id, text: &str, live: bool) {
    let line_h = FS_SMALL * 1.45;
    let max_h = if live {
        THINKING_LIVE_PREVIEW_LINES as f32 * line_h
    } else {
        THINKING_MAX_SCROLL_HEIGHT
    };
    let frame = Frame::new()
        .inner_margin(Margin {
            left: 12,
            right: 4,
            top: 2,
            bottom: 2,
        })
        .show(ui, |ui| {
            let inner = ui.available_width().max(40.0);
            ui.set_width(inner);
            let content_h = thinking_body_height(ui, text, inner, max_h, line_h);
            show_bounded_scroll_box(
                ui,
                scroll_id,
                inner,
                content_h,
                max_h,
                line_h,
                c_bg_main(),
                live,
                !live,
                |ui| {
                    selectable_layout_job(ui, thinking_wrapped_job(text.to_string(), inner), !live);
                },
            );
        });
    let r = frame.response.rect;
    ui.painter().vline(
        r.left() + 1.0,
        (r.top() + 2.0)..=(r.bottom() - 2.0),
        Stroke::new(2.0, c_thinking_rail()),
    );
}

/// One thinking group inside the activity list:
/// - While actively streaming (`live`), shows a compact 4-line live peek (collapsible on click).
/// - Once finished (`!live`), collapses automatically to a sleek 1-line row
///   (`› Thought for Xs · <first-line snippet>`) so reasoning doesn't eat vertical space.
/// - One click unfolds the full reasoning inside a height-bounded scrollable rail panel.
pub(super) fn render_thinking_group_block(
    ui: &mut Ui,
    msg_idx: usize,
    salt: usize,
    combined: String,
    live: bool,
) {
    let bubble_w = content_wrap_width(ui);
    ui.set_width(bubble_w);

    // Separate live vs done persistence keys so active reasoning can show a brief live peek
    // while streaming and then settle cleanly into a compact folded row once finished.
    let persist_id = expand_persist_id(Id::new((
        msg_idx,
        salt,
        if live {
            "thinking_live_open"
        } else {
            "thinking_done_open"
        },
    )));
    // Keep one expansion snapshot for the entire frame so clicking never invalidates layout
    // mid-frame.
    let expanded = if live {
        is_expanded_or(ui, persist_id, true)
    } else {
        is_expanded(ui, persist_id)
    };

    let elapsed = thinking_elapsed(ui, msg_idx, salt, &combined, live);

    // Allocate the clickable header row up front (hover-only sense so wheel events always pass
    // through to the outer transcript ScrollArea).
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(bubble_w, THINKING_HEADER_H),
        egui::Sense::hover(),
    );
    let hovered = ui
        .ctx()
        .input(|i| i.pointer.interact_pos().is_some_and(|p| rect.contains(p)));
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        ui.painter()
            .rect_filled(rect, CornerRadius::same(RADIUS_ROW), c_row_hover());
        if ui.ctx().input(|i| i.pointer.primary_clicked()) {
            set_expanded(ui, persist_id, !expanded);
        }
    }

    let inner_rect = rect.shrink2(egui::vec2(5.0, 0.0));
    ui.scope_builder(egui::UiBuilder::new().max_rect(inner_rect), |ui| {
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 5.0;
            // Fixed-width chevron slot keeps the caption horizontally anchored when toggling.
            ui.add_sized(
                egui::vec2(10.0, THINKING_HEADER_H),
                egui::Label::new(
                    RichText::new(if expanded {
                        ICON_ANGLE_DOWN
                    } else {
                        ICON_CHEVRON_RIGHT
                    })
                    .font(egui::FontId::new(FS_TINY - 1.5, icon_font()))
                    .color(if hovered { c_accent() } else { c_text_faint() }),
                )
                .selectable(false),
            );

            if live {
                animated_status_label(ui, "Thinking", FS_TINY);
                if let Some(d) = elapsed {
                    ui.add(
                        egui::Label::new(
                            RichText::new(format!("for {}", format_stream_elapsed(d)))
                                .size(FS_TINY)
                                .color(c_text_muted()),
                        )
                        .selectable(false),
                    );
                }
            } else {
                // Sub-second reasoning reads as "Thought for 0s"; drop the duration like an
                // untracked one (same rule as the "Worked for" row).
                let caption = match elapsed {
                    Some(d) if d.as_secs() > 0 => {
                        format!("Thought for {}", format_stream_elapsed(d))
                    }
                    _ => "Thought".to_string(),
                };
                ui.add(
                    egui::Label::new(RichText::new(caption).size(FS_TINY).color(if hovered {
                        c_text_muted()
                    } else {
                        c_text_faint()
                    }))
                    .selectable(false),
                );
            }

            if !expanded && ui.available_width() > 48.0 {
                let snippet = thinking_snippet(&combined, live);
                if !snippet.is_empty() {
                    ui.add(
                        egui::Label::new(
                            RichText::new("·")
                                .size(FS_TINY)
                                .color(c_text_faint().gamma_multiply(0.6)),
                        )
                        .selectable(false),
                    );
                    ui.add(
                        egui::Label::new(
                            RichText::new(snippet)
                                .size(FS_TINY)
                                .color(c_text_faint().gamma_multiply(0.85)),
                        )
                        .truncate()
                        .selectable(false),
                    );
                }
            }
        });
    });

    if expanded {
        ui.add_space(2.0);
        render_thinking_body(
            ui,
            Id::new((
                msg_idx,
                salt,
                if live {
                    "thinking_tail"
                } else {
                    "thinking_scroll"
                },
            )),
            combined.as_str(),
            live,
        );
        ui.add_space(6.0);
    } else {
        ui.add_space(2.0);
    }
}

pub(super) fn thinking_group_is_live(
    blocks: &[AssistantBlock],
    after_idx: usize,
    streaming: bool,
) -> bool {
    streaming
        && blocks[after_idx..].iter().all(|block| match block {
            AssistantBlock::Answer(text) => text.trim().is_empty(),
            _ => false,
        })
}

#[cfg(test)]
mod tests {
    use super::thinking_snippet;

    #[test]
    fn thinking_snippet_picks_first_or_last_clean_line() {
        let text = "\n## **Checking** `retry.rs`\nSecond line of thought\nLast insight here\n";
        assert_eq!(thinking_snippet(text, false), "Checking retry.rs");
        assert_eq!(thinking_snippet(text, true), "Last insight here");
    }
}
