//! Chat transcript rendering (bubbles, assistant blocks, markdown).
//!
//! Split by responsibility: [`tool_format`] (text formatting for tool calls: summaries,
//! diff/output layout jobs, icons), [`tool_pill`] (the tool-call pill/detail UI and the
//! "explored" cluster), and [`thinking`] (reasoning-block rendering). This file keeps
//! the top-level per-message orchestration: turning a `ChatMessage` (or run of them)
//! into user bubbles / assistant activity summaries / markdown answers.

mod thinking;
mod tool_format;
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

use thinking::{render_thinking_group_block_opts, thinking_group_is_live};
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

pub(super) fn selectable_layout_job(ui: &mut Ui, job: LayoutJob, allow_select: bool) {
    if job.text.is_empty() {
        return;
    }
    if !allow_select {
        ui.add(Label::new(job).wrap().selectable(false));
        return;
    }

    let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
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

pub(super) fn block_state_tag(streaming: bool) -> &'static str {
    if streaming { "live" } else { "done" }
}

/// Write/edit-like tools render as full detail blocks, not explore-cluster rows.
pub(super) fn is_edit_like_tool(block: &AssistantBlock) -> bool {
    matches!(
        block,
        AssistantBlock::Tool { name, .. }
            if matches!(name.to_ascii_lowercase().as_str(), "write" | "delete" | "move" | "mkdir")
    ) || tool_breaks_explore_cluster(block)
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
                        egui::CollapsingHeader::new(
                            RichText::new("Conversation summary")
                                .size(FS_SMALL)
                                .color(c_text_muted()),
                        )
                        .id_salt(("summary", msg_idx))
                        .default_open(false)
                        .show(ui, |ui| {
                            markdown::render_markdown(ui, &msg.text);
                        });
                    });
            })
            .response;
        ui.add_space(8.0);
        return response;
    }

    if msg.role == MsgRole::User {
        // Right-aligned chat bubble (capped width) so user turns read as chat, not a document.
        let bubble_w = (col_w * 0.78).clamp(220.0, col_w);
        let response = ui
            .with_layout(egui::Layout::top_down(egui::Align::Max), |ui| {
                ui.set_max_width(col_w);
                Frame::new()
                    .fill(c_user_bubble())
                    .stroke(Stroke::new(1.0, c_user_bubble_border()))
                    .corner_radius(CornerRadius::same(RADIUS_CARD))
                    .inner_margin(Margin::symmetric(12, 9))
                    .show(ui, |ui| {
                        ui.set_max_width(bubble_w);
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
        if !msg.text.is_empty() {
            crate::ui::chrome::copy_message_context_menu(
                &response,
                egui::Id::new(("copy_user_msg", msg_idx)),
                &msg.text,
            );
        }
        ui.add_space(10.0);
        return response;
    }

    ui.vertical(|ui| {
        render_assistant_message_run(ui, msg_idx, std::slice::from_ref(msg));
    })
    .response
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
                        Frame::new()
                            .corner_radius(CornerRadius::same(RADIUS_CHIP))
                            .stroke(Stroke::new(1.0, c_border()))
                            .show(ui, |ui| {
                                ui.add(Image::new((tex.id(), sz)));
                            });
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

pub fn render_assistant_message_run(ui: &mut Ui, msg_idx: usize, messages: &[ChatMessage]) {
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
        render_assistant_blocks(ui, msg_idx, &blocks, streaming, started_at, worked_duration);
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
                render_thinking_group_block_opts(
                    ui,
                    msg_idx,
                    global[0],
                    combined,
                    thinking_live,
                    true,
                );
            }
            AssistantBlockGroup::Answer(i) => {
                let gi = start + i;
                if let AssistantBlock::Answer(text) = &blocks[gi]
                    && !text.trim().is_empty()
                {
                    let response = ui
                        .vertical(|ui| {
                            markdown::render_markdown(ui, text);
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
        let base = match worked_duration {
            Some(d) => format!("Worked for {}", format_worked_duration(d)),
            None => "Worked".to_string(),
        };
        let tool_n = blocks[..worked_end]
            .iter()
            .filter(|b| matches!(b, AssistantBlock::Tool { .. }))
            .count();
        if tool_n == 0 {
            base
        } else if tool_n == 1 {
            format!("{base} · 1 tool")
        } else {
            format!("{base} · {tool_n} tools")
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
    let hovered = click.hovered();
    // Only the chevron lights up on hover (matching the Thinking block behavior); the
    // "Worked..." text stays in its quiet muted tone so the row doesn't shout.
    let chevron_col = if hovered { c_accent() } else { c_text_faint() };

    // Paint the chevron + label inside the reserved rect.
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 5.0;
            ui.label(
                RichText::new(if expanded {
                    ICON_ANGLE_UP
                } else {
                    ICON_ANGLE_DOWN
                })
                .font(FontId::new(FS_TINY, icon_font()))
                .color(chevron_col),
            );
            ui.label(RichText::new(label).size(FS_SMALL).color(c_text_muted()));
        });
    });

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

    for (i, block) in blocks[worked_end..].iter().enumerate() {
        if let AssistantBlock::Answer(text) = block
            && (!text.trim().is_empty() || streaming)
        {
            let response = ui
                .vertical(|ui| {
                    markdown::render_markdown(ui, text);
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
}
