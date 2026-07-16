//! Tool-call rendering: the compact pill (icon + summary + expand-on-click detail),
//! the full-detail block for write/edit-like tools, and the "explored" cluster that
//! caps a live run of read/grep/find pills to its newest entries.

use eframe::egui::text::LayoutJob;
use eframe::egui::{
    self, Color32, CornerRadius, FontId, Frame, Id, Label, Margin, RichText, Stroke, Ui,
};

use crate::model::{AssistantBlock, concat_thinking_blocks};
use crate::theme::*;
use crate::ui::preview_expand::{
    clickable_expand_overlay, expand_persist_id, is_expanded, truncate_lines_preview,
};

use super::thinking::{render_thinking_group_block, thinking_group_is_live};
use crate::ui::diff::diff_layout_job;

use super::tool_format::{diff_counts, mono_output_job, tool_icon, tool_summary_text};
use super::{is_edit_like_tool, selectable_layout_job};

const BLOCK_PREVIEW_LINES: usize = 10;
const EDIT_PREVIEW_LINES: usize = 10;
/// Max tool pills shown while streaming (the oldest entries are hidden).
const MAX_VISIBLE_STREAMING_TOOL_PILLS: usize = 5;
/// Vertical gap between consecutive pills.
const TOOL_PILL_GAP: f32 = 3.0;

fn render_static_preview_job_panel(
    ui: &mut Ui,
    panel_fill: Color32,
    preview_job: LayoutJob,
    full_job: impl Fn(f32) -> LayoutJob,
    persist_id: Id,
    overflows: bool,
) {
    let frame = Frame::new()
        .fill(panel_fill)
        .stroke(Stroke::new(1.0, c_border()))
        .corner_radius(CornerRadius::same(RADIUS_CHIP))
        .inner_margin(Margin::symmetric(8, 5))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let inner = ui.available_width().max(40.0);
            let allow_select = !overflows || is_expanded(ui, persist_id);
            if !overflows || is_expanded(ui, persist_id) {
                selectable_layout_job(ui, full_job(inner), allow_select);
            } else {
                selectable_layout_job(ui, preview_job, allow_select);
            }
        });
    if overflows {
        clickable_expand_overlay(ui, frame.response.rect, persist_id);
    }
}

/// Render a single tool call as a compact Cursor-style chip:
/// [ icon  ToolName  short_arg ]  with one line of output on hover/expand.
pub(super) fn render_tool_pill(
    ui: &mut Ui,
    msg_idx: usize,
    block_idx: usize,
    block: &AssistantBlock,
    streaming: bool,
    is_last_in_run: bool,
    expandable: bool,
) {
    let AssistantBlock::Tool {
        tool_call_id,
        name,
        output,
        args_summary,
        is_error,
        diff,
        full_output_path,
        output_truncated,
    } = block
    else {
        return;
    };

    let has_error = *is_error == Some(true);
    let has_diff = diff.as_deref().is_some_and(|t| !t.trim().is_empty());
    let has_output = !output.trim().is_empty();
    // A tool is "actively running" only while streaming AND it has not been finalized yet
    // (is_error is set by finalize_tool_run — None means still in-flight).
    // Additionally only the last pill in the visual run gets the spinner.
    let tool_in_flight = streaming && is_error.is_none() && !has_output && !has_diff;
    let running = tool_in_flight && is_last_in_run;

    let status_done = !running && !has_error;
    let pill_bg = if has_error {
        crate::theme::c_tool_error_bg()
    } else if running {
        crate::theme::c_tool_running_bg()
    } else {
        crate::theme::c_tool_pill_bg()
    };
    let pill_border = if has_error {
        crate::theme::c_tool_error_border()
    } else if running {
        crate::theme::c_tool_running_border()
    } else {
        crate::theme::c_tool_pill_border()
    };
    let icon_color = if has_error {
        crate::theme::c_tool_error_fg()
    } else {
        c_text_faint()
    };
    let name_color = if has_error {
        crate::theme::c_tool_error_fg()
    } else {
        c_text()
    };
    let summary_color = if has_error {
        crate::theme::c_tool_error_fg()
    } else {
        c_text_muted()
    };

    let icon = tool_icon(name);
    let summary = tool_summary_text(
        name,
        args_summary.as_ref(),
        output,
        diff.as_ref(),
        *is_error,
        running,
    );

    // Click-to-expand: keyed on the stable provider tool-call id so the fold state survives
    // re-layout; falls back to the transcript position when the id is missing.
    let persist_id = expand_persist_id(if tool_call_id.is_empty() {
        Id::new(("tool_pill", msg_idx, block_idx))
    } else {
        Id::new(("tool_pill", tool_call_id.as_str()))
    });
    let can_expand = expandable && !running && (has_output || has_diff);
    let expanded = can_expand && is_expanded(ui, persist_id);

    let frame = Frame::new()
        .fill(pill_bg)
        .stroke(Stroke::new(1.0, pill_border))
        .corner_radius(CornerRadius::same(RADIUS_BUTTON))
        .inner_margin(Margin::symmetric(10, 5))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 7.0;
                ui.label(
                    RichText::new(icon)
                        .font(FontId::new(FS_SMALL + 0.5, icon_font()))
                        .color(icon_color),
                );
                ui.label(
                    RichText::new(tool_status_label(name))
                        .size(FS_SMALL)
                        .color(name_color),
                );
                let avail = ui.available_width();
                let summary_resp = ui.add(
                    Label::new(
                        RichText::new(summary.as_str())
                            .size(FS_SMALL)
                            .color(summary_color)
                            .monospace(),
                    )
                    .truncate(),
                );
                let full_w = ui.fonts_mut(|f| {
                    f.layout_no_wrap(summary.clone(), FontId::monospace(FS_SMALL), summary_color)
                        .rect
                        .width()
                });
                if full_w > avail {
                    summary_resp
                        .on_hover_text(RichText::new(summary.as_str()).size(FS_TINY).monospace());
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if can_expand {
                        ui.add(
                            Label::new(
                                RichText::new(if expanded {
                                    ICON_ANGLE_UP
                                } else {
                                    ICON_ANGLE_DOWN
                                })
                                .font(FontId::new(FS_TINY, icon_font()))
                                .color(c_text_faint()),
                            )
                            .selectable(false),
                        );
                        ui.add_space(2.0);
                    }
                    if running {
                        ui.add(
                            eframe::egui::Spinner::new()
                                .size(10.0)
                                .color(c_text_muted()),
                        );
                        ui.add_space(4.0);
                        crate::ui::chrome::running_badge(ui);
                    } else if has_error {
                        crate::ui::chrome::failed_badge(ui);
                    } else if status_done {
                        crate::ui::chrome::done_badge(ui);
                    }
                });
            });
        });
    if can_expand {
        clickable_expand_overlay(ui, frame.response.rect, persist_id);
    }

    // Folded, the pill is the whole tool-call bubble (keeps the transcript compact); a click
    // unfolds the raw output / diff below it, and the next click folds it back.
    if expanded {
        ui.add_space(3.0);
        let bubble_w = ui.available_width().max(40.0);
        let detail_id = persist_id.with("detail");
        if let Some(diff_text) = diff.as_deref().filter(|t| !t.trim().is_empty()) {
            let overflow = diff_text.lines().count() > EDIT_PREVIEW_LINES || diff_text.len() > 2000;
            let preview = truncate_lines_preview(diff_text, EDIT_PREVIEW_LINES);
            render_static_preview_job_panel(
                ui,
                crate::theme::c_tool_diff_bg(),
                diff_layout_job(&preview, bubble_w),
                |inner| diff_layout_job(diff_text, inner),
                detail_id,
                overflow,
            );
        } else {
            let text = output.trim_end();
            let overflow = text.lines().count() > BLOCK_PREVIEW_LINES || text.len() > 2000;
            let preview = truncate_lines_preview(text, BLOCK_PREVIEW_LINES);
            render_static_preview_job_panel(
                ui,
                crate::theme::c_tool_diff_bg(),
                mono_output_job(&preview, bubble_w),
                |inner| mono_output_job(text, inner),
                detail_id,
                overflow,
            );
        }
        if *output_truncated || full_output_path.is_some() {
            let caption = match full_output_path.as_deref() {
                Some(path) => format!("output truncated — full output: {path}"),
                None => "output truncated".to_string(),
            };
            ui.label(RichText::new(caption).size(FS_TINY).color(c_text_faint()));
        }
    }
    ui.add_space(3.0);
}

fn write_content_from_args(args_summary: Option<&String>) -> Option<String> {
    let raw = args_summary?;
    let value = serde_json::from_str::<serde_json::Value>(raw).ok()?;
    value
        .get("content")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
}

fn pseudo_diff_from_write_content(content: &str) -> String {
    if content.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for (i, line) in content.lines().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push('+');
        out.push_str(line);
    }
    if content.ends_with('\n') {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push('+');
    }
    out
}

pub(super) fn render_single_tool_block(
    ui: &mut Ui,
    msg_idx: usize,
    bi: usize,
    block: &AssistantBlock,
    streaming: bool,
) {
    if is_edit_like_tool(block) {
        render_edit_tool_block(ui, msg_idx, bi, block, streaming);
        return;
    }

    render_tool_pill(ui, msg_idx, bi, block, streaming, streaming, true);
}

fn render_edit_tool_block(
    ui: &mut Ui,
    msg_idx: usize,
    block_idx: usize,
    block: &AssistantBlock,
    streaming: bool,
) {
    let AssistantBlock::Tool {
        name,
        args_summary,
        diff,
        output: _,
        is_error,
        ..
    } = block
    else {
        return;
    };

    let write_preview = if name.eq_ignore_ascii_case("write") {
        write_content_from_args(args_summary.as_ref())
            .map(|content| pseudo_diff_from_write_content(&content))
            .filter(|text| !text.trim().is_empty())
    } else {
        None
    };
    let rendered_diff = diff
        .as_ref()
        .filter(|t| !t.trim().is_empty())
        .cloned()
        .or(write_preview);

    let has_error = *is_error == Some(true);
    let has_diff = rendered_diff
        .as_deref()
        .is_some_and(|t| !t.trim().is_empty());
    // Spinner only while the tool is truly in-flight: streaming + not finalized (is_error=None) + no diff yet
    let running = streaming && is_error.is_none() && !has_diff;

    // Diff stats badge
    let (added, removed) = rendered_diff
        .as_deref()
        .filter(|t| !t.trim().is_empty())
        .map(diff_counts)
        .unwrap_or((0, 0));

    let outer_border = if has_error {
        crate::theme::c_tool_error_border()
    } else if running {
        crate::theme::c_tool_running_border()
    } else {
        c_border_subtle()
    };
    let header_bg = if has_error {
        crate::theme::c_tool_error_bg()
    } else if running {
        crate::theme::c_tool_running_bg()
    } else {
        crate::theme::c_tool_pill_bg()
    };
    let diff_bg = crate::theme::c_tool_diff_bg();

    let is_open = true;

    // ── Outer frame wraps header + diff in one visual block ──────────────────
    Frame::new()
        .fill(Color32::TRANSPARENT)
        .stroke(Stroke::new(1.0, outer_border))
        .corner_radius(CornerRadius::same(RADIUS_CHIP))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());

            let header_resp = Frame::new()
                .fill(header_bg)
                .corner_radius(if is_open && has_diff {
                    eframe::egui::CornerRadius {
                        nw: RADIUS_CHIP,
                        ne: RADIUS_CHIP,
                        sw: 0,
                        se: 0,
                    }
                } else {
                    CornerRadius::same(RADIUS_CHIP)
                })
                .inner_margin(Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;

                        ui.label(
                            RichText::new(tool_icon(name))
                                .font(FontId::new(FS_SMALL + 0.5, icon_font()))
                                .color(if has_error {
                                    crate::theme::c_tool_error_fg()
                                } else {
                                    c_text_faint()
                                }),
                        );

                        ui.label(
                            RichText::new(tool_summary_text(
                                name,
                                args_summary.as_ref(),
                                "",
                                rendered_diff.as_ref(),
                                *is_error,
                                running,
                            ))
                            .size(FS_SMALL)
                            .color(if has_error {
                                crate::theme::c_tool_error_fg()
                            } else {
                                c_text()
                            })
                            .monospace()
                            .strong(),
                        );

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if running {
                                ui.add(
                                    eframe::egui::Spinner::new()
                                        .size(10.0)
                                        .color(c_text_muted()),
                                );
                                ui.add_space(4.0);
                                crate::ui::chrome::running_badge(ui);
                            } else if has_diff {
                                ui.label(
                                    RichText::new(format!("-{removed}"))
                                        .size(FS_TINY)
                                        .color(c_diff_del_fg())
                                        .monospace(),
                                );
                                ui.add_space(2.0);
                                ui.label(
                                    RichText::new(format!("+{added}"))
                                        .size(FS_TINY)
                                        .color(c_diff_add_fg())
                                        .monospace(),
                                );
                                ui.add_space(4.0);
                                crate::ui::chrome::done_badge(ui);
                            } else if has_error {
                                crate::ui::chrome::failed_badge(ui);
                            }
                        });
                    });
                })
                .response;

            let _ = header_resp;

            // ── Diff block ───────────────────────────────────────────────────
            if let Some(diff_text) = rendered_diff.as_deref().filter(|t| !t.trim().is_empty()) {
                Frame::new()
                    .fill(diff_bg)
                    .corner_radius(eframe::egui::CornerRadius {
                        nw: 0,
                        ne: 0,
                        sw: 8,
                        se: 8,
                    })
                    .inner_margin(Margin::symmetric(10, 8))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        let bubble_w = ui.available_width().max(40.0);
                        let overflow = diff_text.lines().count() > EDIT_PREVIEW_LINES
                            || diff_text.len() > 2000;
                        let preview = truncate_lines_preview(diff_text, EDIT_PREVIEW_LINES);
                        render_static_preview_job_panel(
                            ui,
                            diff_bg,
                            diff_layout_job(&preview, bubble_w),
                            |inner| diff_layout_job(diff_text, inner),
                            expand_persist_id(Id::new((msg_idx, block_idx, "edit_diff"))),
                            overflow,
                        );
                    });
            }
        });

    ui.add_space(6.0);
}

/// Shared inputs for [`render_explored_tool_pill_run`] (keeps the renderer under clippy’s
/// argument limit).
struct ExploredToolPillCtx<'a> {
    msg_idx: usize,
    blocks: &'a [AssistantBlock],
    capped: bool,
    hidden_before: &'a mut usize,
    visible_start: usize,
    last_tool_idx: Option<usize>,
    streaming: bool,
}

/// Renders a consecutive run of tool block indices. When capped, only the newest pills are
/// visible; thinking/prose are handled separately and are never hidden.
fn render_explored_tool_pill_run(
    ui: &mut Ui,
    tool_run: &[usize],
    ctx: &mut ExploredToolPillCtx<'_>,
) {
    if tool_run.is_empty() {
        return;
    }

    // While the live explored cluster is capped to the last N tool pills, older tool batches can
    // become fully hidden. Skip those batches instead of leaving blank gaps between thinking blocks.
    let visible_run: &[usize] = if ctx.capped {
        let hidden_in_this_run = ctx
            .visible_start
            .saturating_sub(*ctx.hidden_before)
            .min(tool_run.len());
        *ctx.hidden_before += hidden_in_this_run;
        let visible = &tool_run[hidden_in_this_run..];
        if visible.is_empty() {
            return;
        }
        visible
    } else {
        tool_run
    };

    let msg_idx = ctx.msg_idx;
    let blocks = ctx.blocks;
    let last_tool_idx = ctx.last_tool_idx;
    let streaming = ctx.streaming;

    // `visible_run` is already trimmed to the last N tools above, so a nested ScrollArea buys
    // us nothing here. More importantly, egui clamps a nested ScrollArea to the outer transcript's
    // remaining viewport rect. Direct layout lets each pill reserve its real height and prevents
    // the first tool run after a collapsing Thinking block from being clipped/painted underneath it.
    // Keep details folded while trimming is active so a single expanded output cannot defeat the cap.
    let expandable = !ctx.capped;
    for &ti in visible_run {
        let block = &blocks[ti];
        let is_last = Some(ti) == last_tool_idx;
        render_tool_pill(ui, msg_idx, ti, block, streaming, is_last, expandable);
        ui.add_space(TOOL_PILL_GAP);
    }
}

fn render_explored_tool_list(
    ui: &mut Ui,
    msg_idx: usize,
    blocks: &[AssistantBlock],
    start: usize,
    end: usize,
    streaming: bool,
) {
    // Collect only Tool indices (not Thinking/Answer) for the live display cap.
    let tool_count = blocks[start..end]
        .iter()
        .filter(|b| matches!(b, AssistantBlock::Tool { .. }))
        .count();
    // While streaming, retain only the last N tool pills so a long tool flood doesn't grow the
    // transcript unbounded. Thinking and markdown answers stay outside that trimmed set.
    let capped = streaming && tool_count > MAX_VISIBLE_STREAMING_TOOL_PILLS;

    let last_tool_idx = blocks[start..end]
        .iter()
        .enumerate()
        .rev()
        .find(|(_, b)| matches!(b, AssistantBlock::Tool { .. }))
        .map(|(j, _)| start + j);

    let visible_start = tool_count.saturating_sub(MAX_VISIBLE_STREAMING_TOOL_PILLS);
    let mut hidden_before = 0usize;
    let mut i = start;

    while i < end {
        match &blocks[i] {
            AssistantBlock::Thinking(_) => {
                let first = i;
                while i < end && matches!(blocks[i], AssistantBlock::Thinking(_)) {
                    i += 1;
                }
                let indices: Vec<usize> = (first..i).collect();
                let combined = concat_thinking_blocks(blocks, &indices);
                if combined.trim().is_empty() {
                    continue;
                }
                let thinking_live = thinking_group_is_live(blocks, i, streaming);
                render_thinking_group_block(ui, msg_idx, first, combined, thinking_live);
            }
            AssistantBlock::Answer(text) => {
                if !text.trim().is_empty() {
                    crate::markdown::render_markdown(ui, text);
                }
                i += 1;
            }
            AssistantBlock::Tool { .. } => {
                let batch_lo = i;
                while i < end {
                    match &blocks[i] {
                        AssistantBlock::Tool { .. } => i += 1,
                        AssistantBlock::Answer(t) if t.trim().is_empty() => i += 1,
                        _ => break,
                    }
                }
                let tool_run: Vec<usize> = (batch_lo..i)
                    .filter(|&j| matches!(blocks[j], AssistantBlock::Tool { .. }))
                    .collect();
                let mut pill_ctx = ExploredToolPillCtx {
                    msg_idx,
                    blocks,
                    capped,
                    hidden_before: &mut hidden_before,
                    visible_start,
                    last_tool_idx,
                    streaming,
                };
                render_explored_tool_pill_run(ui, &tool_run, &mut pill_ctx);
            }
        }
    }

    if capped {
        let hidden = tool_count.saturating_sub(MAX_VISIBLE_STREAMING_TOOL_PILLS);
        if hidden > 0 {
            ui.add_space(2.0);
            ui.label(
                RichText::new(format!("+{hidden} earlier tool calls"))
                    .size(FS_TINY)
                    .color(c_text_muted()),
            );
        }
    }
}

/// Inputs for [`render_explored_cluster`].
pub(super) struct ExploredClusterCtx<'a> {
    pub msg_idx: usize,
    pub blocks: &'a [AssistantBlock],
    pub start: usize,
    pub end: usize,
    pub streaming: bool,
}

pub(super) fn render_explored_cluster(ui: &mut Ui, ctx: ExploredClusterCtx<'_>) {
    let ExploredClusterCtx {
        msg_idx,
        blocks,
        start,
        end,
        streaming,
    } = ctx;

    render_explored_tool_list(ui, msg_idx, blocks, start, end, streaming);
    ui.add_space(4.0);
}
