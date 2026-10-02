//! Shared continuous selection contour for editor, composer, chat and terminal.

use crate::theme::active_palette;
use eframe::egui;

pub fn paint_selection(painter: &egui::Painter, rects: &[egui::Rect], fill: egui::Color32) {
    painter.add(selection_shape(rects, fill));
}

pub fn selection_shape(rects: &[egui::Rect], fill: egui::Color32) -> egui::Shape {
    let mut shapes = Vec::new();
    const RADIUS: f32 = 2.0;
    let Some(first) = rects.first() else {
        return egui::Shape::Noop;
    };
    let outline = active_palette().selection_stroke;
    let stroke = egui::Stroke::new(
        1.0,
        egui::Color32::from_rgba_unmultiplied(outline.r(), outline.g(), outline.b(), 120),
    );

    for rect in rects {
        shapes.push(egui::Shape::rect_filled(
            *rect,
            egui::CornerRadius::same(RADIUS as u8),
            fill,
        ));
    }
    for rows in rects.windows(2) {
        let upper = rows[0];
        let lower = rows[1];
        let left = upper.left().max(lower.left());
        let right = upper.right().min(lower.right());
        if right > left {
            shapes.push(egui::Shape::rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(left, upper.bottom() - RADIUS),
                    egui::pos2(right, lower.top() + RADIUS),
                ),
                0.0,
                fill,
            ));
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
    shapes.push(egui::Shape::Path(egui::epaint::PathShape {
        points: rounded,
        closed: true,
        fill: egui::Color32::TRANSPARENT,
        stroke: stroke.into(),
    }));
    egui::Shape::Vec(shapes)
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

pub fn galley_selection_rects(
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
