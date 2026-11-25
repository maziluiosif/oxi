//! A read-only code pane of the diff editor: the base side of the split view, and both sides of
//! a staged diff. It is a `TextEdit` over immutable text, so selection, copy and keyboard
//! navigation behave like the editor's; its vertical scroll is driven by the diff.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use eframe::egui::scroll_area::{ScrollBarVisibility, ScrollSource};
use eframe::egui::text::LayoutJob;
use eframe::egui::{self, Galley, Margin, Rect, TextEdit, Ui, UiBuilder, pos2, vec2};

use crate::theme::*;

use crate::ui::text_selection::selection_shape;

use super::super::editor_paint::editor_selection_rects;
use super::super::line_layout;
use super::decor::{
    Alignment, DiffDecor, LineGeometry, PaneRows, Side, Underlay, ZoneText, apply_gaps,
};

/// The pane's laid-out text: measured (wrapped, without gaps) until the text, its colors or the
/// wrap width change, and with its gaps until those change too.
#[derive(Default)]
pub(crate) struct PaneCache {
    measure_key: u64,
    /// The text as laid out for the pane, before gaps.
    laid: Option<Arc<Galley>>,
    /// Which rows each line of `laid` takes.
    pub(super) lines: LineGeometry,
    /// Bumped whenever `lines` changes.
    pub(super) lines_generation: u64,
    /// Digits of the last line number, for the gutter, and the text version they count.
    digits: usize,
    digits_version: Option<u64>,
    /// What `laid` is wrapped at (infinite when it doesn't wrap).
    pub(super) wrap_width: f32,
    key: u64,
    galley: Option<Arc<Galley>>,
    /// `galley` with transparent glyphs, for `TextEdit`: egui recolors selected glyphs with a
    /// single color, so the colored text is painted on top of it instead.
    geometry: Option<Arc<Galley>>,
    scroll_x: f32,
}

pub(super) struct PaneInput<'a> {
    pub id: egui::Id,
    pub text: &'a str,
    /// Changes whenever `text` does (the diff's text version).
    pub text_version: u64,
    /// The whole text's syntax colors, once highlighted.
    pub job: Option<&'a LayoutJob>,
    /// Soft-wrap the text to the pane's width.
    pub wrap: bool,
    pub side: Side,
    pub inline: bool,
    pub decor: &'a DiffDecor,
    /// The space that lines this pane up with the other side (or holds the removed lines).
    pub alignment: &'a Alignment,
    /// Changes whenever `alignment` does.
    pub alignment_generation: u64,
    pub zones: Option<ZoneText<'a>>,
    pub scroll_y: f32,
}

pub(super) struct PaneOutput {
    /// Vertical wheel scrolling over the pane, for the diff to apply to every pane.
    pub wheel_y: f32,
    /// Screen spans of the changes, and the text column they were drawn in.
    pub spans: Vec<(f32, f32)>,
    pub clip: Rect,
    pub content_h: f32,
    pub viewport_h: f32,
}

/// `TextEdit`'s margin around the pane's text.
const TEXT_MARGIN: i8 = 8;
const LEFT_PAD: f32 = 10.0;
const RIGHT_PAD: f32 = 12.0;

/// The line-number gutter and the text column of a pane in `rect`.
fn columns(ui: &Ui, rect: Rect, digits: usize) -> (Rect, Rect) {
    let digit_w = ui.fonts_mut(|fonts| {
        fonts
            .glyph_width(&crate::theme::editor_font(), '0')
            .max(crate::theme::editor_font_size() * 0.5)
    });
    let gutter = Rect::from_min_size(
        rect.min,
        vec2(
            digits.max(2) as f32 * digit_w + LEFT_PAD + RIGHT_PAD + 2.0,
            rect.height(),
        ),
    );
    let text_rect = Rect::from_min_max(pos2(gutter.right() + 1.0, rect.top()), rect.max);
    (gutter, text_rect)
}

/// Lay out the text of a pane in `rect` (soft-wrapped to its text column with `wrap`, the way
/// the editor wraps) and measure its lines into `cache`.
pub(super) fn measure(
    ui: &mut Ui,
    rect: Rect,
    text: &str,
    text_version: u64,
    job: Option<&LayoutJob>,
    wrap: bool,
    cache: &mut PaneCache,
) {
    if cache.digits_version != Some(text_version) {
        cache.digits = (memchr::memchr_iter(b'\n', text.as_bytes()).count() + 1)
            .to_string()
            .len();
        cache.digits_version = Some(text_version);
    }
    let (_, text_rect) = columns(ui, rect, cache.digits);
    let wrap_width = if wrap {
        (text_rect.width() - 2.0 * f32::from(TEXT_MARGIN))
            .max(40.0)
            .round()
    } else {
        f32::INFINITY
    };
    let key = {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (
            text_version,
            job.is_some(),
            wrap_width.to_bits(),
            crate::theme::palette_generation(),
            crate::theme::fonts_generation(),
            crate::theme::editor_wrap_indent(),
            ui.ctx().pixels_per_point().to_bits(),
        )
            .hash(&mut hasher);
        hasher.finish() | 1
    };
    if cache.measure_key == key && cache.laid.is_some() {
        return;
    }
    cache.wrap_width = wrap_width;
    let font = crate::theme::editor_font();
    let row_h = ui.fonts_mut(|fonts| fonts.row_height(&font));
    let mut job = job
        .cloned()
        .unwrap_or_else(|| LayoutJob::simple(text.to_owned(), font, c_text(), f32::INFINITY));
    job.wrap.max_width = wrap_width;
    let pixels_per_point = ui.ctx().pixels_per_point();
    let laid = ui.fonts_mut(|fonts| {
        if wrap_width.is_finite() {
            // Paragraph by paragraph with the editor's hanging indent, so the base wraps like
            // the editable side next to it.
            line_layout::layout_paragraphs(
                job,
                pixels_per_point,
                crate::theme::editor_wrap_indent(),
                |_| false,
                |job| fonts.layout_job(job),
            )
        } else {
            fonts.layout_job(job)
        }
    });
    let lines = LineGeometry::measure(&laid, row_h);
    if lines != cache.lines {
        cache.lines = lines;
        cache.lines_generation += 1;
    }
    cache.laid = Some(laid);
    cache.measure_key = key;
}

pub(super) fn readonly_pane(
    ui: &mut Ui,
    rect: Rect,
    input: PaneInput<'_>,
    cache: &mut PaneCache,
) -> PaneOutput {
    let font = crate::theme::editor_font();
    measure(
        ui,
        rect,
        input.text,
        input.text_version,
        input.job,
        input.wrap,
        cache,
    );
    // Where egui really placed the rows, which the alignment was measured with too.
    let row_h = cache.lines.row_h;
    let key = {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (cache.measure_key, input.alignment_generation, input.side).hash(&mut hasher);
        hasher.finish()
    };
    if cache.key != key || cache.galley.is_none() {
        let laid = cache.laid.as_ref().expect("measured above");
        let galley = apply_gaps(laid, 0, input.alignment.gaps(input.side));
        cache.geometry = Some(transparent_glyphs(&galley));
        cache.galley = Some(galley);
        cache.key = key;
    }
    let galley = cache.galley.clone().expect("laid out above");
    let geometry = cache.geometry.clone().expect("laid out above");

    let (gutter, text_rect) = columns(ui, rect, cache.digits);
    ui.painter().vline(
        gutter.right(),
        gutter.y_range(),
        egui::Stroke::new(1.0, c_border_subtle()),
    );

    let hovered = ui.rect_contains_pointer(rect);
    let wheel = if hovered {
        ui.input(|i| i.smooth_scroll_delta)
    } else {
        egui::Vec2::ZERO
    };
    // Wrapped lines fit the pane, so it never scrolls sideways (like the editor beside it).
    let max_x = if cache.wrap_width.is_finite() {
        0.0
    } else {
        (galley.rect.width() + 24.0 - text_rect.width()).max(0.0)
    };
    cache.scroll_x = (cache.scroll_x - wheel.x).clamp(0.0, max_x);

    let mut child = ui.new_child(
        UiBuilder::new()
            .id_salt(input.id)
            .max_rect(text_rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    child.set_clip_rect(text_rect.intersect(ui.clip_rect()));
    let mut text_ref = input.text;
    let output = egui::ScrollArea::both()
        .id_salt(input.id.with("scroll"))
        .scroll_source(ScrollSource::NONE)
        .scroll_bar_visibility(ScrollBarVisibility::AlwaysHidden)
        .auto_shrink([false, false])
        .scroll_offset(vec2(cache.scroll_x, input.scroll_y))
        .show(&mut child, |ui| {
            // The underlay goes under the text TextEdit paints: its slot is taken first and
            // filled once TextEdit has placed the galley.
            let clip = ui.clip_rect();
            let underlay = ui.painter().add(egui::Shape::Noop);
            let (origin, selection) = ui
                .scope(|ui| {
                    // egui's selection would recolor the selected glyphs; it is painted in the
                    // underlay instead, like the editor's.
                    ui.visuals_mut().selection.bg_fill = egui::Color32::TRANSPARENT;
                    ui.visuals_mut().selection.stroke = egui::Stroke::NONE;
                    let laid_out = Arc::clone(&geometry);
                    let output = TextEdit::multiline(&mut text_ref)
                        .id(input.id.with("text"))
                        .font(font.clone())
                        .code_editor()
                        .frame(egui::Frame::NONE)
                        .background_color(egui::Color32::TRANSPARENT)
                        .desired_width(f32::INFINITY)
                        .min_size(text_rect.size())
                        .margin(Margin::same(TEXT_MARGIN))
                        .layouter(&mut |_, _, _| Arc::clone(&laid_out))
                        .show(ui);
                    let selection = output.cursor_range.filter(|range| !range.is_empty());
                    ui.painter()
                        .galley(output.galley_pos, Arc::clone(&galley), c_text());
                    (output.galley_pos, selection)
                })
                .inner;
            let rows = PaneRows {
                galley: &galley,
                origin,
                row_h,
                lines: &cache.lines,
                alignment: input.alignment,
            };
            let mut shapes = Underlay {
                decor: input.decor,
                side: input.side,
                inline: input.inline,
                clip,
                zones: input.zones,
            }
            .shapes(ui.painter(), &rows, &font);
            if let Some(selection) = selection {
                let rects = editor_selection_rects(&galley, origin, clip, selection);
                shapes.push(selection_shape(&rects, editor_selection_fill()));
            }
            ui.painter().set(underlay, egui::Shape::Vec(shapes));
            let spans = input
                .decor
                .changes
                .iter()
                .enumerate()
                .map(|(index, change)| rows.span(index, change, input.side, input.inline))
                .collect::<Vec<_>>();
            (spans, clip, origin)
        });
    let (spans, clip, origin) = output.inner;

    // Line numbers, fixed while the text scrolls sideways.
    let gutter_clip = gutter.intersect(ui.clip_rect());
    let painter = ui.painter().with_clip_rect(gutter_clip);
    let first = galley
        .rows
        .partition_point(|row| origin.y + row.pos.y + row_h < gutter.top());
    let changed_color = match input.side {
        Side::Old => c_diff_del_fg(),
        Side::New => c_diff_add_fg(),
    }
    .gamma_multiply(0.75);
    // At each line's first row: its wrapped rows go unnumbered.
    for line in cache.lines.line_of_row(first).. {
        let Some(row) = galley.rows.get(cache.lines.first_row(line)) else {
            break;
        };
        let y = origin.y + row.pos.y + row_h * 0.5;
        if y - row_h > gutter.bottom() {
            break;
        }
        painter.text(
            pos2(gutter.right() - RIGHT_PAD, y),
            egui::Align2::RIGHT_CENTER,
            line + 1,
            font.clone(),
            if input.decor.is_changed(input.side, line) {
                changed_color
            } else {
                c_text_faint()
            },
        );
    }
    if input.inline {
        let rows = PaneRows {
            galley: &galley,
            origin,
            row_h,
            lines: &cache.lines,
            alignment: input.alignment,
        };
        for (number, y) in input.decor.zone_numbers(&rows, gutter.y_range()) {
            painter.text(
                pos2(gutter.right() - RIGHT_PAD, y),
                egui::Align2::RIGHT_CENTER,
                number,
                font.clone(),
                c_diff_del_fg().gamma_multiply(0.75),
            );
        }
    }

    PaneOutput {
        wheel_y: wheel.y,
        spans,
        clip,
        content_h: output.content_size.y,
        viewport_h: output.inner_rect.height(),
    }
}

/// A copy of `galley` whose glyphs are transparent (backgrounds keep their colors).
fn transparent_glyphs(galley: &Galley) -> Arc<Galley> {
    let mut out = galley.clone();
    for placed in &mut out.rows {
        let row = Arc::make_mut(&mut placed.row);
        let range = row.visuals.glyph_vertex_range.clone();
        for vertex in &mut row.visuals.mesh.vertices[range] {
            vertex.color = egui::Color32::TRANSPARENT;
        }
    }
    Arc::new(out)
}
