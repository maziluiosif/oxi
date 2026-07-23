//! Cached editor minimap geometry and rendering.

use eframe::egui::{self, Ui};

use crate::theme::*;

use super::editor_body::EditorScrollOutput;

/// One horizontal stroke in the minimap silhouette: a tab-expanded column run on a
/// single line, colored by the syntax section it came from.
struct MinimapSegment {
    line: usize,
    start_col: usize,
    end_col: usize,
    color: egui::Color32,
}

/// Cached minimap silhouette for a document. The strokes and horizontal scale depend
/// only on the buffer and syntax palette, so they are rebuilt on change instead of
/// rescanning the whole file every frame; painting then just culls to the visible strip.
pub(crate) struct MinimapGeometry {
    palette: crate::theme::SyntaxPalette,
    pub(super) line_count: usize,
    max_columns: usize,
    pub(super) indent_columns: Vec<Option<usize>>,
    segments: Vec<MinimapSegment>,
}

const MINIMAP_TAB_WIDTH: usize = 4;
const MINIMAP_MIN_COLUMNS: usize = 60;

fn advance_columns(mut column: usize, text: &str) -> usize {
    for character in text.chars() {
        column += match character {
            '\t' => MINIMAP_TAB_WIDTH - column % MINIMAP_TAB_WIDTH,
            _ => 1,
        };
    }
    column
}

fn build_geometry(
    content: &str,
    highlight_job: &egui::text::LayoutJob,
    palette: crate::theme::SyntaxPalette,
) -> MinimapGeometry {
    // Build line metadata and indentation guides in one pass. Blank lines inherit the shallower
    // indentation of their nearest non-empty neighbours, matching the previous visual behavior.
    let mut indent_columns = Vec::new();
    let mut max_columns = MINIMAP_MIN_COLUMNS;
    for line in content.split('\n') {
        let columns = advance_columns(0, line);
        max_columns = max_columns.max(columns);
        let indentation = advance_columns(
            0,
            line.get(..line.len() - line.trim_start_matches([' ', '\t']).len())
                .unwrap_or_default(),
        );
        indent_columns.push((!line.trim().is_empty()).then_some(indentation));
    }
    let line_count = indent_columns.len();
    let mut indentation_before = Vec::with_capacity(line_count);
    let mut nearest = None;
    for indentation in &indent_columns {
        indentation_before.push(nearest);
        if indentation.is_some() {
            nearest = *indentation;
        }
    }
    let mut nearest = None;
    for index in (0..indent_columns.len()).rev() {
        if indent_columns[index].is_some() {
            nearest = indent_columns[index];
        } else if let (Some(before), Some(after)) = (indentation_before[index], nearest) {
            indent_columns[index] = Some(before.min(after));
        }
    }

    // One stroke per visible run of source, keyed to its logical line. Only section byte
    // ranges and colors are read, so the minimap reuses the editor's cached highlight.
    let mut segments = Vec::new();
    let mut line_index = 0usize;
    let mut column = 0usize;
    for section in &highlight_job.sections {
        let start = section.byte_range.start.0.min(content.len());
        let end = section.byte_range.end.0.min(content.len());
        // Never let stale or malformed byte ranges take down the editor.
        let Some(section_text) = content.get(start..end) else {
            continue;
        };
        for fragment in section_text.split_inclusive('\n') {
            let text = fragment.trim_end_matches('\n');
            let leading_text: String = text
                .chars()
                .take_while(|character| character.is_whitespace())
                .collect();
            let visible_start = advance_columns(column, &leading_text);
            let visible_end = advance_columns(visible_start, text.trim());
            if visible_end > visible_start {
                segments.push(MinimapSegment {
                    line: line_index,
                    start_col: visible_start,
                    end_col: visible_end,
                    color: section.format.color,
                });
            }
            if fragment.ends_with('\n') {
                line_index += 1;
                column = 0;
            } else {
                column = advance_columns(column, text);
            }
        }
    }

    MinimapGeometry {
        palette,
        line_count,
        max_columns,
        indent_columns,
        segments,
    }
}

pub(super) fn ensure_geometry(
    content: &str,
    highlight_job: &egui::text::LayoutJob,
    cache: &mut Option<MinimapGeometry>,
) {
    let palette = active_palette().syntax;
    if cache
        .as_ref()
        .is_none_or(|geometry| geometry.palette != palette)
    {
        *cache = Some(build_geometry(content, highlight_job, palette));
    }
}

pub(super) fn paint(
    ui: &mut Ui,
    size: egui::Vec2,
    scroll: &EditorScrollOutput,
    selected_lines: Option<(usize, usize)>,
    geometry: &MinimapGeometry,
) -> Option<f32> {
    const SCROLLBAR_WIDTH: f32 = 10.0;
    let (whole_rect, response) = ui.allocate_exact_size(
        egui::vec2(size.x + SCROLLBAR_WIDTH, size.y),
        egui::Sense::click_and_drag(),
    );
    let minimap_rect = egui::Rect::from_min_max(
        whole_rect.min,
        egui::pos2(whole_rect.right() - SCROLLBAR_WIDTH, whole_rect.bottom()),
    );
    let scrollbar_rect = egui::Rect::from_min_max(
        egui::pos2(minimap_rect.right(), whole_rect.top()),
        whole_rect.max,
    );
    ui.painter().rect_filled(minimap_rect, 0.0, c_bg_main());
    ui.painter()
        .rect_filled(scrollbar_rect, 0.0, c_bg_elevated());

    // Fixed-scale rows, VS Code style. When the file outgrows the strip, the map scrolls in sync
    // with the editor instead of crushing the complete file into sub-pixel noise.
    const ROW_HEIGHT: f32 = 2.0;
    let natural_height = geometry.line_count as f32 * ROW_HEIGHT;
    let max_y = (scroll.content_size.y - scroll.inner_rect.height()).max(0.0);
    let exact_viewport_fraction =
        (scroll.inner_rect.height() / scroll.content_size.y.max(1.0)).clamp(0.0, 1.0);
    let offset_fraction = if max_y > 0.0 {
        (scroll.state.offset.y / max_y).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let map_offset = offset_fraction * (natural_height - minimap_rect.height()).max(0.0);
    let line_top = |line: usize| minimap_rect.top() + line as f32 * ROW_HEIGHT - map_offset;

    let map_painter = ui.painter().with_clip_rect(minimap_rect);
    let scale = minimap_rect.width() / geometry.max_columns as f32;
    let first_visible_line = ((map_offset - ROW_HEIGHT) / ROW_HEIGHT).floor().max(0.0) as usize;
    let after_visible_line =
        ((map_offset + minimap_rect.height()) / ROW_HEIGHT).ceil() as usize + 1;
    let first_segment = geometry
        .segments
        .partition_point(|segment| segment.line < first_visible_line);
    let after_segment = geometry
        .segments
        .partition_point(|segment| segment.line < after_visible_line);
    for segment in &geometry.segments[first_segment..after_segment] {
        let y = line_top(segment.line);
        let x = minimap_rect.left() + segment.start_col as f32 * scale;
        let width = ((segment.end_col - segment.start_col) as f32 * scale).max(1.0);
        map_painter.hline(
            x..=(x + width).min(minimap_rect.right()),
            y,
            egui::Stroke::new(
                1.35,
                crate::theme::blend_color(segment.color, c_bg_main(), 0.58),
            ),
        );
    }

    if let Some((start_line, end_line)) = selected_lines {
        let top = line_top(start_line).max(minimap_rect.top());
        let bottom = line_top(end_line + 1).min(minimap_rect.bottom());
        if bottom > minimap_rect.top() && top < minimap_rect.bottom() {
            let selection = active_palette().selection_stroke;
            map_painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(minimap_rect.left(), top),
                    egui::pos2(minimap_rect.right(), bottom.max(top + 1.0)),
                ),
                0.0,
                egui::Color32::from_rgba_unmultiplied(
                    selection.r(),
                    selection.g(),
                    selection.b(),
                    38,
                ),
            );
        }
    }

    // Show which part of the file is currently visible without overpowering the code map.
    let content_height = natural_height.min(minimap_rect.height());
    let viewport_height = (natural_height * exact_viewport_fraction).max(8.0);
    let viewport_top =
        minimap_rect.top() + offset_fraction * (content_height - viewport_height).max(0.0);
    let viewport_rect = egui::Rect::from_min_size(
        egui::pos2(minimap_rect.left() + 1.0, viewport_top),
        egui::vec2((minimap_rect.width() - 2.0).max(0.0), viewport_height),
    );
    let accent = c_accent();
    ui.painter().rect_filled(
        viewport_rect,
        1.0,
        egui::Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 18),
    );
    ui.painter().rect_stroke(
        viewport_rect,
        1.0,
        egui::Stroke::new(
            1.0,
            egui::Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 52),
        ),
        egui::StrokeKind::Inside,
    );

    let scrollbar_viewport_fraction = exact_viewport_fraction.clamp(0.06, 1.0);
    let handle_height = scrollbar_rect.height() * scrollbar_viewport_fraction;
    let handle_top =
        scrollbar_rect.top() + offset_fraction * (scrollbar_rect.height() - handle_height);
    ui.painter().rect_filled(
        egui::Rect::from_min_size(
            egui::pos2(scrollbar_rect.left() + 2.0, handle_top),
            egui::vec2(SCROLLBAR_WIDTH - 4.0, handle_height),
        ),
        3.0,
        crate::theme::blend_color(c_text_faint(), c_bg_elevated(), 0.25),
    );
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if response.clicked() || response.dragged() {
        response.interact_pointer_pos().map(|position| {
            ((position.y - whole_rect.top() + map_offset) / natural_height.max(1.0)).clamp(0.0, 1.0)
        })
    } else if response.hovered() && max_y > 0.0 {
        // Forward wheel/trackpad movement because the minimap is outside the editor ScrollArea.
        let wheel_y = ui.input(|input| input.smooth_scroll_delta.y);
        (wheel_y != 0.0).then(|| ((scroll.state.offset.y - wheel_y) / max_y).clamp(0.0, 1.0))
    } else {
        None
    }
}
