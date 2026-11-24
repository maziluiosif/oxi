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
    copy_chip_lazy(ui, id, || text.to_string());
}

/// [`copy_chip`] that only builds the copied text when clicked.
pub fn copy_chip_lazy(ui: &mut Ui, id: egui::Id, text: impl FnOnce() -> String) {
    let copied = copied_recently(ui.ctx(), id);
    let resp = if copied {
        flat_button_icon(
            ui,
            ICON_CHECK,
            "Copied",
            FS_TINY,
            egui::vec2(0.0, 20.0),
            crate::theme::c_success(),
        )
    } else {
        super::icon_button_core(
            ui,
            ICON_COPY,
            egui::vec2(22.0, 20.0),
            FS_TINY + 0.5,
            false,
            &super::IconButtonLook {
                fill: egui::Color32::TRANSPARENT,
                hover_fill: c_row_hover(),
                stroke: egui::Color32::TRANSPARENT,
                hover_stroke: egui::Color32::TRANSPARENT,
                rounding: egui::CornerRadius::same(RADIUS_ROW),
                glyph: c_text_muted(),
            },
        )
        .on_hover_text("Copy")
    };
    if resp.clicked() {
        ui.ctx().copy_text(text());
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

fn modal_pass_id() -> egui::Id {
    egui::Id::new("oxi_modal_backdrop_pass")
}

/// Whether a [`modal_backdrop`] was drawn this pass or the one before. Widgets drawn under the
/// modals (they run first in a frame) use it to leave Enter / Escape to an open modal.
pub fn modal_shown_recently(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<u64>(modal_pass_id()))
        .is_some_and(|pass| pass + 1 >= ctx.cumulative_pass_nr())
}

/// Dimmed full-window backdrop behind a modal. Clicking it counts as dismissing.
pub fn modal_backdrop(ctx: &egui::Context, id: &str) -> bool {
    let pass = ctx.cumulative_pass_nr();
    ctx.data_mut(|d| d.insert_temp(modal_pass_id(), pass));
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

/// Shared surface frame for centered modal dialogs: elevated fill, crisp border, card rounding,
/// popup shadow, and generous inner padding.
pub fn modal_frame(ui: &Ui) -> Frame {
    Frame::new()
        .fill(c_bg_elevated())
        .stroke(Stroke::new(1.0, c_border()))
        .corner_radius(RADIUS_CARD)
        .shadow(ui.visuals().popup_shadow)
        .inner_margin(Margin::same(16))
}

/// Consistent bold title for modal dialogs.
pub fn modal_title(ui: &mut Ui, title: &str) {
    ui.label(strong_rich(title, FS_BODY, c_text_strong()));
}

/// Shared confirmation modal for destructive actions. `extra` adds content (a checkbox, details)
/// between the text and the buttons. While `confirm_enabled` is false the confirm button is
/// disabled and Enter does nothing.
#[allow(clippy::too_many_arguments)]
pub fn confirm_modal(
    ctx: &egui::Context,
    id: &str,
    title: &str,
    body: &str,
    note: Option<&str>,
    extra: impl FnOnce(&mut Ui),
    confirm_label: &str,
    confirm_enabled: bool,
) -> ModalOutcome {
    let mut outcome = ModalOutcome::Open;
    if modal_backdrop(ctx, &format!("{id}_backdrop")) {
        outcome = ModalOutcome::Cancelled;
    }

    egui::Area::new(egui::Id::new(id))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            modal_frame(ui).show(ui, |ui| {
                ui.set_width(MODAL_W);
                modal_title(ui, title);
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
                extra(ui);
                ui.add_space(14.0);
                // A fixed-height row: a right-to-left layout would otherwise take the height
                // the area had last frame, and a shorter dialog after a taller one (the
                // same area) would keep a gap above its buttons.
                let row = egui::vec2(ui.available_width(), 28.0);
                ui.allocate_ui_with_layout(row, Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add_enabled(
                            confirm_enabled,
                            super::ghost_button_widget(confirm_label, true),
                        )
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
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
        if confirm_enabled && ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
            outcome = ModalOutcome::Confirmed;
        } else if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            outcome = ModalOutcome::Cancelled;
        }
    }
    outcome
}

/// Shared width for the small centered modal dialogs.
pub const MODAL_W: f32 = 300.0;
