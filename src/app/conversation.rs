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

                let sel_scroll = conversation_selection_scroll_delta(ui);
                if sel_scroll != egui::Vec2::ZERO {
                    ui.scroll_with_delta_animation(
                        sel_scroll,
                        egui::style::ScrollAnimation::none(),
                    );
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

                if force_scroll_bottom && !user_has_selection {
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
/// Size of the top/bottom band where edge auto-scroll kicks in.
const SELECTION_EDGE_BAND_PX: f32 = 24.0;
/// Per-frame cap for the edge auto-scroll delta (points).
const SELECTION_EDGE_MAX_DELTA_PX: f32 = 24.0;

/// Vertical edge auto-scroll delta for the transcript while the user is dragging a text
/// selection near the top/bottom of the viewport. Same sign convention as
/// [`eframe::egui::Ui::scroll_with_delta`]: **`y > 0` scrolls the viewport up** (reveals
/// content above), **`y < 0` scrolls down**.
///
/// We intentionally do **not** forward wheel/trackpad here — egui's [`ScrollArea`] already
/// handles wheel events when hovered, and double-dispatching caused occasional inverted
/// scrolls while text was selected.
pub(crate) fn conversation_selection_scroll_delta(ui: &Ui) -> egui::Vec2 {
    let ctx = ui.ctx();
    let primary_down = ctx.input(|i| i.pointer.primary_down());
    let has_selection = egui::text_selection::LabelSelectionState::load(ctx).has_selection();
    if !primary_down || !has_selection {
        return egui::Vec2::ZERO;
    }

    let dragged_far = ctx.input(
        |i| match (i.pointer.press_origin(), i.pointer.interact_pos()) {
            (Some(origin), Some(pos)) => origin.distance(pos) > SELECTION_SCROLL_MIN_DRAG_PX,
            _ => false,
        },
    );
    if !dragged_far {
        return egui::Vec2::ZERO;
    }

    let Some(pointer) = ctx.input(|i| i.pointer.interact_pos()) else {
        return egui::Vec2::ZERO;
    };

    // `ui.clip_rect()` is the **visible** viewport; `ui.max_rect()` is the whole virtual
    // content rect inside the scroll area (can be thousands of px tall / above the screen),
    // which made the old edge-detection misfire.
    let viewport = ui.clip_rect();
    let edge = SELECTION_EDGE_BAND_PX;
    let max = SELECTION_EDGE_MAX_DELTA_PX;

    let mut delta = egui::Vec2::ZERO;
    if pointer.y < viewport.top() + edge {
        delta.y += (viewport.top() + edge - pointer.y).min(max);
    } else if pointer.y > viewport.bottom() - edge {
        delta.y -= (pointer.y - (viewport.bottom() - edge)).min(max);
    }
    delta
}
