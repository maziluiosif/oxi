//! Chat transcript rendering (bubbles, assistant blocks, markdown).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use eframe::egui::scroll_area::ScrollBarVisibility;
use eframe::egui::text::{LayoutJob, LayoutSection, TextFormat, TextWrapping};
use eframe::egui::{
    self, Color32, FontId, Frame, Id, Image, Label, Margin, RichText, Rounding, ScrollArea, Stroke,
    TextureOptions, Ui,
};

use crate::markdown;
use crate::model::{
    assistant_is_effectively_empty, build_assistant_block_groups, concat_thinking_blocks,
    tool_breaks_explore_cluster, AssistantBlock, AssistantBlockGroup, ChatMessage, MsgRole,
    UserAttachment,
};
use crate::theme::*;
use crate::ui::preview_expand::{
    clickable_expand_overlay, clickable_expand_overlay_quiet, expand_persist_id, is_expanded,
    toggle_expanded, truncate_lines_preview,
};

const BLOCK_PREVIEW_LINES: usize = 10;
const EDIT_PREVIEW_LINES: usize = 10;
/// Max tool pills visible in the scroll window while streaming (last N are shown, oldest scroll away).
const MAX_VISIBLE_STREAMING_TOOL_PILLS: usize = 5;
/// Pill height used to size the scroll window: inner_margin(3px top+bottom) + FS_SMALL text ≈ 24px.
const TOOL_PILL_HEIGHT: f32 = 24.0;
/// Vertical gap between consecutive pills.
const TOOL_PILL_GAP: f32 = 3.0;

struct ExploredClusterCtx<'a> {
    msg_idx: usize,
    blocks: &'a [AssistantBlock],
    start: usize,
    end: usize,
    streaming: bool,
}

/// Thinking-block body text: proportional, small, airy — deliberately lighter than the
/// monospace tool-output panels so reasoning reads as a side note, not a code dump.
fn thinking_wrapped_job(text: String, wrap_width: f32) -> LayoutJob {
    LayoutJob {
        sections: vec![LayoutSection {
            leading_space: 0.0,
            byte_range: 0..text.len(),
            format: TextFormat {
                font_id: FontId::proportional(FS_SMALL),
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

fn diff_counts(diff: &str) -> (usize, usize) {
    let mut added = 0;
    let mut removed = 0;
    for line in diff.lines() {
        if line.starts_with("+++") || line.starts_with("---") {
            continue;
        }
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

    let context_text = c_text_muted();

    for (i, line) in lines.iter().enumerate() {
        let start = job.text.len();
        job.text.push_str(line);
        if i + 1 < lines.len() {
            job.text.push('\n');
        }
        let end = job.text.len();
        let (color, background) = if line.starts_with('+') {
            (c_diff_add_fg(), c_diff_add_bg())
        } else if line.starts_with('-') {
            (c_diff_del_fg(), c_diff_del_bg())
        } else {
            (context_text, Color32::TRANSPARENT)
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

fn short_path(path: &str, max_segments: usize) -> String {
    let segs: Vec<&str> = path
        .trim_start_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    if segs.len() > max_segments && max_segments > 0 {
        let start = segs.len() - max_segments;
        format!("…/{}", segs[start..].join("/"))
    } else {
        path.to_string()
    }
}

/// "https://www.example.com/a/b?x=1" → "example.com/a/b?x=1…" (scheme + www stripped, truncated).
fn short_url(url: &str, max_chars: usize) -> String {
    let s = url
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let s = s.strip_prefix("www.").unwrap_or(s);
    let s = s.trim_end_matches('/');
    let mut out: String = s.chars().take(max_chars).collect();
    if s.chars().count() > max_chars {
        out.push('…');
    }
    out
}

fn command_preview(command: &str, max_chars: usize) -> String {
    let first_line = command.lines().next().unwrap_or(command).trim();
    let mut out: String = first_line.chars().take(max_chars).collect();
    if first_line.chars().count() > max_chars {
        out.push('…');
    }
    out
}

fn count_output_lines(output: &str) -> usize {
    if output.trim().is_empty() {
        0
    } else {
        output.lines().count().max(1)
    }
}

fn tool_action_label(name: &str) -> &'static str {
    match name {
        "read" => "Read",
        "write" => "Wrote",
        "edit" => "Edited",
        "bash" => "Ran",
        "grep" => "Searched",
        "find" => "Found files",
        "ls" => "Listed",
        "web_search" => "Searched",
        "web_fetch" => "Fetched",
        _ => "Used",
    }
}

fn tool_summary_text(
    name: &str,
    args_summary: Option<&String>,
    output: &str,
    diff: Option<&String>,
    is_error: Option<bool>,
    running: bool,
) -> String {
    if running {
        let target = tool_short_arg(name, args_summary)
            .map(|s| format!(" · {s}"))
            .unwrap_or_default();
        return format!("{}{}", tool_status_label(name), target);
    }

    let has_error = is_error == Some(true);
    let action = if has_error {
        "Failed"
    } else {
        tool_action_label(name)
    };
    let mut parts = vec![action.to_string()];

    match name {
        "bash" => {
            if let Some(raw) = args_summary {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) {
                    if let Some(cmd) = v.get("command").and_then(|x| x.as_str()) {
                        let p = command_preview(cmd, 42);
                        if !p.is_empty() {
                            parts.push(format!("`{p}`"));
                        }
                    }
                }
            }
        }
        "grep" => {
            if let Some(raw) = args_summary {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) {
                    if let Some(pattern) = v.get("pattern").and_then(|x| x.as_str()) {
                        parts.push(format!("`{}`", command_preview(pattern, 32)));
                    }
                    if let Some(path) = v.get("path").and_then(|x| x.as_str()) {
                        if !path.is_empty() {
                            parts.push(format!("in {}", short_path(path, 2)));
                        }
                    }
                }
            }
        }
        "read" | "write" | "edit" | "find" | "ls" => {
            if let Some(path) = tool_path_from_args(args_summary) {
                parts.push(short_path(&path, 2));
            }
        }
        "web_search" => {
            if let Some(raw) = args_summary {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) {
                    if let Some(q) = v.get("query").and_then(|x| x.as_str()) {
                        let p = command_preview(q, 40);
                        if !p.is_empty() {
                            parts.push(p);
                        }
                    }
                }
            }
        }
        "web_fetch" => {
            if let Some(raw) = args_summary {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) {
                    if let Some(u) = v.get("url").and_then(|x| x.as_str()) {
                        parts.push(short_url(u, 44));
                    }
                }
            }
        }
        _ => {
            if let Some(arg) = tool_short_arg(name, args_summary) {
                parts.push(arg);
            }
        }
    }

    if let Some(diff_text) = diff.filter(|d| !d.trim().is_empty()) {
        let (added, removed) = diff_counts(diff_text);
        parts.push(format!("+{added} -{removed}"));
    } else {
        let lines = count_output_lines(output);
        if lines > 0 {
            parts.push(format!("{lines} line{}", if lines == 1 { "" } else { "s" }));
        }
    }

    parts.join(" · ")
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
        .stroke(Stroke::new(1.0, c_border()))
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

/// Tool icons — Nerd Font PUA codepoints rendered with the dedicated `icons` font family.
fn tool_icon(name: &str) -> &'static str {
    match name {
        "read" => "\u{f021b}",  // nf-md-file_document
        "write" => "\u{f0193}", // nf-md-file_edit
        "edit" => "\u{f03eb}",  // nf-md-pencil
        "bash" => "\u{f018d}",  // nf-md-console
        "grep" => "\u{f021e}",  // nf-md-file_find
        "find" => "\u{f0349}",  // nf-md-magnify
        "ls" => "\u{f0645}",    // nf-md-folder_open
        "web_search" => crate::theme::ICON_WEB_SEARCH,
        "web_fetch" => crate::theme::ICON_GLOBE,
        _ => "\u{f0214}", // nf-md-file
    }
}

/// Argument scurt, relevant, din `args_summary` JSON: path > command > prima valoare.
fn tool_short_arg(name: &str, args_summary: Option<&String>) -> Option<String> {
    let raw = args_summary?;
    let v = serde_json::from_str::<serde_json::Value>(raw).ok()?;
    // path / filePath pentru read/write/edit/find
    if let Some(p) = v
        .get("path")
        .or_else(|| v.get("filePath"))
        .and_then(|x| x.as_str())
    {
        // afișăm doar ultimele 2 segmente
        let segs: Vec<&str> = p.trim_start_matches('/').split('/').collect();
        let short = if segs.len() > 2 {
            format!("…/{}/{}", segs[segs.len() - 2], segs[segs.len() - 1])
        } else {
            p.to_string()
        };
        // adaugă range de linii dacă există
        let offset = v.get("offset").and_then(|x| x.as_u64());
        let limit = v.get("limit").and_then(|x| x.as_u64());
        return Some(match (offset, limit) {
            (Some(o), Some(l)) => format!("{short}  L{o}–{}", o + l - 1),
            (Some(o), None) => format!("{short}  L{o}+"),
            _ => short,
        });
    }
    // command pentru bash
    if let Some(cmd) = v.get("command").and_then(|x| x.as_str()) {
        let tok: String = cmd.split_whitespace().take(6).collect::<Vec<_>>().join(" ");
        let mut s: String = tok.chars().take(60).collect();
        if tok.chars().count() > 60 {
            s.push('…');
        }
        return Some(s);
    }
    // pattern + path pentru grep
    if name == "grep" {
        let pat = v.get("pattern").and_then(|x| x.as_str()).unwrap_or("");
        let dir = v.get("path").and_then(|x| x.as_str()).unwrap_or("");
        if !pat.is_empty() {
            return Some(if dir.is_empty() {
                format!("`{pat}`")
            } else {
                format!("`{pat}`  in {dir}")
            });
        }
    }
    // URL scurtat pentru web_fetch
    if name == "web_fetch" {
        if let Some(u) = v.get("url").and_then(|x| x.as_str()) {
            return Some(short_url(u, 44));
        }
    }
    // fallback: prima string din obiect
    if let serde_json::Value::Object(map) = &v {
        if let Some(s) = map.values().find_map(|x| x.as_str()) {
            let mut t: String = s.chars().take(48).collect();
            if s.chars().count() > 48 {
                t.push('…');
            }
            return Some(t);
        }
    }
    None
}

/// Randează un singur tool call ca un chip compact Cursor-style:
/// [ icon  ToolName  arg_scurt ]  cu o linie de output on-hover/expand.
fn render_tool_pill(
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
        c_diff_del_fg()
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

    let frame = Frame::none()
        .fill(pill_bg)
        .stroke(Stroke::new(1.0, pill_border))
        .rounding(Rounding::same(7.0))
        .inner_margin(Margin::symmetric(10.0, 5.0))
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
                ui.add(
                    Label::new(
                        RichText::new(summary.as_str())
                            .size(FS_SMALL)
                            .color(summary_color)
                            .monospace(),
                    )
                    .truncate(),
                );
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
            let overflow =
                diff_text.lines().count() > EDIT_PREVIEW_LINES || diff_text.len() > 2000;
            let preview = truncate_lines_preview(diff_text, EDIT_PREVIEW_LINES);
            render_static_preview_job_panel(
                ui,
                crate::theme::c_tool_diff_bg(),
                diff_wrapped_job(&preview, bubble_w),
                |inner| diff_wrapped_job(diff_text, inner),
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

/// Plain monospace layout job for raw tool output shown under an expanded tool pill.
fn mono_output_job(text: &str, wrap_width: f32) -> LayoutJob {
    let mut job = LayoutJob {
        wrap: TextWrapping {
            max_width: wrap_width,
            ..Default::default()
        },
        break_on_newline: true,
        ..Default::default()
    };
    job.append(
        text,
        0.0,
        TextFormat::simple(FontId::monospace(FS_TINY), c_text_muted()),
    );
    job
}

fn block_state_tag(streaming: bool) -> &'static str {
    if streaming {
        "live"
    } else {
        "done"
    }
}

/// Write/edit-like tools render as full detail blocks, not explore-cluster rows.
fn is_edit_like_tool(block: &AssistantBlock) -> bool {
    matches!(block, AssistantBlock::Tool { name, .. } if name.eq_ignore_ascii_case("write"))
        || tool_breaks_explore_cluster(block)
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

pub fn render_message(
    ui: &mut Ui,
    msg_idx: usize,
    msg: &ChatMessage,
    agent_ack: bool,
) -> egui::Response {
    let col_w = content_wrap_width(ui);

    if msg.role == MsgRole::User {
        let response = ui
            .vertical(|ui| {
                ui.set_width(col_w);
                Frame::none()
                    .fill(c_user_bubble())
                    .stroke(Stroke::new(1.0, c_border_subtle()))
                    .rounding(Rounding::same(10.0))
                    .inner_margin(Margin::symmetric(12.0, 9.0))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        if !msg.text.is_empty() {
                            ui.add(
                                Label::new(
                                    RichText::new(&msg.text)
                                        .size(FS_BODY)
                                        .line_height(Some(FS_BODY * 1.35))
                                        .color(c_text()),
                                )
                                .wrap()
                                .selectable(true),
                            );
                        }
                        if !msg.attachments.is_empty() {
                            if !msg.text.is_empty() {
                                ui.add_space(6.0);
                            }
                            render_user_attachments(ui, msg_idx, &msg.attachments);
                        }
                    });
            })
            .response;
        ui.add_space(8.0);
        return response;
    }

    let response = ui
        .vertical(|ui| {
            render_assistant_message_run(ui, msg_idx, std::slice::from_ref(msg), agent_ack);
        })
        .response;
    response
}

fn render_user_attachments(ui: &mut Ui, msg_idx: usize, attachments: &[UserAttachment]) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = eframe::egui::vec2(6.0, 6.0);
        for (i, att) in attachments.iter().enumerate() {
            match att {
                UserAttachment::Image { mime, data } => {
                    if let Some(tex) = user_image_texture(ui, msg_idx, i, data) {
                        // Larger thumbnail: max 200px, maintain aspect ratio
                        let mut sz = tex.size_vec2();
                        let max = 200.0;
                        let m = sz.x.max(sz.y);
                        if m > max {
                            sz *= max / m;
                        }
                        // Wrap in a subtle rounded frame
                        Frame::none()
                            .rounding(Rounding::same(8.0))
                            .stroke(Stroke::new(1.0, c_border()))
                            .show(ui, |ui| {
                                ui.add(Image::new((tex.id(), sz)));
                            });
                    } else {
                        // Fallback badge when texture loading fails
                        Frame::none()
                            .fill(c_bg_elevated_2())
                            .rounding(Rounding::same(6.0))
                            .inner_margin(Margin::symmetric(8.0, 4.0))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 4.0;
                                    ui.label(crate::ui::chrome::icon_glyph_rich(
                                        ICON_ATTACH,
                                        FS_TINY,
                                        c_text_muted(),
                                    ));
                                    ui.label(
                                        RichText::new(mime).size(FS_TINY).color(c_text_muted()),
                                    );
                                });
                            });
                    }
                }
            }
        }
    });
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
    let mut started_at = None;
    let mut worked_duration = None;
    for msg in messages {
        if msg.role != MsgRole::Assistant {
            continue;
        }
        streaming |= msg.streaming;
        started_at = started_at.or(msg.started_at);
        worked_duration = worked_duration.or(msg.worked_duration);
        blocks.extend(msg.blocks.iter().cloned());
    }

    if !streaming && !has_visible_assistant_content(&blocks, false) {
        return;
    }

    ui.vertical(|ui| {
        ui.set_width(col_w);
        render_assistant_blocks(
            ui,
            msg_idx,
            &blocks,
            streaming,
            agent_ack,
            started_at,
            worked_duration,
        );
    });
    ui.add_space(8.0);
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

fn render_single_tool_block(
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
    Frame::none()
        .fill(Color32::TRANSPARENT)
        .stroke(Stroke::new(1.0, outer_border))
        .rounding(Rounding::same(8.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());

            let header_resp = Frame::none()
                .fill(header_bg)
                .rounding(if is_open && has_diff {
                    eframe::egui::Rounding {
                        nw: 8.0,
                        ne: 8.0,
                        sw: 0.0,
                        se: 0.0,
                    }
                } else {
                    Rounding::same(8.0)
                })
                .inner_margin(Margin::symmetric(12.0, 8.0))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;

                        ui.label(
                            RichText::new(tool_icon(name))
                                .font(FontId::new(FS_SMALL + 0.5, icon_font()))
                                .color(if has_error {
                                    c_diff_del_fg()
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
                Frame::none()
                    .fill(diff_bg)
                    .rounding(eframe::egui::Rounding {
                        nw: 0.0,
                        ne: 0.0,
                        sw: 8.0,
                        se: 8.0,
                    })
                    .inner_margin(Margin::symmetric(10.0, 8.0))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        let bubble_w = ui.available_width().max(40.0);
                        let overflow = diff_text.lines().count() > EDIT_PREVIEW_LINES
                            || diff_text.len() > 2000;
                        let preview = truncate_lines_preview(diff_text, EDIT_PREVIEW_LINES);
                        render_static_preview_job_panel(
                            ui,
                            diff_bg,
                            diff_wrapped_job(&preview, bubble_w),
                            |inner| diff_wrapped_job(diff_text, inner),
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
    needs_scroll: bool,
    hidden_before: &'a mut usize,
    visible_start: usize,
    last_tool_idx: Option<usize>,
    streaming: bool,
}

/// Renders a consecutive run of tool block indices. When `needs_scroll`, only a fixed number of
/// pills are visible (oldest hidden); thinking/prose must **not** live inside this region — the
/// caller keeps those blocks outside the [`ScrollArea`].
fn render_explored_tool_pill_run(
    ui: &mut Ui,
    tool_run: &[usize],
    ctx: &mut ExploredToolPillCtx<'_>,
) {
    if tool_run.is_empty() {
        return;
    }

    // While the live explored cluster is capped to the last N tool pills, older tool batches can
    // become fully hidden. In that case, do not allocate the fixed-height scroll region at all,
    // otherwise we leave large blank gaps between thinking blocks during streaming.
    let visible_run: &[usize] = if ctx.needs_scroll {
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

    // While the streaming strip is capped to a fixed-height scroll region, an expanded pill
    // would overflow it — pills become expandable once the run leaves the capped strip.
    let expandable = !ctx.needs_scroll;
    let render_pills = |ui: &mut Ui| {
        for &ti in visible_run {
            let block = &blocks[ti];
            let is_last = Some(ti) == last_tool_idx;
            render_tool_pill(ui, msg_idx, ti, block, streaming, is_last, expandable);
            ui.add_space(TOOL_PILL_GAP);
        }
    };

    if ctx.needs_scroll {
        let scroll_h = (TOOL_PILL_HEIGHT + TOOL_PILL_GAP) * MAX_VISIBLE_STREAMING_TOOL_PILLS as f32;
        ScrollArea::vertical()
            .id_salt((msg_idx, visible_run[0], "tool_pill_scroll"))
            .auto_shrink([false, false])
            .max_height(scroll_h)
            .scroll_bar_visibility(ScrollBarVisibility::AlwaysHidden)
            .stick_to_bottom(true)
            .enable_scrolling(false)
            .show(ui, render_pills);
    } else {
        render_pills(ui);
    }
}

fn render_explored_tool_list(
    ui: &mut Ui,
    msg_idx: usize,
    blocks: &[AssistantBlock],
    start: usize,
    end: usize,
    streaming: bool,
    expanded: bool,
) {
    // Collect only Tool indices (not Thinking/Answer) to count for the scroll window.
    let tool_count = blocks[start..end]
        .iter()
        .filter(|b| matches!(b, AssistantBlock::Tool { .. }))
        .count();
    // While streaming and cluster is not manually expanded: tool *pills* use a fixed-height
    // ScrollArea stuck to the bottom. Thinking and markdown answers stay **outside** that strip
    // so they are never clipped or skipped.
    let needs_scroll = streaming && !expanded && tool_count > MAX_VISIBLE_STREAMING_TOOL_PILLS;

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
                    needs_scroll,
                    hidden_before: &mut hidden_before,
                    visible_start,
                    last_tool_idx,
                    streaming,
                };
                render_explored_tool_pill_run(ui, &tool_run, &mut pill_ctx);
            }
        }
    }

    if needs_scroll {
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

fn render_explored_cluster(ui: &mut Ui, ctx: ExploredClusterCtx<'_>) {
    let ExploredClusterCtx {
        msg_idx,
        blocks,
        start,
        end,
        streaming,
    } = ctx;

    render_explored_tool_list(ui, msg_idx, blocks, start, end, streaming, true);
    ui.add_space(4.0);
}

/// Frameless thinking body: no card, just a thin accent rail on the left and airy
/// proportional text. Collapse/expand behavior matches the monospace panels; while
/// live-streaming the collapsed view is pinned to the tail (newest reasoning).
fn render_thinking_text_panel(
    ui: &mut Ui,
    max_preview_lines: usize,
    persist_id: Id,
    content_overflows: bool,
    text: &str,
    tail: bool,
) {
    let frame = Frame::none()
        .inner_margin(Margin {
            left: 12.0,
            right: 4.0,
            top: 2.0,
            bottom: 2.0,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let inner = ui.available_width().max(40.0);
            let display = if !content_overflows || is_expanded(ui, persist_id) {
                text.to_string()
            } else if tail {
                crate::ui::preview_expand::truncate_lines_tail_preview(text, max_preview_lines)
            } else {
                truncate_lines_preview(text, max_preview_lines)
            };
            let allow_select = !content_overflows || is_expanded(ui, persist_id);
            selectable_layout_job(ui, thinking_wrapped_job(display, inner), allow_select);
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

fn render_thinking_group_block(
    ui: &mut Ui,
    msg_idx: usize,
    salt: usize,
    combined: String,
    live: bool,
) {
    let bubble_w = content_wrap_width(ui);
    ui.set_width(bubble_w);
    let overflow = combined.lines().count() > BLOCK_PREVIEW_LINES;
    let persist_id = expand_persist_id(Id::new((
        msg_idx,
        salt,
        "thinking_body",
        block_state_tag(live),
    )));

    if live {
        ui.horizontal(|ui| {
            animated_status_label(ui, "Thinking", FS_TINY);
        });
    } else {
        // Quiet caption; clickable (with a chevron) when there is more to unfold.
        let expanded = is_expanded(ui, persist_id);
        if overflow {
            let chevron = if expanded {
                ICON_ANGLE_UP
            } else {
                ICON_ANGLE_DOWN
            };
            if crate::ui::chrome::flat_button_icon(
                ui,
                chevron,
                "Thinking",
                FS_TINY,
                egui::vec2(0.0, 18.0),
                c_text_faint(),
            )
            .clicked()
            {
                toggle_expanded(ui, persist_id);
            }
        } else {
            ui.add(
                Label::new(
                    RichText::new("Thinking")
                        .size(FS_TINY)
                        .color(c_text_faint()),
                )
                .selectable(false),
            );
        }
    }
    ui.add_space(4.0);

    // While the model is actively streaming reasoning, keep the collapsed view pinned to
    // the newest text (tail) so you can follow along instead of seeing a frozen first page.
    render_thinking_text_panel(
        ui,
        BLOCK_PREVIEW_LINES,
        persist_id,
        overflow,
        combined.as_str(),
        live,
    );
    ui.add_space(8.0);
}

fn thinking_group_is_live(blocks: &[AssistantBlock], after_idx: usize, streaming: bool) -> bool {
    streaming
        && blocks[after_idx..].iter().all(|block| match block {
            AssistantBlock::Answer(text) => text.trim().is_empty(),
            _ => false,
        })
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

fn has_activity_after_last_tool(blocks: &[AssistantBlock]) -> bool {
    let Some(last_tool_idx) = blocks
        .iter()
        .rposition(|block| matches!(block, AssistantBlock::Tool { .. }))
    else {
        return false;
    };

    blocks[last_tool_idx + 1..].iter().any(|block| match block {
        AssistantBlock::Thinking(text) | AssistantBlock::Answer(text) => !text.trim().is_empty(),
        AssistantBlock::Tool { .. } => false,
    })
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
                let thinking_live = thinking_group_is_live(
                    blocks,
                    global.last().copied().unwrap_or(global[0]) + 1,
                    streaming,
                );
                render_thinking_group_block(ui, msg_idx, global[0], combined, thinking_live);
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
            } => {
                let rs = start + range_start;
                let re = start + range_end;
                render_explored_cluster(
                    ui,
                    ExploredClusterCtx {
                        msg_idx,
                        blocks,
                        start: rs,
                        end: re,
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

/// Formats an elapsed duration for the Cursor-style "Worked for ..." summary row.
fn format_worked_duration(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m {}s", secs / 60, secs % 60)
    }
}

/// Single collapsed-by-default summary row ("Worked for Xm Ys ›") standing in for a turn's
/// thinking + tool-call activity, Cursor-style. Expanded live while streaming so progress
/// stays visible; collapses automatically once the turn finishes. A manual click overrides
/// the default expand state for that message.
fn render_activity_summary(
    ui: &mut Ui,
    msg_idx: usize,
    blocks: &[AssistantBlock],
    worked_end: usize,
    streaming: bool,
    started_at: Option<std::time::Instant>,
    worked_duration: Option<std::time::Duration>,
) {
    // `started_at`/`worked_duration` are `None` for turns hydrated from a saved session
    // (never tracked at save time) — fall back to a plain "Worked" with no duration rather
    // than a misleading "Worked for 0s".
    let label = if streaming {
        match started_at {
            Some(t) => format!("Working for {}", format_worked_duration(t.elapsed())),
            None => "Working".to_string(),
        }
    } else {
        match worked_duration {
            Some(d) => format!("Worked for {}", format_worked_duration(d)),
            None => "Worked".to_string(),
        }
    };

    // While the turn is still streaming it always stays unfolded (live progress shouldn't
    // be foldable mid-flight); only once it's done does a manual click's collapsed/expanded
    // choice take effect, defaulting to collapsed.
    let persist_id = expand_persist_id(Id::new(("activity_summary", msg_idx)));
    let expanded = if streaming {
        true
    } else {
        ui.ctx()
            .data_mut(|d| d.get_persisted::<bool>(persist_id))
            .unwrap_or(false)
    };

    let row = ui
        .horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 5.0;
            ui.label(
                RichText::new(if expanded { ICON_ANGLE_UP } else { ICON_ANGLE_DOWN })
                    .font(FontId::new(FS_TINY, icon_font()))
                    .color(c_text_faint()),
            );
            ui.label(RichText::new(label).size(FS_SMALL).color(c_text_muted()));
        })
        .response;

    let click = ui.interact(
        row.rect,
        persist_id.with("activity_summary_click"),
        egui::Sense::click(),
    );
    if click.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if !streaming && click.clicked() {
        ui.ctx()
            .data_mut(|d| d.insert_persisted(persist_id, !expanded));
    }

    if expanded {
        ui.add_space(3.0);
        render_activity_range(ui, msg_idx, blocks, 0, worked_end, streaming);
    }
}

pub fn render_assistant_blocks(
    ui: &mut Ui,
    msg_idx: usize,
    blocks: &[AssistantBlock],
    streaming: bool,
    _agent_ack: bool,
    started_at: Option<std::time::Instant>,
    worked_duration: Option<std::time::Duration>,
) {
    if assistant_is_effectively_empty(blocks, streaming) {
        if streaming {
            ui.horizontal(|ui| {
                animated_status_label(ui, "Planning next steps", FS_SMALL);
            });
        }
        return;
    }

    // If we have tool calls but all are finished and no new thinking/answer text arrived
    // after the latest tool, the model is deciding what to do next. Hide this as soon as
    // reasoning or final text starts streaming so it does not overlap with "Thinking".
    let planning_overlay = streaming
        && blocks
            .iter()
            .any(|b| matches!(b, AssistantBlock::Tool { .. }))
        && blocks
            .iter()
            .filter(|b| matches!(b, AssistantBlock::Tool { .. }))
            .all(|b| {
                matches!(
                    b,
                    AssistantBlock::Tool {
                        is_error: Some(_),
                        ..
                    }
                )
            })
        && !has_activity_after_last_tool(blocks);

    let worked_end = trailing_answer_start(blocks);
    if worked_end > 0 {
        render_activity_summary(
            ui,
            msg_idx,
            blocks,
            worked_end,
            streaming,
            started_at,
            worked_duration,
        );
        ui.add_space(4.0);
    }

    // "Planning next steps" overlay: shown while streaming, after tool calls all completed,
    // but before any answer text has arrived (the model is deciding what to do next).
    if planning_overlay {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            animated_status_label(ui, "Planning next steps", FS_SMALL);
        });
        ui.add_space(2.0);
    }

    for block in &blocks[worked_end..] {
        if let AssistantBlock::Answer(text) = block {
            if !text.trim().is_empty() || streaming {
                markdown::render_markdown(ui, text);
            }
        }
    }
}
