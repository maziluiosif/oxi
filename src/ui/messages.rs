//! Chat transcript rendering (bubbles, assistant blocks, markdown).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use eframe::egui::scroll_area::ScrollBarVisibility;
use eframe::egui::text::{LayoutJob, LayoutSection, TextFormat, TextWrapping};
use eframe::egui::{
    self, CollapsingHeader, Color32, FontId, Frame, Id, Image, Label, Margin, RichText, Rounding,
    ScrollArea, Sense, Stroke, TextureOptions, Ui,
};

use crate::markdown;
use crate::model::{
    assistant_is_effectively_empty, build_assistant_block_groups, concat_thinking_blocks,
    estimate_thought_seconds, tool_breaks_explore_cluster, AssistantBlock, AssistantBlockGroup,
    ChatMessage, MsgRole, UserAttachment,
};
use crate::theme::{
    animated_status_label, content_wrap_width, icon_font, tool_status_label, C_BG_ELEVATED,
    C_BG_INPUT, C_BORDER, C_TEXT, C_TEXT_MUTED, C_USER_BUBBLE, FS_BODY, FS_SMALL, FS_TINY,
};
use crate::ui::preview_expand::{
    clickable_expand_overlay, expand_persist_id, is_expanded, truncate_lines_preview,
};

const BLOCK_PREVIEW_LINES: usize = 10;
const EDIT_PREVIEW_LINES: usize = 10;
/// Max tool pills visible in the scroll window while streaming (last N are shown, oldest scroll away).
const MAX_VISIBLE_STREAMING_TOOL_PILLS: usize = 5;
/// Pill height used to size the scroll window: inner_margin(3px top+bottom) + FS_SMALL text ≈ 24px.
const TOOL_PILL_HEIGHT: f32 = 24.0;
/// Vertical gap between consecutive pills.
const TOOL_PILL_GAP: f32 = 3.0;
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
        _ => "\u{f0214}",       // nf-md-file
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
) {
    let AssistantBlock::Tool {
        name,
        output,
        args_summary,
        is_error,
        diff,
        full_output_path: _,
        output_truncated: _,
        ..
    } = block
    else {
        return;
    };

    let has_error = *is_error == Some(true);
    let has_diff = diff.as_deref().is_some_and(|t| !t.trim().is_empty());
    let has_output = !output.trim().is_empty();
    let has_body = has_diff || has_output;
    // A tool is "actively running" only while streaming AND it has not been finalized yet
    // (is_error is set by finalize_tool_run — None means still in-flight).
    // Additionally only the last pill in the visual run gets the spinner.
    let tool_in_flight = streaming && is_error.is_none() && !has_output && !has_diff;
    let running = tool_in_flight && is_last_in_run;

    let pill_bg = if has_error {
        Color32::from_rgb(0x28, 0x14, 0x14)
    } else {
        Color32::from_rgb(0x1a, 0x1b, 0x1f)
    };
    let pill_border = if has_error {
        Color32::from_rgb(0x50, 0x1c, 0x1c)
    } else {
        Color32::from_rgb(0x28, 0x29, 0x2f)
    };
    // All icons and text use the same muted palette — no colour coding by tool type.
    let icon_color = if has_error {
        Color32::from_rgb(0xff, 0x80, 0x80)
    } else {
        C_TEXT_MUTED
    };
    let name_color = if has_error {
        Color32::from_rgb(0xff, 0xa0, 0xa0)
    } else {
        C_TEXT
    };
    let arg_color = Color32::from_rgb(0x7a, 0x7d, 0x8c);

    let icon = tool_icon(name);
    let arg = tool_short_arg(name, args_summary.as_ref());

    // Dacă are body, wrapper-ul e un CollapsingHeader pe pill; altfel doar pill.
    let state_tag = block_state_tag(streaming);

    // Construim header-ul custom
    let draw_pill = |ui: &mut Ui, expanded: Option<bool>| {
        Frame::none()
            .fill(pill_bg)
            .stroke(Stroke::new(1.0, pill_border))
            .rounding(Rounding::same(5.0))
            .inner_margin(Margin::symmetric(7.0, 3.0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 5.0;
                    if let Some(open) = expanded {
                        ui.label(
                            RichText::new(if open { "▾" } else { "▸" })
                                .size(FS_TINY)
                                .color(C_TEXT_MUTED),
                        );
                    }
                    // Icon — plain monospace glyph, no coloured badge background
                    ui.label(
                        RichText::new(icon)
                            .font(FontId::new(FS_TINY + 1.0, icon_font()))
                            .color(icon_color),
                    );
                    // Tool name
                    ui.label(
                        RichText::new(name.as_str())
                            .size(FS_SMALL)
                            .color(name_color)
                            .strong(),
                    );
                    // Argument (truncated)
                    if let Some(ref a) = arg {
                        ui.add(
                            Label::new(
                                RichText::new(a.as_str())
                                    .size(FS_SMALL)
                                    .color(arg_color)
                                    .monospace(),
                            )
                            .truncate(),
                        );
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // Keep the spinner pinned at the far right on the latest live tool.
                        if running {
                            ui.add(eframe::egui::Spinner::new().size(9.0).color(C_TEXT_MUTED));
                        }
                    });
                });
            });
    };

    if !has_body && !has_error {
        draw_pill(ui, None);
        ui.add_space(3.0);
        return;
    }

    let collapse_id = Id::new((msg_idx, block_idx, "tool_pill_collapse", state_tag));
    let default_open = running;
    let mut open = egui::collapsing_header::CollapsingState::load_with_default_open(
        ui.ctx(),
        collapse_id,
        default_open,
    );

    let header_response = Frame::none()
        .show(ui, |ui| {
            let response = ui
                .scope(|ui| {
                    ui.style_mut().interaction.selectable_labels = false;
                    draw_pill(ui, Some(open.is_open()));
                })
                .response;
            response
        })
        .inner;

    if header_response.hovered() {
        ui.ctx()
            .set_cursor_icon(eframe::egui::CursorIcon::PointingHand);
    }
    if header_response.clicked() {
        open.toggle(ui);
    }

    open.show_body_unindented(ui, |ui| {
        nest_under_collapsed_header(ui, |ui| {
            render_tool_block_details(ui, msg_idx, block_idx, block, streaming, false);
        });
    });
    ui.add_space(3.0);
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
                        if !msg.text.is_empty() {
                            ui.add_space(6.0);
                        }
                        render_user_attachments(ui, msg_idx, &msg.attachments);
                    }
                });
        });
        ui.add_space(8.0);
        return;
    }

    render_assistant_message_run(ui, msg_idx, std::slice::from_ref(msg), agent_ack);
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
                            .stroke(Stroke::new(1.0, C_BORDER))
                            .show(ui, |ui| {
                                ui.add(Image::new((tex.id(), sz)));
                            });
                    } else {
                        // Fallback badge when texture loading fails
                        Frame::none()
                            .fill(Color32::from_rgb(0x25, 0x25, 0x28))
                            .rounding(Rounding::same(6.0))
                            .inner_margin(Margin::symmetric(8.0, 4.0))
                            .show(ui, |ui| {
                                ui.label(
                                    RichText::new(format!("📎 {mime}"))
                                        .size(FS_TINY)
                                        .color(C_TEXT_MUTED),
                                );
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

fn render_tool_block_details(
    ui: &mut Ui,
    msg_idx: usize,
    block_idx: usize,
    block: &AssistantBlock,
    streaming: bool,
    show_args_summary: bool,
) {
    let AssistantBlock::Tool {
        name: _,
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

    if !has_output && !tool_running && !has_diff {
        ui.label(
            RichText::new("No output")
                .size(FS_TINY)
                .color(Color32::from_gray(110)),
        );
    }

    if let Some(diff_text) = diff.as_ref().filter(|text| !text.trim().is_empty()) {
        let (added, removed) = diff_counts(diff_text);
        if !is_edit_like_tool(block) {
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
        } else {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("+{added}  -{removed}"))
                        .size(FS_TINY)
                        .color(C_TEXT_MUTED),
                );
            });
        }
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
        render_edit_tool_block(ui, msg_idx, bi, block, streaming);
        return;
    }

    render_tool_pill(ui, msg_idx, bi, block, streaming, streaming);
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

    // Full path from args for the header; fallback to tool_short_arg
    let file_path = tool_path_from_args(args_summary.as_ref()).unwrap_or_else(|| name.clone());
    // Display: last 2 path segments so it fits without truncation most of the time
    let display_path = {
        let segs: Vec<&str> = file_path.trim_start_matches('/').split('/').collect();
        if segs.len() > 2 {
            format!("{}/{}", segs[segs.len() - 2], segs[segs.len() - 1])
        } else {
            file_path.clone()
        }
    };

    // Diff stats badge
    let (added, removed) = rendered_diff
        .as_deref()
        .filter(|t| !t.trim().is_empty())
        .map(diff_counts)
        .unwrap_or((0, 0));

    let outer_border = if has_error {
        Color32::from_rgb(0x5c, 0x20, 0x20)
    } else {
        C_BORDER
    };
    let header_bg = if has_error {
        Color32::from_rgb(0x22, 0x14, 0x14)
    } else {
        Color32::from_rgb(0x16, 0x17, 0x1a)
    };
    let diff_bg = Color32::from_rgb(0x10, 0x11, 0x14);

    let collapse_id = Id::new((msg_idx, block_idx, "edit_diff_collapse"));
    let mut open = egui::collapsing_header::CollapsingState::load_with_default_open(
        ui.ctx(),
        collapse_id,
        true,
    );
    let is_open = open.is_open();

    // ── Outer frame wraps header + diff in one visual block ──────────────────
    Frame::none()
        .fill(Color32::TRANSPARENT)
        .stroke(Stroke::new(1.0, outer_border))
        .rounding(Rounding::same(8.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());

            // ── Header row ───────────────────────────────────────────────────
            let header_resp = Frame::none()
                .fill(header_bg)
                .rounding(if is_open && has_diff {
                    // round only top corners when diff is visible below
                    eframe::egui::Rounding {
                        nw: 8.0,
                        ne: 8.0,
                        sw: 0.0,
                        se: 0.0,
                    }
                } else {
                    Rounding::same(8.0)
                })
                .inner_margin(Margin::symmetric(10.0, 7.0))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;

                        // Chevron toggle (only when diff available)
                        if has_diff {
                            ui.label(
                                RichText::new(if is_open { "▾" } else { "▸" })
                                    .size(FS_TINY)
                                    .color(C_TEXT_MUTED),
                            );
                        }

                        ui.label(
                            RichText::new(&display_path)
                                .size(FS_SMALL)
                                .color(if has_error {
                                    Color32::from_rgb(0xff, 0xb0, 0xb0)
                                } else {
                                    C_TEXT
                                })
                                .strong()
                                .monospace(),
                        );

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if running {
                                ui.add(eframe::egui::Spinner::new().size(10.0).color(C_TEXT_MUTED));
                            } else if has_diff {
                                // +added / -removed badges
                                ui.label(
                                    RichText::new(format!("-{removed}"))
                                        .size(FS_TINY)
                                        .color(Color32::from_rgb(0xfe, 0xa0, 0xa0))
                                        .monospace(),
                                );
                                ui.label(
                                    RichText::new(format!("+{added}"))
                                        .size(FS_TINY)
                                        .color(Color32::from_rgb(0x86, 0xef, 0xac))
                                        .monospace(),
                                );
                            } else if has_error {
                                ui.label(
                                    RichText::new("error")
                                        .size(FS_TINY)
                                        .color(Color32::from_rgb(0xff, 0x80, 0x80)),
                                );
                            }
                        });
                    });
                })
                .response;

            if has_diff && header_resp.interact(Sense::click()).clicked() {
                open.toggle(ui);
            }
            if has_diff && header_resp.interact(Sense::hover()).hovered() {
                ui.ctx()
                    .set_cursor_icon(eframe::egui::CursorIcon::PointingHand);
            }

            // ── Diff block ───────────────────────────────────────────────────
            if has_diff {
                open.show_body_unindented(ui, |ui| {
                    if let Some(diff_text) =
                        rendered_diff.as_deref().filter(|t| !t.trim().is_empty())
                    {
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
            }
        });

    ui.add_space(6.0);
}

/// Renders a consecutive run of tool block indices. When `needs_scroll`, only a fixed number of
/// pills are visible (oldest hidden); thinking/prose must **not** live inside this region — the
/// caller keeps those blocks outside the [`ScrollArea`].
fn render_explored_tool_pill_run(
    ui: &mut Ui,
    msg_idx: usize,
    blocks: &[AssistantBlock],
    tool_run: &[usize],
    needs_scroll: bool,
    hidden_before: &mut usize,
    visible_start: usize,
    last_tool_idx: Option<usize>,
    streaming: bool,
) {
    if tool_run.is_empty() {
        return;
    }

    // While the live explored cluster is capped to the last N tool pills, older tool batches can
    // become fully hidden. In that case, do not allocate the fixed-height scroll region at all,
    // otherwise we leave large blank gaps between thinking blocks during streaming.
    let visible_run: &[usize] = if needs_scroll {
        let hidden_in_this_run = visible_start.saturating_sub(*hidden_before).min(tool_run.len());
        *hidden_before += hidden_in_this_run;
        let visible = &tool_run[hidden_in_this_run..];
        if visible.is_empty() {
            return;
        }
        visible
    } else {
        tool_run
    };

    let render_pills = |ui: &mut Ui| {
        for &ti in visible_run {
            let block = &blocks[ti];
            let is_last = Some(ti) == last_tool_idx;
            render_tool_pill(ui, msg_idx, ti, block, streaming, is_last);
            ui.add_space(TOOL_PILL_GAP);
        }
    };

    if needs_scroll {
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
                render_explored_tool_pill_run(
                    ui,
                    msg_idx,
                    blocks,
                    &tool_run,
                    needs_scroll,
                    &mut hidden_before,
                    visible_start,
                    last_tool_idx,
                    streaming,
                );
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
                    .color(C_TEXT_MUTED),
            );
        }
    }
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

    let step_count = tool_indices.len();
    let header_label = if streaming {
        if let Some(&last) = tool_indices.last() {
            if let AssistantBlock::Tool { name, .. } = &blocks[last] {
                format!("{}…", tool_status_label(name))
            } else {
                "Working…".to_string()
            }
        } else {
            "Working…".to_string()
        }
    } else {
        format!("{step_count} tool calls")
    };

    // Keep the tools body always visible while streaming; limit it with an inner scroll window.
    // After streaming ends, auto-expand to show all items. Manual fold/unfold still works only
    // after the run is finished, so we don't hide the live tool stream by accident.
    let collapse_id = Id::new((msg_idx, salt, "exploring_v2"));

    let latch_id = Id::new((msg_idx, salt, "exploring_stream_latch"));
    let was_streaming: bool = ui.ctx().data_mut(|d| d.get_temp(latch_id).unwrap_or(false));
    ui.ctx().data_mut(|d| d.insert_temp(latch_id, streaming));
    let just_finished = was_streaming && !streaming;

    let mut open_state = egui::collapsing_header::CollapsingState::load_with_default_open(
        ui.ctx(),
        collapse_id,
        true,
    );
    if just_finished {
        open_state.set_open(true);
    }
    let expanded = open_state.is_open();

    let tool_count = blocks[start..end]
        .iter()
        .filter(|b| matches!(b, AssistantBlock::Tool { .. }))
        .count();
    let needs_scroll = streaming && tool_count > MAX_VISIBLE_STREAMING_TOOL_PILLS;
    let body_height = if needs_scroll {
        (TOOL_PILL_HEIGHT + TOOL_PILL_GAP) * MAX_VISIBLE_STREAMING_TOOL_PILLS as f32
    } else {
        ((tool_count.max(1) as f32) * (TOOL_PILL_HEIGHT + TOOL_PILL_GAP)).max(TOOL_PILL_HEIGHT)
    };

    if streaming {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("▾")
                    .size(FS_TINY)
                    .color(Color32::from_rgb(0x5a, 0x5d, 0x6b)),
            );
            ui.label(
                RichText::new(&header_label)
                    .size(FS_TINY)
                    .color(Color32::from_rgb(0x5a, 0x5d, 0x6b)),
            );
        });
        ui.vertical(|ui| {
            ui.set_width(ui.available_width());
            render_explored_tool_list(ui, msg_idx, blocks, start, end, streaming, false);
        });
    } else {
        let header_response = open_state.show_header(ui, |ui| {
            ui.label(
                RichText::new(&header_label)
                    .size(FS_TINY)
                    .color(Color32::from_rgb(0x5a, 0x5d, 0x6b)),
            );
        });
        header_response.body_unindented(|ui| {
            ui.horizontal(|ui| {
                let bar_rect = ui
                    .allocate_exact_size(eframe::egui::vec2(2.0, body_height), Sense::hover())
                    .0;
                ui.painter().rect_filled(
                    bar_rect,
                    Rounding::same(1.0),
                    Color32::from_rgb(0x2a, 0x2c, 0x35),
                );
                ui.add_space(6.0);
                ui.vertical(|ui| {
                    ui.set_width(ui.available_width());
                    render_explored_tool_list(ui, msg_idx, blocks, start, end, streaming, expanded);
                });
            });
        });
    }
    ui.add_space(4.0);
}

fn render_thinking_group_block(
    ui: &mut Ui,
    msg_idx: usize,
    salt: usize,
    combined: String,
    live: bool,
) {
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
        expand_persist_id(Id::new((msg_idx, salt, "thinking_body", block_state_tag(live)))),
        overflow,
        combined.as_str(),
        C_TEXT_MUTED,
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
                let thinking_live = thinking_group_is_live(blocks, global.last().copied().unwrap_or(global[0]) + 1, streaming);
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
    _agent_ack: bool,
) {
    if assistant_is_effectively_empty(blocks, streaming) {
        if streaming {
            ui.horizontal(|ui| {
                animated_status_label(ui, "Planning next steps", FS_SMALL);
            });
        }
        return;
    }

    // If we have tool calls but all are finished and no answer text yet, the model is
    // deciding what to do next (between tool calls or before writing the final answer).
    // Show a subtle animated label so the UI does not look frozen.
    let all_tools_done = streaming
        && blocks
            .iter()
            .any(|b| matches!(b, AssistantBlock::Tool { .. }))
        && blocks
            .iter()
            .filter(|b| matches!(b, AssistantBlock::Tool { .. }))
            .all(|b| {
                if let AssistantBlock::Tool { is_error, .. } = b {
                    is_error.is_some()
                } else {
                    false
                }
            })
        && !blocks
            .iter()
            .any(|b| matches!(b, AssistantBlock::Answer(t) if !t.trim().is_empty()));
    let planning_overlay = all_tools_done;

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
