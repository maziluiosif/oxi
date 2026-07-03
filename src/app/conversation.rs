//! Conversation transcript view (scroll area, messages, status banner).

use eframe::egui::scroll_area::ScrollBarVisibility;
use eframe::egui::{
    self, Align, Button, FontId, Frame, Label, Margin, RichText, Rounding, ScrollArea, Stroke, Ui,
};

use crate::agent::ApprovalDecision;
use crate::model::MsgRole;
use crate::theme::*;
use crate::ui::messages::{render_assistant_message_run, render_message};

use super::{OxiApp, PendingApproval};

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
            crate::ui::chrome::alert_banner(ui, &format!("Connection: {e}"), true);
            ui.add_space(4.0);
        }
        if let Some(e) = active_stream_error {
            crate::ui::chrome::alert_banner(ui, &format!("Agent: {e}"), false);
            ui.add_space(4.0);
        }
    }

    /// Approve/deny prompt for a mutating tool call (`bash` / `write` / `edit`).
    /// Rendered at the bottom of the transcript (above the floating composer) so it stays in
    /// view while the run is paused — `stick_to_bottom` keeps the tail visible during a run.
    fn render_approval_card(&mut self, ui: &mut Ui, pa: PendingApproval) {
        Frame::none()
            .fill(crate::theme::c_info_bg())
            .stroke(Stroke::new(1.0, c_accent()))
            .rounding(Rounding::same(6.0))
            .inner_margin(Margin::symmetric(10.0, 8.0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(
                    RichText::new(format!("Approve `{}`?", pa.name))
                        .size(FS_SMALL)
                        .color(c_text())
                        .strong(),
                );
                if !pa.summary.is_empty() {
                    ui.add_space(2.0);
                    ui.label(
                        RichText::new(&pa.summary)
                            .size(FS_TINY)
                            .color(c_text_muted())
                            .monospace(),
                    );
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if crate::ui::chrome::primary_button(ui, "Approve").clicked() {
                        self.respond_to_approval(ApprovalDecision::Approve);
                    }
                    if crate::ui::chrome::ghost_button(ui, "Approve rest", false)
                        .on_hover_text("Run this and auto-approve the rest of this turn")
                        .clicked()
                    {
                        self.respond_to_approval(ApprovalDecision::ApproveRest);
                    }
                    if crate::ui::chrome::ghost_button(ui, "Deny", true).clicked() {
                        self.respond_to_approval(ApprovalDecision::Deny);
                    }
                });
            });
        ui.add_space(4.0);
    }

    pub(crate) fn render_chat_header(&mut self, ui: &mut Ui, column_center_w: f32) {
        let col_w = column_center_w.min(CHAT_COLUMN_MAX);
        let pad = ((column_center_w - col_w) * 0.5).max(0.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            if pad > 0.0 {
                ui.add_space(pad);
            }
            ui.allocate_ui_with_layout(
                egui::vec2(col_w, 38.0),
                egui::Layout::right_to_left(Align::Center),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;

                    // Right cluster — sized to its content so it never overflows the title.
                    ui.allocate_ui_with_layout(
                        egui::vec2(ui.available_width(), 28.0),
                        egui::Layout::right_to_left(Align::Center),
                        |ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            if ui
                                .add_sized(
                                    [120.0, 28.0],
                                    Button::new(crate::ui::chrome::icon_label_job(
                                        ICON_PLUS,
                                        "New",
                                        FS_SMALL,
                                        crate::theme::c_on_accent(),
                                    ))
                                    .fill(c_accent())
                                    .stroke(Stroke::NONE)
                                    .rounding(8.0),
                                )
                                .on_hover_cursor(egui::CursorIcon::PointingHand)
                                .on_hover_text("Start a new chat in this workspace")
                                .clicked()
                            {
                                self.new_chat();
                            }
                            let term_on = self.conv.terminal_open;
                            if crate::ui::chrome::icon_button_framed(
                                ui,
                                ICON_TERMINAL,
                                egui::vec2(34.0, 28.0),
                                term_on,
                            )
                            .on_hover_text("Toggle terminal panel")
                            .clicked()
                            {
                                self.toggle_terminal();
                            }
                            let git_on = self.conv.git_open;
                            if crate::ui::chrome::icon_button_framed(
                                ui,
                                ICON_GIT,
                                egui::vec2(34.0, 28.0),
                                git_on,
                            )
                            .on_hover_text("Toggle source-control (git) panel")
                            .clicked()
                            {
                                self.toggle_git_panel();
                            }
                            self.render_header_status_chip(ui);
                        },
                    );

                    // Left group gets whatever the right cluster didn’t take.
                    let left_w = ui.available_width();
                    ui.allocate_ui_with_layout(
                        egui::vec2(left_w, 38.0),
                        egui::Layout::left_to_right(Align::Center),
                        |ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            if !self.conv.sidebar_open
                                && crate::ui::chrome::icon_button_framed(
                                    ui,
                                    ICON_MENU,
                                    egui::vec2(30.0, 28.0),
                                    false,
                                )
                                .on_hover_text("Show sidebar")
                                .clicked()
                            {
                                self.conv.sidebar_open = true;
                            }

                            let workspace =
                                workspace_sidebar_label(&self.active_workspace().root_path);
                            let session_title =
                                sidebar_session_title_display(&self.active_session().title);
                            ui.vertical(|ui| {
                                ui.set_width(ui.available_width());
                                ui.add(
                                    Label::new(
                                        RichText::new(session_title)
                                            .size(FS_SMALL)
                                            .color(c_text())
                                            .strong(),
                                    )
                                    .truncate(),
                                );
                                let profile = self
                                    .conv
                                    .settings
                                    .active_profile()
                                    .map(|p| p.subtitle())
                                    .unwrap_or_else(|| "No active profile".to_string());
                                ui.add(
                                    Label::new(
                                        RichText::new(format!("{workspace} · {profile}"))
                                            .size(FS_TINY)
                                            .color(c_text_muted()),
                                    )
                                    .truncate(),
                                );
                            });
                        },
                    );
                },
            );
            if pad > 0.0 {
                ui.add_space(pad);
            }
        });
    }

    fn render_header_status_chip(&self, ui: &mut Ui) {
        let (label, dot, hover) = if let Some(err) = self.active_stream_error() {
            (
                "Error".to_string(),
                crate::theme::c_danger(),
                err.to_string(),
            )
        } else if self.active_waiting_response() {
            let elapsed = self
                .active_run_state()
                .and_then(|s| s.stream_started_at)
                .map(|t| format!(" · {}", format_stream_elapsed(t.elapsed())))
                .unwrap_or_default();
            (
                format!("Running{elapsed}"),
                c_accent(),
                "Agent is working".to_string(),
            )
        } else {
            (
                "Ready".to_string(),
                crate::theme::c_success(),
                "Ready to send".to_string(),
            )
        };

        // Hand-painted at the same 28px height as the neighboring header buttons —
        // a Frame sizes itself to the text and sits visually off-line next to them.
        const H: f32 = 28.0;
        const PAD_X: f32 = 10.0;
        const GAP: f32 = 5.0;
        let text_galley =
            ui.painter()
                .layout_no_wrap(label, FontId::proportional(FS_TINY), c_text_muted());
        let dot_galley =
            ui.painter()
                .layout_no_wrap("●".to_string(), FontId::proportional(8.0), dot);
        let w = PAD_X * 2.0 + text_galley.rect.width() + GAP + dot_galley.rect.width();
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, H), egui::Sense::hover());
        ui.painter().rect(
            rect,
            Rounding::same(999.0),
            c_bg_input(),
            Stroke::new(1.0, c_border_subtle()),
        );
        let text_pos = egui::pos2(
            rect.left() + PAD_X,
            rect.center().y - text_galley.rect.height() * 0.5,
        );
        let dot_pos = egui::pos2(
            rect.left() + PAD_X + text_galley.rect.width() + GAP,
            rect.center().y - dot_galley.rect.height() * 0.5,
        );
        ui.painter().galley(text_pos, text_galley, c_text_muted());
        ui.painter().galley(dot_pos, dot_galley, dot);
        resp.on_hover_text(hover);
    }

    pub(crate) fn render_empty_state(&mut self, ui: &mut Ui) {
        ui.add_space(44.0);
        ui.set_max_width(520.0);
        ui.vertical(|ui| {
            ui.label(
                RichText::new("What should oxi help with?")
                    .size(FS_H1)
                    .color(c_text())
                    .strong(),
            );
            ui.add_space(5.0);
            ui.label(
                RichText::new(
                    "Start with a workspace task, inspect code, or configure your provider.",
                )
                .size(FS_BODY)
                .color(c_text_muted()),
            );
            ui.add_space(18.0);

            ui.horizontal_wrapped(|ui| {
                if crate::ui::chrome::ghost_button_icon(
                    ui,
                    ICON_FOLDER_PLUS,
                    "Add workspace",
                    false,
                )
                .clicked()
                {
                    self.open_workspace_folder();
                }
                if crate::ui::chrome::ghost_button_icon(ui, ICON_SETTINGS, "Open settings", false)
                    .clicked()
                {
                    self.conv.settings_open = true;
                }
            });

            ui.add_space(20.0);
            ui.label(
                RichText::new("Try one of these")
                    .size(FS_TINY)
                    .color(c_text_faint())
                    .strong(),
            );
            ui.add_space(6.0);
            let prompts = [
                "Analyze this repo and suggest the highest-impact improvements",
                "Find TODOs and risky code paths in this workspace",
                "Explain how this project is structured",
                "Run the tests and fix the first failing issue",
            ];
            for prompt in prompts {
                let response = Frame::none()
                    .fill(c_bg_input())
                    .stroke(Stroke::new(1.0, c_border_subtle()))
                    .rounding(Rounding::same(9.0))
                    .inner_margin(Margin::symmetric(12.0, 8.0))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.add_space(1.0);
                            ui.label(
                                RichText::new(ICON_EXTERNAL)
                                    .font(FontId::new(FS_SMALL, icon_font()))
                                    .color(c_accent()),
                            );
                            ui.add_space(5.0);
                            ui.label(RichText::new(prompt).size(FS_SMALL).color(c_text()));
                        });
                    })
                    .response
                    .interact(egui::Sense::click());
                if response.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    ui.painter().rect_stroke(
                        response.rect,
                        Rounding::same(9.0),
                        Stroke::new(1.0, crate::theme::c_pill_selected_border()),
                    );
                }
                if response.clicked() {
                    self.conv.input = prompt.to_string();
                }
                ui.add_space(5.0);
            }
        });
    }

    pub(crate) fn render_conversation(
        &mut self,
        ui: &mut Ui,
        column_center_w: f32,
        scroll_budget: f32,
        bottom_overlay_h: f32,
    ) {
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

                ui.add_space(16.0);

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

                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    if pad > 0.0 {
                        ui.add_space(pad);
                    }
                    ui.vertical(|ui| {
                        ui.set_width(col_w);
                        let messages = &self.conv.workspaces[wi].sessions[si].messages;
                        if messages.is_empty() {
                            self.render_empty_state(ui);
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

                // Pending approval prompt: shown at the tail of the transcript so it is visible
                // just above the floating composer while the agent run is blocked on the user.
                if let Some(pa) = self.active_pending_approval() {
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        if pad > 0.0 {
                            ui.add_space(pad);
                        }
                        ui.vertical(|ui| {
                            ui.set_width(col_w);
                            self.render_approval_card(ui, pa);
                        });
                        if pad > 0.0 {
                            ui.add_space(pad);
                        }
                    });
                }

                // The composer is floating over the transcript. Add scrollable tail padding so
                // the last messages can move above/behind it instead of being permanently hidden
                // at the bottom edge.
                ui.add_space(bottom_overlay_h.max(0.0));

                if force_scroll_bottom && !user_has_selection {
                    ui.scroll_to_cursor(Some(Align::BOTTOM));
                }
            });

        if self.conv.scroll_to_bottom_once {
            self.conv.scroll_to_bottom_once = false;
        }
    }
}

/// Same idea as egui’s default click-vs-drag distance (~6px).
const SELECTION_SCROLL_MIN_DRAG_PX: f32 = 6.0;

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
