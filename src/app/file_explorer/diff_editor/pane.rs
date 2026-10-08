//! A read-only code pane of the diff editor: the base side of the split view, and both sides of
//! a staged diff. It is a `TextEdit` over immutable text, so selection, copy and keyboard
//! navigation behave like the editor's; its vertical scroll is driven by the diff.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use eframe::egui::scroll_area::{ScrollBarVisibility, ScrollSource};
use eframe::egui::text::LayoutJob;
use eframe::egui::{self, FontId, Galley, Margin, Rect, TextEdit, Ui, UiBuilder, pos2, vec2};

use crate::theme::*;

use crate::ui::text_selection::selection_shape;

use super::super::editor_paint::editor_selection_rects;
use super::decor::{DiffDecor, PaneRows, Side, Underlay, ZoneText, apply_gaps};

/// The pane's laid-out text, kept until the text, its colors or the gaps change.
#[derive(Default)]
pub(crate) struct PaneCache {
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
    pub side: Side,
    pub inline: bool,
    pub decor: &'a DiffDecor,
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

pub(super) fn readonly_pane(
    ui: &mut Ui,
    rect: Rect,
    input: PaneInput<'_>,
    cache: &mut PaneCache,
) -> PaneOutput {
    let font = FontId::monospace(FS_SMALL);
    let row_h = ui.fonts_mut(|fonts| fonts.row_height(&font));
    let gaps = input.decor.gaps(input.side, input.inline);
    let key = {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (
            input.text_version,
            input.job.is_some(),
            &gaps,
            crate::theme::palette_generation(),
            crate::theme::fonts_generation(),
            ui.ctx().pixels_per_point().to_bits(),
        )
            .hash(&mut hasher);
        hasher.finish()
    };
    if cache.key != key || cache.galley.is_none() {
        let mut job = input.job.cloned().unwrap_or_else(|| {
            LayoutJob::simple(input.text.to_owned(), font.clone(), c_text(), f32::INFINITY)
        });
        job.wrap.max_width = f32::INFINITY;
        let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
        let galley = apply_gaps(&galley, 0, &gaps, row_h);
        cache.geometry = Some(transparent_glyphs(&galley));
        cache.galley = Some(galley);
        cache.key = key;
    }
    let galley = cache.galley.clone().expect("laid out above");
    let geometry = cache.geometry.clone().expect("laid out above");

    let digit_w = ui.fonts_mut(|fonts| fonts.glyph_width(&font, '0').max(FS_SMALL * 0.5));
    let digits = (input.text.bytes().filter(|byte| *byte == b'\n').count() + 1)
        .to_string()
        .len()
        .max(2) as f32;
    const LEFT_PAD: f32 = 10.0;
    const RIGHT_PAD: f32 = 12.0;
    let gutter = Rect::from_min_size(
        rect.min,
        vec2(digits * digit_w + LEFT_PAD + RIGHT_PAD + 2.0, rect.height()),
    );
    let text_rect = Rect::from_min_max(pos2(gutter.right() + 1.0, rect.top()), rect.max);
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
    let max_x = (galley.rect.width() + 24.0 - text_rect.width()).max(0.0);
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
                        .margin(Margin::same(8))
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
                .map(|change| rows.span(change, input.side, input.inline))
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
    for (line, row) in galley.rows.iter().enumerate().skip(first) {
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
