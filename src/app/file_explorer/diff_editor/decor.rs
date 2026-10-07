//! The changes between a diff's two texts and how they are laid out: the blank rows that keep
//! both sides aligned ("gaps", VS Code's view zones), and the painting that sits under the
//! text (line tints, word highlights, hatched fillers and the removed lines shown inline).

use std::ops::Range;
use std::sync::Arc;

use eframe::egui::text::{CharIndex, LayoutJob, TextFormat, TextWrapping};
use eframe::egui::{self, Color32, FontId, Galley, Painter, Pos2, Rect, Shape, Stroke, pos2};

use crate::theme::*;

/// One change: both sides' lines (0-based positions; where the lines would be inserted when a
/// side is empty) and the changed words of the lines paired across the sides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Change {
    pub old_start: usize,
    pub old_lines: Vec<String>,
    pub new_start: usize,
    pub new_lines: Vec<String>,
    /// Changed byte ranges of old line `k`, for `k` below both sides' lengths.
    pub old_words: Vec<Vec<Range<usize>>>,
    pub new_words: Vec<Vec<Range<usize>>>,
}

impl Change {
    /// Rows the change takes on either side of the split view.
    pub fn rows(&self) -> usize {
        self.old_lines.len().max(self.new_lines.len())
    }

    fn lines(&self, side: Side) -> (usize, &[String], &[Vec<Range<usize>>]) {
        match side {
            Side::Old => (self.old_start, &self.old_lines, &self.old_words),
            Side::New => (self.new_start, &self.new_lines, &self.new_words),
        }
    }
}

/// Which text of the diff a pane shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Side {
    Old,
    New,
}

/// Blank rows inserted before logical line `line` (`line` may be the line count: after the end).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Gap {
    pub line: usize,
    pub rows: usize,
}

/// The changes from a base text to a new text.
#[derive(Debug, Default)]
pub(crate) struct DiffDecor {
    pub changes: Vec<Change>,
    pub added: usize,
    pub removed: usize,
}

impl DiffDecor {
    pub fn compute(base: &str, new: &str) -> Self {
        let mut decor = Self::default();
        for hunk in crate::git::text_hunks(base, new) {
            let mut change = Change {
                old_start: hunk.old_start.saturating_sub(1),
                new_start: hunk.new_start.saturating_sub(1),
                old_words: Vec::new(),
                new_words: Vec::new(),
                old_lines: hunk.old_lines,
                new_lines: hunk.new_lines,
            };
            for (old, new) in change.old_lines.iter().zip(&change.new_lines) {
                let (old_words, new_words) = crate::ui::diff_view::word_diff(old, new);
                change.old_words.push(old_words);
                change.new_words.push(new_words);
            }
            decor.added += change.new_lines.len();
            decor.removed += change.old_lines.len();
            decor.changes.push(change);
        }
        decor
    }

    /// Gaps that align `side` with the other side (split), or that hold the removed lines
    /// above the new ones (inline, new side only).
    pub fn gaps(&self, side: Side, inline: bool) -> Vec<Gap> {
        self.changes
            .iter()
            .filter_map(|change| {
                let (old, new) = (change.old_lines.len(), change.new_lines.len());
                let gap = match (side, inline) {
                    (Side::New, true) => Gap {
                        line: change.new_start,
                        rows: old,
                    },
                    (Side::New, false) => Gap {
                        line: change.new_start + new,
                        rows: old.saturating_sub(new),
                    },
                    (Side::Old, _) => Gap {
                        line: change.old_start + old,
                        rows: new.saturating_sub(old),
                    },
                };
                (gap.rows > 0).then_some(gap)
            })
            .collect()
    }

    /// Whether 0-based `line` of `side` is one of the changed lines.
    pub fn is_changed(&self, side: Side, line: usize) -> bool {
        let index = self
            .changes
            .partition_point(|change| change.lines(side).0 <= line);
        index > 0 && {
            let (start, lines, _) = self.changes[index - 1].lines(side);
            line < start + lines.len()
        }
    }

    /// Old line numbers (1-based) of the removed lines shown inline, with their row centers.
    pub fn zone_numbers(&self, rows: &PaneRows<'_>, visible: egui::Rangef) -> Vec<(usize, f32)> {
        let mut out = Vec::new();
        for change in &self.changes {
            let top = rows.top(change.new_start) - change.old_lines.len() as f32 * rows.row_h;
            for k in 0..change.old_lines.len() {
                let y = top + (k as f32 + 0.5) * rows.row_h;
                if visible.contains(y) {
                    out.push((change.old_start + k + 1, y));
                }
            }
        }
        out
    }

    /// Change whose lines (on the new side) include 0-based `line`, or that removed lines
    /// right above it.
    pub fn change_at_new_line(&self, line: usize) -> Option<usize> {
        self.changes.iter().position(|change| {
            let end = change.new_start + change.new_lines.len();
            (change.new_start..end.max(change.new_start + 1)).contains(&line)
        })
    }
}

/// Logical-line geometry of a diff pane's galley. Diff panes never wrap, so row `n` is line `n`.
pub(crate) struct PaneRows<'a> {
    pub galley: &'a Galley,
    /// Screen position of the galley.
    pub origin: Pos2,
    pub row_h: f32,
}

impl PaneRows<'_> {
    /// Screen y of the top of `line` (its gap above excluded); the galley's bottom past the end.
    pub fn top(&self, line: usize) -> f32 {
        self.origin.y
            + self
                .galley
                .rows
                .get(line)
                .map_or(self.galley.rect.bottom(), |row| row.pos.y)
    }

    /// Screen span of `change` in a pane showing `side` (its lines plus the rows that align or
    /// hold it).
    pub fn span(&self, change: &Change, side: Side, inline: bool) -> (f32, f32) {
        let (old, new) = (change.old_lines.len(), change.new_lines.len());
        let (top, rows) = match (side, inline) {
            (Side::New, true) => (
                self.top(change.new_start) - old as f32 * self.row_h,
                old + new,
            ),
            (Side::New, false) if new == 0 => {
                (self.top(change.new_start) - old as f32 * self.row_h, old)
            }
            (Side::New, false) => (self.top(change.new_start), change.rows()),
            (Side::Old, _) if old == 0 => {
                (self.top(change.old_start) - new as f32 * self.row_h, new)
            }
            (Side::Old, _) => (self.top(change.old_start), change.rows()),
        };
        (top, top + rows as f32 * self.row_h)
    }
}

/// Insert `gaps` (sorted by line) into a galley whose first row is logical line `first`.
/// A gap before `first` itself is left out: callers place such a galley at that line's top.
/// `whole` galleys (starting at line 0) also grow by the gaps after their last line.
pub(crate) fn apply_gaps(galley: &Galley, first: usize, gaps: &[Gap], row_h: f32) -> Arc<Galley> {
    let mut out = galley.clone();
    let mut next = gaps.partition_point(|gap| if first == 0 { false } else { gap.line <= first });
    let mut offset = 0.0;
    let mut line = first;
    for (index, row) in out.rows.iter_mut().enumerate() {
        if index > 0 && galley.rows[index - 1].ends_with_newline {
            line += 1;
        }
        while let Some(gap) = gaps.get(next).filter(|gap| gap.line <= line) {
            offset += gap.rows as f32 * row_h;
            next += 1;
        }
        row.pos.y += offset;
    }
    if first == 0 {
        offset += gaps[next..]
            .iter()
            .map(|gap| gap.rows as f32 * row_h)
            .sum::<f32>();
    }
    out.rect.max.y += offset;
    out.mesh_bounds.max.y += offset;
    Arc::new(out)
}

/// Line tints, word highlights and the gap fillers of one pane, painted under its text.
/// `zones` (inline view) holds the removed lines to show in the new side's gaps.
pub(crate) struct Underlay<'a> {
    pub decor: &'a DiffDecor,
    pub side: Side,
    pub inline: bool,
    /// Text column: tints span its width; word highlights and zones are positioned in it.
    pub clip: Rect,
    /// The removed lines' colored layout (whole base text) and each base line's byte offset.
    pub zones: Option<ZoneText<'a>>,
}

pub(crate) struct ZoneText<'a> {
    pub base: &'a str,
    pub base_job: Option<&'a LayoutJob>,
    pub line_starts: &'a [usize],
}

impl Underlay<'_> {
    pub fn paint(&self, painter: &Painter, rows: &PaneRows<'_>, font: &FontId) {
        let shapes = self.shapes(painter, rows, font);
        painter
            .with_clip_rect(self.clip.intersect(painter.clip_rect()))
            .extend(shapes);
    }

    /// The underlay's shapes, to be painted clipped to `clip`. `painter` only lays out text.
    pub fn shapes(&self, painter: &Painter, rows: &PaneRows<'_>, font: &FontId) -> Vec<Shape> {
        let mut out = Vec::new();
        let visible = self.clip.y_range();
        let (fill, word_fill) = match self.side {
            Side::Old => (c_diff_del_bg(), c_diff_del_fg().gamma_multiply(0.3)),
            Side::New => (c_diff_add_bg(), c_diff_add_fg().gamma_multiply(0.3)),
        };
        for change in &self.decor.changes {
            let (top, bottom) = rows.span(change, self.side, self.inline);
            if bottom < visible.min || top > visible.max {
                continue;
            }
            let (start, lines, words) = change.lines(self.side);
            if !lines.is_empty() {
                let line_rect = Rect::from_x_y_ranges(
                    self.clip.x_range(),
                    rows.top(start)..=rows.top(start) + lines.len() as f32 * rows.row_h,
                );
                out.push(Shape::rect_filled(line_rect, 0.0, fill));
                for (k, ranges) in words.iter().enumerate() {
                    let Some(row) = rows.galley.rows.get(start + k) else {
                        break;
                    };
                    let y = rows.top(start + k);
                    for range in ranges {
                        let text = &lines[k];
                        let (a, b) = (
                            text[..range.start.min(text.len())].chars().count(),
                            text[..range.end.min(text.len())].chars().count(),
                        );
                        let x = rows.origin.x + row.pos.x;
                        out.push(Shape::rect_filled(
                            Rect::from_min_max(
                                pos2(x + row.x_offset(CharIndex(a)), y),
                                pos2(x + row.x_offset(CharIndex(b)), y + rows.row_h),
                            ),
                            0.0,
                            word_fill,
                        ));
                    }
                }
            }
            if self.inline {
                if let (Side::New, Some(zones)) = (self.side, &self.zones) {
                    self.zone_shapes(&mut out, painter, rows, change, zones, font);
                }
            } else {
                // Filler rows where the other side has more lines.
                let gap = match self.side {
                    Side::Old => change
                        .new_lines
                        .len()
                        .saturating_sub(change.old_lines.len()),
                    Side::New => change
                        .old_lines
                        .len()
                        .saturating_sub(change.new_lines.len()),
                };
                if gap > 0 {
                    let below = rows.top(start + lines.len());
                    hatch(
                        &mut out,
                        Rect::from_x_y_ranges(
                            self.clip.x_range(),
                            below - gap as f32 * rows.row_h..=below,
                        ),
                    );
                }
            }
        }
        out
    }

    /// The removed lines of `change`, above its new lines.
    fn zone_shapes(
        &self,
        out: &mut Vec<Shape>,
        painter: &Painter,
        rows: &PaneRows<'_>,
        change: &Change,
        zones: &ZoneText<'_>,
        font: &FontId,
    ) {
        if change.old_lines.is_empty() {
            return;
        }
        let top = rows.top(change.new_start) - change.old_lines.len() as f32 * rows.row_h;
        out.push(Shape::rect_filled(
            Rect::from_x_y_ranges(
                self.clip.x_range(),
                top..=top + change.old_lines.len() as f32 * rows.row_h,
            ),
            0.0,
            c_diff_del_bg(),
        ));
        let x = rows.origin.x + rows.galley.rows.first().map_or(0.0, |row| row.pos.x);
        for (k, text) in change.old_lines.iter().enumerate() {
            let y = top + k as f32 * rows.row_h;
            if y + rows.row_h < self.clip.top() || y > self.clip.bottom() || text.is_empty() {
                continue;
            }
            let offset = zones
                .line_starts
                .get(change.old_start + k)
                .copied()
                .filter(|start| zones.base.get(*start..*start + text.len()) == Some(text));
            let words = change.old_words.get(k).map_or(&[][..], Vec::as_slice);
            let job = line_job(
                text,
                zones.base_job.zip(offset),
                words,
                c_diff_del_fg().gamma_multiply(0.3),
                font,
            );
            let galley = painter.layout_job(job);
            out.push(Shape::galley(pos2(x, y), galley, c_text()));
        }
    }
}

/// Diagonal hatching over `rect`, like VS Code's filler rows (and the patch view's). The lines
/// are cut to `rect` here, so they can share a clip with other shapes.
fn hatch(out: &mut Vec<Shape>, rect: Rect) {
    const STEP: f32 = 8.0;
    let stroke = Stroke::new(1.0, c_text_faint().gamma_multiply(0.25));
    // Lines x + y = c in absolute coordinates, so the pattern continues across rows.
    let first = ((rect.left() + rect.top()) / STEP).floor() as i64;
    let last = ((rect.right() + rect.bottom()) / STEP).ceil() as i64;
    for k in first..=last {
        let c = k as f32 * STEP;
        let top = (c - rect.right()).max(rect.top());
        let bottom = (c - rect.left()).min(rect.bottom());
        if top < bottom {
            out.push(Shape::line_segment(
                [pos2(c - top, top), pos2(c - bottom, bottom)],
                stroke,
            ));
        }
    }
}

/// One line's layout: syntax colors from `colored` (the whole text's job and the line's byte
/// offset in it) and a background on `words`.
pub(crate) fn line_job(
    text: &str,
    colored: Option<(&LayoutJob, usize)>,
    words: &[Range<usize>],
    word_bg: Color32,
    font: &FontId,
) -> LayoutJob {
    let len = text.len();
    let (sections, offset) = colored.map_or((&[][..], 0), |(job, offset)| {
        let first = job
            .sections
            .partition_point(|s| s.byte_range.end.0 <= offset);
        let end =
            job.sections[first..].partition_point(|s| s.byte_range.start.0 < offset + len) + first;
        (&job.sections[first..end], offset)
    });
    let mut cuts = vec![0, len];
    for word in words {
        cuts.extend([word.start.min(len), word.end.min(len)]);
    }
    for section in sections {
        cuts.push(section.byte_range.start.0.saturating_sub(offset).min(len));
        cuts.push(section.byte_range.end.0.saturating_sub(offset).min(len));
    }
    cuts.sort_unstable();
    cuts.dedup();
    let mut job = LayoutJob {
        wrap: TextWrapping {
            max_width: f32::INFINITY,
            ..Default::default()
        },
        ..Default::default()
    };
    for pair in cuts.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if a >= b || !text.is_char_boundary(a) || !text.is_char_boundary(b) {
            continue;
        }
        let color = sections
            .iter()
            .find(|s| s.byte_range.start.0 <= offset + a && offset + a < s.byte_range.end.0)
            .map_or(c_text(), |s| s.format.color);
        let background = if words.iter().any(|w| w.start <= a && b <= w.end) {
            word_bg
        } else {
            Color32::TRANSPARENT
        };
        job.append(
            &text[a..b],
            0.0,
            TextFormat {
                font_id: font.clone(),
                color,
                background,
                ..Default::default()
            },
        );
    }
    job
}

/// Byte offset of each line of `text`.
pub(crate) fn line_starts(text: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(memchr::memchr_iter(b'\n', text.as_bytes()).map(|index| index + 1))
        .collect()
}

/// The overview ruler right of a diff: where the changes are, and the viewport as a draggable
/// thumb. `spans` are the changes' content-space spans; returns a requested scroll offset.
pub(crate) fn ruler(
    ui: &mut egui::Ui,
    rect: Rect,
    spans: &[(f32, f32, bool, bool)],
    content_h: f32,
    viewport_h: f32,
    scroll_y: f32,
) -> Option<f32> {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, c_bg_main());
    painter.vline(
        rect.left() + 0.5,
        rect.y_range(),
        egui::Stroke::new(1.0, c_border_subtle()),
    );
    let total = content_h.max(1.0);
    // Short diffs map 1:1 instead of stretching their few changes over the whole ruler.
    let scale = rect.height() / total.max(viewport_h);
    let lane = (rect.width() - 4.0) / 2.0;
    for &(top, bottom, removed, added) in spans {
        let y = rect.top() + top * scale;
        let h = ((bottom - top) * scale).max(2.0);
        if removed {
            painter.rect_filled(
                Rect::from_min_size(pos2(rect.left() + 2.0, y), egui::vec2(lane, h)),
                0.0,
                c_diff_del_fg().gamma_multiply(0.8),
            );
        }
        if added {
            painter.rect_filled(
                Rect::from_min_size(pos2(rect.left() + 2.0 + lane, y), egui::vec2(lane, h)),
                0.0,
                c_diff_add_fg().gamma_multiply(0.8),
            );
        }
    }
    if total <= viewport_h {
        return None;
    }
    let thumb = Rect::from_min_size(
        pos2(rect.left() + 1.0, rect.top() + scroll_y * scale),
        egui::vec2(rect.width() - 1.0, (viewport_h * scale).max(8.0)),
    );
    let response = ui.interact(
        rect,
        ui.id().with("diff_ruler"),
        egui::Sense::click_and_drag(),
    );
    painter.rect_filled(
        thumb,
        0.0,
        c_text().gamma_multiply(if response.hovered() || response.dragged() {
            0.16
        } else {
            0.08
        }),
    );
    if (response.clicked() || response.dragged())
        && let Some(pointer) = response.interact_pointer_pos()
    {
        let y = (pointer.y - rect.top()) / scale - viewport_h / 2.0;
        return Some(y.clamp(0.0, total - viewport_h));
    }
    None
}
