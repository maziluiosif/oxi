//! Conversation transcript view (scroll area, messages, status banner).

use eframe::egui::scroll_area::ScrollBarVisibility;
use eframe::egui::{
    self, Align, Button, Color32, Frame, Label, Margin, RichText, Rounding, ScrollArea, Stroke, Ui,
};

use crate::model::MsgRole;
use crate::theme::{
    CHAT_COLUMN_MAX, C_BG_ELEVATED, C_BORDER_SUBTLE, C_TEXT, C_TEXT_MUTED, C_USER_BUBBLE, FS_BODY,
    FS_SMALL,
};
use crate::ui::chrome::render_empty_state;
use crate::ui::messages::{render_assistant_message_run, render_message};

use super::OxiApp;

impl OxiApp {
    /// Error banner rendered above the transcript.
    pub(crate) fn render_status_banner(&mut self, ui: &mut Ui) {
        let active_stream_error = self.active_stream_error().map(str::to_string);
        let has_err = self.conn.connect_error.is_some() || active_stream_error.is_some();
        if !has_err {
            return;
        }
        ui.spacing_mut().item_spacing.y = 4.0;
        if let Some(ref e) = self.conn.connect_error {
            Frame::none()
                .fill(Color32::from_rgb(0x32, 0x18, 0x18))
                .stroke(Stroke::new(1.0, Color32::from_rgb(0x70, 0x38, 0x38)))
                .rounding(Rounding::same(6.0))
                .inner_margin(Margin::symmetric(8.0, 6.0))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(
                        RichText::new(format!("Connection: {e}"))
                            .size(FS_SMALL)
                            .color(Color32::from_rgb(0xff, 0xb0, 0xb0)),
                    );
                });
            ui.add_space(4.0);
        }
        if let Some(e) = active_stream_error {
            Frame::none()
                .fill(Color32::from_rgb(0x38, 0x28, 0x14))
                .stroke(Stroke::new(1.0, Color32::from_rgb(0x78, 0x58, 0x28)))
                .rounding(Rounding::same(6.0))
                .inner_margin(Margin::symmetric(8.0, 6.0))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(
                        RichText::new(format!("Agent: {e}"))
                            .size(FS_SMALL)
                            .color(Color32::from_rgb(0xff, 0xd0, 0xa0)),
                    );
                });
            ui.add_space(4.0);
        }
    }

    pub(crate) fn render_conversation(
        &mut self,
        ui: &mut Ui,
        column_center_w: f32,
        scroll_budget: f32,
    ) {
        let show_sidebar_button = !self.conv.sidebar_open;
        let transcript_h = scroll_budget.max(40.0);
        let wi = self.conv.active_workspace;
        let si = self.conv.workspaces[wi].active;
        let agent_ack = self.active_agent_ack();

        let scroll_outer_w = ui.available_width();
        let force_scroll_bottom = self.conv.scroll_to_bottom_once;
        // Suppress auto-stick whenever the user has an active text selection
        // (dragging or just holding one) so streaming growth doesn't yank the
        // viewport away from their selection.
        let user_has_selection = {
            let ctx = ui.ctx();
            let has_selection =
                egui::text_selection::LabelSelectionState::load(ctx).has_selection();
            let primary_down = ctx.input(|i| i.pointer.primary_down());
            let dragged_far =
                ctx.input(
                    |i| match (i.pointer.press_origin(), i.pointer.interact_pos()) {
                        (Some(origin), Some(pos)) => {
                            origin.distance(pos) > SELECTION_SCROLL_MIN_DRAG_PX
                        }
                        _ => false,
                    },
                );
            has_selection || (primary_down && dragged_far)
        };

        let stick_bottom = !user_has_selection
            && (force_scroll_bottom
                || self.active_waiting_response()
                || self.conv.workspaces[wi].sessions[si]
                    .messages
                    .last()
                    .is_some_and(|m| m.role == MsgRole::Assistant && m.streaming));

        let scroll_output = ScrollArea::vertical()
            .max_width(scroll_outer_w)
            .id_salt(self.conv.chat_scroll_id)
            .max_height(transcript_h)
            // Keep full height when the transcript is short so the composer stays bottom-anchored.
            .auto_shrink([false, false])
            .scroll_bar_visibility(ScrollBarVisibility::AlwaysVisible)
            .drag_to_scroll(false)
            .stick_to_bottom(stick_bottom)
            .show(ui, |ui| {
                let viewport_w = ui.max_rect().width();
                ui.set_max_width(viewport_w);
                let col_w = column_center_w.min(CHAT_COLUMN_MAX);
                let pad = ((column_center_w - col_w) * 0.5).max(0.0);

                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    if pad > 0.0 {
                        ui.add_space(pad);
                    }
                    ui.vertical(|ui| {
                        ui.set_width(col_w);
                        self.render_status_banner(ui);
                    });
                    if pad > 0.0 {
                        ui.add_space(pad);
                    }
                });

                ui.add_space(38.0);

                let (sel_scroll, consume) = conversation_selection_scroll_delta(ui);
                if sel_scroll != egui::Vec2::ZERO {
                    // Apply instantly (no egui scroll animation): we feed a small per-frame delta
                    // every frame, so egui's built-in smoothing would re-ease each step and stutter.
                    // Our own time-based velocity already produces smooth motion.
                    ui.scroll_with_delta_animation(
                        sel_scroll,
                        egui::style::ScrollAnimation::none(),
                    );
                    if consume {
                        ui.ctx().input_mut(|i| {
                            i.smooth_scroll_delta = egui::Vec2::ZERO;
                        });
                    }
                    ui.ctx().request_repaint();
                }

                let user_message_tops = ui
                    .horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        if pad > 0.0 {
                            ui.add_space(pad);
                        }
                        let user_message_tops = ui
                            .vertical(|ui| {
                                ui.set_width(col_w);
                                let messages = &self.conv.workspaces[wi].sessions[si].messages;
                                let mut user_message_tops: Vec<(usize, f32)> = Vec::new();
                                if messages.is_empty() {
                                    render_empty_state(ui);
                                } else {
                                    let mut mi = 0;
                                    while mi < messages.len() {
                                        let msg = &messages[mi];
                                        if msg.role == MsgRole::Assistant {
                                            let start = mi;
                                            mi += 1;
                                            while mi < messages.len()
                                                && messages[mi].role == MsgRole::Assistant
                                            {
                                                mi += 1;
                                            }
                                            render_assistant_message_run(
                                                ui,
                                                start,
                                                &messages[start..mi],
                                                agent_ack,
                                            );
                                        } else {
                                            let response = render_message(ui, mi, msg, agent_ack);
                                            if msg.role == MsgRole::User {
                                                user_message_tops.push((mi, response.rect.top()));
                                            }
                                            mi += 1;
                                        }
                                    }
                                }
                                user_message_tops
                            })
                            .inner;
                        if pad > 0.0 {
                            ui.add_space(pad);
                        }
                        user_message_tops
                    })
                    .inner;

                if force_scroll_bottom && !user_has_selection {
                    ui.scroll_to_cursor(Some(Align::BOTTOM));
                }

                user_message_tops
            });

        let sticky_user_text =
            sticky_user_message_index(&scroll_output.inner, scroll_output.inner_rect.top())
                .and_then(|idx| {
                    self.conv.workspaces[wi].sessions[si]
                        .messages
                        .get(idx)
                        .and_then(|msg| (!msg.text.is_empty()).then(|| msg.text.clone()))
                });
        if let Some(text) = sticky_user_text {
            let col_w = column_center_w.min(CHAT_COLUMN_MAX);
            let pad = ((column_center_w - col_w) * 0.5).max(0.0);
            let x = scroll_output.inner_rect.left() + pad;
            let y = scroll_output.inner_rect.top() + 2.0;
            egui::Area::new(ui.id().with("sticky_user_input"))
                .order(egui::Order::Foreground)
                .fixed_pos(egui::pos2(x, y))
                .show(ui.ctx(), |ui| {
                    ui.set_width(col_w);
                    Frame::none()
                        .fill(C_USER_BUBBLE)
                        .stroke(Stroke::new(1.0, C_BORDER_SUBTLE))
                        .rounding(Rounding::same(10.0))
                        .inner_margin(Margin::symmetric(12.0, 7.0))
                        .show(ui, |ui| {
                            ui.set_width(col_w);
                            ui.add(
                                Label::new(
                                    RichText::new(text)
                                        .size(FS_BODY)
                                        .line_height(Some(21.0))
                                        .color(C_TEXT),
                                )
                                .wrap(),
                            );
                        });
                });
        }

        if show_sidebar_button {
            let pos = ui.min_rect().min + egui::vec2(0.0, 0.0);
            egui::Area::new(ui.id().with("show_sidebar_button"))
                .order(egui::Order::Foreground)
                .fixed_pos(pos)
                .show(ui.ctx(), |ui| {
                    if ui
                        .add_sized(
                            [30.0, 28.0],
                            Button::new(RichText::new("☰").size(14.0).color(C_TEXT_MUTED))
                                .fill(C_BG_ELEVATED)
                                .stroke(Stroke::new(1.0, C_BORDER_SUBTLE))
                                .rounding(8.0),
                        )
                        .on_hover_text("Show sidebar")
                        .clicked()
                    {
                        self.conv.sidebar_open = true;
                    }
                });
        }

        if self.conv.scroll_to_bottom_once {
            self.conv.scroll_to_bottom_once = false;
        }
    }
}

/// Same idea as egui’s default click-vs-drag distance (~6px).
const SELECTION_SCROLL_MIN_DRAG_PX: f32 = 6.0;

fn sticky_user_message_index(
    user_message_tops: &[(usize, f32)],
    viewport_top: f32,
) -> Option<usize> {
    user_message_tops
        .iter()
        .rev()
        .find(|(_, top)| *top <= viewport_top)
        .map(|(idx, _)| *idx)
}

/// Vertical edge auto-scroll delta for the transcript while the user is dragging a text
/// selection near the top/bottom of the viewport. Same sign convention as
/// [`eframe::egui::Ui::scroll_with_delta`]: **`y > 0` scrolls the viewport up** (reveals
/// content above), **`y < 0` scrolls down**.
///
/// Returns the delta plus a flag that is `true` while a label selection is being dragged; the
/// caller uses it to zero egui's own `smooth_scroll_delta` so the forwarded wheel/trackpad delta
/// is not applied twice.
pub(crate) fn conversation_selection_scroll_delta(ui: &Ui) -> (egui::Vec2, bool) {
    let ctx = ui.ctx();
    let widget_dragging = ctx.dragged_id().is_some();
    let label_selection_dragging = ctx.input(|i| i.pointer.primary_down())
        && egui::text_selection::LabelSelectionState::load(ctx).has_selection();

    if !widget_dragging && !label_selection_dragging {
        return (egui::Vec2::ZERO, false);
    }

    let mut delta = egui::Vec2::ZERO;
    let wheel_delta = ctx.input(|i| i.smooth_scroll_delta);
    if wheel_delta != egui::Vec2::ZERO {
        delta += wheel_delta;
    }

    if label_selection_dragging {
        // Keep frames flowing for the whole drag so time-based scrolling stays smooth even while
        // the pointer is outside the window, where pointer-move events arrive irregularly.
        ctx.request_repaint();

        // While the button is held the pointer can leave the window (e.g. dragging up past the
        // top). `interact_pos` then returns None on some frames, which made the edge auto-scroll
        // flicker between full speed and zero (the stutter when dragging up). Fall back to the
        // last seen Y so the velocity stays constant across those gaps.
        let last_y_id = egui::Id::new("conv_sel_last_pointer_y");
        let current_y = ctx.input(|i| i.pointer.interact_pos()).map(|p| p.y);
        if let Some(y) = current_y {
            ctx.data_mut(|d| d.insert_temp(last_y_id, y));
        }
        let pointer_y = current_y.or_else(|| ctx.data(|d| d.get_temp::<f32>(last_y_id)));

        if let Some(py) = pointer_y {
            // Use the visible viewport (clip rect) in screen coordinates — `max_rect` is the full
            // scrolled content rect, so its edges fall outside the viewport and would trigger a
            // constant edge-scroll. Only auto-scroll when the pointer reaches a viewport edge, and
            // in the natural direction (positive y reveals earlier content, negative reveals later).
            let rect = ui.clip_rect();
            // Time-based velocity so the speed is independent of frame rate (smooth instead of
            // per-frame jumps). The closer the pointer gets past the edge, the faster it scrolls.
            const EDGE: f32 = 36.0;
            const MAX_SPEED: f32 = 780.0; // points per second at full depth
            let dt = ctx.input(|i| i.stable_dt).clamp(1.0 / 240.0, 1.0 / 30.0);
            let mut velocity = 0.0;
            if py < rect.top() + EDGE {
                let depth = ((rect.top() + EDGE - py) / EDGE).clamp(0.0, 1.0);
                velocity = depth * MAX_SPEED; // positive: reveal earlier content
            } else if py > rect.bottom() - EDGE {
                let depth = ((py - (rect.bottom() - EDGE)) / EDGE).clamp(0.0, 1.0);
                velocity = -depth * MAX_SPEED; // negative: reveal later content
            }
            delta.y += velocity * dt;
        }
    }

    (delta, label_selection_dragging)
}
