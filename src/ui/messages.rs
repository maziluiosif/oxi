//! Chat transcript rendering (bubbles, assistant blocks, markdown).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use eframe::egui::text::{LayoutJob, LayoutSection, TextFormat, TextWrapping};
use eframe::egui::{
    CollapsingHeader, Color32, FontId, Frame, Id, Image, Label, Margin, RichText, Rounding, Stroke,
    TextureOptions, Ui,
};

use crate::markdown;
use crate::model::{
    assistant_is_effectively_empty, bash_command_tokens, build_assistant_block_groups,
    concat_thinking_blocks, estimate_thought_seconds, tool_breaks_explore_cluster,
    tool_compact_header, AssistantBlock, AssistantBlockGroup, ChatMessage, MsgRole, UserAttachment,
};
use crate::theme::{
    animated_status_label, content_wrap_width, tool_status_label, C_BG_ELEVATED, C_BG_INPUT,
    C_BORDER, C_TEXT, C_TEXT_MUTED, C_USER_BUBBLE, FS_BODY, FS_SMALL, FS_TINY,
};
use crate::ui::preview_expand::{
    clickable_expand_overlay, expand_persist_id, is_expanded, truncate_lines_preview,
};

const BLOCK_PREVIEW_LINES: usize = 10;
const EDIT_PREVIEW_LINES: usize = 10;
/// Left inset for bodies inside Worked / Explored so nested content reads under the parent header.
const NESTED_SECTION_INDENT: f32 = 12.0;

struct ExploredClusterCtx<'a> {
    msg_idx: usize,
    salt: usize,
    blocks: &'a [AssistantBlock],
    start: usize,
    end: usize,
    tool_indices: &'a [usize],
    streaming: bool,
}

fn nest_under_collapsed_header<R>(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
    ui.horizontal(|ui| {
        ui.add_space(NESTED_SECTION_INDENT);
        ui.vertical(|ui| {
            ui.set_width(ui.available_width());
            add_contents(ui)
        })
        .inner
    })
    .inner
}

fn monospace_wrapped_job(text: String, wrap_width: f32, color: Color32) -> LayoutJob {
    LayoutJob {
        sections: vec![LayoutSection {
            leading_space: 0.0,
            byte_range: 0..text.len(),
            format: TextFormat::simple(FontId::monospace(FS_SMALL), color),
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

fn diff_counts(diff: &str) -> (usize, usize) {
    let mut added = 0;
    let mut removed = 0;
    for line in diff.lines() {
        if line.starts_with('+') {
            added += 1;
        } else if line.starts_with('-') {
            removed += 1;
        }
    }
    (added, removed)
}

fn diff_wrapped_job(diff: &str, wrap_width: f32) -> LayoutJob {
    let lines: Vec<&str> = diff.lines().collect();
    let mut job = LayoutJob {
        wrap: TextWrapping {
            max_width: wrap_width,
            ..Default::default()
        },
        break_on_newline: true,
        ..Default::default()
    };

    for (i, line) in lines.iter().enumerate() {
        let start = job.text.len();
        job.text.push_str(line);
        if i + 1 < lines.len() {
            job.text.push('\n');
        }
        let end = job.text.len();
        let (color, background) = if line.starts_with('+') {
            (
                Color32::from_rgb(0xa7, 0xf3, 0xd0),
                Color32::from_rgb(0x12, 0x2c, 0x22),
            )
        } else if line.starts_with('-') {
            (
                Color32::from_rgb(0xfe, 0xca, 0xca),
                Color32::from_rgb(0x31, 0x16, 0x1b),
            )
        } else {
            (Color32::from_gray(160), Color32::TRANSPARENT)
        };
        job.sections.push(LayoutSection {
            leading_space: 0.0,
            byte_range: start..end,
            format: TextFormat {
                font_id: FontId::monospace(FS_TINY),
                color,
                background,
                ..Default::default()
            },
        });
    }

    job
}

fn tool_path_from_args(args_summary: Option<&String>) -> Option<String> {
    let raw = args_summary?;
    let value = serde_json::from_str::<serde_json::Value>(raw).ok()?;
    value
        .get("path")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .or_else(|| {
            value
                .get("filePath")
                .and_then(|v| v.as_str())
                .map(str::to_owned)
        })
}

fn user_image_texture(
    ui: &Ui,
    msg_idx: usize,
    i: usize,
    data: &[u8],
) -> Option<eframe::egui::TextureHandle> {
    let mut hasher = DefaultHasher::new();
    data.hash(&mut hasher);
    let h = hasher.finish();
    let cache_id = Id::new(("pi_user_att_tex", h));
    if let Some(tex) = ui
        .ctx()
        .data_mut(|d| d.get_persisted::<eframe::egui::TextureHandle>(cache_id))
    {
        return Some(tex.clone());
    }
    let dyn_img = image::load_from_memory(data).ok()?;
    let rgba = dyn_img.thumbnail(160, 160).to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    let color_image = eframe::egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
    let tex = ui.ctx().load_texture(
        format!("user_att_{msg_idx}_{i}_{h:016x}"),
        color_image,
        TextureOptions::default(),
    );
    ui.ctx()
        .data_mut(|d| d.insert_persisted(cache_id, tex.clone()));
    Some(tex)
}

fn selectable_layout_job(ui: &mut Ui, job: LayoutJob, allow_select: bool) {
    if job.text.is_empty() {
        return;
    }
    if !allow_select {
        ui.add(Label::new(job).wrap().selectable(false));
        return;
    }

    let galley = ui.fonts(|fonts| fonts.layout_job(job));
    let (rect, response) =
        ui.allocate_exact_size(galley.size(), eframe::egui::Sense::click_and_drag());
    let galley_pos = rect.left_top();
    eframe::egui::text_selection::LabelSelectionState::label_text_selection(
        ui,
        &response,
        galley_pos,
        galley,
        ui.style().visuals.text_color(),
        Stroke::NONE,
    );
}

fn render_expandable_monospace_panel(
    ui: &mut Ui,
    panel_fill: Color32,
    max_preview_lines: usize,
    persist_id: Id,
    content_overflows: bool,
    text: &str,
    color: Color32,
) {
    let frame = Frame::none()
        .fill(panel_fill)
        .stroke(Stroke::new(1.0, C_BORDER))
        .rounding(Rounding::same(8.0))
        .inner_margin(Margin::symmetric(8.0, 5.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let inner = ui.available_width().max(40.0);
            let display = if !content_overflows || is_expanded(ui, persist_id) {
                text.to_string()
            } else {
                truncate_lines_preview(text, max_preview_lines)
            };
            let allow_select = !content_overflows || is_expanded(ui, persist_id);
            selectable_layout_job(
                ui,
                monospace_wrapped_job(display, inner, color),
                allow_select,
            );
        });
    if content_overflows {
        clickable_expand_overlay(ui, frame.response.rect, persist_id);
    }
}

fn render_static_preview_job_panel(
    ui: &mut Ui,
    panel_fill: Color32,
    preview_job: LayoutJob,
    full_job: impl Fn(f32) -> LayoutJob,
    persist_id: Id,
    overflows: bool,
) {
    let frame = Frame::none()
        .fill(panel_fill)
        .stroke(Stroke::new(1.0, C_BORDER))
        .rounding(Rounding::same(8.0))
        .inner_margin(Margin::symmetric(8.0, 5.0))
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

fn block_state_tag(streaming: bool) -> &'static str {
    if streaming {
        "live"
    } else {
        "done"
    }
}

/// Edit-like tools (diffs or `edit`) render as full detail blocks, not explore-cluster rows.
fn is_edit_like_tool(block: &AssistantBlock) -> bool {
    tool_breaks_explore_cluster(block)
}

fn has_visible_assistant_content(blocks: &[AssistantBlock], streaming: bool) -> bool {
    blocks.iter().any(|block| match block {
        AssistantBlock::Answer(text) => !text.trim().is_empty(),
        AssistantBlock::Tool { output, diff, .. } => {
            !output.trim().is_empty()
                || diff
                    .as_deref()
                    .is_some_and(|diff_text| !diff_text.trim().is_empty())
        }
        AssistantBlock::Thinking(text) => streaming && !text.trim().is_empty(),
    })
}

pub fn render_message(ui: &mut Ui, msg_idx: usize, msg: &ChatMessage, agent_ack: bool) {
    let col_w = content_wrap_width(ui);

    if msg.role == MsgRole::User {
        ui.vertical(|ui| {
            ui.set_width(col_w);
            Frame::none()
                .fill(C_USER_BUBBLE)
                .stroke(Stroke::new(1.0, C_BORDER))
                .rounding(Rounding::same(18.0))
                .inner_margin(Margin::symmetric(12.0, 8.0))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    if !msg.text.is_empty() {
                        ui.add(
                            Label::new(
                                RichText::new(&msg.text)
                                    .size(FS_BODY)
                                    .line_height(Some(21.0))
                                    .color(C_TEXT),
                            )
                            .wrap()
                            .selectable(true),
                        );
                    }
                    if !msg.attachments.is_empty() {
                        ui.add_space(6.0);
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = eframe::egui::vec2(6.0, 6.0);
                            for (i, att) in msg.attachments.iter().enumerate() {
                                match att {
                                    UserAttachment::Image { mime, data } => {
                                        if let Some(tex) = user_image_texture(ui, msg_idx, i, data)
                                        {
                                            let mut sz = tex.size_vec2();
                                            let max = 120.0;
                                            let m = sz.x.max(sz.y);
                                            if m > max {
                                                sz *= max / m;
                                            }
                                            ui.add(Image::new((tex.id(), sz)));
                                        } else {
                                            ui.label(
                                                RichText::new(format!("[image: {mime}]"))
                                                    .size(FS_TINY)
                                                    .color(C_TEXT_MUTED),
                                            );
                                        }
                                    }
                                }
                            }
                        });
                    }
                });
        });
        ui.add_space(8.0);
        return;
    }

    render_assistant_message_run(ui, msg_idx, std::slice::from_ref(msg), agent_ack);
}

pub fn render_assistant_message_run(
    ui: &mut Ui,
    msg_idx: usize,
    messages: &[ChatMessage],
    agent_ack: bool,
) {
    let col_w = content_wrap_width(ui);
    let mut blocks = Vec::new();
    let mut streaming = false;
    for msg in messages {
        if msg.role != MsgRole::Assistant {
            continue;
        }
        streaming |= msg.streaming;
        blocks.extend(msg.blocks.iter().cloned());
    }

    if !streaming && !has_visible_assistant_content(&blocks, false) {
        return;
    }

    ui.vertical(|ui| {
        ui.set_width(col_w);
        render_assistant_blocks(ui, msg_idx, &blocks, streaming, agent_ack);
    });
    ui.add_space(8.0);
}

fn render_tool_block_details(
    ui: &mut Ui,
    msg_idx: usize,
    block_idx: usize,
    block: &AssistantBlock,
    streaming: bool,
    show_args_summary: bool,
) {
    let AssistantBlock::Tool {
        name,
        output,
        diff,
        args_summary,
        is_error,
        full_output_path,
        output_truncated,
        ..
    } = block
    else {
        return;
    };
    let tool_running = streaming && is_error.is_none();
    let has_diff = diff.as_deref().is_some_and(|text| !text.trim().is_empty());
    let has_output = !(output.trim().is_empty() || (is_edit_like_tool(block) && has_diff));

    if show_args_summary && !(is_edit_like_tool(block) && has_diff) {
        if let Some(args) = args_summary {
            if !args.is_empty() {
                ui.label(
                    RichText::new(args)
                        .size(FS_TINY)
                        .color(Color32::from_gray(120)),
                );
                ui.add_space(4.0);
            }
        }
    }

    if tool_running {
        animated_status_label(ui, &tool_status_label(name), FS_TINY);
        ui.add_space(6.0);
    }

    if !has_output && !tool_running && !has_diff {
        ui.label(
            RichText::new("No output")
                .size(FS_TINY)
                .color(Color32::from_gray(110)),
        );
    }

    if let Some(diff_text) = diff.as_ref().filter(|text| !text.trim().is_empty()) {
        let (added, removed) = diff_counts(diff_text);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("Diff  +{added}  -{removed}"))
                    .size(FS_TINY)
                    .color(C_TEXT_MUTED),
            );
            if let Some(path) = tool_path_from_args(args_summary.as_ref()) {
                ui.label(
                    RichText::new(path)
                        .size(FS_TINY)
                        .color(Color32::from_gray(120))
                        .monospace(),
                );
            }
        });
        let bubble_w = content_wrap_width(ui);
        ui.set_width(bubble_w);
        let diff_overflow =
            diff_text.lines().count() > EDIT_PREVIEW_LINES || diff_text.len() > 2000;
        let preview = truncate_lines_preview(diff_text, EDIT_PREVIEW_LINES);
        render_static_preview_job_panel(
            ui,
            C_BG_ELEVATED,
            diff_wrapped_job(&preview, bubble_w.max(40.0)),
            |inner| diff_wrapped_job(diff_text, inner),
            expand_persist_id(Id::new((msg_idx, block_idx, "tool_diff"))),
            diff_overflow,
        );
        if has_output {
            ui.add_space(8.0);
        }
    }

    if has_output {
        ui.horizontal(|ui| {
            if *output_truncated {
                ui.label(
                    RichText::new("truncated")
                        .size(FS_TINY)
                        .color(Color32::from_rgb(0xff, 0xc8, 0x80)),
                );
            }
            if let Some(p) = full_output_path {
                ui.label(
                    RichText::new(format!("full: {p}"))
                        .size(FS_TINY)
                        .color(Color32::from_gray(110)),
                );
            }
        });
        if let Some(p) = full_output_path {
            ui.horizontal(|ui| {
                if ui.small_button("Open file").clicked() {
                    #[cfg(target_os = "macos")]
                    let _ = std::process::Command::new("open").arg(p).spawn();
                    #[cfg(target_os = "linux")]
                    let _ = std::process::Command::new("xdg-open").arg(p).spawn();
                    #[cfg(target_os = "windows")]
                    let _ = std::process::Command::new("cmd")
                        .args(["/C", "start", "", p])
                        .spawn();
                }
            });
        }
        let bubble_w = content_wrap_width(ui);
        ui.set_width(bubble_w);
        let overflow = output.lines().count() > BLOCK_PREVIEW_LINES || output.len() > 2000;
        render_expandable_monospace_panel(
            ui,
            C_BG_INPUT,
            BLOCK_PREVIEW_LINES,
            expand_persist_id(Id::new((msg_idx, block_idx, "tool_output"))),
            overflow,
            output.as_str(),
            Color32::from_gray(165),
        );
    }
}

fn render_single_tool_block(
    ui: &mut Ui,
    msg_idx: usize,
    bi: usize,
    block: &AssistantBlock,
    streaming: bool,
) {
    if is_edit_like_tool(block) {
        render_tool_block_details(ui, msg_idx, bi, block, streaming, true);
        ui.add_space(6.0);
        return;
    }

    let AssistantBlock::Tool {
        name,
        output,
        is_error,
        ..
    } = block
    else {
        return;
    };
    let header = tool_compact_header(name, output);
    let hdr_color = if *is_error == Some(true) {
        Color32::from_rgb(0xff, 0xa0, 0xa0)
    } else {
        C_TEXT_MUTED
    };
    CollapsingHeader::new(RichText::new(header).size(FS_SMALL).color(hdr_color))
        .id_salt((msg_idx, bi, "tool", block_state_tag(streaming)))
        .default_open(streaming)
        .show_unindented(ui, |ui| {
            render_tool_block_details(ui, msg_idx, bi, block, streaming, true);
        });
    ui.add_space(6.0);
}

fn render_explored_cluster(ui: &mut Ui, ctx: ExploredClusterCtx<'_>) {
    let ExploredClusterCtx {
        msg_idx,
        salt,
        blocks,
        start,
        end,
        tool_indices,
        streaming,
    } = ctx;
    let commands = bash_command_tokens(blocks, tool_indices);
    let mut title = if tool_indices.len() == 1 {
        "Explored 1 step".to_string()
    } else {
        format!("Explored {} steps", tool_indices.len())
    };
    if streaming {
        if let Some(&last_tool_idx) = tool_indices.last() {
            if let AssistantBlock::Tool {
                name,
                output,
                is_error,
                ..
            } = &blocks[last_tool_idx]
            {
                if is_error.is_none() {
                    title.push_str("  ·  ");
                    title.push_str(&tool_compact_header(name, output));
                }
            }
        }
    } else if !commands.is_empty() {
        title.push_str("  ");
        title.push_str(&commands);
    }
    CollapsingHeader::new(RichText::new(title).size(FS_SMALL).color(C_TEXT_MUTED))
        .id_salt((msg_idx, salt, "exploring", block_state_tag(streaming)))
        .default_open(streaming)
        .show_unindented(ui, |ui| {
            nest_under_collapsed_header(ui, |ui| {
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
                            let secs = estimate_thought_seconds(combined.len());
                            let header = format!("Thought for {secs}s");
                            render_thinking_group_block(
                                ui, msg_idx, first, header, combined, streaming,
                            );
                        }
                        AssistantBlock::Answer(text) => {
                            if !text.trim().is_empty() {
                                markdown::render_markdown(ui, text);
                            }
                            i += 1;
                        }
                        AssistantBlock::Tool { .. } => {
                            let block = &blocks[i];
                            render_tool_block_details(ui, msg_idx, i, block, streaming, false);
                            ui.add_space(6.0);
                            i += 1;
                        }
                    }
                }
            });
        });
    ui.add_space(6.0);
}

fn render_thinking_group_block(
    ui: &mut Ui,
    msg_idx: usize,
    salt: usize,
    title: String,
    combined: String,
    live: bool,
) {
    CollapsingHeader::new(RichText::new(title).size(FS_SMALL).color(C_TEXT_MUTED))
        .id_salt((msg_idx, salt, "thinking", block_state_tag(live)))
        .default_open(true)
        .show_unindented(ui, |ui| {
            if live {
                ui.horizontal(|ui| {
                    animated_status_label(ui, "Thinking", FS_TINY);
                });
                ui.add_space(4.0);
            }
            let bubble_w = content_wrap_width(ui);
            ui.set_width(bubble_w);
            let overflow = combined.lines().count() > BLOCK_PREVIEW_LINES;
            render_expandable_monospace_panel(
                ui,
                C_BG_ELEVATED,
                BLOCK_PREVIEW_LINES,
                expand_persist_id(Id::new((msg_idx, salt, "thinking_body"))),
                overflow,
                combined.as_str(),
                C_TEXT_MUTED,
            );
        });
    ui.add_space(8.0);
}

fn trailing_answer_start(blocks: &[AssistantBlock]) -> usize {
    let mut idx = blocks.len();
    while idx > 0 {
        match &blocks[idx - 1] {
            AssistantBlock::Answer(_) => idx -= 1,
            _ => break,
        }
    }
    idx
}

fn has_worked_prefix(blocks: &[AssistantBlock], end: usize, streaming: bool) -> bool {
    blocks[..end].iter().any(|block| match block {
        AssistantBlock::Answer(text) => !text.trim().is_empty(),
        AssistantBlock::Thinking(text) => !text.trim().is_empty(),
        AssistantBlock::Tool { output, diff, .. } => {
            streaming
                || !output.trim().is_empty()
                || diff
                    .as_deref()
                    .is_some_and(|diff_text| !diff_text.trim().is_empty())
        }
    })
}

fn format_work_duration(secs: u32) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else {
        let mins = secs / 60;
        let rem = secs % 60;
        format!("{mins}m {rem:02}s")
    }
}

fn worked_header_label(blocks: &[AssistantBlock]) -> String {
    let thinking_chars: usize = blocks
        .iter()
        .map(|block| match block {
            AssistantBlock::Thinking(text) => text.len(),
            _ => 0,
        })
        .sum();
    let tool_count = blocks
        .iter()
        .filter(|block| matches!(block, AssistantBlock::Tool { .. }))
        .count() as u32;
    let estimate = estimate_thought_seconds(thinking_chars) + tool_count;
    format!("Worked for {}", format_work_duration(estimate.max(1)))
}

fn render_activity_range(
    ui: &mut Ui,
    msg_idx: usize,
    blocks: &[AssistantBlock],
    start: usize,
    end: usize,
    streaming: bool,
) {
    let slice = &blocks[start..end];
    for group in build_assistant_block_groups(slice) {
        match group {
            AssistantBlockGroup::Thinking(indices) => {
                let global: Vec<usize> = indices.iter().map(|&i| start + i).collect();
                let combined = concat_thinking_blocks(blocks, &global);
                if combined.trim().is_empty() {
                    continue;
                }
                let secs = estimate_thought_seconds(combined.len());
                let header = format!("Thought for {secs}s");
                render_thinking_group_block(ui, msg_idx, global[0], header, combined, streaming);
            }
            AssistantBlockGroup::Answer(i) => {
                let gi = start + i;
                if let AssistantBlock::Answer(text) = &blocks[gi] {
                    if !text.trim().is_empty() {
                        markdown::render_markdown(ui, text);
                    }
                }
            }
            AssistantBlockGroup::ExploringTools {
                range_start,
                range_end,
                tool_indices,
            } => {
                let rs = start + range_start;
                let re = start + range_end;
                let tools: Vec<usize> = tool_indices.iter().map(|&i| start + i).collect();
                render_explored_cluster(
                    ui,
                    ExploredClusterCtx {
                        msg_idx,
                        salt: rs,
                        blocks,
                        start: rs,
                        end: re,
                        tool_indices: &tools,
                        streaming,
                    },
                );
            }
            AssistantBlockGroup::Tool(i) => {
                let gi = start + i;
                render_single_tool_block(ui, msg_idx, gi, &blocks[gi], streaming);
            }
        }
    }
}

pub fn render_assistant_blocks(
    ui: &mut Ui,
    msg_idx: usize,
    blocks: &[AssistantBlock],
    streaming: bool,
    agent_ack: bool,
) {
    if assistant_is_effectively_empty(blocks, streaming) {
        if streaming {
            ui.horizontal(|ui| {
                if agent_ack {
                    animated_status_label(ui, "Planning next steps", FS_SMALL);
                } else {
                    animated_status_label(ui, "Planning next steps", FS_SMALL);
                }
            });
        }
        return;
    }

    let worked_end = trailing_answer_start(blocks);
    if has_worked_prefix(blocks, worked_end, streaming) {
        let header = worked_header_label(&blocks[..worked_end]);
        // Stable id: do not key on streaming ("live"/"done") or egui creates a new header when the
        // stream ends and `default_open(true)` re-expands Worked. Latch detects stream end to fold once.
        let stream_latch_id = Id::new(("pi_worked_stream_latch", msg_idx));
        let prev_streaming: bool = ui
            .ctx()
            .data_mut(|d| d.get_temp(stream_latch_id).unwrap_or(false));
        let just_finished_stream = prev_streaming && !streaming;
        ui.ctx()
            .data_mut(|d| d.insert_temp(stream_latch_id, streaming));
        let worked_open = if just_finished_stream {
            Some(false)
        } else if streaming {
            Some(true)
        } else {
            None
        };
        CollapsingHeader::new(RichText::new(header).size(FS_SMALL).color(C_TEXT_MUTED))
            .id_salt((msg_idx, "worked"))
            .default_open(false)
            .open(worked_open)
            .show_unindented(ui, |ui| {
                nest_under_collapsed_header(ui, |ui| {
                    render_activity_range(ui, msg_idx, blocks, 0, worked_end, streaming);
                });
            });
        ui.add_space(8.0);
    }

    for block in &blocks[worked_end..] {
        if let AssistantBlock::Answer(text) = block {
            if !text.trim().is_empty() || streaming {
                markdown::render_markdown(ui, text);
            }
        }
    }
}
