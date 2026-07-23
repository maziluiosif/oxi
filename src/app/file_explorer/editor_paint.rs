//! Editor cursor, selection, whitespace, and indentation painting helpers.

use eframe::egui::{self, FontId, Ui};

use crate::theme::*;

pub(super) fn byte_range_rects(
    galley: &egui::Galley,
    galley_pos: egui::Pos2,
    content: &str,
    range: &std::ops::Range<usize>,
) -> Vec<egui::Rect> {
    let start = content[..range.start].chars().count();
    let end = start + content[range.clone()].chars().count();
    selection_rects(
        galley,
        galley_pos,
        egui::text::CCursorRange::two(
            egui::text::CCursor::new(start),
            egui::text::CCursor::new(end),
        ),
    )
}

pub(super) fn editor_selection_rects(
    galley: &egui::Galley,
    galley_pos: egui::Pos2,
    clip_rect: egui::Rect,
    range: egui::text::CCursorRange,
) -> Vec<egui::Rect> {
    // The font's baseline leaves more visual space below the glyphs than above them. Moving only
    // the painted selection slightly upward keeps the text optically centered without changing
    // cursor positioning, line height, or hit-testing geometry.
    const VERTICAL_OFFSET: f32 = 1.0;
    const CARET_CLEARANCE: f32 = 1.5;
    let [start, end] = range.sorted_cursors();
    let primary_is_start = range.primary.index == start.index;
    let primary_is_end = range.primary.index == end.index;
    // Keep one row outside each clip edge so clipping never exposes the artificial start/end
    // of this reduced contour. A select-all should cost roughly one viewport to paint.
    let mut rects = selection_rects_in_clip(galley, galley_pos, clip_rect, range)
        .into_iter()
        .map(|rect| rect.translate(egui::vec2(0.0, -VERTICAL_OFFSET)))
        .collect::<Vec<_>>();

    // Leave the active edge to the custom caret instead of repainting a second caret.
    let start_is_painted = galley
        .pos_from_cursor(start)
        .translate(galley_pos.to_vec2())
        .intersects(clip_rect);
    let end_is_painted = galley
        .pos_from_cursor(end)
        .translate(galley_pos.to_vec2())
        .intersects(clip_rect);
    if primary_is_start && start_is_painted {
        if let Some(first) = rects.first_mut() {
            first.min.x = (first.min.x + CARET_CLEARANCE).min(first.max.x);
        }
    } else if primary_is_end
        && end_is_painted
        && let Some(last) = rects.last_mut()
    {
        last.max.x = (last.max.x - CARET_CLEARANCE).max(last.min.x);
    }
    rects
}

fn selection_rects_in_clip(
    galley: &egui::Galley,
    galley_pos: egui::Pos2,
    clip_rect: egui::Rect,
    range: egui::text::CCursorRange,
) -> Vec<egui::Rect> {
    let [start_cursor, end_cursor] = range.sorted_cursors();
    let start = galley.layout_from_cursor(start_cursor);
    let end = galley.layout_from_cursor(end_cursor);
    let local_clip = clip_rect.translate(-galley_pos.to_vec2());

    let first_visible = galley
        .rows
        .partition_point(|row| row.max_y() < local_clip.top());
    let after_visible = galley
        .rows
        .partition_point(|row| row.min_y() <= local_clip.bottom());
    let first_row = start.row.max(first_visible.saturating_sub(1));
    let last_row = end
        .row
        .min(after_visible.min(galley.rows.len().saturating_sub(1)));
    if first_row > last_row {
        return Vec::new();
    }

    let mut rects = Vec::with_capacity(last_row - first_row + 1);
    for row_index in first_row..=last_row {
        let row = &galley.rows[row_index];
        let left = if row_index == start.row {
            row.row.x_offset(start.column)
        } else {
            0.0
        };
        let right = if row_index == end.row {
            row.row.x_offset(end.column)
        } else {
            row.row.size.x
                + if row.ends_with_newline {
                    row.row.height() * 0.5
                } else {
                    0.0
                }
        };
        if right > left {
            rects.push(egui::Rect::from_min_max(
                galley_pos + egui::vec2(row.pos.x + left, row.pos.y),
                galley_pos + egui::vec2(row.pos.x + right, row.pos.y + row.row.height()),
            ));
        }
    }
    rects
}

pub(super) fn caret_logical_line(galley: &egui::Galley, cursor: egui::text::CCursor) -> usize {
    let row = galley.layout_from_cursor(cursor).row;
    galley.rows[..row]
        .iter()
        .filter(|row| row.ends_with_newline)
        .count()
}

pub(super) fn selected_logical_lines(
    galley: &egui::Galley,
    range: egui::text::CCursorRange,
) -> (usize, usize) {
    let [start, end] = range.sorted_cursors();
    let start_row = galley.layout_from_cursor(start).row;
    let end_row = galley.layout_from_cursor(end).row;
    let start_line = galley.rows[..start_row]
        .iter()
        .filter(|row| row.ends_with_newline)
        .count();
    let end_line = start_line
        + galley.rows[start_row..end_row]
            .iter()
            .filter(|row| row.ends_with_newline)
            .count();
    (start_line, end_line)
}

fn selection_rects(
    galley: &egui::Galley,
    galley_pos: egui::Pos2,
    range: egui::text::CCursorRange,
) -> Vec<egui::Rect> {
    let [start, end] = range.sorted_cursors();
    let start = galley.layout_from_cursor(start);
    let end = galley.layout_from_cursor(end);
    let mut rects = Vec::new();
    for row_index in start.row..=end.row {
        let row = &galley.rows[row_index];
        let left = if row_index == start.row {
            row.row.x_offset(start.column)
        } else {
            0.0
        };
        let right = if row_index == end.row {
            row.row.x_offset(end.column)
        } else {
            row.row.size.x
                + if row.ends_with_newline {
                    row.row.height() * 0.5
                } else {
                    0.0
                }
        };
        if right > left {
            rects.push(egui::Rect::from_min_max(
                galley_pos + egui::vec2(row.pos.x + left, row.pos.y),
                galley_pos + egui::vec2(row.pos.x + right, row.pos.y + row.row.height()),
            ));
        }
    }
    rects
}

pub(super) fn paint_selected_whitespace(
    ui: &Ui,
    galley: &egui::Galley,
    galley_pos: egui::Pos2,
    clip_rect: egui::Rect,
    range: egui::text::CCursorRange,
) {
    let painter = ui.painter().with_clip_rect(clip_rect);
    let selected = range.as_sorted_char_range();
    let marker_base =
        crate::theme::blend_color(c_text_muted(), active_palette().selection_stroke, 0.25);
    let marker_color = egui::Color32::from_rgba_unmultiplied(
        marker_base.r(),
        marker_base.g(),
        marker_base.b(),
        105,
    );
    let local_clip = clip_rect.translate(-galley_pos.to_vec2());
    let first_visible = galley
        .rows
        .partition_point(|row| row.max_y() < local_clip.top());
    let Some(first_row) = galley.rows.get(first_visible) else {
        return;
    };
    let mut row_start = galley
        .cursor_from_pos(egui::vec2(
            first_row.rect().left(),
            first_row.rect().center().y,
        ))
        .index
        .0;
    for (row_index, row) in galley.rows.iter().enumerate().skip(first_visible) {
        let row_len = row.row.char_count_excluding_newline().0 + usize::from(row.ends_with_newline);
        if row.min_y() > local_clip.bottom() {
            break;
        }
        for (column, glyph) in row.row.glyphs.iter().enumerate() {
            let char_index = row_start + column;
            if char_index >= selected.end.0 {
                break;
            }
            if char_index < selected.start.0 || (char_index == selected.start.0 && glyph.chr == ' ')
            {
                continue;
            }
            let marker = match glyph.chr {
                ' ' => "·",
                '\t' => "→",
                _ => continue,
            };
            let rect = galley
                .pos_from_layout_cursor(&egui::epaint::text::cursor::LayoutCursor {
                    row: row_index,
                    column: egui::text::CharIndex(column),
                })
                .translate(galley_pos.to_vec2());
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                marker,
                FontId::monospace(FS_TINY),
                marker_color,
            );
        }
        row_start += row_len;
        if row_start >= selected.end.0 {
            break;
        }
    }
}

pub(super) fn paint_caret(
    ui: &Ui,
    galley: &egui::Galley,
    galley_pos: egui::Pos2,
    clip_rect: egui::Rect,
    cursor: egui::text::CCursor,
) {
    let caret = galley
        .pos_from_cursor(cursor)
        .translate(galley_pos.to_vec2())
        .expand(1.5);
    let pixels_per_point = ui.ctx().pixels_per_point();
    let stroke_width = 3.0 / pixels_per_point;
    let x = (caret.center().x * pixels_per_point).round() / pixels_per_point;
    let palette = active_palette();
    let color = if palette == Palette::MARIANA {
        palette.syntax.number
    } else {
        c_text()
    };
    ui.painter().with_clip_rect(clip_rect).line_segment(
        [egui::pos2(x, caret.top()), egui::pos2(x, caret.bottom())],
        egui::Stroke::new(stroke_width, color),
    );
}

pub(super) fn paint_selection(painter: &egui::Painter, rects: &[egui::Rect], fill: egui::Color32) {
    const RADIUS: f32 = 2.0;
    let Some(first) = rects.first() else {
        return;
    };
    let outline = active_palette().selection_stroke;
    let stroke = egui::Stroke::new(
        1.0,
        egui::Color32::from_rgba_unmultiplied(outline.r(), outline.g(), outline.b(), 120),
    );

    for rect in rects {
        painter.rect_filled(*rect, egui::CornerRadius::same(RADIUS as u8), fill);
    }
    for rows in rects.windows(2) {
        let upper = rows[0];
        let lower = rows[1];
        let left = upper.left().max(lower.left());
        let right = upper.right().min(lower.right());
        if right > left {
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(left, upper.bottom() - RADIUS),
                    egui::pos2(right, lower.top() + RADIUS),
                ),
                0.0,
                fill,
            );
        }
    }

    let mut contour = vec![first.left_top(), first.right_top()];
    for rows in rects.windows(2) {
        let upper = rows[0];
        let lower = rows[1];
        let boundary_y = (upper.bottom() + lower.top()) * 0.5;
        contour.push(egui::pos2(upper.right(), boundary_y));
        contour.push(egui::pos2(lower.right(), boundary_y));
    }
    let last = *rects.last().unwrap_or(first);
    contour.extend([last.right_bottom(), last.left_bottom()]);
    for rows in rects.windows(2).rev() {
        let upper = rows[0];
        let lower = rows[1];
        let boundary_y = (upper.bottom() + lower.top()) * 0.5;
        contour.push(egui::pos2(lower.left(), boundary_y));
        contour.push(egui::pos2(upper.left(), boundary_y));
    }
    simplify_orthogonal_contour(&mut contour);
    let rounded = rounded_contour(&contour, RADIUS);
    painter.add(egui::Shape::Path(egui::epaint::PathShape {
        points: rounded,
        closed: true,
        fill: egui::Color32::TRANSPARENT,
        stroke: stroke.into(),
    }));
}

fn simplify_orthogonal_contour(points: &mut Vec<egui::Pos2>) {
    let mut changed = true;
    while changed && points.len() > 2 {
        changed = false;
        for index in 0..points.len() {
            let previous = points[(index + points.len() - 1) % points.len()];
            let current = points[index];
            let next = points[(index + 1) % points.len()];
            let duplicate = current.distance_sq(previous) < 0.01;
            let vertical =
                (previous.x - current.x).abs() < 0.01 && (current.x - next.x).abs() < 0.01;
            let horizontal =
                (previous.y - current.y).abs() < 0.01 && (current.y - next.y).abs() < 0.01;
            if duplicate || vertical || horizontal {
                points.remove(index);
                changed = true;
                break;
            }
        }
    }
}

fn rounded_contour(points: &[egui::Pos2], radius: f32) -> Vec<egui::Pos2> {
    const STEPS: usize = 4;
    let mut rounded = Vec::with_capacity(points.len() * (STEPS + 1));
    for index in 0..points.len() {
        let previous = points[(index + points.len() - 1) % points.len()];
        let corner = points[index];
        let next = points[(index + 1) % points.len()];
        let incoming = corner - previous;
        let outgoing = next - corner;
        let corner_radius = radius
            .min(incoming.length() * 0.5)
            .min(outgoing.length() * 0.5);
        let start = corner - incoming.normalized() * corner_radius;
        let end = corner + outgoing.normalized() * corner_radius;
        rounded.push(start);
        for step in 1..=STEPS {
            let t = step as f32 / STEPS as f32;
            let one_minus_t = 1.0 - t;
            rounded.push(egui::pos2(
                start.x * one_minus_t.powi(2)
                    + corner.x * (2.0 * one_minus_t * t)
                    + end.x * t.powi(2),
                start.y * one_minus_t.powi(2)
                    + corner.y * (2.0 * one_minus_t * t)
                    + end.y * t.powi(2),
            ));
        }
    }
    rounded
}

/// Paint subtle dotted guides at each complete indentation level. Blank lines inherit the
/// shallower indentation of their nearest non-empty neighbours.
pub(super) fn paint_indent_guides(
    ui: &Ui,
    galley: &egui::Galley,
    galley_pos: egui::Pos2,
    clip_rect: egui::Rect,
    indent_columns: &[Option<usize>],
) {
    const TAB_WIDTH: usize = 4;
    const DASH_LENGTH: f32 = 1.5;
    const DASH_GAP: f32 = 2.5;

    let local_clip = clip_rect.translate(-galley_pos.to_vec2());
    let first_visible = galley
        .rows
        .partition_point(|row| row.max_y() < local_clip.top());
    let after_visible = galley
        .rows
        .partition_point(|row| row.min_y() <= local_clip.bottom());
    let logical_line_before = galley.rows[..first_visible]
        .iter()
        .filter(|row| row.ends_with_newline)
        .count();
    let glyph_width = ui.fonts_mut(|fonts| {
        fonts
            .glyph_width(&FontId::monospace(FS_SMALL), ' ')
            .max(FS_SMALL * 0.25)
    });
    let color = crate::theme::blend_color(c_text_faint(), c_bg_main(), 0.38);
    let painter = ui.painter().with_clip_rect(clip_rect);

    let mut logical_line = logical_line_before;
    for row_index in first_visible..after_visible {
        let row = &galley.rows[row_index];
        let starts_line = row_index == 0 || galley.rows[row_index - 1].ends_with_newline;
        if starts_line && let Some(columns) = indent_columns.get(logical_line).copied().flatten() {
            let row_rect = row.rect().translate(galley_pos.to_vec2());
            for column in (TAB_WIDTH..=columns).step_by(TAB_WIDTH) {
                let x = galley_pos.x + column as f32 * glyph_width;
                let mut y = row_rect.top().max(clip_rect.top());
                let bottom = row_rect.bottom().min(clip_rect.bottom());
                while y < bottom {
                    painter.vline(
                        x,
                        y..=(y + DASH_LENGTH).min(bottom),
                        egui::Stroke::new(1.0, color),
                    );
                    y += DASH_LENGTH + DASH_GAP;
                }
            }
        }
        if row.ends_with_newline {
            logical_line += 1;
        }
    }
}
