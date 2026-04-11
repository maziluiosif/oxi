//! Click-to-expand for truncated panels (tool output, thinking, fenced code). No inner scroll when
//! collapsed — first click expands, second folds.
//!
//! Uses [`Sense::hover`] for the hit target so wheel events still reach the parent transcript
//! [`ScrollArea`] (a full [`Sense::click`] rect was stealing scroll).

use eframe::egui::{Color32, CursorIcon, Id, Rect, Rounding, Sense, Stroke, Ui};

/// First `max_lines` lines; adds a final `…` line when the source continues.
pub fn truncate_lines_preview(s: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = s.lines().take(max_lines).collect();
    let truncated = s.lines().nth(max_lines).is_some();
    let mut preview = lines.join("\n");
    if truncated {
        if !preview.is_empty() {
            preview.push('\n');
        }
        preview.push('…');
    }
    preview
}

pub fn expand_persist_id(base: Id) -> Id {
    base.with("full")
}

pub fn is_expanded(ui: &Ui, persist_id: Id) -> bool {
    ui.ctx()
        .data_mut(|d| d.get_persisted::<bool>(persist_id).unwrap_or(false))
}

pub fn toggle_expanded(ui: &Ui, persist_id: Id) {
    let v = is_expanded(ui, persist_id);
    ui.ctx().data_mut(|d| d.insert_persisted(persist_id, !v));
}

pub fn clickable_expand_overlay(ui: &mut Ui, rect: Rect, persist_id: Id) {
    let id = persist_id.with("preview_click");
    let response = ui.interact(rect, id, Sense::hover());
    if response.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter().rect_stroke(
            rect,
            Rounding::same(8.0),
            Stroke::new(1.0, Color32::from_rgb(0x4a, 0x4a, 0x52)),
        );
    }
    if response.hovered()
        && ui.ctx().input(|i| {
            i.pointer.primary_clicked()
                && i.pointer.interact_pos().is_some_and(|p| rect.contains(p))
        })
    {
        toggle_expanded(ui, persist_id);
    }
}
