//! Conversation transcript view (scroll area, messages, status banner).

use eframe::egui::scroll_area::ScrollBarVisibility;
use eframe::egui::{
    self, Align, Button, Color32, Frame, Margin, RichText, Rounding, ScrollArea, Stroke, Ui,
};

use crate::model::MsgRole;
use crate::theme::{CHAT_COLUMN_MAX, C_BG_ELEVATED, C_BORDER_SUBTLE, C_TEXT_MUTED, FS_SMALL};
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
        // Suppress auto-stick when the user is dragging to select text so the
        // viewport stays put and the selection works naturally.
        let user_selecting_text = {
            let ctx = ui.ctx();
            let primary_down = ctx.input(|i| i.pointer.primary_down());
            let dragged_far = ctx.input(|i| {
                match (i.pointer.press_origin(), i.pointer.interact_pos()) {
                    (Some(origin), Some(pos)) => {
                        origin.distance(pos) > SELECTION_SCROLL_MIN_DRAG_PX
                    }
                    _ => false,
                }
            });
            primary_down && dragged_far
        };

        let stick_bottom = !user_selecting_text
            && (force_scroll_bottom
                || self.active_waiting_response()
                || self.conv.workspaces[wi].sessions[si]
                    .messages
                    .last()
                    .is_some_and(|m| m.role == MsgRole::Assistant && m.streaming));

        ScrollArea::vertical()
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
                    ui.scroll_with_delta(sel_scroll);
                    if consume {
                        ui.ctx().input_mut(|i| {
                            i.smooth_scroll_delta = egui::Vec2::ZERO;
                        });
                    }
                    ui.ctx().request_repaint();
                }

                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    if pad > 0.0 {
                        ui.add_space(pad);
                    }
                    ui.vertical(|ui| {
                        ui.set_width(col_w);
                        let messages = &self.conv.workspaces[wi].sessions[si].messages;
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
                                    render_message(ui, mi, msg, agent_ack);
                                    mi += 1;
                                }
                            }
                        }
                    });
                    if pad > 0.0 {
                        ui.add_space(pad);
                    }
                });

                if force_scroll_bottom && !user_selecting_text {
                    ui.scroll_to_cursor(Some(Align::BOTTOM));
                }
            });

        if show_sidebar_button {
            let pos = ui.min_rect().min + egui::vec2(0.0, 0.0);
            egui::Area::new(ui.id().with("show_sidebar_button"))
                .order(egui::Order::Foreground)
                .fixed_pos(pos)
                .show(ui.ctx(), |ui| {
                    if ui
                        .add_sized(
                            [28.0, 28.0],
                            Button::new(RichText::new("☰").size(14.0).color(C_TEXT_MUTED))
                                .fill(C_BG_ELEVATED)
                                .stroke(Stroke::new(1.0, C_BORDER_SUBTLE))
                                .rounding(6.0),
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

/// Returns `(scroll_delta, consume_smooth_scroll)`.
///
/// - **Wheel / trackpad**: forwarded while selecting text (button down + selection) or dragging a
///   widget, so the transcript still scrolls — inner labels can steal scroll otherwise.
/// - **Edge auto-scroll** (near top/bottom while dragging): only after the pointer moved past
///   [`SELECTION_SCROLL_MIN_DRAG_PX`], so a click in the top band does not jump the view.
pub(crate) fn conversation_selection_scroll_delta(ui: &Ui) -> (egui::Vec2, bool) {
    let ctx = ui.ctx();
    let widget_dragging = ctx.dragged_id().is_some();

    let label_extend_active = ctx.input(|i| i.pointer.primary_down())
        && egui::text_selection::LabelSelectionState::load(ctx).has_selection();
    let label_drag_past_click_dist = ctx.input(|i| {
        match (i.pointer.press_origin(), i.pointer.interact_pos()) {
            (Some(origin), Some(pos)) => origin.distance(pos) > SELECTION_SCROLL_MIN_DRAG_PX,
            _ => false,
        }
    });
    let label_selection_dragging = label_extend_active && label_drag_past_click_dist;

    if !widget_dragging && !label_extend_active {
        return (egui::Vec2::ZERO, false);
    }

    let mut delta = egui::Vec2::ZERO;
    let wheel_delta = ctx.input(|i| i.smooth_scroll_delta);
    // Trackpads often emit a tiny scroll together with a tap; ignore on the press frame.
    let click_frame_scroll_noise =
        ctx.input(|i| i.pointer.primary_pressed()) && wheel_delta.length() < 8.0;
    if wheel_delta != egui::Vec2::ZERO && !click_frame_scroll_noise {
        // Same sign convention as `ScrollArea` wheel handling and `scroll_with_delta` (see egui
        // `scroll_area.rs`): e.g. `y > 0` scrolls the viewport up. Forward as-is, then consume
        // `smooth_scroll_delta` so the area does not apply it twice.
        delta += wheel_delta;
    }

    // Edge auto-scroll while extending a selection near the top/bottom of the viewport.
    if label_selection_dragging {
        let pointer = ctx.input(|i| i.pointer.interact_pos());
        if let Some(pointer) = pointer {
            let rect = ui.max_rect();
            let edge = 24.0;
            if pointer.y < rect.top() + edge {
                // cursor near top → scroll up → delta.y positive (egui convention)
                delta.y += (rect.top() + edge - pointer.y).min(24.0);
            } else if pointer.y > rect.bottom() - edge {
                // cursor near bottom → scroll down → delta.y negative
                delta.y -= (pointer.y - (rect.bottom() - edge)).min(24.0);
            }
        }
    }

    // Consume wheel when we applied it manually for an active text selection (avoid double scroll).
    let consume = label_extend_active && delta != egui::Vec2::ZERO;
    (delta, consume)
}
