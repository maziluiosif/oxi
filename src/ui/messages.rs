//! Chat transcript rendering (bubbles, assistant blocks, markdown).
//!
//! Split by responsibility: [`tool_format`] (text formatting for tool calls: summaries,
//! diff/output layout jobs, icons), [`tool_pill`] (the tool-call pill/detail UI and the
//! "explored" cluster), and [`thinking`] (reasoning-block rendering). This file keeps
//! the top-level per-message orchestration: turning a `ChatMessage` (or run of them)
//! into user bubbles / assistant activity summaries / markdown answers.

#[cfg(test)]
mod acp_tests;
#[cfg(test)]
mod stream_layout_tests;

mod thinking;
mod tool_format;
pub use tool_format::set_display_root;
mod tool_pill;

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use eframe::egui::text::LayoutJob;
use eframe::egui::{
    self, CornerRadius, FontId, Frame, Id, Image, Label, Margin, RichText, Stroke, TextureOptions,
    Ui,
};

use crate::markdown;
use crate::model::{
    AssistantBlock, AssistantBlockGroup, ChatMessage, MsgRole, UserAttachment,
    assistant_is_effectively_empty, build_assistant_block_groups, concat_thinking_blocks,
    tool_breaks_explore_cluster,
};
use crate::theme::*;
use crate::ui::preview_expand::expand_persist_id;

use thinking::{render_thinking_group_block, thinking_group_is_live};
use tool_pill::{ExploredClusterCtx, render_explored_cluster, render_single_tool_block};

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
    // Decoded at 2x the display size so the thumbnail stays crisp on HiDPI screens.
    let rgba = dyn_img.thumbnail(640, 640).to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    let color_image = eframe::egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
    let tex = ui.ctx().load_texture(
        format!("user_att_{msg_idx}_{i}_{h:016x}"),
        color_image,
        TextureOptions::LINEAR,
    );
    ui.ctx()
        .data_mut(|d| d.insert_persisted(cache_id, tex.clone()));
    Some(tex)
}

pub(super) fn selectable_layout_job(ui: &mut Ui, job: LayoutJob, allow_select: bool) {
    selectable_layout_job_with_wrap(ui, job, allow_select, true);
}

pub(super) fn selectable_layout_job_with_wrap(
    ui: &mut Ui,
    job: LayoutJob,
    allow_select: bool,
    wrap: bool,
) {
    if job.text.is_empty() {
        return;
    }
    if !allow_select {
        let wrap_mode = if wrap {
            egui::TextWrapMode::Wrap
        } else {
            egui::TextWrapMode::Extend
        };
        ui.add(Label::new(job).wrap_mode(wrap_mode).selectable(false));
        return;
    }

    crate::theme::selectable_text_job(ui, job);
}

/// The transcript's [`ToolDetail`](crate::settings::ToolDetail), published once per frame by the
/// app so the tool renderers need not thread settings through every call.
pub fn set_tool_detail(ctx: &egui::Context, detail: crate::settings::ToolDetail) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new("tool_detail"), detail));
}

pub(super) fn tool_detail(ui: &egui::Ui) -> crate::settings::ToolDetail {
    ui.ctx()
        .data(|d| d.get_temp(egui::Id::new("tool_detail")))
        .unwrap_or_default()
}

/// Paint a soft vertical fade strip along the top or bottom edge of a scrollable viewport so
/// clipped content visibly trails off instead of ending with a hard scissor cut.
pub(super) fn paint_vertical_fade(
    painter: &egui::Painter,
    rect: egui::Rect,
    bg: egui::Color32,
    top: bool,
) {
    let fade_h = 18.0_f32.min(rect.height() * 0.35);
    if fade_h <= 1.0 || rect.width() <= 0.0 {
        return;
    }
    let solid = bg.gamma_multiply(0.92);
    let clear = egui::Color32::TRANSPARENT;
    let (y0, y1, c_top, c_bot) = if top {
        (rect.top(), rect.top() + fade_h, solid, clear)
    } else {
        (rect.bottom() - fade_h, rect.bottom(), clear, solid)
    };
    let mut mesh = egui::epaint::Mesh::default();
    let uv = egui::epaint::WHITE_UV;
    mesh.vertices.push(egui::epaint::Vertex {
        pos: egui::pos2(rect.left(), y0),
        uv,
        color: c_top,
    });
    mesh.vertices.push(egui::epaint::Vertex {
        pos: egui::pos2(rect.right(), y0),
        uv,
        color: c_top,
    });
    mesh.vertices.push(egui::epaint::Vertex {
        pos: egui::pos2(rect.right(), y1),
        uv,
        color: c_bot,
    });
    mesh.vertices.push(egui::epaint::Vertex {
        pos: egui::pos2(rect.left(), y1),
        uv,
        color: c_bot,
    });
    mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
    painter.add(egui::Shape::mesh(mesh));
}

/// Render `add_contents` inline when it fits within `max_height`, or inside an exact-height
/// vertical [`egui::ScrollArea`] when it overflows (or when `stick_to_bottom` pins a live tail).
///
/// Pre-allocating the exact rect before entering the nested `ScrollArea` prevents egui from
/// clamping the nested viewport to whatever sliver of `available_rect_before_wrap` remains near
/// the bottom of the outer transcript `ScrollArea`. Drag-scroll is disabled so click-drag selects
/// text cleanly, and soft top/bottom fades signal when more content exists out of view.
#[allow(clippy::too_many_arguments)]
pub(super) fn show_bounded_scroll_box(
    ui: &mut Ui,
    scroll_id: Id,
    width: f32,
    content_height: f32,
    max_height: f32,
    min_height: f32,
    fade_bg: egui::Color32,
    stick_to_bottom: bool,
    allow_wheel: bool,
    add_contents: impl FnOnce(&mut Ui),
) {
    if !stick_to_bottom && content_height <= max_height + 1.0 {
        ui.vertical(|ui| {
            ui.set_width(width);
            add_contents(ui);
        });
        return;
    }

    let box_h = content_height.clamp(min_height, max_height);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, box_h), egui::Sense::hover());
    let mut offset_y = 0.0_f32;
    let mut inner_h = 0.0_f32;
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
        let scroll_source = if allow_wheel {
            egui::containers::scroll_area::ScrollSource {
                drag: egui::containers::scroll_area::DragScroll::Never,
                ..Default::default()
            }
        } else {
            egui::containers::scroll_area::ScrollSource::NONE
        };
        let bar_vis = if allow_wheel {
            egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded
        } else {
            egui::scroll_area::ScrollBarVisibility::AlwaysHidden
        };
        let output = egui::ScrollArea::vertical()
            .id_salt(scroll_id)
            .auto_shrink([false, false])
            .max_height(max_height)
            .min_scrolled_height(min_height)
            .scroll_bar_visibility(bar_vis)
            .stick_to_bottom(stick_to_bottom)
            .scroll_source(scroll_source)
            .show(ui, |ui| {
                ui.set_width(width);
                add_contents(ui);
            });
        offset_y = output.state.offset.y;
        inner_h = output.content_size.y;
    });
    if offset_y > 1.5 {
        paint_vertical_fade(ui.painter(), rect, fade_bg, true);
    }
    if offset_y + rect.height() < inner_h - 1.5 {
        paint_vertical_fade(ui.painter(), rect, fade_bg, false);
    }
}

/// Write/edit-like tools render as full detail blocks, not explore-cluster rows.
pub(super) fn is_edit_like_tool(block: &AssistantBlock) -> bool {
    matches!(
        block,
        AssistantBlock::Tool { name, .. }
            if matches!(name.to_ascii_lowercase().as_str(), "write" | "delete" | "move" | "mkdir")
    ) || tool_breaks_explore_cluster(block)
}

const ANSWER_RENDER_CHUNK_BYTES: usize = 24 * 1024;

fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    index = index.min(text.len());
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// Render large answers incrementally. Parsing and laying out an entire multi-hundred-KiB Markdown
/// response (especially fenced code) every frame is expensive in immediate-mode egui. Completed
/// answers start with a bounded prefix and can be revealed in chunks; a live answer keeps a
/// bounded tail visible so the newest streamed output is never hidden.
fn render_answer_markdown(
    ui: &mut Ui,
    msg_idx: usize,
    block_idx: usize,
    text: &str,
    streaming: bool,
) {
    if text.len() <= ANSWER_RENDER_CHUNK_BYTES {
        markdown::render_markdown(ui, text);
        return;
    }

    let state_id = Id::new(("answer_render_budget", msg_idx, block_idx));
    if streaming {
        let start = floor_char_boundary(text, text.len() - ANSWER_RENDER_CHUNK_BYTES);
        ui.label(
            RichText::new(format!(
                "Showing the latest {} KiB while this response streams",
                ANSWER_RENDER_CHUNK_BYTES / 1024
            ))
            .size(FS_TINY)
            .color(c_text_muted()),
        );
        markdown::render_markdown(ui, &text[start..]);
        return;
    }

    let shown = ui
        .ctx()
        .data_mut(|d| d.get_persisted::<usize>(state_id))
        .unwrap_or(ANSWER_RENDER_CHUNK_BYTES)
        .max(ANSWER_RENDER_CHUNK_BYTES)
        .min(text.len());
    let end = floor_char_boundary(text, shown);
    markdown::render_markdown(ui, &text[..end]);

    if end < text.len() {
        ui.add_space(4.0);
        let remaining_kib = (text.len() - end).div_ceil(1024);
        let label = format!("Show more of this response ({remaining_kib} KiB remaining)");
        if crate::ui::chrome::ghost_button(ui, &label, false).clicked() {
            let next = shown
                .saturating_add(ANSWER_RENDER_CHUNK_BYTES)
                .min(text.len());
            ui.ctx().data_mut(|d| d.insert_persisted(state_id, next));
            ui.ctx().request_repaint();
        }
    }
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

pub fn render_message(ui: &mut Ui, msg_idx: usize, msg: &ChatMessage) -> egui::Response {
    let col_w = content_wrap_width(ui);

    if msg.role == MsgRole::User && msg.is_summary {
        let response = ui
            .vertical(|ui| {
                ui.set_width(col_w);
                Frame::new()
                    .fill(c_bg_elevated_2())
                    .stroke(Stroke::new(1.0, c_border_subtle()))
                    .corner_radius(CornerRadius::same(RADIUS_CARD))
                    .inner_margin(Margin::symmetric(12, 9))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        if crate::ui::chrome::disclosure_row(
                            ui,
                            ui.id().with(("summary", msg_idx)),
                            "Conversation summary",
                            false,
                        ) {
                            ui.add_space(6.0);
                            markdown::render_markdown(ui, &msg.text);
                        }
                    });
            })
            .response;
        ui.add_space(8.0);
        return response;
    }

    if msg.is_system_report() {
        let view = parse_system_report(&msg.text);
        let response = ui
            .vertical(|ui| {
                ui.set_width(col_w);
                Frame::new()
                    .fill(c_bg_elevated_2())
                    .stroke(Stroke::new(1.0, c_border_subtle()))
                    .corner_radius(CornerRadius::same(RADIUS_CARD))
                    .inner_margin(Margin::symmetric(12, 8))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        let (icon, icon_color) = if view.any_failed {
                            (ICON_WARNING, c_error_fg())
                        } else if view.all_completed {
                            (ICON_CHECK_CIRCLE, c_success())
                        } else {
                            (ICON_INFO, c_text_muted())
                        };
                        if system_report_disclosure_row(
                            ui,
                            ui.id().with(("system_report", msg_idx)),
                            icon,
                            icon_color,
                            &view.title,
                        ) {
                            ui.add_space(6.0);
                            markdown::render_markdown(ui, &view.body_markdown);
                        }
                    });
            })
            .response;
        crate::ui::chrome::copy_message_context_menu(
            &response,
            egui::Id::new(("copy_system_report", msg_idx)),
            &msg.text,
        );
        ui.add_space(8.0);
        return response;
    }

    if msg.role == MsgRole::User {
        // Right-aligned rounded pill bubble (capped width) matching Antigravity's calm dark pill.
        let bubble_w = (col_w * 0.78).max(220.0).min(col_w);
        let response = ui
            .with_layout(egui::Layout::top_down(egui::Align::Max), |ui| {
                ui.set_max_width(col_w);
                Frame::new()
                    .fill(c_user_bubble())
                    .stroke(Stroke::new(1.0, c_border()))
                    .corner_radius(CornerRadius::same(RADIUS_PANEL))
                    .inner_margin(Margin::symmetric(16, 10))
                    .show(ui, |ui| {
                        ui.set_max_width(bubble_w);
                        if !msg.text.is_empty() {
                            let mut job = egui::text::LayoutJob::default();
                            let mut format = egui::text::TextFormat::simple(
                                egui::FontId::proportional(FS_BODY),
                                c_text_strong(),
                            );
                            format.line_height = Some(FS_BODY * 1.5);
                            job.append(&msg.text, 0.0, format);
                            job.wrap.max_width = ui.available_width();
                            crate::theme::selectable_text_job(ui, job);
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
        if !msg.text.is_empty() {
            crate::ui::chrome::copy_message_context_menu(
                &response,
                egui::Id::new(("copy_user_msg", msg_idx)),
                &msg.text,
            );
        }
        // Quiet, always-visible action row under the user bubble (matches the assistant footer,
        // with zero layout shift on hover).
        let busy = turn_busy(ui.ctx());
        let plain_text = crate::app::mentions::strip_mention_context(&msg.text).to_owned();
        ui.add_space(3.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.set_max_width(col_w);
            ui.spacing_mut().item_spacing.x = 2.0;
            let rewind_resp = ui
                .add_enabled_ui(!busy, |ui| {
                    crate::ui::chrome::icon_button_core(
                        ui,
                        ICON_UNDO,
                        egui::vec2(22.0, 22.0),
                        FS_TINY,
                        false,
                        &crate::ui::chrome::IconButtonLook {
                            fill: egui::Color32::TRANSPARENT,
                            hover_fill: c_row_hover(),
                            stroke: egui::Color32::TRANSPARENT,
                            hover_stroke: egui::Color32::TRANSPARENT,
                            rounding: CornerRadius::same(RADIUS_ROW),
                            glyph: if busy {
                                c_text_faint().gamma_multiply(0.5)
                            } else {
                                c_text_faint()
                            },
                        },
                    )
                })
                .inner
                .on_hover_text("Rewind chat to before this message…")
                .on_disabled_hover_text("Wait for the response to finish");
            if !busy && rewind_resp.clicked() {
                request_user_message_action(ui.ctx(), UserMessageAction::Rewind(msg_idx));
            }
            let fork_resp = ui
                .add_enabled_ui(!busy, |ui| {
                    crate::ui::chrome::icon_button_core(
                        ui,
                        ICON_BRANCH,
                        egui::vec2(22.0, 22.0),
                        FS_TINY,
                        false,
                        &crate::ui::chrome::IconButtonLook {
                            fill: egui::Color32::TRANSPARENT,
                            hover_fill: c_row_hover(),
                            stroke: egui::Color32::TRANSPARENT,
                            hover_stroke: egui::Color32::TRANSPARENT,
                            rounding: CornerRadius::same(RADIUS_ROW),
                            glyph: if busy {
                                c_text_faint().gamma_multiply(0.5)
                            } else {
                                c_text_faint()
                            },
                        },
                    )
                })
                .inner
                .on_hover_text("Edit in a fork")
                .on_disabled_hover_text("Wait for the response to finish");
            if !busy && fork_resp.clicked() {
                request_user_message_action(ui.ctx(), UserMessageAction::Fork(msg_idx));
            }
            if !plain_text.is_empty() {
                let copy_id = Id::new(("user_footer_copy", msg_idx));
                let copied = crate::ui::chrome::copied_recently(ui.ctx(), copy_id);
                let icon = if copied { ICON_CHECK } else { ICON_COPY };
                let copy_resp = crate::ui::chrome::icon_button_core(
                    ui,
                    icon,
                    egui::vec2(22.0, 22.0),
                    FS_TINY,
                    false,
                    &crate::ui::chrome::IconButtonLook {
                        fill: egui::Color32::TRANSPARENT,
                        hover_fill: c_row_hover(),
                        stroke: egui::Color32::TRANSPARENT,
                        hover_stroke: egui::Color32::TRANSPARENT,
                        rounding: CornerRadius::same(RADIUS_ROW),
                        glyph: if copied { c_success() } else { c_text_faint() },
                    },
                )
                .on_hover_text(if copied { "Copied" } else { "Copy message" });
                if copy_resp.clicked() {
                    ui.ctx().copy_text(plain_text);
                    crate::ui::chrome::mark_copied(ui.ctx(), copy_id);
                    crate::ui::chrome::show_toast(ui.ctx(), "Copied message");
                }
            }
        });
        ui.add_space(10.0);
        return response;
    }

    ui.vertical(|ui| {
        render_assistant_message_run(ui, msg_idx, std::slice::from_ref(msg), None);
    })
    .response
}

fn render_user_attachments(ui: &mut Ui, msg_idx: usize, attachments: &[UserAttachment]) {
    let image_count = attachments
        .iter()
        .filter(|a| matches!(a, UserAttachment::Image { .. }))
        .count();
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = eframe::egui::vec2(6.0, 6.0);
        for (i, att) in attachments.iter().enumerate() {
            match att {
                UserAttachment::Text { name, text } => {
                    if ui
                        .button(format!(
                            "{ICON_FILE} {name} · {} lines",
                            text.lines().count()
                        ))
                        .clicked()
                    {
                        crate::ui::text_attachment::open(ui.ctx(), name, text);
                    }
                }
                UserAttachment::Image { mime, data } => {
                    if let Some(tex) = user_image_texture(ui, msg_idx, i, data) {
                        // One image shows large; several share the bubble as a grid of smaller
                        // thumbnails. Never wider than the bubble.
                        let max = if image_count > 1 { 180.0 } else { 320.0 };
                        let mut sz = tex.size_vec2();
                        let m = sz.x.max(sz.y);
                        if m > max {
                            sz *= max / m;
                        }
                        let room = ui.max_rect().width();
                        if sz.x > room && room > 0.0 {
                            sz *= room / sz.x;
                        }
                        // Allocated at its final size so the row wraps: a `Frame` is placed
                        // before its size is known, so many images ran off the right of the
                        // bubble and pushed the whole chat column sideways.
                        let (rect, resp) = ui.allocate_exact_size(sz, egui::Sense::click());
                        let radius = CornerRadius::same(RADIUS_CHIP);
                        Image::new((tex.id(), sz))
                            .corner_radius(radius)
                            .paint_at(ui, rect);
                        ui.painter().rect_stroke(
                            rect,
                            radius,
                            Stroke::new(1.0, c_border()),
                            egui::StrokeKind::Inside,
                        );
                        let resp = resp.on_hover_cursor(egui::CursorIcon::ZoomIn);
                        if resp.clicked() {
                            crate::ui::image_viewer::open(ui.ctx(), data);
                        }
                    } else {
                        // Fallback badge when texture loading fails
                        Frame::new()
                            .fill(c_bg_elevated_2())
                            .corner_radius(CornerRadius::same(RADIUS_BUTTON))
                            .inner_margin(Margin::symmetric(8, 4))
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

/// `previous_agent` answered the turn before this run; a different directly chosen agent here
/// gets a handoff line.
pub fn render_assistant_message_run(
    ui: &mut Ui,
    msg_idx: usize,
    messages: &[ChatMessage],
    previous_agent: Option<crate::settings::LlmProviderKind>,
) {
    let col_w = content_wrap_width(ui);
    let mut blocks = Vec::new();
    let mut streaming = false;
    let mut started_at = None;
    let mut worked_duration = None;
    let mut route = None;
    for msg in messages {
        if msg.role != MsgRole::Assistant {
            continue;
        }
        route = route.or(msg.route.as_deref());
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
        match route {
            Some(route) if route.direct => {
                let from = previous_agent.filter(|p| *p != route.provider);
                if from.is_some() || route.failover_from.is_some() {
                    render_handoff_note(ui, from, route);
                }
            }
            Some(route) => render_route_note(ui, route),
            None => {}
        }
        render_assistant_blocks(
            ui,
            msg_idx,
            msg_idx + messages.len(),
            &blocks,
            streaming,
            started_at,
            worked_duration,
        );
    });
    ui.add_space(SPACE_LG);
}

/// The chat moved to another agent with this turn: "Claude Code → Codex · gpt-5.5". The new
/// agent got the earlier turns, so it is still one conversation. A turn that moved because the
/// picked agent ran out of quota says so.
fn render_handoff_note(
    ui: &mut Ui,
    from: Option<crate::settings::LlmProviderKind>,
    route: &crate::model::RouteNote,
) {
    let short = |kind: crate::settings::LlmProviderKind| kind.label().trim_end_matches(" (ACP)");
    let mut title = match from {
        Some(from) => format!("{} → {}", short(from), short(route.provider)),
        None => format!("→ {}", short(route.provider)),
    };
    if !route.model.is_empty() {
        title.push_str(&format!(" · {}", route.model));
    }
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 5.0;
        ui.label(crate::ui::chrome::icon_glyph_rich(
            ICON_PROVIDERS,
            FS_TINY,
            c_text_faint(),
        ));
        ui.label(RichText::new(title).size(FS_SMALL).color(c_text_muted()));
        if route.failover_from.is_none() {
            ui.label(
                RichText::new("· same conversation, context handed over")
                    .size(FS_TINY)
                    .color(c_text_faint()),
            );
        }
    });
    if let Some(reason) = &route.failover_from {
        ui.add(
            Label::new(
                RichText::new(format!("Switched automatically · {reason}"))
                    .size(FS_TINY)
                    .color(c_warning_fg()),
            )
            .wrap(),
        );
    }
    ui.add_space(4.0);
}

/// The Router's pick for this turn: "Router → Claude Code · opus · high" with the reason
/// underneath; hovering lists the runner-up and skipped candidates.
fn render_route_note(ui: &mut Ui, route: &crate::model::RouteNote) {
    let mut title = format!("Router → {} · {}", route.provider.label(), route.model);
    if !route.effort.is_empty() {
        title.push_str(&format!(" · {} effort", route.effort));
    }
    let response = ui
        .horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 5.0;
            ui.label(crate::ui::chrome::icon_glyph_rich(
                ICON_PROVIDERS,
                FS_TINY,
                c_accent(),
            ));
            ui.label(RichText::new(title).size(FS_SMALL).color(c_text_muted()));
            if !route.strategy.is_empty() {
                ui.label(
                    RichText::new(format!("({})", route.strategy))
                        .size(FS_TINY)
                        .color(c_text_faint()),
                );
            }
        })
        .response;
    if let Some(from) = &route.failover_from {
        ui.add(
            Label::new(
                RichText::new(format!("Switched from {from}"))
                    .size(FS_TINY)
                    .color(c_warning_fg()),
            )
            .wrap(),
        );
    }
    let reason = ui.add(
        Label::new(
            RichText::new(&route.reason)
                .size(FS_TINY)
                .color(c_text_faint()),
        )
        .wrap(),
    );
    if !route.alternatives.is_empty() {
        let details = route.alternatives.join("\n");
        response
            .union(reason)
            .on_hover_text(format!("Other candidates:\n{details}"));
    }
    ui.add_space(4.0);
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
    let last_streaming_edit = streaming
        .then(|| blocks.iter().rposition(is_edit_like_tool))
        .flatten();
    let newest_tool = blocks
        .iter()
        .rposition(|block| matches!(block, AssistantBlock::Tool { .. }));
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
                if let AssistantBlock::Answer(text) = &blocks[gi]
                    && !text.trim().is_empty()
                {
                    let response = ui
                        .vertical(|ui| {
                            render_answer_markdown(ui, msg_idx, gi, text, streaming);
                        })
                        .response;
                    crate::ui::chrome::copy_message_context_menu(
                        &response,
                        egui::Id::new(("copy_answer", msg_idx, gi)),
                        text,
                    );
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
                render_single_tool_block(
                    ui,
                    msg_idx,
                    gi,
                    &blocks[gi],
                    streaming,
                    last_streaming_edit == Some(gi),
                    newest_tool == Some(gi),
                );
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
        // Sub-second turns read as "Worked for 0s"; drop the duration like an untracked one.
        let base = match worked_duration {
            Some(d) if d.as_secs() > 0 => format!("Worked for {}", format_worked_duration(d)),
            _ => "Worked".to_string(),
        };
        let tool_n = blocks[..worked_end]
            .iter()
            .filter(|b| matches!(b, AssistantBlock::Tool { .. }))
            .count();
        let bg_n = blocks[..worked_end]
            .iter()
            .filter(|b| {
                matches!(
                    b,
                    AssistantBlock::Tool {
                        tool_call_id,
                        metadata: Some(meta),
                        ..
                    } if !tool_call_id.is_empty()
                        && matches!(
                            meta.status,
                            crate::model::ToolStatus::Pending
                                | crate::model::ToolStatus::InProgress
                        )
                        && is_background_tool(ui.ctx(), tool_call_id)
                )
            })
            .count();
        let summary = if tool_n == 0 {
            base
        } else if tool_n == 1 {
            format!("{base} · 1 tool")
        } else {
            format!("{base} · {tool_n} tools")
        };
        if bg_n > 0 {
            format!("{summary} · {bg_n} running")
        } else {
            summary
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

    // Allocate the row rect up front so we can read hover state *before* painting, then
    // tint the chevron with the accent (copper) color on hover — matching the Thinking
    // block behavior. The "Worked..." text stays in its quiet muted tone so the row
    // doesn't shout.
    let row_height = ui.spacing().interact_size.y;
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), row_height),
        egui::Sense::click(),
    );
    let click = ui.interact(
        rect,
        persist_id.with("activity_summary_click"),
        egui::Sense::click(),
    );
    // A live turn can't be folded, so it gets no hover affordance either.
    let hovered = !streaming && click.hovered();
    let label_col = if hovered { c_text() } else { c_text_muted() };
    let chevron_col = if hovered {
        c_text_muted()
    } else {
        c_text_faint()
    };

    // Label followed by trailing chevron ("Worked for 9m ›" / "Worked for 20s ⌄").
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.label(RichText::new(label).size(FS_SMALL).color(label_col));
            ui.label(
                RichText::new(if expanded {
                    ICON_ANGLE_DOWN
                } else {
                    ICON_CHEVRON_RIGHT
                })
                .font(FontId::new(FS_TINY - 1.5, icon_font()))
                .color(chevron_col),
            );
        });
    });

    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if !streaming && click.clicked() {
        crate::ui::preview_expand::toggle_expanded(ui, persist_id);
    }

    if expanded {
        ui.add_space(3.0);
        render_activity_range(ui, msg_idx, blocks, 0, worked_end, streaming);
    }
}

pub fn render_assistant_blocks(
    ui: &mut Ui,
    msg_idx: usize,
    run_end: usize,
    blocks: &[AssistantBlock],
    streaming: bool,
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
        ui.add_space(SPACE_SM);
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

    for (i, block) in blocks[worked_end..].iter().enumerate() {
        if let AssistantBlock::Answer(text) = block
            && (!text.trim().is_empty() || streaming)
        {
            let (body, error) = split_error_tail(text);
            let response = ui
                .vertical(|ui| {
                    if !body.trim().is_empty() || (streaming && error.is_none()) {
                        render_answer_markdown(ui, msg_idx, worked_end + i, body, streaming);
                    }
                    if let Some(error) = error {
                        render_error_callout(ui, error);
                    }
                })
                .response;
            if !text.trim().is_empty() {
                crate::ui::chrome::copy_message_context_menu(
                    &response,
                    egui::Id::new(("copy_trailing_answer", msg_idx, worked_end + i)),
                    text,
                );
            }
        }
    }

    // Quiet action row under a completed assistant answer (above the changed-files summary):
    // Copy response and Fork chat from here.
    if !streaming {
        let answer_text: String = blocks[worked_end..]
            .iter()
            .filter_map(|b| match b {
                AssistantBlock::Answer(t) if !t.trim().is_empty() => Some(t.trim()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        let busy = turn_busy(ui.ctx());
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if !answer_text.is_empty() {
                let copy_id = Id::new(("assistant_footer_copy", msg_idx));
                let copied = crate::ui::chrome::copied_recently(ui.ctx(), copy_id);
                let icon = if copied { ICON_CHECK } else { ICON_COPY };
                let resp = crate::ui::chrome::icon_button_core(
                    ui,
                    icon,
                    egui::vec2(24.0, 24.0),
                    FS_SMALL,
                    false,
                    &crate::ui::chrome::IconButtonLook {
                        fill: egui::Color32::TRANSPARENT,
                        hover_fill: c_row_hover(),
                        stroke: egui::Color32::TRANSPARENT,
                        hover_stroke: egui::Color32::TRANSPARENT,
                        rounding: CornerRadius::same(RADIUS_ROW),
                        glyph: if copied { c_success() } else { c_text_faint() },
                    },
                )
                .on_hover_text(if copied { "Copied" } else { "Copy response" });
                if resp.clicked() {
                    ui.ctx().copy_text(answer_text);
                    crate::ui::chrome::mark_copied(ui.ctx(), copy_id);
                    crate::ui::chrome::show_toast(ui.ctx(), "Copied response");
                }
            }
            let fork_resp = ui
                .add_enabled_ui(!busy, |ui| {
                    crate::ui::chrome::icon_button_core(
                        ui,
                        ICON_BRANCH,
                        egui::vec2(24.0, 24.0),
                        FS_SMALL,
                        false,
                        &crate::ui::chrome::IconButtonLook {
                            fill: egui::Color32::TRANSPARENT,
                            hover_fill: c_row_hover(),
                            stroke: egui::Color32::TRANSPARENT,
                            hover_stroke: egui::Color32::TRANSPARENT,
                            rounding: CornerRadius::same(RADIUS_ROW),
                            glyph: if busy {
                                c_text_faint().gamma_multiply(0.5)
                            } else {
                                c_text_faint()
                            },
                        },
                    )
                })
                .inner
                .on_hover_text("Fork chat from here")
                .on_disabled_hover_text("Wait for the response to finish");
            if !busy && fork_resp.clicked() {
                request_fork_message(ui.ctx(), msg_idx, run_end);
            }
        });
    }

    // Text can be interim commentary, especially with ACP: only Finished ends the turn.
    // Keep activity at the visible tail even when no tool or reasoning update follows it;
    // the activity summary above the answer may already be outside the viewport.
    if streaming
        && !turn_stalled(ui.ctx())
        && blocks[worked_end..]
            .iter()
            .any(|block| matches!(block, AssistantBlock::Answer(text) if !text.trim().is_empty()))
    {
        ui.add_space(SPACE_SM);
        ui.horizontal(|ui| {
            animated_status_label(ui, "Working", FS_SMALL);
        });
    }
}

fn turn_stalled_id() -> Id {
    Id::new("oxi_turn_stalled")
}

/// The chat says the running turn went quiet (and offers Stop/Continue) below the transcript,
/// so the "Working" line is dropped for that frame instead of claiming progress.
pub fn set_turn_stalled(ctx: &egui::Context, stalled: bool) {
    ctx.data_mut(|d| d.insert_temp(turn_stalled_id(), stalled));
}

fn turn_stalled(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp(turn_stalled_id())).unwrap_or(false)
}

/// Run failures are appended to the answer as a `[Error] …` line. Split that tail off so it
/// renders as a callout instead of reading like part of the model's reply.
fn split_error_tail(text: &str) -> (&str, Option<&str>) {
    const MARKER: &str = "[Error] ";
    let at = if text.starts_with(MARKER) {
        Some(0)
    } else {
        text.find(&format!("\n{MARKER}")).map(|i| i + 1)
    };
    match at {
        Some(at) => (&text[..at], Some(text[at + MARKER.len()..].trim())),
        None => (text, None),
    }
}

fn render_error_callout(ui: &mut Ui, error: &str) {
    ui.add_space(4.0);
    Frame::new()
        .fill(c_error_bg())
        .stroke(Stroke::new(1.0, c_error_fg().gamma_multiply(0.35)))
        .corner_radius(CornerRadius::same(RADIUS_CARD))
        .inner_margin(Margin::symmetric(12, 9))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                ui.label(crate::ui::chrome::icon_glyph_rich(
                    ICON_WARNING,
                    FS_SMALL,
                    c_error_fg(),
                ));
                // Errors quote commands in Markdown backticks; show them as plain text here.
                let hint = error_hint(error);
                let error = error.replace('`', "");
                ui.vertical(|ui| {
                    ui.add(
                        Label::new(
                            RichText::new(error)
                                .size(FS_SMALL)
                                .line_height(Some(FS_SMALL * 1.4))
                                .color(c_text()),
                        )
                        .wrap()
                        .selectable(true),
                    );
                    if let Some(hint) = hint {
                        ui.add_space(2.0);
                        ui.add(
                            Label::new(RichText::new(hint).size(FS_SMALL).color(c_text_muted()))
                                .wrap(),
                        );
                    }
                });
            });
        });
    ui.add_space(4.0);
}

/// A next step for the most common run failures, which otherwise surface as raw provider
/// messages.
fn error_hint(error: &str) -> Option<&'static str> {
    let e = error.to_ascii_lowercase();
    // Status codes count only as whole numbers ("HTTP 429", not a port like 14290).
    let code = |code: &str| {
        e.match_indices(code).any(|(at, _)| {
            let before = e[..at].chars().next_back();
            let after = e[at + code.len()..].chars().next();
            !before.is_some_and(|c| c.is_ascii_digit())
                && !after.is_some_and(|c| c.is_ascii_digit())
        })
    };
    let has = |needles: &[&str]| {
        needles.iter().any(|n| {
            if n.bytes().all(|b| b.is_ascii_digit()) {
                code(n)
            } else {
                e.contains(n)
            }
        })
    };
    if has(&[
        "401",
        "unauthorized",
        "invalid api key",
        "invalid_api_key",
        "incorrect api key",
    ]) {
        Some("Check the API key in Settings → Models & providers.")
    } else if has(&["403", "forbidden", "permission denied"]) {
        Some(
            "The key works but has no access to this model; pick another model or check the account.",
        )
    } else if has(&[
        "429",
        "rate limit",
        "rate_limit",
        "too many requests",
        "quota",
    ]) {
        Some("The provider is rate limiting or the quota is used up; wait a moment and try again.")
    } else if has(&[
        "context length",
        "context_length",
        "maximum context",
        "too many tokens",
        "prompt is too long",
    ]) {
        Some(
            "The conversation no longer fits the model's context; start a new chat or use a model with a larger window.",
        )
    } else if has(&[
        "connection refused",
        "error sending request",
        "dns error",
        "failed to connect",
        "could not connect",
    ]) {
        Some(
            "Couldn't reach the server. For a local model, make sure it is running; otherwise check the base URL and your connection.",
        )
    } else if has(&["timed out", "timeout"]) {
        Some("The request timed out; the server may be busy or still loading the model.")
    } else {
        None
    }
}

fn terminal_command_id() -> eframe::egui::Id {
    eframe::egui::Id::new("oxi_open_command_in_terminal")
}

/// Ask the app to type `command` into a new terminal tab (picked up once per frame).
pub(crate) fn request_terminal_command(ctx: &eframe::egui::Context, command: &str) {
    ctx.data_mut(|d| d.insert_temp(terminal_command_id(), command.to_owned()));
}

/// The command a transcript widget asked to open in the terminal, if any.
pub(crate) fn take_terminal_command(ctx: &eframe::egui::Context) -> Option<String> {
    ctx.data_mut(|d| d.remove_temp::<String>(terminal_command_id()))
}

fn tool_output_path_id() -> eframe::egui::Id {
    eframe::egui::Id::new("oxi_open_tool_output_path")
}

/// Ask the app to open a truncated tool's full output file.
pub(crate) fn request_open_tool_output(ctx: &eframe::egui::Context, path: &str) {
    ctx.data_mut(|d| d.insert_temp(tool_output_path_id(), path.to_owned()));
}

/// The truncated tool output path a transcript widget asked to inspect, if any.
pub(crate) fn take_open_tool_output(ctx: &eframe::egui::Context) -> Option<String> {
    ctx.data_mut(|d| d.remove_temp::<String>(tool_output_path_id()))
}

fn turn_busy_id() -> eframe::egui::Id {
    eframe::egui::Id::new("oxi_turn_busy")
}

/// Publish whether the active chat is currently busy so message action bars disable Fork while
/// streaming.
pub fn set_turn_busy(ctx: &eframe::egui::Context, busy: bool) {
    ctx.data_mut(|d| d.insert_temp(turn_busy_id(), busy));
}

fn turn_busy(ctx: &eframe::egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(turn_busy_id()))
        .unwrap_or(false)
}

fn background_tools_id() -> eframe::egui::Id {
    eframe::egui::Id::new("oxi_acp_background_tools")
}

/// Set once per conversation frame with tool call ids still running in the active ACP subprocess
/// after `end_turn`, so their pills and summaries stay visibly running.
pub fn set_background_tools(
    ctx: &eframe::egui::Context,
    tool_ids: std::collections::HashSet<String>,
) {
    ctx.data_mut(|d| {
        d.insert_temp(background_tools_id(), std::sync::Arc::new(tool_ids));
    });
}

pub(super) fn is_background_tool(ctx: &eframe::egui::Context, tool_call_id: &str) -> bool {
    if tool_call_id.is_empty() {
        return false;
    }
    ctx.data(|d| {
        d.get_temp::<std::sync::Arc<std::collections::HashSet<String>>>(background_tools_id())
            .is_some_and(|ids| ids.contains(tool_call_id))
    })
}

fn fork_message_id() -> eframe::egui::Id {
    eframe::egui::Id::new("oxi_fork_message_request")
}

fn request_fork_message(ctx: &eframe::egui::Context, start: usize, end: usize) {
    ctx.data_mut(|d| d.insert_temp(fork_message_id(), (start, end)));
}

/// Assistant message run `(start, end)` the user asked to fork from its footer action row.
pub(crate) fn take_fork_message(ctx: &eframe::egui::Context) -> Option<(usize, usize)> {
    ctx.data_mut(|d| d.remove_temp::<(usize, usize)>(fork_message_id()))
}

#[derive(Clone, Copy)]
pub(crate) enum UserMessageAction {
    Fork(usize),
    Rewind(usize),
}

fn user_message_action_id() -> eframe::egui::Id {
    eframe::egui::Id::new("oxi_user_message_action")
}

fn request_user_message_action(ctx: &eframe::egui::Context, action: UserMessageAction) {
    ctx.data_mut(|d| d.insert_temp(user_message_action_id(), action));
}

/// User message action triggered from the quiet row below a user bubble, if any.
pub(crate) fn take_user_message_action(ctx: &eframe::egui::Context) -> Option<UserMessageAction> {
    ctx.data_mut(|d| {
        let id = user_message_action_id();
        let action = d.get_temp::<UserMessageAction>(id);
        if action.is_some() {
            d.remove::<UserMessageAction>(id);
        }
        action
    })
}

/// Short single-line summary for a worker/delegated completion report delivered by oxi.
pub(crate) fn system_report_title(text: &str) -> String {
    parse_system_report(text).title
}

struct SystemReportView {
    title: String,
    body_markdown: String,
    all_completed: bool,
    any_failed: bool,
}

fn system_report_disclosure_row(
    ui: &mut Ui,
    id: egui::Id,
    status_icon: &str,
    status_color: egui::Color32,
    title: &str,
) -> bool {
    let mut open = ui.data_mut(|d| *d.get_persisted_mut_or_insert_with(id, || false));
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 24.0), egui::Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::CollapsingHeader,
            ui.is_enabled(),
            open,
            title,
        )
    });
    let hovered = ui.is_enabled() && (response.hovered() || response.has_focus());
    let radius = CornerRadius::same(RADIUS_ROW);
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        ui.painter().rect_filled(rect, radius, c_row_hover());
    }
    crate::ui::chrome::keyboard_focus_ring(ui, &response, radius);
    let ink = if hovered { c_text() } else { c_text_muted() };
    ui.painter().text(
        egui::pos2(rect.left() + 8.0, rect.center().y),
        egui::Align2::CENTER_CENTER,
        if open {
            ICON_ANGLE_DOWN
        } else {
            ICON_CHEVRON_RIGHT
        },
        FontId::new(FS_TINY - 1.5, icon_font()),
        ink,
    );
    ui.painter().text(
        egui::pos2(rect.left() + 24.0, rect.center().y),
        egui::Align2::CENTER_CENTER,
        status_icon,
        FontId::new(FS_TINY, icon_font()),
        status_color,
    );
    let text_left = rect.left() + 38.0;
    let max_text_w = (rect.right() - text_left - 8.0).max(0.0);
    let mut job = LayoutJob::single_section(
        title.to_string(),
        egui::TextFormat::simple(FontId::proportional(FS_SMALL), ink),
    );
    job.wrap = egui::text::TextWrapping {
        max_width: max_text_w,
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    let galley = ui.painter().layout_job(job);
    let text_y = rect.center().y - galley.size().y * 0.5;
    ui.painter()
        .galley(egui::pos2(text_left, text_y), galley, ink);
    if response.clicked() {
        open = !open;
        ui.data_mut(|d| d.insert_persisted(id, open));
    }
    open
}

fn parse_system_report(text: &str) -> SystemReportView {
    if let Some(rest) = text.strip_prefix("[Delegated task] ") {
        let first_line = rest.lines().next().unwrap_or(rest).trim();
        let (who, state) = parse_task_header_line(first_line);
        let any_failed = matches!(state, Some("failed" | "not started"));
        let all_completed = matches!(state, Some("completed"));
        let noun = if any_failed {
            "Delegated task failed"
        } else {
            "Delegated task finished"
        };
        let title = match who {
            Some(who) => format!("{noun} · {who}"),
            None => noun.to_string(),
        };
        return SystemReportView {
            title,
            body_markdown: rest.trim().to_string(),
            all_completed,
            any_failed,
        };
    }

    // Worker batch report (`[Worker finished]\n...` or `[Workers finished]\n...`).
    let mut lines = text.lines();
    let header = lines.next().unwrap_or("");
    let is_plural = header.starts_with("[Workers finished]");
    let mut body_lines: Vec<String> = Vec::new();
    let mut total = 0usize;
    let mut completed = 0usize;
    let mut failed = 0usize;
    let mut single_who: Option<String> = None;

    for line in lines {
        if line == "Worker output for the lead to review. Each quoted report comes from the named worker."
        {
            continue;
        }
        let unquoted = line
            .strip_prefix("> ")
            .or_else(|| line.strip_prefix('>'))
            .unwrap_or(line);
        if unquoted.starts_with("Task ") {
            let (who, state) = parse_task_header_line(unquoted);
            total += 1;
            if total == 1 {
                single_who = who.map(str::to_owned);
            }
            match state {
                Some("completed") => completed += 1,
                Some("failed" | "not started") => failed += 1,
                _ => {}
            }
            if total > 1 {
                body_lines.push(String::new());
                body_lines.push("---".to_string());
                body_lines.push(String::new());
            }
            body_lines.push(format!("**{}**", unquoted.trim()));
        } else {
            body_lines.push(unquoted.to_string());
        }
    }

    let body_markdown = body_lines.join("\n").trim().to_string();
    let any_failed = failed > 0;
    let all_completed = total > 0 && completed == total;
    let title = match total {
        0 => if is_plural {
            "Workers finished"
        } else {
            "Worker finished"
        }
        .to_string(),
        1 => {
            let verb = if any_failed { "failed" } else { "finished" };
            match single_who {
                Some(who) => format!("Worker {verb} · {who}"),
                None => format!("Worker {verb}"),
            }
        }
        n if all_completed => format!("{n} workers finished"),
        n if failed == n => format!("{n} workers failed"),
        n => {
            let mut parts = Vec::new();
            if completed > 0 {
                parts.push(format!("{completed} done"));
            }
            if failed > 0 {
                parts.push(format!("{failed} failed"));
            }
            let other = n.saturating_sub(completed + failed);
            if other > 0 {
                parts.push(format!("{other} stopped"));
            }
            format!("{n} workers finished · {}", parts.join(", "))
        }
    };

    SystemReportView {
        title,
        body_markdown,
        all_completed,
        any_failed,
    }
}

/// Extract `(who, state)` from a line shaped like `Task w6be92e65 (Antigravity · model): completed.`.
fn parse_task_header_line(line: &str) -> (Option<&str>, Option<&str>) {
    let Some(after_open) = line.split_once(" (").map(|(_, r)| r) else {
        return (None, None);
    };
    let Some((who, after_close)) = after_open.split_once("): ") else {
        return (None, None);
    };
    let state = after_close
        .split('.')
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let who = (!who.trim().is_empty()).then_some(who.trim());
    (who, state)
}

#[cfg(test)]
mod answer_render_tests {
    use super::{error_hint, floor_char_boundary, parse_system_report, split_error_tail};

    #[test]
    fn system_report_parser_builds_clean_titles_and_unquoted_markdown() {
        let two_done = "[Workers finished]\n\
            Worker output for the lead to review. Each quoted report comes from the named worker.\n\n\
            > Task w6be92e65 (Antigravity (ACP) · gemini-4-neon): completed.\n\
            >\n\
            > Report:\n\
            > Found backend.\n\n\
            > Task w2d6e07fe (Antigravity (ACP) · gemini-4-neon): completed.\n\
            >\n\
            > Report:\n\
            > Found UI.";
        let view = parse_system_report(two_done);
        assert_eq!(view.title, "2 workers finished");
        assert!(view.all_completed);
        assert!(!view.any_failed);
        assert!(!view.body_markdown.contains("[Workers finished]"));
        assert!(!view.body_markdown.contains("Worker output for the lead"));
        assert!(view.body_markdown.contains("Found backend."));
        assert!(view.body_markdown.contains("Found UI."));
        assert!(view.body_markdown.contains("---"));

        let mixed = "[Workers finished]\n\
            Worker output for the lead to review. Each quoted report comes from the named worker.\n\n\
            > Task w1 (Codex · gpt): completed.\n\n\
            > Task w2 (Antigravity): failed. Ended without a report.";
        let view = parse_system_report(mixed);
        assert_eq!(view.title, "2 workers finished · 1 done, 1 failed");
        assert!(!view.all_completed);
        assert!(view.any_failed);

        let single = "[Worker finished]\n\
            Worker output for the lead to review. Each quoted report comes from the named worker.\n\n\
            > Task w1 (Antigravity · gemini): failed. Boom.";
        let view = parse_system_report(single);
        assert_eq!(view.title, "Worker failed · Antigravity · gemini");
        assert!(view.any_failed);

        let delegated = "[Delegated task] Task d1 (Codex): completed.\n\nReport:\nAll good.";
        let view = parse_system_report(delegated);
        assert_eq!(view.title, "Delegated task finished · Codex");
        assert!(view.all_completed);
        assert!(!view.any_failed);
        assert_eq!(view.body_markdown, "Task d1 (Codex): completed.\n\nReport:\nAll good.");
    }

    #[test]
    fn common_errors_get_a_hint() {
        assert!(
            error_hint("HTTP 401 Unauthorized")
                .unwrap()
                .contains("API key")
        );
        assert!(error_hint("error sending request for url (http://127.0.0.1:8080)").is_some());
        assert!(error_hint("Agent stopped unexpectedly.").is_none());
        assert!(error_hint("bound to port 14290").is_none());
        assert!(
            error_hint("HTTP 429 Too Many Requests")
                .unwrap()
                .contains("rate")
        );
    }

    #[test]
    fn error_tail_is_split_from_the_answer() {
        assert_eq!(split_error_tail("[Error] boom\n"), ("", Some("boom")));
        assert_eq!(
            split_error_tail("partial reply\n[Error] Agent stopped unexpectedly.\n"),
            ("partial reply\n", Some("Agent stopped unexpectedly."))
        );
        assert_eq!(
            split_error_tail("no [Error] here"),
            ("no [Error] here", None)
        );
    }

    #[test]
    fn floor_boundary_never_splits_utf8() {
        let text = "a🙂b";
        let inside_emoji = 2;
        assert_eq!(floor_char_boundary(text, inside_emoji), 1);
        assert!(text.is_char_boundary(floor_char_boundary(text, inside_emoji)));
    }
}
