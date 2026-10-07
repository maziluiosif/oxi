//! Text selection inside one pane: hit testing, drag/double/triple click, copy.

use super::*;

impl DiffView {
    /// Horizontal extent of a pane's text column, in the rows' coordinates.
    pub(super) fn pane_text_column(
        &self,
        pane: usize,
        left: f32,
        width: f32,
        numbers_w: f32,
    ) -> egui::Rangef {
        let half = (width / 2.0).floor();
        let (pane_left, pane_right, numbers) = match pane {
            0 => (left, left + half, numbers_w),
            1 => (left + half + 1.0, left + width, numbers_w),
            _ => (left, left + width, 2.0 * numbers_w + 8.0),
        };
        egui::Rangef::new(pane_left + numbers + 2.0 * GUTTER_PAD + SIGN_W, pane_right)
    }

    /// The line `pane` shows on `row`, with the side its highlighting comes from.
    pub(super) fn pane_line(&self, row: usize, pane: usize) -> Option<(&DiffFile, &Line, usize)> {
        match (*self.layout.rows.get(row)?, pane) {
            (Row::Split { file, left, .. }, 0) => {
                let f = &self.model.files[file];
                Some((f, f.line(left?)?, 0))
            }
            (Row::Split { file, right, .. }, 1) => {
                let f = &self.model.files[file];
                Some((f, f.line(right?)?, 1))
            }
            (Row::Inline { file, item }, 2) => {
                let f = &self.model.files[file];
                let line = f.line(item)?;
                Some((f, line, usize::from(line.kind != Kind::Removed)))
            }
            _ => None,
        }
    }

    /// `(row, char)` under `pos` in `pane`; above or below the rows snaps to the ends.
    pub(super) fn text_pos_at(
        &self,
        ui: &Ui,
        origin: egui::Pos2,
        width: f32,
        numbers_w: f32,
        pane: usize,
        pos: egui::Pos2,
    ) -> (usize, usize) {
        let rows = self.layout.rows.len();
        let y = pos.y - origin.y;
        if rows == 0 || y < 0.0 {
            return (0, 0);
        }
        if y >= self.layout.total {
            return (rows - 1, usize::MAX);
        }
        let row = self
            .layout
            .ys
            .partition_point(|&top| top <= y)
            .saturating_sub(1);
        let Some((f, line, side)) = self.pane_line(row, pane) else {
            return (row, 0);
        };
        let Some(job) = line_job(f, line, side) else {
            return (row, 0);
        };
        let galley = ui.painter().layout_job(job);
        let text_x = self.pane_text_column(pane, origin.x, width, numbers_w).min - self.scroll_x;
        let col = galley
            .cursor_from_pos(vec2(pos.x - text_x, galley.size().y / 2.0))
            .index;
        (row, col.into())
    }

    pub(super) fn handle_selection(
        &mut self,
        ui: &Ui,
        response: &egui::Response,
        origin: egui::Pos2,
        width: f32,
        numbers_w: f32,
        area: Rect,
    ) {
        let split = self.layout_key.is_some_and(|(split, _, _)| split);
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
        }
        let (pointer, pressed, shift) = ui.input(|i| {
            (
                i.pointer.interact_pos(),
                i.pointer.primary_pressed(),
                i.modifiers.shift,
            )
        });
        if pressed
            && response.is_pointer_button_down_on()
            && let Some(pos) = pointer
        {
            response.request_focus();
            let pane = if !split {
                2
            } else if pos.x < origin.x + (width / 2.0).floor() {
                0
            } else {
                1
            };
            let at = self.text_pos_at(ui, origin, width, numbers_w, pane, pos);
            self.selection = Some(match self.selection {
                Some(sel) if shift && sel.pane == pane => TextSelection { cursor: at, ..sel },
                _ => TextSelection {
                    pane,
                    anchor: at,
                    cursor: at,
                },
            });
        } else if response.dragged()
            && let (Some(pos), Some(sel)) = (pointer, self.selection)
        {
            let at = self.text_pos_at(ui, origin, width, numbers_w, sel.pane, pos);
            self.selection = Some(TextSelection { cursor: at, ..sel });
            // Dragging past the top/bottom edge keeps scrolling, like the editor.
            let dy = if pos.y < area.top() {
                area.top() - pos.y
            } else if pos.y > area.bottom() {
                area.bottom() - pos.y
            } else {
                0.0
            };
            if dy != 0.0 {
                ui.scroll_with_delta_animation(
                    vec2(0.0, dy.clamp(-40.0, 40.0)),
                    egui::style::ScrollAnimation::none(),
                );
                ui.ctx().request_repaint();
            }
        }

        if let Some(sel) = self.selection
            && (response.double_clicked() || response.triple_clicked())
            && let Some((_, line, _)) = self.pane_line(sel.cursor.0, sel.pane)
        {
            let row = sel.cursor.0;
            let (from, to) = if response.triple_clicked() {
                (0, line.text.chars().count())
            } else {
                word_at(&line.text, sel.cursor.1)
            };
            self.selection = Some(TextSelection {
                pane: sel.pane,
                anchor: (row, from),
                cursor: (row, to),
            });
        }

        if response.has_focus() {
            let (copy, select_all) = ui.input_mut(|i| {
                (
                    i.events.iter().any(|e| matches!(e, egui::Event::Copy)),
                    i.consume_key(egui::Modifiers::COMMAND, egui::Key::A),
                )
            });
            if select_all && !self.layout.rows.is_empty() {
                let pane = self
                    .selection
                    .map_or(if split { 1 } else { 2 }, |sel| sel.pane);
                self.selection = Some(TextSelection {
                    pane,
                    anchor: (0, 0),
                    cursor: (self.layout.rows.len() - 1, usize::MAX),
                });
            }
            if copy && let Some(text) = self.selected_text() {
                ui.ctx().copy_text(text);
            }
        }
    }

    /// The selected text, one line per row of the pane that has a line.
    pub(super) fn selected_text(&self) -> Option<String> {
        let sel = self.selection?;
        let (start, end) = sel.sorted();
        if start == end {
            return None;
        }
        let mut lines = Vec::new();
        for row in start.0..=end.0.min(self.layout.rows.len().saturating_sub(1)) {
            let Some((_, line, _)) = self.pane_line(row, sel.pane) else {
                continue;
            };
            let from = if row == start.0 { start.1 } else { 0 };
            let to = if row == end.0 { end.1 } else { usize::MAX };
            lines.push(
                line.text
                    .chars()
                    .skip(from)
                    .take(to.saturating_sub(from))
                    .collect::<String>(),
            );
        }
        Some(lines.join("\n"))
    }
}

/// Char range of the word (or run of spaces/punctuation) at char `col`, for double-click.
pub(super) fn word_at(text: &str, col: usize) -> (usize, usize) {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return (0, 0);
    }
    let class = |c: char| {
        if c.is_alphanumeric() || c == '_' {
            0
        } else if c.is_whitespace() {
            1
        } else {
            2
        }
    };
    let at = col.min(chars.len() - 1);
    let kind = class(chars[at]);
    let from = chars[..at]
        .iter()
        .rposition(|&c| class(c) != kind)
        .map_or(0, |i| i + 1);
    let to = chars[at..]
        .iter()
        .position(|&c| class(c) != kind)
        .map_or(chars.len(), |i| at + i);
    (from, to)
}
