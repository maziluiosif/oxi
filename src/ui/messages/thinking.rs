//! Rendering for assistant "thinking"/reasoning blocks: the per-group wall-clock timer,
//! the frameless accent-rail text panel, and the live/done caption row.

use std::time::{Duration, Instant};

use eframe::egui::scroll_area::ScrollBarVisibility;
use eframe::egui::text::{LayoutJob, LayoutSection, TextFormat, TextWrapping};
use eframe::egui::{self, Frame, Id, Margin, RichText, ScrollArea, Stroke, Ui};

use crate::model::AssistantBlock;
use crate::theme::*;
use crate::ui::preview_expand::{
    clickable_expand_overlay_quiet, expand_persist_id, is_expanded, set_expanded,
};

use super::{block_state_tag, selectable_layout_job};

/// Max visual rows the *thinking* bubble shows while collapsed (live tail or done-folded
/// preview). Larger than the tool/code preview cap because reasoning reads as a side note
/// and benefits from more context per glance.
const THINKING_PREVIEW_LINES: usize = 15;

/// Stable id (independent of the live/done tag) for a thinking group's wall-clock timer,
/// so the start instant recorded while live can be read back after the group goes done.
fn thinking_timer_id(msg_idx: usize, salt: usize) -> Id {
    Id::new(("thinking_timer", msg_idx, salt))
}

/// Read/refresh the per-group thinking timer. While `live`, returns the running elapsed
/// (recording the start instant on first sight); once `live` flips false, the elapsed is
/// frozen once and reused thereafter. State lives in egui *temp* memory (not persisted):
/// `Instant` isn't serializable and, like the turn-level `started_at`, the timer only
/// matters for the live session that produced it.
fn thinking_elapsed(ui: &Ui, msg_idx: usize, salt: usize, live: bool) -> Option<Duration> {
    let id = thinking_timer_id(msg_idx, salt);
    let (start, frozen): (Option<Instant>, Option<Duration>) = ui
        .ctx()
        .data_mut(|d| d.get_temp(id).unwrap_or((None, None)));
    if live {
        let now = Instant::now();
        if start.is_none() {
            ui.ctx().data_mut(|d| {
                d.insert_temp::<(Option<Instant>, Option<Duration>)>(id, (Some(now), None))
            });
        }
        Some(now.duration_since(start.unwrap_or(now)))
    } else {
        match frozen {
            Some(d) => Some(d),
            None => {
                // Transition frame: freeze from the recorded start (or nothing if we
                // never saw it live, e.g. a freshly-reloaded done group).
                let d = start.map(|s| s.elapsed());
                if start.is_some() {
                    ui.ctx().data_mut(|dmap| {
                        dmap.insert_temp::<(Option<Instant>, Option<Duration>)>(id, (start, d))
                    });
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

/// Number of *visual* rows the thinking text occupies once wrapped at `wrap_width` (counting
/// both explicit newlines and soft-wraps). The thinking block folds based on this instead of
/// `str::lines().count()`: a single very long paragraph wraps onto many rows, so counting only
/// logical lines would let the panel grow past the preview limit and then snap back to the
/// truncated view the moment enough newlines arrive — a visible flicker while streaming.
fn thinking_visual_row_count(ui: &Ui, text: &str, wrap_width: f32) -> usize {
    if text.is_empty() {
        return 0;
    }
    ui.fonts_mut(|fonts| {
        fonts
            .layout_job(thinking_wrapped_job(text.to_string(), wrap_width))
            .rows
            .len()
    })
}

/// Frameless thinking body: no card, just a thin accent rail on the left and airy
/// proportional text. Collapse/expand behavior matches the monospace panels; while
/// live-streaming the collapsed view is pinned to the tail (newest reasoning).
fn render_thinking_text_panel(
    ui: &mut Ui,
    max_preview_lines: usize,
    persist_id: Id,
    tail_scroll_id: Id,
    content_overflows: bool,
    text: &str,
    tail: bool,
) {
    let frame = Frame::new()
        .inner_margin(Margin {
            left: 12,
            right: 4,
            top: 2,
            bottom: 2,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let inner = ui.available_width().max(40.0);
            let expanded = is_expanded(ui, persist_id);
            let allow_select = !content_overflows || expanded;

            // While the model is actively streaming reasoning (`tail`), keep the whole body
            // inside a fixed-height, bottom-stuck scroll window — even before it crosses the
            // preview limit. This avoids the flicker where the block would grow past the cap
            // (~2x) while wrapping settled and then snap back to the truncated view. Wheel
            // scrolling is disabled so events still reach the outer transcript ScrollArea.
            if tail || (content_overflows && !expanded) {
                let line_h = FS_SMALL * 1.45;
                let max_h = max_preview_lines as f32 * line_h;
                // Size the box to the actual content (1 row while there's only one line),
                // capped at `max_preview_lines` — grows as reasoning streams in instead of
                // reserving the full N-line height up front.
                let rows = thinking_visual_row_count(ui, text, inner)
                    .max(1)
                    .min(max_preview_lines);
                let box_h = rows as f32 * line_h;
                // `ScrollArea::max_height` only *caps* the size — internally it still clamps
                // to `ui.available_rect_before_wrap()`, which shrinks (often to near-zero) the
                // deeper a widget sits inside a long scrolled transcript, since the outer
                // transcript ScrollArea's content `max_rect` is fixed to the viewport height,
                // not infinite. Without an explicit reservation the tail box silently collapses
                // to whatever sliver of layout budget is left instead of the requested N lines.
                // Pre-allocating an exact-size rect (same trick as the activity-summary row
                // above) gives the nested ScrollArea a fresh, unshrunk budget to clamp against.
                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), box_h),
                    egui::Sense::hover(),
                );
                ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                    ScrollArea::vertical()
                        .id_salt(tail_scroll_id)
                        .auto_shrink([false, false])
                        .max_height(max_h)
                        // egui defaults `min_scrolled_height` to 64.0 (~3.5 rows at this font
                        // size) for any scroll-enabled axis, which floors the box well above
                        // our 1-row minimum. Match it to a single row so it can start as small
                        // as the content actually is.
                        .min_scrolled_height(line_h)
                        .scroll_bar_visibility(ScrollBarVisibility::AlwaysHidden)
                        .stick_to_bottom(true)
                        .scroll_source(egui::containers::scroll_area::ScrollSource::NONE)
                        .show(ui, |ui| {
                            selectable_layout_job(
                                ui,
                                thinking_wrapped_job(text.to_string(), inner),
                                allow_select,
                            );
                        });
                });
                return;
            }

            // Done (not streaming). A short body (under the preview limit) renders inline,
            // and so does an *unfolded* overflowing one — expanding a thinking block reveals
            // the FULL reasoning, not a capped scroll window. A folded overflowing one keeps
            // the tail window above, at the same height it had while live.
            selectable_layout_job(
                ui,
                thinking_wrapped_job(text.to_string(), inner),
                allow_select,
            );
        });
    let r = frame.response.rect;
    ui.painter().vline(
        r.left() + 1.0,
        (r.top() + 2.0)..=(r.bottom() - 2.0),
        Stroke::new(2.0, c_thinking_rail()),
    );
    if content_overflows {
        clickable_expand_overlay_quiet(ui, r, persist_id);
    }
}

/// One thinking group inside the expanded activity list: a caption row ("Thinking for…" while
/// live, "Thought for…" once done) above the reasoning body. The caption row and the folded body
/// keep the same height across the live→done transition, so the transcript does not reflow (and
/// visibly jump under stick-to-bottom) every time reasoning hands over to a tool call.
pub(super) fn render_thinking_group_block(
    ui: &mut Ui,
    msg_idx: usize,
    salt: usize,
    combined: String,
    live: bool,
) {
    let bubble_w = content_wrap_width(ui);
    ui.set_width(bubble_w);
    // Fold based on *visual* rows (after wrapping), not logical `\n` line count: a long
    // paragraph without newlines still wraps onto many rows and must trigger the truncated
    // preview at the same threshold the body would otherwise reach, instead of overshooting
    // the limit and snapping back once enough newlines stream in (flicker).
    let inner_w = (bubble_w - 16.0).max(40.0);
    let overflow = thinking_visual_row_count(ui, &combined, inner_w) > THINKING_PREVIEW_LINES;
    let persist_id = expand_persist_id(Id::new((
        msg_idx,
        salt,
        "thinking_body",
        block_state_tag(live),
    )));
    // Keep one expansion snapshot for the entire frame. In particular, do not toggle the state
    // from the caption and then read it again below: inserting/removing the full thinking body in
    // the click frame invalidates the rest of the transcript's geometry mid-frame and makes every
    // block below it flicker. Tool pills follow the same deferred-layout pattern.
    let expanded = is_expanded(ui, persist_id);

    // Per-group wall-clock timer (ms/s/min). While live it ticks from the first frame we
    // saw the group streaming; once thinking ends it freezes once into "Thought for Xs".
    // The caption id (`persist_id`) switches with the live/done tag, so the expand state
    // resets to folded at the live→done transition.
    let elapsed = thinking_elapsed(ui, msg_idx, salt, live);

    // Reserve the caption row up front at a fixed height (like the activity-summary row) so the
    // live label and the done caption occupy exactly the same space, and hover can tint the
    // chevron before it is painted.
    const CAPTION_H: f32 = 18.0;
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), CAPTION_H),
        egui::Sense::hover(),
    );
    // Short reasoning is shown whole below, so only an overflowing done block can unfold.
    let foldable = !live && overflow;
    let hovered = foldable && {
        let click = ui.interact(rect, persist_id.with("caption"), egui::Sense::click());
        if click.clicked() {
            set_expanded(ui, persist_id, !expanded);
        }
        if click.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        click.hovered()
    };
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            if live {
                animated_status_label(ui, "Thinking", FS_TINY);
                if let Some(d) = elapsed {
                    ui.label(
                        RichText::new(format!(" for {}", format_stream_elapsed(d)))
                            .size(FS_TINY)
                            .color(c_text_muted()),
                    );
                }
                return;
            }
            let caption = match elapsed {
                Some(d) => format!("Thought for {}", format_stream_elapsed(d)),
                None => "Thought".to_string(),
            };
            ui.add(
                egui::Label::new(RichText::new(caption).size(FS_TINY).color(c_text_faint()))
                    .selectable(false),
            );
            if foldable {
                ui.add(
                    egui::Label::new(
                        RichText::new(if expanded {
                            ICON_ANGLE_UP
                        } else {
                            ICON_ANGLE_DOWN
                        })
                        .font(egui::FontId::new(FS_TINY, icon_font()))
                        .color(if hovered {
                            c_accent()
                        } else {
                            c_text_faint()
                        }),
                    )
                    .selectable(false),
                );
            }
        });
    });
    ui.add_space(4.0);
    render_thinking_text_panel(
        ui,
        THINKING_PREVIEW_LINES,
        persist_id,
        // Independent of the live/done tag so the window keeps its bottom-stuck offset when
        // thinking ends instead of restarting at the top for a frame.
        Id::new((msg_idx, salt, "thinking_tail")),
        overflow,
        combined.as_str(),
        live,
    );
    ui.add_space(8.0);
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
