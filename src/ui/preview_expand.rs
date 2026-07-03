//! Click-to-expand for truncated panels (tool output, thinking, fenced code). No inner scroll when
//! collapsed — first click expands, second folds.
//!
//! Uses [`Sense::hover`] for the hit target so wheel events still reach the parent transcript
//! [`ScrollArea`] (a full [`Sense::click`] rect was stealing scroll).

use eframe::egui::{CursorIcon, Id, Rect, Rounding, Sense, Stroke, Ui};

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

/// Last `max_lines` lines; prepends a `…` line when the source continues. Kept for the
/// tail-preview tests even though the live thinking panel now uses a capped ScrollArea
/// instead — see `render_thinking_text_panel`.
#[allow(dead_code)]
pub fn truncate_lines_tail_preview(s: &str, max_lines: usize) -> String {
    let all: Vec<&str> = s.lines().collect();
    let truncated = all.len() > max_lines;
    let tail: Vec<&str> = if truncated {
        all[all.len() - max_lines..].to_vec()
    } else {
        all
    };
    let mut preview = tail.join("\n");
    if truncated {
        // Keep the leading ellipsis above the (already joined) tail.
        preview = format!("…\n{}", preview);
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
    clickable_expand_overlay_impl(ui, rect, persist_id, true);
}

/// Like [`clickable_expand_overlay`] but without the hover border — for frameless panels
/// (thinking blocks) where a stroked rectangle would read heavier than the content itself.
pub fn clickable_expand_overlay_quiet(ui: &mut Ui, rect: Rect, persist_id: Id) {
    clickable_expand_overlay_impl(ui, rect, persist_id, false);
}

fn clickable_expand_overlay_impl(ui: &mut Ui, rect: Rect, persist_id: Id, hover_border: bool) {
    let id = persist_id.with("preview_click");
    let response = ui.interact(rect, id, Sense::hover());
    if response.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        if hover_border {
            ui.painter().rect_stroke(
                rect,
                Rounding::same(crate::theme::RADIUS_CHIP),
                Stroke::new(1.0, crate::theme::c_border()),
            );
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_preview_shows_last_lines_with_leading_ellipsis() {
        let s = "1\n2\n3\n4\n5\n6";
        assert_eq!(truncate_lines_tail_preview(s, 3), "…\n4\n5\n6");
    }

    #[test]
    fn tail_preview_under_limit_shows_full_text() {
        let s = "1\n2\n3";
        assert_eq!(truncate_lines_tail_preview(s, 5), "1\n2\n3");
    }

    #[test]
    fn tail_preview_exact_boundary_no_ellipsis() {
        let s = "1\n2\n3";
        assert_eq!(truncate_lines_tail_preview(s, 3), "1\n2\n3");
    }

    #[test]
    fn head_preview_unchanged() {
        // sanity: the existing head truncation still behaves as before.
        let s = "1\n2\n3\n4\n5\n6";
        assert_eq!(truncate_lines_preview(s, 3), "1\n2\n3\n…");
    }
}
