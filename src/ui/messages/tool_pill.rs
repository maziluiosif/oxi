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
    clickable_expand_overlay, expand_persist_id, is_expanded, set_expanded, truncate_lines_preview,
};

use super::thinking::{render_thinking_group_block, thinking_group_is_live};
use crate::ui::diff::{diff_layout_job, split_chat_diff_layout_jobs};

use super::tool_format::{diff_counts, mono_output_job, tool_icon, tool_summary_text};
use super::{is_edit_like_tool, selectable_layout_job, selectable_layout_job_with_wrap};

const BLOCK_PREVIEW_LINES: usize = 10;
const EDIT_PREVIEW_LINES: usize = 10;
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
    let has_output = !output.trim().is_empty();
    // Finalization—not whether the first output chunk arrived—controls the running state. This is
    // especially important for bash, whose output is updated incrementally while it is in flight.
    // Only the last pill in the visual run gets the spinner.
    let tool_in_flight = streaming && is_error.is_none();
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
    let is_bash = name.eq_ignore_ascii_case("bash");
    // Every visible tool keeps an unfold affordance, even before it has output or after an empty
    // result. Bash is forced open while running, then remains user-foldable after completion.
    let can_expand = expandable;
    let expanded = can_expand && ((is_bash && running) || is_expanded(ui, persist_id));

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
            let text = if has_output {
                output.trim_end()
            } else {
                args_summary.as_deref().unwrap_or("Waiting for output…")
            };
            let overflow = text.lines().count() > BLOCK_PREVIEW_LINES || text.len() > 2000;
            let preview = if is_bash && running {
                // Live commands stay at the compact default height and show the newest output.
                let lines: Vec<&str> = text.lines().collect();
                lines[lines.len().saturating_sub(BLOCK_PREVIEW_LINES)..].join("\n")
            } else {
                truncate_lines_preview(text, BLOCK_PREVIEW_LINES)
            };
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

fn edit_preview_from_args(args_summary: Option<&String>) -> Option<String> {
    let raw = args_summary?;
    let value = serde_json::from_str::<serde_json::Value>(raw).ok()?;
    let path = value.get("path").and_then(|v| v.as_str()).unwrap_or("file");
    let mut pairs = Vec::new();
    if let Some(edits) = value.get("edits").and_then(|v| v.as_array()) {
        for edit in edits {
            if let (Some(old), Some(new)) = (
                edit.get("oldText").and_then(|v| v.as_str()),
                edit.get("newText").and_then(|v| v.as_str()),
            ) {
                pairs.push((old, new));
            }
        }
    } else if let (Some(old), Some(new)) = (
        value.get("oldText").and_then(|v| v.as_str()),
        value.get("newText").and_then(|v| v.as_str()),
    ) {
        pairs.push((old, new));
    }
    if pairs.is_empty() {
        return None;
    }
    let mut out = format!("--- a/{path}\n+++ b/{path}\n");
    for (old, new) in pairs {
        out.push_str("@@ preview @@\n");
        for line in old.lines() {
            out.push('-');
            out.push_str(line);
            out.push('\n');
        }
        for line in new.lines() {
            out.push('+');
            out.push_str(line);
            out.push('\n');
        }
    }
    Some(out)
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
    is_last_streaming_edit: bool,
) {
    if is_edit_like_tool(block) {
        render_edit_tool_block(ui, msg_idx, bi, block, streaming, is_last_streaming_edit);
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
    is_last_streaming_edit: bool,
) {
    let AssistantBlock::Tool {
        tool_call_id,
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

    let args_preview = if name.eq_ignore_ascii_case("write") {
        write_content_from_args(args_summary.as_ref())
            .map(|content| pseudo_diff_from_write_content(&content))
            .filter(|text| !text.trim().is_empty())
    } else if name.eq_ignore_ascii_case("edit") {
        edit_preview_from_args(args_summary.as_ref())
    } else {
        None
    };
    let rendered_diff = diff
        .as_ref()
        .filter(|t| !t.trim().is_empty())
        .cloned()
        .or(args_preview);

    let has_error = *is_error == Some(true);
    let has_diff = rendered_diff
        .as_deref()
        .is_some_and(|t| !t.trim().is_empty());
    // Argument-derived previews are visible before execution finishes, so finalization—not the
    // presence of a preview—controls the running state.
    let running = streaming && is_error.is_none();

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

    // Completed turns retain the existing always-open presentation. During a live turn, the newest
    // edit defaults open and earlier edits default folded, but every block keeps a manual override.
    // Key by provider call id so the choice survives re-layout while more blocks stream in.
    let open_id = expand_persist_id(if tool_call_id.is_empty() {
        Id::new((msg_idx, block_idx, "edit_block_open"))
    } else {
        Id::new(("edit_block_open", tool_call_id.as_str()))
    });
    let default_open = if streaming {
        is_last_streaming_edit
    } else {
        true
    };
    let is_open = ui
        .ctx()
        .data_mut(|d| d.get_persisted::<bool>(open_id))
        .unwrap_or(default_open);

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
                            let unfold = ui
                                .add(
                                    Label::new(
                                        RichText::new(if is_open {
                                            ICON_ANGLE_UP
                                        } else {
                                            ICON_ANGLE_DOWN
                                        })
                                        .font(FontId::new(FS_TINY, icon_font()))
                                        .color(c_text_faint()),
                                    )
                                    .sense(egui::Sense::click()),
                                )
                                .on_hover_text(if is_open { "Fold edit" } else { "Unfold edit" });
                            if unfold.clicked() {
                                set_expanded(ui, open_id, !is_open);
                            }

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
            if is_open
                && let Some(diff_text) = rendered_diff.as_deref().filter(|t| !t.trim().is_empty())
            {
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
                        let overflow = diff_text.lines().count() > EDIT_PREVIEW_LINES * 2
                            || diff_text.len() > 3000;
                        let persist_id =
                            expand_persist_id(Id::new((msg_idx, block_idx, "edit_split_diff")));
                        let expanded = !overflow || is_expanded(ui, persist_id);
                        let max_rows = (!expanded).then_some(EDIT_PREVIEW_LINES);
                        let frame = Frame::new()
                            .fill(diff_bg)
                            .stroke(Stroke::new(1.0, c_border()))
                            .corner_radius(CornerRadius::same(RADIUS_CHIP))
                            .inner_margin(Margin::symmetric(8, 5))
                            .show(ui, |ui| {
                                let (left, right) =
                                    split_chat_diff_layout_jobs(diff_text, max_rows);
                                ui.columns(2, |columns| {
                                    egui::ScrollArea::horizontal()
                                        .id_salt((msg_idx, block_idx, "before"))
                                        .auto_shrink([false, true])
                                        .show(&mut columns[0], |ui| {
                                            selectable_layout_job_with_wrap(
                                                ui, left, expanded, false,
                                            );
                                        });
                                    egui::ScrollArea::horizontal()
                                        .id_salt((msg_idx, block_idx, "after"))
                                        .auto_shrink([false, true])
                                        .show(&mut columns[1], |ui| {
                                            selectable_layout_job_with_wrap(
                                                ui, right, expanded, false,
                                            );
                                        });
                                });
                            });
                        if overflow {
                            clickable_expand_overlay(ui, frame.response.rect, persist_id);
                        }
                    });
            }
        });

    ui.add_space(6.0);
}

/// Render one consecutive run of tool blocks directly in transcript order. Tool calls must not be
/// capped independently of thinking blocks: hiding only tools makes interleaved thinking segments
/// collapse together and visually destroys the provider's chronological event order.
fn render_explored_tool_pill_run(
    ui: &mut Ui,
    msg_idx: usize,
    blocks: &[AssistantBlock],
    tool_run: &[usize],
    last_tool_idx: Option<usize>,
    streaming: bool,
) {
    for &ti in tool_run {
        let is_last = Some(ti) == last_tool_idx;
        render_tool_pill(ui, msg_idx, ti, &blocks[ti], streaming, is_last, true);
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
    let last_tool_idx = blocks[start..end]
        .iter()
        .enumerate()
        .rev()
        .find(|(_, b)| matches!(b, AssistantBlock::Tool { .. }))
        .map(|(j, _)| start + j);

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
                render_explored_tool_pill_run(
                    ui,
                    msg_idx,
                    blocks,
                    &tool_run,
                    last_tool_idx,
                    streaming,
                );
            }
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
