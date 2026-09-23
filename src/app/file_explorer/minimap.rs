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
            let leading_text = &text[..text.len() - text.trim_start().len()];
            let visible_start = advance_columns(column, leading_text);
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

/// How long the text must stay unchanged before the minimap is re-colored after an edit.
const RECOLOR_AFTER_EDIT: std::time::Duration = std::time::Duration::from_millis(500);

/// Keep the document's minimap in step with its text.
///
/// After an edit the geometry is rebuilt at once from the plain text: line count and indent
/// guides (which the editor paints from it) stay exact at the cost of a linear scan. The syntax
/// colors need a whole-document highlight, far too slow to redo on every keystroke in a large
/// file, so they are filled in once typing pauses. `full_job` is a whole-document colored job
/// when the caller already has one.
pub(super) fn refresh(
    ctx: &egui::Context,
    document: &mut crate::app::state::EditorDocument,
    extension: &str,
    full_job: Option<&egui::text::LayoutJob>,
) {
    if document.minimap_cache.is_none() {
        match full_job {
            Some(job) => {
                ensure_geometry(&document.content, job, &mut document.minimap_cache);
                document.layout_cache.minimap_placeholder = false;
            }
            None => {
                let plain = plain_job(&document.content);
                ensure_geometry(&document.content, &plain, &mut document.minimap_cache);
                document.layout_cache.minimap_placeholder = true;
            }
        }
        return;
    }
    if !document.layout_cache.minimap_placeholder {
        return;
    }
    if let Some(edited_at) = document.layout_cache.edited_at {
        let settled = edited_at.elapsed();
        if settled < RECOLOR_AFTER_EDIT {
            ctx.request_repaint_after(RECOLOR_AFTER_EDIT - settled);
            return;
        }
    }
    let colored = match full_job {
        Some(job) => Some(job.clone()),
        None => crate::theme::highlight_editor_code_with_revision(
            &mut document.syntax_state,
            &document.content,
            extension,
            egui::FontId::monospace(FS_SMALL),
            Some(document.content_revision),
            None,
        )
        .or_else(|| {
            crate::theme::highlight_code_async(
                &document.content,
                extension,
                egui::FontId::monospace(FS_SMALL),
                ctx,
            )
        }),
    };
    if let Some(job) = colored {
        document.minimap_cache = None;
        ensure_geometry(&document.content, &job, &mut document.minimap_cache);
        document.layout_cache.minimap_placeholder = false;
    }
}

/// One foreground-colored section over the whole text. [`build_geometry`] reads only section
/// ranges and colors, so the text itself is not copied.
fn plain_job(content: &str) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    job.sections.push(egui::text::LayoutSection {
        leading_space: 0.0,
        byte_range: egui::text::ByteIndex(0)..egui::text::ByteIndex(content.len()),
        format: egui::text::TextFormat {
            color: active_palette().syntax.foreground,
            ..Default::default()
        },
    });
    job
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
