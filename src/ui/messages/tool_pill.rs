//! Tool-call rendering: compact unified pills with 1-click height-bounded scrollable drawers,
//! edit/write diff cards with bounded scrollable diffs, and the "explored" cluster that folds
//! runs of read/search calls.

use eframe::egui::text::{LayoutJob, TextFormat};
use eframe::egui::{
    self, Color32, CornerRadius, FontId, Frame, Id, Label, Margin, Rect, RichText, Stroke, Ui,
};

use crate::model::{AssistantBlock, concat_thinking_blocks};
use crate::settings::ToolDetail;
use crate::theme::*;
use crate::ui::diff::{chat_diff_natural_height, diff_layout_job, show_chat_diff};
use crate::ui::preview_expand::{expand_persist_id, is_expanded, is_expanded_or, set_expanded};

use super::thinking::{render_thinking_group_block, thinking_group_is_live};
use super::tool_format::{
    ToolSummary, diff_counts, mono_output_job, relativize_paths, tool_display_summary, tool_icon,
};
use super::{is_edit_like_tool, selectable_layout_job, show_bounded_scroll_box};

/// Unified height of a tool pill / edit card / explore group header row.
const TOOL_PILL_HEADER_H: f32 = 28.0;
/// Corner radius shared by every tool pill and card.
const TOOL_CARD_RADIUS: u8 = RADIUS_CHIP;
/// Vertical gap between consecutive tool pills (combined with `item_spacing.y = 3.0`).
const TOOL_PILL_GAP: f32 = 2.0;
/// Max scrollable height for a tool drawer opened by the user (or in Expanded mode).
const TOOL_DRAWER_MAX_HEIGHT: f32 = 240.0;
/// Compact max height for live-streaming bash / sub-agent output.
const LIVE_TOOL_MAX_HEIGHT: f32 = 168.0;
/// Max scrollable height for an open edit/write diff body.
const EDIT_DIFF_MAX_HEIGHT: f32 = 280.0;

struct ToolHeaderParams<'a> {
    persist_id: Id,
    icon: &'a str,
    summary: &'a ToolSummary,
    expanded: bool,
    /// When `true`, square off the header's bottom corners so a drawer/diff attaches seamlessly.
    attached_body: bool,
    running: bool,
    has_error: bool,
    diff_stats: Option<(usize, usize)>,
    pill_bg: Color32,
    header_hover_bg: Color32,
    icon_color: Color32,
    name_color: Color32,
    summary_color: Color32,
    copy_text: Option<(&'a str, &'static str)>,
    card_hovered: bool,
    clickable: bool,
}

struct ToolHeaderResult {
    rect: Rect,
    toggled: bool,
    copy_hovered: bool,
}

/// Shared single-line header painted inside a tool card's outer frame:
/// `[ icon  Action  monospace detail ...  [+N -M / spinner]  [copy]  chevron ]`.
///
/// Allocates an exact fixed height up front with `Sense::hover()` so hover fills and chevron
/// tints react with zero 1-frame lag and never steal wheel events from the outer transcript.
fn render_tool_header(ui: &mut Ui, params: ToolHeaderParams<'_>) -> ToolHeaderResult {
    let w = ui.available_width().max(40.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, TOOL_PILL_HEADER_H), egui::Sense::hover());
    let pointer_inside = ui
        .ctx()
        .input(|i| i.pointer.interact_pos().is_some_and(|p| rect.contains(p)));
    let hovered = params.clickable && pointer_inside;

    let radius = if params.attached_body {
        CornerRadius {
            nw: TOOL_CARD_RADIUS,
            ne: TOOL_CARD_RADIUS,
            sw: 0,
            se: 0,
        }
    } else {
        CornerRadius::same(TOOL_CARD_RADIUS)
    };
    let fill = if hovered {
        params.header_hover_bg
    } else {
        params.pill_bg
    };
    ui.painter().rect_filled(rect, radius, fill);

    let mut copy_clicked = false;
    let mut copy_hovered = false;
    let copy_id = params.persist_id.with("hdr_copy");
    let copied = crate::ui::chrome::copied_recently(ui.ctx(), copy_id);
    let show_copy =
        params.expanded && params.copy_text.is_some() && (params.card_hovered || hovered || copied);

    let inner = rect.shrink2(egui::vec2(11.0, 0.0));
    ui.scope_builder(egui::UiBuilder::new().max_rect(inner), |ui| {
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 7.0;
            ui.add(
                Label::new(
                    RichText::new(params.icon)
                        .font(FontId::new(FS_SMALL, icon_font()))
                        .color(if hovered && !params.has_error {
                            c_text_muted()
                        } else {
                            params.icon_color
                        }),
                )
                .selectable(false),
            );
            // Right-side status first so the (truncated) summary claims whatever width remains.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add(
                    Label::new(
                        RichText::new(if params.expanded {
                            ICON_ANGLE_DOWN
                        } else {
                            ICON_CHEVRON_RIGHT
                        })
                        .font(FontId::new(FS_TINY - 1.5, icon_font()))
                        .color(if hovered {
                            c_text_muted()
                        } else {
                            c_text_faint()
                        }),
                    )
                    .selectable(false),
                );
                if show_copy && let Some((text, toast)) = params.copy_text {
                    let resp = crate::ui::chrome::icon_button_core(
                        ui,
                        if copied { ICON_CHECK } else { ICON_COPY },
                        egui::vec2(20.0, 20.0),
                        FS_TINY - 0.5,
                        false,
                        &crate::ui::chrome::IconButtonLook {
                            fill: Color32::TRANSPARENT,
                            hover_fill: c_row_hover(),
                            stroke: Color32::TRANSPARENT,
                            hover_stroke: Color32::TRANSPARENT,
                            rounding: CornerRadius::same(RADIUS_ROW),
                            glyph: if copied { c_success() } else { c_text_faint() },
                        },
                    )
                    .on_hover_text(if copied { "Copied" } else { toast });
                    copy_hovered = resp.hovered();
                    if resp.clicked() {
                        copy_clicked = true;
                        ui.ctx().copy_text(text.to_owned());
                        crate::ui::chrome::mark_copied(ui.ctx(), copy_id);
                        crate::ui::chrome::show_toast(ui.ctx(), toast);
                    }
                }
                if params.running {
                    crate::theme::spinner(ui, 11.0, c_text_muted());
                } else if !params.has_error
                    && let Some((added, removed)) = params.diff_stats
                {
                    diff_stat_labels(ui, added, removed);
                }
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    render_summary_text(
                        ui,
                        params.summary,
                        params.name_color,
                        params.summary_color,
                    );
                });
            });
        });
    });

    if hovered && !copy_hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let toggled = hovered
        && !copy_clicked
        && !copy_hovered
        && ui.ctx().input(|i| i.pointer.primary_clicked());

    ToolHeaderResult {
        rect,
        toggled,
        copy_hovered,
    }
}

/// Measure a wrapped `LayoutJob` cheaply: when the line count or byte length obviously exceeds
/// `max_h`, return an overflow height immediately instead of laying out huge blobs twice.
fn measure_job_height(
    ui: &Ui,
    job: &LayoutJob,
    line_count_hint: usize,
    max_h: f32,
    line_h: f32,
) -> f32 {
    let max_lines_guess = (max_h / line_h).ceil() as usize + 6;
    if job.text.len() > 3000 || line_count_hint > max_lines_guess {
        return max_h + line_h * 2.0;
    }
    ui.fonts_mut(|fonts| fonts.layout_job(job.clone()).size().y)
        .max(line_h)
}

/// Paint a 1px horizontal divider inside a tool card at `y` (drawn after adjacent fills so
/// it stays crisp on top of both header and body surfaces).
fn paint_card_divider_at(ui: &Ui, x_range: egui::Rangef, y: f32) {
    ui.painter()
        .hline(x_range, y, Stroke::new(1.0, c_border_subtle()));
}

/// Render a bounded scrollable monospace/diff `LayoutJob` section inside an open tool card.
#[allow(clippy::too_many_arguments)]
fn render_card_job_section(
    ui: &mut Ui,
    panel_fill: Color32,
    bottom_radius: u8,
    scroll_id: Id,
    max_height: f32,
    stick_to_bottom: bool,
    line_count_hint: usize,
    make_job: impl FnOnce(f32) -> LayoutJob,
) {
    let line_h = FS_TINY * 1.45;
    Frame::new()
        .fill(panel_fill)
        .corner_radius(CornerRadius {
            nw: 0,
            ne: 0,
            sw: bottom_radius,
            se: bottom_radius,
        })
        .inner_margin(Margin::symmetric(12, 8))
        .show(ui, |ui| {
            let inner = ui.available_width().max(40.0);
            ui.set_width(inner);
            let job = make_job(inner);
            let content_h = measure_job_height(ui, &job, line_count_hint, max_height, line_h);
            show_bounded_scroll_box(
                ui,
                scroll_id,
                inner,
                content_h,
                max_height,
                line_h,
                panel_fill,
                stick_to_bottom,
                true,
                |ui| {
                    selectable_layout_job(ui, job, true);
                },
            );
        });
}

/// Compact footer strip inside the bottom of a tool card when tool output was truncated.
fn render_truncation_footer(ui: &mut Ui, fill: Color32, full_output_path: Option<&str>) {
    let resp = Frame::new()
        .fill(fill)
        .corner_radius(CornerRadius {
            nw: 0,
            ne: 0,
            sw: TOOL_CARD_RADIUS,
            se: TOOL_CARD_RADIUS,
        })
        .inner_margin(Margin::symmetric(12, 5))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 5.0;
                ui.add(
                    Label::new(
                        RichText::new("Output truncated")
                            .size(FS_TINY)
                            .color(c_text_faint()),
                    )
                    .selectable(false),
                );
                if let Some(path) = full_output_path {
                    ui.add(
                        Label::new(RichText::new("·").size(FS_TINY).color(c_text_faint()))
                            .selectable(false),
                    );
                    let resp = ui
                        .add(
                            Label::new(
                                RichText::new("Open full output")
                                    .size(FS_TINY)
                                    .color(c_accent()),
                            )
                            .sense(egui::Sense::click())
                            .selectable(false),
                        )
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .on_hover_text(path);
                    if resp.clicked() {
                        crate::ui::messages::request_open_tool_output(ui.ctx(), path);
                    }
                }
            });
        })
        .response;
    paint_card_divider_at(ui, resp.rect.x_range(), resp.rect.top());
}

/// Whether the pointer is inside this card's rect from the previous frame (used to reveal the
/// subtle header Copy icon when hovering anywhere over an open card).
fn card_was_hovered(ui: &Ui, card_rect_id: Id) -> bool {
    let Some(rect) = ui.ctx().data(|d| d.get_temp::<Rect>(card_rect_id)) else {
        return false;
    };
    ui.ctx()
        .input(|i| i.pointer.interact_pos().is_some_and(|p| rect.contains(p)))
}

fn store_card_rect(ui: &Ui, card_rect_id: Id, rect: Rect) {
    ui.ctx().data_mut(|d| d.insert_temp(card_rect_id, rect));
}

/// Render a single tool call as a compact pill that opens its complete output/diff in a single
/// click inside a height-bounded scrollable drawer.
pub(super) fn render_tool_pill(
    ui: &mut Ui,
    msg_idx: usize,
    block_idx: usize,
    block: &AssistantBlock,
    streaming: bool,
    is_last_in_run: bool,
    is_newest_tool: bool,
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
        metadata,
    } = block
    else {
        return;
    };

    let has_error = *is_error == Some(true);
    let has_output = !output.trim().is_empty();
    // Finalization—not whether the first output chunk arrived—controls the running state. This is
    // especially important for bash, whose output is updated incrementally while it is in flight.
    // Only the last pill in the visual run gets the spinner, except sub-agents: parallel `task`
    // calls genuinely run side by side and each reports its own end.
    let is_bash = name.eq_ignore_ascii_case("bash");
    let is_task = name == "task";
    let in_bg = !tool_call_id.is_empty() && super::is_background_tool(ui.ctx(), tool_call_id);
    let tool_in_flight = (streaming || in_bg)
        && metadata.as_ref().map_or(is_error.is_none(), |m| {
            matches!(
                m.status,
                crate::model::ToolStatus::Pending | crate::model::ToolStatus::InProgress
            )
        });
    let running = tool_in_flight && (is_last_in_run || is_task || in_bg);

    let pill_bg = if has_error {
        crate::theme::c_tool_error_bg()
    } else if running {
        crate::theme::c_tool_running_bg()
    } else {
        crate::theme::c_tool_pill_bg()
    };
    let header_hover_bg = if has_error {
        blend_color(pill_bg, c_danger(), 0.08)
    } else {
        blend_color(pill_bg, c_row_hover(), 0.75)
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
    let summary = tool_display_summary(block, running);
    let diff_text = diff.as_deref().filter(|d| !d.trim().is_empty());
    let diff_stats = diff_text.map(diff_counts);

    // Click-to-expand: keyed on the stable provider tool-call id so the fold state survives
    // re-layout; falls back to the transcript position when the id is missing.
    let persist_id = expand_persist_id(if tool_call_id.is_empty() {
        Id::new(("tool_pill", msg_idx, block_idx))
    } else {
        Id::new(("tool_pill", tool_call_id.as_str()))
    });
    // Every visible tool keeps an unfold affordance, even before it has output or after an empty
    // result. Bash and sub-agents stay open while running and while they are the newest tool of
    // a live turn so their live progress is visible without jumping the transcript on completion.
    let detail = super::tool_detail(ui);
    let live = detail == ToolDetail::Balanced
        && (is_bash || is_task)
        && (running || (streaming && is_newest_tool));
    let open_by_default = detail == ToolDetail::Expanded && (has_output || diff_stats.is_some());
    let expanded = live || is_expanded_or(ui, persist_id, open_by_default);

    let bash_cmd = is_bash
        .then(|| bash_command(args_summary.as_deref()))
        .flatten();
    let copy_payload: Option<(&str, &'static str)> = if let Some(d) = diff_text {
        Some((d, "Copy diff"))
    } else if has_output {
        Some((output.trim_end(), "Copy output"))
    } else {
        bash_cmd.as_deref().map(|cmd| (cmd, "Copy command"))
    };

    let card_w = ui.available_width().max(40.0);
    let card_rect_id = persist_id.with("card_rect");
    let card_hovered = card_was_hovered(ui, card_rect_id);
    let preview_header =
        Rect::from_min_size(ui.cursor().min, egui::vec2(card_w, TOOL_PILL_HEADER_H));
    let pre_hovered = ui.ctx().input(|i| {
        i.pointer
            .interact_pos()
            .is_some_and(|p| preview_header.contains(p))
    });
    let card_border = if has_error {
        pill_border
    } else if expanded || pre_hovered {
        c_border()
    } else {
        pill_border
    };

    let show_trunc_footer = *output_truncated || full_output_path.is_some();
    let mut header_rect = Rect::NOTHING;
    let mut header_toggled = false;
    let mut header_copy_hovered = false;

    let card_resp = Frame::new()
        .fill(Color32::TRANSPARENT)
        .stroke(Stroke::new(1.0, card_border))
        .corner_radius(CornerRadius::same(TOOL_CARD_RADIUS))
        .show(ui, |ui| {
            let inner_w = ui.available_width().max(40.0);
            ui.set_width(inner_w);
            ui.spacing_mut().item_spacing.y = 0.0;

            let open_header_bg = if expanded && !has_error {
                blend_color(pill_bg, c_bg_elevated(), 0.6)
            } else {
                pill_bg
            };
            let hdr = render_tool_header(
                ui,
                ToolHeaderParams {
                    persist_id,
                    icon,
                    summary: &summary,
                    expanded,
                    attached_body: expanded,
                    running,
                    has_error,
                    diff_stats,
                    pill_bg: open_header_bg,
                    header_hover_bg,
                    icon_color,
                    name_color,
                    summary_color,
                    copy_text: copy_payload,
                    card_hovered,
                    clickable: true,
                },
            );
            header_rect = hdr.rect;
            header_toggled = hdr.toggled;
            header_copy_hovered = hdr.copy_hovered;

            if expanded {
                let body_radius = if show_trunc_footer {
                    0
                } else {
                    TOOL_CARD_RADIUS
                };
                let max_h = if live {
                    LIVE_TOOL_MAX_HEIGHT
                } else {
                    TOOL_DRAWER_MAX_HEIGHT
                };
                let scroll_id = persist_id.with("drawer_scroll");
                if let Some(diff_text) = diff_text {
                    let line_count = diff_text.lines().count();
                    render_card_job_section(
                        ui,
                        crate::theme::c_tool_diff_bg(),
                        body_radius,
                        scroll_id,
                        EDIT_DIFF_MAX_HEIGHT,
                        false,
                        line_count,
                        |inner| diff_layout_job(diff_text, inner),
                    );
                } else {
                    let text = if has_output {
                        output.trim_end()
                    } else {
                        args_summary.as_deref().unwrap_or("Waiting for output…")
                    };
                    let text = relativize_paths(ui.ctx(), text);
                    let text = text.as_ref();
                    let line_count = text.lines().count() + usize::from(bash_cmd.is_some()) * 2;
                    let cmd_line = bash_cmd.clone();
                    render_card_job_section(
                        ui,
                        crate::theme::c_tool_diff_bg(),
                        body_radius,
                        scroll_id,
                        max_h,
                        live,
                        line_count,
                        move |inner| {
                            if let Some(cmd) = cmd_line.as_deref() {
                                bash_output_job(cmd, text, has_output, inner)
                            } else {
                                mono_output_job(text, inner)
                            }
                        },
                    );
                }
                paint_card_divider_at(ui, header_rect.x_range(), header_rect.bottom());
                if show_trunc_footer {
                    render_truncation_footer(ui, open_header_bg, full_output_path.as_deref());
                }
            }
        })
        .response;

    store_card_rect(ui, card_rect_id, card_resp.rect);

    if let Some(command) = bash_cmd.as_deref()
        && !header_copy_hovered
    {
        command_context_menu(ui, header_rect, persist_id, command);
    }
    if header_toggled {
        set_expanded(
            ui,
            persist_id,
            !is_expanded_or(ui, persist_id, open_by_default),
        );
    }
}

/// Formatted `$ <command>` + optional body output for bash drawers.
fn bash_output_job(cmd: &str, body: &str, has_output: bool, width: f32) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = width;
    job.break_on_newline = true;
    let line_h = Some(FS_TINY * 1.45);
    let fmt = |color: Color32| TextFormat {
        font_id: FontId::monospace(FS_TINY),
        color,
        line_height: line_h,
        ..Default::default()
    };
    job.append("$ ", 0.0, fmt(c_text_faint()));
    job.append(cmd, 0.0, fmt(c_text_strong()));
    if !body.is_empty() && has_output {
        job.append("\n\n", 0.0, fmt(c_text_muted()));
        job.append(body, 0.0, fmt(c_text_muted()));
    }
    job
}

/// Both labels share the space left after reserving the icon and trailing controls.
/// Bound the title as well as the detail: ACP titles and native/MCP names can be arbitrary text.
fn render_summary_text(
    ui: &mut Ui,
    summary: &ToolSummary,
    action_color: Color32,
    detail_color: Color32,
) {
    let available = ui.available_width().max(0.0);
    let has_detail = !summary.detail.is_empty();
    let title = summary.action.replace(['\n', '\r'], " ");
    let title_font = FontId::new(FS_SMALL, strong_font());
    let natural_width = ui
        .painter()
        .layout_no_wrap(title.clone(), title_font.clone(), action_color)
        .size()
        .x
        .ceil();
    let title_width = natural_width.min(if has_detail {
        available * 0.6
    } else {
        available
    });
    ui.add_sized(
        egui::vec2(title_width, 18.0),
        Label::new(RichText::new(title).font(title_font).color(action_color))
            .truncate()
            .selectable(false),
    );
    if has_detail && ui.available_width() > 0.0 {
        ui.add(
            Label::new(
                RichText::new(&summary.detail)
                    .size(FS_SMALL - 0.5)
                    .color(detail_color)
                    .monospace(),
            )
            .truncate()
            .selectable(false),
        );
    }
}

/// Colored `+N -M` line counts, laid out for a right-to-left row (removed is added first so it
/// ends up on the right).
fn diff_stat_labels(ui: &mut Ui, added: usize, removed: usize) {
    ui.spacing_mut().item_spacing.x = 5.0;
    ui.add(
        Label::new(
            RichText::new(format!("-{removed}"))
                .size(FS_TINY)
                .color(c_diff_del_fg())
                .monospace(),
        )
        .selectable(false),
    );
    ui.add(
        Label::new(
            RichText::new(format!("+{added}"))
                .size(FS_TINY)
                .color(c_diff_add_fg())
                .monospace(),
        )
        .selectable(false),
    );
    ui.add_space(2.0);
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
    is_newest_tool: bool,
) {
    if is_edit_like_tool(block) {
        render_edit_tool_block(ui, msg_idx, bi, block, streaming, is_last_streaming_edit);
        return;
    }

    render_tool_pill(ui, msg_idx, bi, block, streaming, streaming, is_newest_tool);
    if let AssistantBlock::Tool {
        tool_call_id,
        args_summary,
        ..
    } = block
    {
        let args = args_summary
            .as_deref()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok());
        // A tool links only to the child it launched. Generic Agent/Workflow calls must not
        // repeat a menu containing every child in the conversation.
        let links = ui
            .ctx()
            .data(|d| {
                d.get_temp::<Vec<(String, String, String, Option<String>)>>(egui::Id::new(
                    "subagent_links",
                ))
            })
            .unwrap_or_default();
        let linked = links.iter().find(|(root, id, _, tool)| {
            tool.as_deref() == Some(tool_call_id.as_str())
                || args.as_ref().is_some_and(|a| {
                    a["session_id"].as_str() == Some(id.as_str())
                        && a["root_session_id"].as_str() == Some(root.as_str())
                })
        });
        if let Some((root, id, label, _)) = linked
            && ui
                .add(
                    egui::Button::new(
                        RichText::new("View agent →")
                            .size(FS_TINY)
                            .color(c_text_muted()),
                    )
                    .frame(false),
                )
                .on_hover_text(label)
                .clicked()
        {
            ui.ctx().data_mut(|d| {
                d.insert_temp(egui::Id::new("open_subagent"), (root.clone(), id.clone()))
            });
        }
    }
    // Same gap as between pills of an explore cluster, so a run of tools does not shift when the
    // third explore call regroups the earlier ones into a cluster.
    ui.add_space(TOOL_PILL_GAP);
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
        output,
        metadata,
        output_truncated,
        full_output_path,
        is_error,
        ..
    } = block
    else {
        return;
    };

    let args_preview = if metadata.is_some() {
        None
    } else if name.eq_ignore_ascii_case("write") {
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
    let diff_text = rendered_diff.as_deref().filter(|t| !t.trim().is_empty());
    let has_diff = diff_text.is_some();
    // Argument-derived previews are visible before execution finishes, so finalization—not the
    // presence of a preview—controls the running state.
    let running = streaming
        && metadata.as_ref().map_or(is_error.is_none(), |m| {
            matches!(
                m.status,
                crate::model::ToolStatus::Pending | crate::model::ToolStatus::InProgress
            )
        });

    let diff_stats = diff_text.map(diff_counts);
    let header_bg = if has_error {
        crate::theme::c_tool_error_bg()
    } else if running {
        crate::theme::c_tool_running_bg()
    } else {
        crate::theme::c_tool_pill_bg()
    };
    let header_hover_bg = if has_error {
        blend_color(header_bg, c_danger(), 0.08)
    } else {
        blend_color(header_bg, c_row_hover(), 0.75)
    };
    let diff_bg = crate::theme::c_tool_diff_bg();

    // Completed turns retain the always-open presentation (bounded in height so large diffs stay
    // scrollable without eating the transcript). During a live turn, the newest edit defaults open
    // and earlier edits default folded, with a manual toggle on every card.
    let open_id = expand_persist_id(if tool_call_id.is_empty() {
        Id::new((msg_idx, block_idx, "edit_block_open"))
    } else {
        Id::new(("edit_block_open", tool_call_id.as_str()))
    });
    let default_open = match super::tool_detail(ui) {
        ToolDetail::Minimal => false,
        ToolDetail::Balanced => !streaming || is_last_streaming_edit,
        ToolDetail::Expanded => true,
    };
    let is_open = is_expanded_or(ui, open_id, default_open);
    let can_toggle = has_diff || !output.is_empty() || args_summary.is_some();

    let bare_confirmation = has_diff && !has_error && output.trim().lines().count() <= 1;
    let extra_detail = if !output.is_empty() && !bare_confirmation {
        output.as_str()
    } else if !has_diff {
        args_summary
            .as_deref()
            .unwrap_or("No diff or output provided by the agent.")
    } else {
        ""
    };
    let extra_detail = relativize_paths(ui.ctx(), extra_detail);
    let extra_detail = extra_detail.as_ref();
    let has_extra = !extra_detail.is_empty();
    let show_trunc_footer = *output_truncated || full_output_path.is_some();
    let has_open_body = is_open && (has_diff || has_extra || show_trunc_footer);

    let card_w = ui.available_width().max(40.0);
    let card_rect_id = open_id.with("card_rect");
    let card_hovered = card_was_hovered(ui, card_rect_id);
    let preview_header =
        Rect::from_min_size(ui.cursor().min, egui::vec2(card_w, TOOL_PILL_HEADER_H));
    let pre_hovered = can_toggle
        && ui.ctx().input(|i| {
            i.pointer
                .interact_pos()
                .is_some_and(|p| preview_header.contains(p))
        });
    let outer_border = if has_error {
        crate::theme::c_tool_error_border()
    } else if has_open_body || pre_hovered {
        c_border()
    } else if running {
        crate::theme::c_tool_running_border()
    } else {
        crate::theme::c_tool_pill_border()
    };

    let summary = tool_display_summary(block, running);
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
    let copy_payload = diff_text
        .map(|d| (d, "Copy diff"))
        .or_else(|| (!extra_detail.is_empty()).then_some((extra_detail, "Copy output")));

    let mut header_toggled = false;
    let card_resp = Frame::new()
        .fill(Color32::TRANSPARENT)
        .stroke(Stroke::new(1.0, outer_border))
        .corner_radius(CornerRadius::same(TOOL_CARD_RADIUS))
        .show(ui, |ui| {
            let inner_w = ui.available_width().max(40.0);
            ui.set_width(inner_w);
            ui.spacing_mut().item_spacing.y = 0.0;

            let open_header_bg = if has_open_body && !has_error {
                blend_color(header_bg, c_bg_elevated(), 0.6)
            } else {
                header_bg
            };
            let hdr = render_tool_header(
                ui,
                ToolHeaderParams {
                    persist_id: open_id,
                    icon: tool_icon(name),
                    summary: &summary,
                    expanded: is_open,
                    attached_body: has_open_body,
                    running,
                    has_error,
                    diff_stats,
                    pill_bg: open_header_bg,
                    header_hover_bg,
                    icon_color,
                    name_color,
                    summary_color,
                    copy_text: copy_payload,
                    card_hovered,
                    clickable: can_toggle,
                },
            );
            header_toggled = hdr.toggled;
            let edit_header_rect = hdr.rect;

            if is_open && let Some(diff_text) = diff_text {
                let diff_bottom_radius = if has_extra || show_trunc_footer {
                    0
                } else {
                    TOOL_CARD_RADIUS
                };
                Frame::new()
                    .fill(diff_bg)
                    .corner_radius(CornerRadius {
                        nw: 0,
                        ne: 0,
                        sw: diff_bottom_radius,
                        se: diff_bottom_radius,
                    })
                    .inner_margin(Margin::symmetric(10, 8))
                    .show(ui, |ui| {
                        let inner_w = ui.available_width().max(40.0);
                        ui.set_width(inner_w);
                        let natural_h = chat_diff_natural_height(ui, diff_text, inner_w);
                        let row_h = ui.fonts_mut(|f| f.row_height(&FontId::monospace(FS_CODE)));
                        show_bounded_scroll_box(
                            ui,
                            open_id.with("diff_scroll"),
                            inner_w,
                            natural_h,
                            EDIT_DIFF_MAX_HEIGHT,
                            row_h,
                            diff_bg,
                            false,
                            true,
                            |ui| {
                                show_chat_diff(
                                    ui,
                                    diff_text,
                                    None,
                                    Id::new((msg_idx, block_idx, "split_diff")),
                                    true,
                                );
                            },
                        );
                    });
                paint_card_divider_at(ui, edit_header_rect.x_range(), edit_header_rect.bottom());
            }

            if is_open && has_extra {
                let extra_top = ui.cursor().top();
                let extra_bottom_radius = if show_trunc_footer {
                    0
                } else {
                    TOOL_CARD_RADIUS
                };
                let line_count = extra_detail.lines().count();
                render_card_job_section(
                    ui,
                    diff_bg,
                    extra_bottom_radius,
                    open_id.with("output_scroll"),
                    TOOL_DRAWER_MAX_HEIGHT,
                    false,
                    line_count,
                    |inner| mono_output_job(extra_detail, inner),
                );
                paint_card_divider_at(ui, edit_header_rect.x_range(), extra_top);
            }

            if is_open && show_trunc_footer {
                render_truncation_footer(ui, open_header_bg, full_output_path.as_deref());
            }
        })
        .response;

    store_card_rect(ui, card_rect_id, card_resp.rect);

    if header_toggled {
        set_expanded(ui, open_id, !is_open);
    }

    ui.add_space(TOOL_PILL_GAP);
}

/// Whether a completed tool call is a quiet read-only exploration step that can be folded into
/// an "Explored N files · M searches" summary group in Balanced/Minimal mode.
fn is_collapsible_explore_tool(block: &AssistantBlock) -> bool {
    let AssistantBlock::Tool {
        name,
        is_error,
        diff,
        ..
    } = block
    else {
        return false;
    };
    if *is_error == Some(true) || diff.as_deref().is_some_and(|d| !d.trim().is_empty()) {
        return false;
    }
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "read"
            | "read_file"
            | "view_file"
            | "view_file_outline"
            | "view_code_item"
            | "grep"
            | "grep_search"
            | "search"
            | "find"
            | "find_by_name"
            | "glob"
            | "ls"
            | "list"
            | "list_dir"
            | "fetch"
            | "web_fetch"
            | "codebase_search"
            | "web_search"
            | "search_web"
            | "read_url_content"
    )
}

#[derive(Clone, Copy)]
enum ExploreKind {
    File,
    Search,
    List,
    Web,
}

fn explore_kind(name: &str) -> ExploreKind {
    match name.to_ascii_lowercase().as_str() {
        "read" | "read_file" | "view_file" | "view_file_outline" | "view_code_item" => {
            ExploreKind::File
        }
        "grep" | "grep_search" | "search" | "find" | "find_by_name" | "glob" => ExploreKind::Search,
        "ls" | "list" | "list_dir" => ExploreKind::List,
        _ => ExploreKind::Web,
    }
}

fn explore_group_summary(blocks: &[AssistantBlock], tool_run: &[usize]) -> String {
    let (mut files, mut searches, mut lists, mut web) = (0usize, 0usize, 0usize, 0usize);
    for &ti in tool_run {
        if let AssistantBlock::Tool { name, .. } = &blocks[ti] {
            match explore_kind(name) {
                ExploreKind::File => files += 1,
                ExploreKind::Search => searches += 1,
                ExploreKind::List => lists += 1,
                ExploreKind::Web => web += 1,
            }
        }
    }
    let mut parts = Vec::new();
    if files > 0 {
        parts.push(format!(
            "{files} {}",
            if files == 1 { "file" } else { "files" }
        ));
    }
    if searches > 0 {
        parts.push(format!(
            "{searches} {}",
            if searches == 1 { "search" } else { "searches" }
        ));
    }
    if lists > 0 {
        parts.push(format!(
            "{lists} {}",
            if lists == 1 { "folder" } else { "folders" }
        ));
    }
    if web > 0 {
        parts.push(format!(
            "{web} {}",
            if web == 1 {
                "web lookup"
            } else {
                "web lookups"
            }
        ));
    }
    if parts.is_empty() {
        format!("Explored {} items", tool_run.len())
    } else {
        format!("Explored {}", parts.join(" · "))
    }
}

/// Render one consecutive run of tool blocks directly in transcript order. Tool calls must not be
/// capped independently of thinking blocks: hiding only tools makes interleaved thinking segments
/// collapse together and visually destroys the provider's chronological event order.
///
/// Once the turn finishes, runs of 3+ quiet read-only exploration calls fold into one clickable
/// summary pill in Balanced/Minimal mode so long investigation turns stay scannable.
fn render_explored_tool_pill_run(
    ui: &mut Ui,
    msg_idx: usize,
    blocks: &[AssistantBlock],
    tool_run: &[usize],
    last_tool_idx: Option<usize>,
    newest_tool_idx: Option<usize>,
    streaming: bool,
) {
    let may_collapse = !streaming && super::tool_detail(ui) != ToolDetail::Expanded;
    let segments = if may_collapse {
        explore_segments(blocks, tool_run)
    } else {
        vec![(false, tool_run)]
    };
    for (collapse, segment) in segments {
        if collapse {
            render_explore_group(
                ui,
                msg_idx,
                blocks,
                segment,
                last_tool_idx,
                newest_tool_idx,
                streaming,
            );
            continue;
        }
        for &ti in segment {
            let is_last = Some(ti) == last_tool_idx;
            let is_newest = Some(ti) == newest_tool_idx;
            render_tool_pill(ui, msg_idx, ti, &blocks[ti], streaming, is_last, is_newest);
            ui.add_space(TOOL_PILL_GAP);
        }
    }
}

/// Splits a run of tool calls into stretches to render: `true` marks 3+ consecutive quiet
/// exploration calls that fold into one summary pill, `false` calls shown one by one. A run
/// that reads six files and then edits one still folds the reads.
fn explore_segments<'a>(
    blocks: &[AssistantBlock],
    tool_run: &'a [usize],
) -> Vec<(bool, &'a [usize])> {
    let mut segments: Vec<(bool, &'a [usize])> = Vec::new();
    let mut start = 0;
    while start < tool_run.len() {
        let explore = is_collapsible_explore_tool(&blocks[tool_run[start]]);
        let len = tool_run[start..]
            .iter()
            .take_while(|&&ti| is_collapsible_explore_tool(&blocks[ti]) == explore)
            .count();
        let stretch = &tool_run[start..start + len];
        let collapse = explore && len >= 3;
        // Short explore stretches render like their neighbours; merge them so the plain calls
        // stay one slice.
        match segments.last_mut() {
            Some((false, prev)) if !collapse => {
                let begin = start - prev.len();
                *prev = &tool_run[begin..start + len];
            }
            _ => segments.push((collapse, stretch)),
        }
        start += len;
    }
    segments
}

/// One folded "Explored …" pill for `tool_run`, expandable to the calls themselves.
fn render_explore_group(
    ui: &mut Ui,
    msg_idx: usize,
    blocks: &[AssistantBlock],
    tool_run: &[usize],
    last_tool_idx: Option<usize>,
    newest_tool_idx: Option<usize>,
    streaming: bool,
) {
    let persist_id = expand_persist_id(Id::new(("explore_group", msg_idx, tool_run[0])));
    let expanded = is_expanded(ui, persist_id);
    let summary_text = explore_group_summary(blocks, tool_run);
    let (action, detail) = match summary_text.strip_prefix("Explored ") {
        Some(rest) => ("Explored".to_string(), rest.to_string()),
        None => (summary_text, String::new()),
    };
    let summary = ToolSummary { action, detail };

    let card_w = ui.available_width().max(40.0);
    let pill_bg = crate::theme::c_tool_pill_bg();
    let header_hover_bg = blend_color(pill_bg, c_row_hover(), 0.75);
    let preview_header =
        Rect::from_min_size(ui.cursor().min, egui::vec2(card_w, TOOL_PILL_HEADER_H));
    let pre_hovered = ui.ctx().input(|i| {
        i.pointer
            .interact_pos()
            .is_some_and(|p| preview_header.contains(p))
    });
    let border = if expanded || pre_hovered {
        c_border()
    } else {
        crate::theme::c_tool_pill_border()
    };

    let mut toggled = false;
    Frame::new()
        .fill(Color32::TRANSPARENT)
        .stroke(Stroke::new(1.0, border))
        .corner_radius(CornerRadius::same(TOOL_CARD_RADIUS))
        .show(ui, |ui| {
            let inner_w = ui.available_width().max(40.0);
            ui.set_width(inner_w);
            let hdr = render_tool_header(
                ui,
                ToolHeaderParams {
                    persist_id,
                    icon: ICON_SEARCH,
                    summary: &summary,
                    expanded,
                    attached_body: false,
                    running: false,
                    has_error: false,
                    diff_stats: None,
                    pill_bg,
                    header_hover_bg,
                    icon_color: c_text_faint(),
                    name_color: c_text_muted(),
                    summary_color: c_text_faint(),
                    copy_text: None,
                    card_hovered: false,
                    clickable: true,
                },
            );
            toggled = hdr.toggled;
        });
    if toggled {
        set_expanded(ui, persist_id, !expanded);
    }
    ui.add_space(TOOL_PILL_GAP);
    if expanded {
        let group_resp = ui.indent(persist_id.with("items"), |ui| {
            for &ti in tool_run {
                let is_last = Some(ti) == last_tool_idx;
                let is_newest = Some(ti) == newest_tool_idx;
                render_tool_pill(ui, msg_idx, ti, &blocks[ti], streaming, is_last, is_newest);
                ui.add_space(TOOL_PILL_GAP);
            }
        });
        let r = group_resp.response.rect;
        ui.painter().vline(
            r.left() - 6.0,
            (r.top() + 2.0)..=(r.bottom() - TOOL_PILL_GAP - 2.0),
            Stroke::new(1.5, c_border_subtle()),
        );
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
    let newest_tool_idx = blocks
        .iter()
        .rposition(|b| matches!(b, AssistantBlock::Tool { .. }));

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
                    newest_tool_idx,
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
}

/// The shell command of a `bash` call, from its (possibly truncated) JSON arguments.
fn bash_command(args_summary: Option<&str>) -> Option<String> {
    let args: serde_json::Value = serde_json::from_str(args_summary?).ok()?;
    let command = args
        .get("command")
        .or_else(|| args.get("CommandLine"))?
        .as_str()?
        .trim();
    (!command.is_empty()).then(|| command.to_owned())
}

/// Right-click menu on a command pill's header: copy it, or type it into a new terminal tab.
fn command_context_menu(ui: &mut Ui, rect: eframe::egui::Rect, persist_id: Id, command: &str) {
    if !rect.is_positive() {
        return;
    }
    let response = ui.interact(
        rect,
        persist_id.with("command_menu"),
        eframe::egui::Sense::click(),
    );
    response.context_menu(|ui| {
        if ui
            .button("Open in terminal")
            .on_hover_text(
                "Type this command into a new terminal tab (not run until you press Enter)",
            )
            .clicked()
        {
            crate::ui::messages::request_terminal_command(ui.ctx(), command);
            ui.close();
        }
        if ui.button("Copy command").clicked() {
            ui.ctx().copy_text(command.to_owned());
            ui.close();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str, is_error: Option<bool>) -> AssistantBlock {
        AssistantBlock::Tool {
            tool_call_id: format!("call_{name}"),
            name: name.into(),
            args_summary: None,
            output: String::new(),
            diff: None,
            is_error,
            full_output_path: None,
            output_truncated: false,
            metadata: None,
        }
    }

    #[test]
    fn explore_group_summarizes_files_searches_and_folders() {
        let blocks = vec![
            tool("read", Some(false)),
            tool("read_file", Some(false)),
            tool("grep", Some(false)),
            tool("list_dir", Some(false)),
            tool("search_web", Some(false)),
        ];
        assert_eq!(
            explore_group_summary(&blocks, &[0, 1, 2, 3, 4]),
            "Explored 2 files · 1 search · 1 folder · 1 web lookup"
        );
        assert_eq!(explore_group_summary(&blocks, &[0]), "Explored 1 file");
    }

    #[test]
    fn collapsible_explore_excludes_errors_and_mutating_or_shell_tools() {
        assert!(is_collapsible_explore_tool(&tool("read", Some(false))));
        assert!(is_collapsible_explore_tool(&tool(
            "grep_search",
            Some(false)
        )));
        assert!(!is_collapsible_explore_tool(&tool("read", Some(true))));
        assert!(!is_collapsible_explore_tool(&tool("bash", Some(false))));
        assert!(!is_collapsible_explore_tool(&tool("edit", Some(false))));
        assert!(is_collapsible_explore_tool(&tool("web_fetch", Some(false))));
        assert!(is_collapsible_explore_tool(&tool(
            "codebase_search",
            Some(false)
        )));
    }

    #[test]
    fn explore_stretches_fold_even_next_to_other_tools() {
        let names = [
            "read", "grep", "read", "read", "edit", "read", "bash", "ls", "ls", "ls",
        ];
        let blocks: Vec<_> = names.iter().map(|name| tool(name, Some(false))).collect();
        let run: Vec<usize> = (0..blocks.len()).collect();
        let segments = explore_segments(&blocks, &run);
        assert_eq!(
            segments,
            vec![
                (true, &run[0..4]),
                // A lone read between other tools stays inline with them.
                (false, &run[4..7]),
                (true, &run[7..10]),
            ]
        );
        let short: Vec<usize> = run[0..2].to_vec();
        assert_eq!(explore_segments(&blocks, &short), vec![(false, &short[..])]);
    }
}
