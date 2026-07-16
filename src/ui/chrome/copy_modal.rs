//! Clipboard feedback and shared modal dialogs.

use eframe::egui::{self, Align, Frame, Layout, Margin, Response, RichText, Stroke, Ui};

use super::{flat_button_icon, ghost_button, icon_label_job};
use crate::theme::*;

/// How long the "Copied" confirmation lingers on copy controls.
const COPY_FEEDBACK: std::time::Duration = std::time::Duration::from_millis(1500);

/// Record that the control identified by `id` just copied something.
pub fn mark_copied(ctx: &egui::Context, id: egui::Id) {
    ctx.data_mut(|d| d.insert_temp(id, std::time::Instant::now()));
    ctx.request_repaint_after(COPY_FEEDBACK);
}

/// True while the "Copied" confirmation for `id` should still show.
pub fn copied_recently(ctx: &egui::Context, id: egui::Id) -> bool {
    let Some(at) = ctx.data(|d| d.get_temp::<std::time::Instant>(id)) else {
        return false;
    };
    let elapsed = at.elapsed();
    if elapsed < COPY_FEEDBACK {
        ctx.request_repaint_after(COPY_FEEDBACK - elapsed);
        true
    } else {
        false
    }
}

/// Frameless copy chip with transient success feedback.
pub fn copy_chip(ui: &mut Ui, id: egui::Id, text: &str) {
    let copied = copied_recently(ui.ctx(), id);
    let (icon, label, ink) = if copied {
        (ICON_CHECK, "Copied", crate::theme::c_success())
    } else {
        (ICON_COPY, "Copy", c_text_muted())
    };
    let resp = flat_button_icon(ui, icon, label, FS_TINY, egui::vec2(0.0, 18.0), ink);
    if resp.clicked() {
        ui.ctx().copy_text(text.to_string());
        mark_copied(ui.ctx(), id);
    }
}

/// "Copy message" context menu with inline feedback.
pub fn copy_message_context_menu(response: &Response, id: egui::Id, text: &str) {
    egui::Popup::context_menu(response)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.set_min_width(120.0);
            let label = if copied_recently(ui.ctx(), id) {
                icon_label_job(ICON_CHECK, "Copied", FS_SMALL, crate::theme::c_success())
            } else {
                egui::WidgetText::from(RichText::new("Copy message").size(FS_SMALL))
            };
            if ui.button(label).clicked() {
                ui.ctx().copy_text(text.to_string());
                mark_copied(ui.ctx(), id);
            }
        });
}

/// Outcome of one frame of a modal dialog.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ModalOutcome {
    Open,
    Confirmed,
    Cancelled,
}

/// Dimmed full-window backdrop behind a modal. Clicking it counts as dismissing.
pub fn modal_backdrop(ctx: &egui::Context, id: &str) -> bool {
    let screen = ctx.content_rect();
    let mut clicked = false;
    egui::Area::new(egui::Id::new(id))
        .order(egui::Order::Foreground)
        .fixed_pos(screen.min)
        .interactable(true)
        .show(ctx, |ui| {
            let (rect, response) = ui.allocate_exact_size(screen.size(), egui::Sense::click());
            ui.painter()
                .rect_filled(rect, 0.0, crate::theme::c_modal_backdrop());
            clicked = response.clicked();
        });
    clicked
}

/// Shared confirmation modal for destructive actions.
pub fn confirm_modal(
    ctx: &egui::Context,
    id: &str,
    title: &str,
    body: &str,
    note: Option<&str>,
    confirm_label: &str,
) -> ModalOutcome {
    let mut outcome = ModalOutcome::Open;
    if modal_backdrop(ctx, &format!("{id}_backdrop")) {
        outcome = ModalOutcome::Cancelled;
    }

    egui::Area::new(egui::Id::new(id))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            Frame::new()
                .fill(c_bg_elevated())
                .stroke(Stroke::new(1.0, c_border()))
                .corner_radius(RADIUS_CHIP)
                .inner_margin(Margin::same(16))
                .show(ui, |ui| {
                    ui.set_width(MODAL_W);
                    ui.label(RichText::new(title).size(FS_BODY).color(c_text()).strong());
                    ui.add_space(4.0);
                    ui.label(RichText::new(body).size(FS_SMALL).color(c_text_muted()));
                    if let Some(note) = note {
                        ui.add_space(2.0);
                        ui.label(
                            RichText::new(note)
                                .size(FS_TINY)
                                .color(crate::theme::c_error_fg()),
                        );
                    }
                    ui.add_space(14.0);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ghost_button(ui, confirm_label, true)
                            .on_hover_text("Enter")
                            .clicked()
                        {
                            outcome = ModalOutcome::Confirmed;
                        }
                        if ghost_button(ui, "Cancel", false)
                            .on_hover_text("Esc")
                            .clicked()
                        {
                            outcome = ModalOutcome::Cancelled;
                        }
                    });
                });
        });

    if outcome == ModalOutcome::Open {
        if ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
            outcome = ModalOutcome::Confirmed;
        } else if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            outcome = ModalOutcome::Cancelled;
        }
    }
    outcome
}

/// Shared width for the small centered modal dialogs.
pub const MODAL_W: f32 = 300.0;
