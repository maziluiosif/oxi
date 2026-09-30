//! Cached editor minimap geometry and rendering.

use std::{
    ops::Range,
    sync::{Arc, Weak},
};

use eframe::egui::{self, Ui};

use crate::theme::*;

use super::editor_body::EditorScrollOutput;

struct MinimapSection {
    bytes: Range<usize>,
    color: egui::Color32,
}

/// One horizontal stroke on a visual row, in editor layout coordinates.
struct MinimapSegment {
    row: usize,
    start_x: f32,
    end_x: f32,
    color: egui::Color32,
}

struct MinimapLayout {
    job: Weak<egui::text::LayoutJob>,
    built_at: std::time::Instant,
    row_count: usize,
    width: f32,
    line_rows: Vec<usize>,
    segments: Vec<MinimapSegment>,
}

/// Cached source metadata and silhouette. The silhouette is projected onto the editor's
/// actual wrapped rows only when its layout or syntax colors change.
pub(crate) struct MinimapGeometry {
    palette: crate::theme::SyntaxPalette,
    pub(super) line_count: usize,
    pub(super) indent_columns: Vec<Option<usize>>,
    sections: Vec<MinimapSection>,
    layout: Option<MinimapLayout>,
}

const MINIMAP_TAB_WIDTH: usize = 4;

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
    for line in content.split('\n') {
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

    // Keep just syntax ranges and colors; the editor's galley supplies wrapping and glyph widths.
    let mut sections = Vec::new();
    for section in &highlight_job.sections {
        let start = section.byte_range.start.0.min(content.len());
        let end = section.byte_range.end.0.min(content.len());
        // Never let stale or malformed byte ranges take down the editor.
        if start < end && content.get(start..end).is_some() {
            sections.push(MinimapSection {
                bytes: start..end,
                color: section.format.color,
            });
        }
    }

    MinimapGeometry {
        palette,
        line_count,
        indent_columns,
        sections,
        layout: None,
    }
}

/// How often the silhouette is re-projected while the text keeps changing. The projection walks
/// every glyph in the document, so redoing it per keystroke made typing in large files lag; a
/// 2 px-per-row overview that trails the text by a fraction of a second is not noticeable.
const RELAYOUT_WHILE_EDITING: std::time::Duration = std::time::Duration::from_millis(250);

/// Carry the silhouette of the previous text over an edit, so [`ensure_layout`] can keep painting
/// it until [`RELAYOUT_WHILE_EDITING`] has passed instead of rebuilding it on every keystroke.
pub(super) fn carry_layout_over_edit(
    previous: Option<MinimapGeometry>,
    next: &mut Option<MinimapGeometry>,
) {
    if let (Some(previous), Some(next)) = (previous, next.as_mut())
        && next.layout.is_none()
        && previous.palette == next.palette
    {
        next.layout = previous.layout;
    }
}

pub(super) fn ensure_layout(
    ctx: &egui::Context,
    geometry: &mut MinimapGeometry,
    galley: &Arc<egui::Galley>,
    editing: bool,
) {
    // TextEdit can clone a galley to paint selection visuals without changing its layout job.
    let weak = Arc::downgrade(&galley.job);
    if let Some(layout) = geometry.layout.as_ref() {
        if layout.job.ptr_eq(&weak) {
            return;
        }
        let age = layout.built_at.elapsed();
        if editing && age < RELAYOUT_WHILE_EDITING {
            ctx.request_repaint_after(RELAYOUT_WHILE_EDITING - age);
            return;
        }
    }

    let mut segments: Vec<MinimapSegment> = Vec::new();
    let mut line_rows = vec![0];
    let mut byte = 0;
    let mut section_index = 0;
    for (row_index, placed) in galley.rows.iter().enumerate() {
        for glyph in &placed.row.glyphs {
            while section_index < geometry.sections.len()
                && geometry.sections[section_index].bytes.end <= byte
            {
                section_index += 1;
            }
            if !glyph.chr.is_whitespace()
                && let Some(section) = geometry.sections.get(section_index)
                && section.bytes.contains(&byte)
            {
                let start_x = placed.pos.x + glyph.pos.x;
                let end_x = placed.pos.x + glyph.max_x();
                if let Some(last) = segments.last_mut()
                    && last.row == row_index
                    && last.color == section.color
                {
                    last.end_x = end_x;
                } else {
                    segments.push(MinimapSegment {
                        row: row_index,
                        start_x,
                        end_x,
                        color: section.color,
                    });
                }
            }
            byte += glyph.chr.len_utf8();
        }
        if placed.ends_with_newline {
            byte += 1;
            line_rows.push(row_index + 1);
        }
    }
    let width = if galley.job.wrap.max_width.is_finite() {
        galley.job.wrap.max_width.max(galley.rect.width())
    } else {
        galley.rect.width()
    };
    geometry.layout = Some(MinimapLayout {
        job: weak,
        built_at: std::time::Instant::now(),
        row_count: galley.rows.len(),
        width: width.max(1.0),
        line_rows,
        segments,
    });
}

/// How long the text must stay unchanged before the minimap is re-colored after an edit.
pub(super) const RECOLOR_AFTER_EDIT: std::time::Duration = std::time::Duration::from_millis(500);

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
        None => {
            let job = crate::theme::highlight_editor_code_with_revision(
                &mut document.syntax_state,
                &document.content,
                extension,
                egui::FontId::monospace(FS_SMALL),
                Some(document.content_revision),
                None,
            );
            if document
                .syntax_state
                .as_ref()
                .is_some_and(|state| state.parse_pending())
            {
                // Wait for the background reparse rather than keep provisional colors.
                ctx.request_repaint_after(std::time::Duration::from_millis(16));
                return;
            }
            job.or_else(|| {
                crate::theme::highlight_code_async(
                    &document.content,
                    extension,
                    egui::FontId::monospace(FS_SMALL),
                    ctx,
                )
            })
        }
    };
    if let Some(job) = colored {
        document.minimap_cache = None;
        ensure_geometry(&document.content, &job, &mut document.minimap_cache);
        document.layout_cache.minimap_placeholder = false;
    }
}

/// One foreground-colored section over the whole text. Only section ranges and colors
/// are cached, so the text itself is not copied.
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
    let layout = geometry
        .layout
        .as_ref()
        .expect("minimap layout was prepared");
    let natural_height = layout.row_count as f32 * ROW_HEIGHT;
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
    let scale = minimap_rect.width() / layout.width;
    let first_visible_line = ((map_offset - ROW_HEIGHT) / ROW_HEIGHT).floor().max(0.0) as usize;
    let after_visible_line =
        ((map_offset + minimap_rect.height()) / ROW_HEIGHT).ceil() as usize + 1;
    let first_segment = layout
        .segments
        .partition_point(|segment| segment.row < first_visible_line);
    let after_segment = layout
        .segments
        .partition_point(|segment| segment.row < after_visible_line);
    for segment in &layout.segments[first_segment..after_segment] {
        let y = line_top(segment.row);
        let x = minimap_rect.left() + segment.start_x * scale;
        let width = ((segment.end_x - segment.start_x) * scale).max(1.0);
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
        let start_row = layout
            .line_rows
            .get(start_line)
            .copied()
            .unwrap_or(layout.row_count);
        let end_row = layout
            .line_rows
            .get(end_line + 1)
            .copied()
            .unwrap_or(layout.row_count);
        let top = line_top(start_row).max(minimap_rect.top());
        let bottom = line_top(end_row).min(minimap_rect.bottom());
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

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(ctx: &egui::Context, text: &str, width: f32) -> Arc<egui::Galley> {
        let mut galley = None;
        let _ = ctx.run_ui(Default::default(), |ui| {
            galley = Some(ui.fonts_mut(|fonts| {
                fonts.layout_job(egui::text::LayoutJob::simple(
                    text.to_owned(),
                    egui::FontId::monospace(FS_SMALL),
                    egui::Color32::TRANSPARENT,
                    width,
                ))
            }));
        });
        galley.unwrap()
    }

    fn geometry(text: &str) -> MinimapGeometry {
        build_geometry(text, &plain_job(text), active_palette().syntax)
    }

    #[test]
    fn long_lines_use_the_editors_wrapped_rows() {
        let ctx = egui::Context::default();
        for first_line in ["word ".repeat(60), "x".repeat(300)] {
            let text = format!("{first_line}\ntail");
            let galley = layout(&ctx, &text, 120.0);
            let mut geometry = geometry(&text);
            ensure_layout(&ctx, &mut geometry, &galley, false);
            let minimap = geometry.layout.as_ref().unwrap();
            let tail_row = galley
                .rows
                .iter()
                .position(|row| row.ends_with_newline)
                .unwrap()
                + 1;

            assert!(tail_row > 1);
            assert_eq!(geometry.line_count, 2);
            assert_eq!(minimap.row_count, galley.rows.len());
            assert_eq!(minimap.line_rows, vec![0, tail_row]);
            assert_eq!(minimap.segments.last().unwrap().row, tail_row);
            for (row, placed) in galley.rows.iter().enumerate() {
                if placed
                    .row
                    .glyphs
                    .iter()
                    .any(|glyph| !glyph.chr.is_whitespace())
                {
                    assert!(minimap.segments.iter().any(|segment| segment.row == row));
                }
            }
            assert!(
                minimap
                    .segments
                    .iter()
                    .all(|segment| segment.end_x <= minimap.width)
            );
        }
    }

    #[test]
    fn syntax_colors_follow_utf8_bytes_across_wrapped_and_blank_rows() {
        let ctx = egui::Context::default();
        let first = "\tșir é漢 ".repeat(20);
        let text = format!("{first}\n\n\tfinal\n");
        let mut highlight = egui::text::LayoutJob::default();
        for (bytes, color) in [
            (0..first.len(), egui::Color32::RED),
            (first.len()..text.len(), egui::Color32::BLUE),
        ] {
            highlight.sections.push(egui::text::LayoutSection {
                leading_space: 0.0,
                byte_range: egui::text::ByteIndex(bytes.start)..egui::text::ByteIndex(bytes.end),
                format: egui::text::TextFormat {
                    color,
                    ..Default::default()
                },
            });
        }
        let mut geometry = build_geometry(&text, &highlight, active_palette().syntax);
        let galley = layout(&ctx, &text, 120.0);
        ensure_layout(&ctx, &mut geometry, &galley, false);
        let minimap = geometry.layout.as_ref().unwrap();
        let final_row = minimap.line_rows[2];

        assert_eq!(minimap.line_rows.len(), 4);
        assert_eq!(minimap.line_rows[3], minimap.row_count - 1);
        assert!(
            minimap
                .segments
                .iter()
                .filter(|segment| segment.row < final_row)
                .all(|segment| segment.color == egui::Color32::RED)
        );
        let final_segment = minimap.segments.last().unwrap();
        assert_eq!(final_segment.row, final_row);
        assert_eq!(final_segment.color, egui::Color32::BLUE);
        let final_glyph = &galley.rows[final_row].row.glyphs[1];
        assert_eq!(
            final_segment.start_x,
            galley.rows[final_row].pos.x + final_glyph.pos.x
        );
        assert!(
            !minimap
                .segments
                .iter()
                .any(|segment| segment.row == minimap.line_rows[1])
        );
        assert_eq!(
            geometry.indent_columns,
            vec![Some(4), Some(4), Some(4), None]
        );
    }

    #[test]
    fn resizing_rebuilds_rows_but_selection_only_clones_reuse_them() {
        let ctx = egui::Context::default();
        let text = format!("{}\ntail", "word ".repeat(60));
        let mut geometry = geometry(&text);
        let wide = layout(&ctx, &text, 240.0);
        ensure_layout(&ctx, &mut geometry, &wide, false);
        let wide_count = geometry.layout.as_ref().unwrap().row_count;
        let segments = geometry.layout.as_ref().unwrap().segments.as_ptr();

        let selection_clone = Arc::new((*wide).clone());
        ensure_layout(&ctx, &mut geometry, &selection_clone, false);
        assert_eq!(
            geometry.layout.as_ref().unwrap().segments.as_ptr(),
            segments
        );

        let narrow = layout(&ctx, &text, 80.0);
        ensure_layout(&ctx, &mut geometry, &narrow, false);
        let minimap = geometry.layout.as_ref().unwrap();
        assert!(minimap.row_count > wide_count);
        assert_eq!(minimap.row_count, narrow.rows.len());
        assert_eq!(minimap.width, 80.0);
        assert_eq!(minimap.line_rows[1], minimap.row_count - 1);
    }

    #[test]
    fn edits_keep_the_previous_silhouette_until_the_relayout_interval() {
        let ctx = egui::Context::default();
        let before = "fn a() {}\n";
        let mut previous = geometry(before);
        ensure_layout(&ctx, &mut previous, &layout(&ctx, before, 240.0), false);
        let segments = previous.layout.as_ref().unwrap().segments.as_ptr();

        let after = "fn a() {}\nfn b() {}\n";
        let edited = layout(&ctx, after, 240.0);
        let mut next = Some(geometry(after));
        carry_layout_over_edit(Some(previous), &mut next);
        let mut next = next.unwrap();
        ensure_layout(&ctx, &mut next, &edited, true);
        assert_eq!(next.layout.as_ref().unwrap().segments.as_ptr(), segments);

        // Once typing stops (or the interval passes) the silhouette follows the new text.
        ensure_layout(&ctx, &mut next, &edited, false);
        assert_eq!(next.layout.as_ref().unwrap().row_count, edited.rows.len());
    }

    #[test]
    fn empty_document_has_one_blank_visual_row() {
        let ctx = egui::Context::default();
        let mut geometry = geometry("");
        let galley = layout(&ctx, "", 120.0);
        ensure_layout(&ctx, &mut geometry, &galley, false);
        let minimap = geometry.layout.as_ref().unwrap();

        assert_eq!(geometry.line_count, 1);
        assert_eq!(minimap.row_count, 1);
        assert_eq!(minimap.line_rows, vec![0]);
        assert!(minimap.segments.is_empty());
    }
}
