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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Side {
    Old,
    New,
}

/// Blank space, `height` pixels tall, inserted before logical line `line` (`line` may be the
/// line count: after the end).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Gap {
    pub line: usize,
    pub height: f32,
}

/// Which galley rows each logical line of a pane takes, measured without gaps. When no line
/// soft-wraps, every line is one row and nothing is stored.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct LineGeometry {
    /// Logical lines in the galley.
    pub lines: usize,
    /// First row of each line; empty when row `n` is line `n`.
    first_rows: Vec<usize>,
    /// Height of each line (all of its rows); empty when each is `row_h`.
    heights: Vec<f32>,
    pub row_h: f32,
}

impl LineGeometry {
    /// Every line one row of `row_h`: a pane that doesn't wrap, or one not laid out yet.
    pub fn unwrapped(lines: usize, row_h: f32) -> Self {
        Self {
            lines,
            first_rows: Vec::new(),
            heights: Vec::new(),
            row_h,
        }
    }

    /// The lines of `galley`, by where its rows really are: egui rounds row positions to
    /// pixels, so the font's row height would drift from them line by line. `row_h` is only
    /// for an empty galley.
    pub fn measure(galley: &Galley, row_h: f32) -> Self {
        let rows = &galley.rows;
        let row_h = match rows.len() {
            0 => row_h,
            1 => rows[0].rect().height(),
            n => (rows[n - 1].pos.y - rows[0].pos.y) / (n - 1) as f32,
        };
        // Every row but the last ends a line: nothing wraps.
        if rows.iter().rev().skip(1).all(|row| row.ends_with_newline) {
            return Self::unwrapped(rows.len(), row_h);
        }
        // Each row takes the space down to the next one.
        let pitch = |index: usize| match rows.get(index + 1) {
            Some(next) => next.pos.y - rows[index].pos.y,
            None => rows[index].rect().height(),
        };
        let mut first_rows = Vec::with_capacity(rows.len());
        let mut heights: Vec<f32> = Vec::with_capacity(rows.len());
        for index in 0..rows.len() {
            if index == 0 || rows[index - 1].ends_with_newline {
                first_rows.push(index);
                heights.push(0.0);
            }
            if let Some(height) = heights.last_mut() {
                *height += pitch(index);
            }
        }
        Self {
            lines: first_rows.len(),
            first_rows,
            heights,
            row_h,
        }
    }

    /// Lines of `rows` rows each, as if laid out.
    #[cfg(test)]
    pub fn for_test(rows: Vec<usize>, row_h: f32) -> Self {
        let mut first_rows = Vec::with_capacity(rows.len());
        let mut next = 0;
        for count in &rows {
            first_rows.push(next);
            next += count;
        }
        Self {
            lines: rows.len(),
            first_rows,
            heights: rows.iter().map(|count| *count as f32 * row_h).collect(),
            row_h,
        }
    }

    pub fn wraps(&self) -> bool {
        !self.heights.is_empty()
    }

    /// Height of `line`'s own rows.
    pub fn height(&self, line: usize) -> f32 {
        self.heights.get(line).copied().unwrap_or(self.row_h)
    }

    /// Height of the lines in `lines`.
    fn lines_height(&self, lines: Range<usize>) -> f32 {
        if self.wraps() {
            lines.map(|line| self.height(line)).sum()
        } else {
            lines.len() as f32 * self.row_h
        }
    }

    /// First galley row of `line`; past the last line, a row that doesn't exist.
    pub fn first_row(&self, line: usize) -> usize {
        if self.first_rows.is_empty() {
            line
        } else {
            self.first_rows.get(line).copied().unwrap_or(usize::MAX)
        }
    }

    /// The line galley row `row` belongs to.
    pub fn line_of_row(&self, row: usize) -> usize {
        if self.first_rows.is_empty() {
            row
        } else {
            self.first_rows
                .partition_point(|first| *first <= row)
                .saturating_sub(1)
        }
    }
}

/// How the two sides of a diff line up, like VS Code's diff editor: wherever a line or a change
/// is taller on one side (more lines, or lines that soft-wrap into more rows), the other side
/// gets blank space after it. Every unchanged line, and every pair of changed lines, then starts
/// at the same height on both sides.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct Alignment {
    pub old: Vec<Gap>,
    pub new: Vec<Gap>,
    /// Each change's extent in galley coordinates, the same on both sides (split view only).
    pub spans: Vec<(f32, f32)>,
    /// Inline view: the height of each change's removed lines, as they wrap in its zone.
    pub zones: Vec<Vec<f32>>,
}

impl Alignment {
    pub fn gaps(&self, side: Side) -> &[Gap] {
        match side {
            Side::Old => &self.old,
            Side::New => &self.new,
        }
    }

    /// Galley y of the top of change `index` on the new side (`lines` is that side's geometry):
    /// its span in the split view, its zone of removed lines inline.
    pub fn change_top(&self, index: usize, change: &Change, lines: &LineGeometry) -> f32 {
        if let Some(&(top, _)) = self.spans.get(index) {
            return top;
        }
        let gaps: f32 = self
            .new
            .iter()
            .take_while(|gap| gap.line < change.new_start)
            .map(|gap| gap.height)
            .sum();
        lines.lines_height(0..change.new_start) + gaps
    }

    /// Height of removed line `k` of change `index` in its inline zone.
    fn zone_line(&self, index: usize, k: usize, row_h: f32) -> f32 {
        self.zones
            .get(index)
            .and_then(|heights| heights.get(k))
            .copied()
            .unwrap_or(row_h)
    }

    /// Height of change `index`'s whole inline zone (`lines` removed lines).
    fn zone_height(&self, index: usize, lines: usize, row_h: f32) -> f32 {
        match self.zones.get(index) {
            Some(heights) if heights.len() == lines => heights.iter().sum(),
            _ => lines as f32 * row_h,
        }
    }
}

/// Both sides' progress through [`DiffDecor::align`]: the next line and its top on each side.
struct AlignWalk<'a> {
    old: &'a LineGeometry,
    new: &'a LineGeometry,
    o: usize,
    n: usize,
    yo: f32,
    yn: f32,
    out: Alignment,
}

impl AlignWalk<'_> {
    /// Move both sides past one line.
    fn pair(&mut self) {
        self.yo += self.old.height(self.o);
        self.yn += self.new.height(self.n);
        self.o += 1;
        self.n += 1;
    }

    /// Move each side to its line `old` / `new`.
    fn skip_to(&mut self, old: usize, new: usize) {
        if old > self.o {
            self.yo += self.old.lines_height(self.o..old);
            self.o = old;
        }
        if new > self.n {
            self.yn += self.new.lines_height(self.n..new);
            self.n = new;
        }
    }

    /// The next lines of both sides must start level: pad the side that is behind.
    fn anchor(&mut self) {
        fn push(gaps: &mut Vec<Gap>, line: usize, height: f32) {
            match gaps.last_mut() {
                Some(last) if last.line == line => last.height += height,
                _ => gaps.push(Gap { line, height }),
            }
        }
        // Less than a pixel apart gets no gap; the difference is kept, not dropped, so many
        // small ones still add up to a gap later.
        let behind = self.yo - self.yn;
        if behind > 0.5 {
            push(&mut self.out.new, self.n, behind);
            self.yn = self.yo;
        } else if behind < -0.5 {
            push(&mut self.out.old, self.o, -behind);
            self.yo = self.yn;
        }
    }

    /// Unchanged lines up to old line `old` (new line `new`), aligned line by line when
    /// either side wraps.
    fn unchanged(&mut self, old: usize, new: usize) {
        if self.old.wraps() || self.new.wraps() {
            while self.o < old && self.n < new {
                self.pair();
                self.anchor();
            }
        }
        self.skip_to(old, new);
        self.anchor();
    }
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

    /// Line both sides of the split view up (see [`Alignment`]), given how tall each side's
    /// lines are laid out.
    pub fn align(&self, old: &LineGeometry, new: &LineGeometry) -> Alignment {
        let mut walk = AlignWalk {
            old,
            new,
            o: 0,
            n: 0,
            yo: 0.0,
            yn: 0.0,
            out: Alignment::default(),
        };
        for change in &self.changes {
            walk.unchanged(change.old_start, change.new_start);
            let top = walk.yo.max(walk.yn);
            // Changed lines pair up across the sides (their word highlights do too), so each
            // pair starts level; the side with fewer lines is padded at the change's end.
            for _ in 0..change.old_lines.len().min(change.new_lines.len()) {
                walk.pair();
                walk.anchor();
            }
            walk.skip_to(
                change.old_start + change.old_lines.len(),
                change.new_start + change.new_lines.len(),
            );
            walk.anchor();
            walk.out.spans.push((top, walk.yo.max(walk.yn)));
        }
        walk.unchanged(old.lines.max(walk.o), new.lines.max(walk.n));
        walk.out
    }

    /// The inline view's gaps: each change's removed lines above its new lines, removed line
    /// `old` (0-based, with its text) taking `height(old, text)`.
    pub fn inline_alignment(&self, mut height: impl FnMut(usize, &str) -> f32) -> Alignment {
        let mut out = Alignment::default();
        for change in &self.changes {
            let heights: Vec<f32> = change
                .old_lines
                .iter()
                .enumerate()
                .map(|(k, text)| height(change.old_start + k, text))
                .collect();
            if !heights.is_empty() {
                out.new.push(Gap {
                    line: change.new_start,
                    height: heights.iter().sum(),
                });
            }
            out.zones.push(heights);
        }
        out
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

    /// Old line numbers (1-based) of the removed lines shown inline, with the centers of their
    /// first rows.
    pub fn zone_numbers(&self, rows: &PaneRows<'_>, visible: egui::Rangef) -> Vec<(usize, f32)> {
        let mut out = Vec::new();
        for (index, change) in self.changes.iter().enumerate() {
            for (k, top, _) in rows.zone_lines(index, change) {
                let y = top + 0.5 * rows.row_h;
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

/// Logical-line geometry of a diff pane's (gapped) galley on screen.
pub(crate) struct PaneRows<'a> {
    pub galley: &'a Galley,
    /// Screen position of the galley.
    pub origin: Pos2,
    pub row_h: f32,
    /// The rows each line takes.
    pub lines: &'a LineGeometry,
    /// The pane's gaps and what they hold: the changes' extents (split view) or the removed
    /// lines' heights (inline).
    pub alignment: &'a Alignment,
}

impl PaneRows<'_> {
    /// Screen y of the top of `line` (its gap above excluded); the galley's bottom past the end.
    pub fn top(&self, line: usize) -> f32 {
        self.origin.y
            + self
                .galley
                .rows
                .get(self.lines.first_row(line))
                .map_or(self.galley.rect.bottom(), |row| row.pos.y)
    }

    /// Screen span of change `index` in a pane showing `side` (its lines plus the space that
    /// aligns or holds it).
    pub fn span(&self, index: usize, change: &Change, side: Side, inline: bool) -> (f32, f32) {
        if !inline && let Some(&(top, bottom)) = self.alignment.spans.get(index) {
            return (self.origin.y + top, self.origin.y + bottom);
        }
        let (old, new) = (change.old_lines.len(), change.new_lines.len());
        if inline && side == Side::New {
            // The zone of removed lines, then the new lines below it.
            let start = self.top(change.new_start);
            let top = start - self.alignment.zone_height(index, old, self.row_h);
            let bottom = match new {
                0 => start,
                _ => {
                    let last = change.new_start + new - 1;
                    self.top(last) + self.lines.height(last)
                }
            };
            return (top, bottom);
        }
        let (top, rows) = match side {
            Side::New if new == 0 => (self.top(change.new_start) - old as f32 * self.row_h, old),
            Side::New => (self.top(change.new_start), change.rows()),
            Side::Old if old == 0 => (self.top(change.old_start) - new as f32 * self.row_h, new),
            Side::Old => (self.top(change.old_start), change.rows()),
        };
        (top, top + rows as f32 * self.row_h)
    }

    /// Inline view: the removed lines of change `index` in its zone above the new lines, as
    /// (k, top, height).
    pub fn zone_lines<'b>(
        &'b self,
        index: usize,
        change: &Change,
    ) -> impl Iterator<Item = (usize, f32, f32)> + 'b {
        let lines = change.old_lines.len();
        let mut top =
            self.top(change.new_start) - self.alignment.zone_height(index, lines, self.row_h);
        (0..lines).map(move |k| {
            let height = self.alignment.zone_line(index, k, self.row_h);
            top += height;
            (k, top - height, height)
        })
    }
}

/// Insert `gaps` (sorted by line) into a galley whose first row is logical line `first`.
/// A gap before `first` itself is left out: callers place such a galley at that line's top.
/// `whole` galleys (starting at line 0) also grow by the gaps after their last line.
pub(crate) fn apply_gaps(galley: &Galley, first: usize, gaps: &[Gap]) -> Arc<Galley> {
    let mut out = galley.clone();
    let mut next = gaps.partition_point(|gap| if first == 0 { false } else { gap.line <= first });
    let mut offset = 0.0;
    let mut line = first;
    for (index, row) in out.rows.iter_mut().enumerate() {
        if index > 0 && galley.rows[index - 1].ends_with_newline {
            line += 1;
        }
        while let Some(gap) = gaps.get(next).filter(|gap| gap.line <= line) {
            offset += gap.height;
            next += 1;
        }
        row.pos.y += offset;
    }
    if first == 0 {
        offset += gaps[next..].iter().map(|gap| gap.height).sum::<f32>();
    }
    out.rect.max.y += offset;
    out.mesh_bounds.max.y += offset;
    Arc::new(out)
}

/// Where each logical line of `galley` starts, down to the next one's start: a line owns the
/// gap after it (the first line also the gap before it, from 0), and the last one runs to the
/// galley's bottom.
fn line_tops(galley: &Galley) -> Vec<f32> {
    let mut starts = vec![0.0];
    for (index, row) in galley.rows.iter().enumerate().skip(1) {
        if galley.rows[index - 1].ends_with_newline {
            starts.push(row.pos.y);
        }
    }
    starts
}

/// The y in `new` of the point at `y` in `old`, two layouts of the same text: the same fraction
/// down the same logical line, however each wraps and whatever gaps each has.
pub(crate) fn same_place(old: &Galley, new: &Galley, y: f32) -> f32 {
    let (old_starts, new_starts) = (line_tops(old), line_tops(new));
    let line = old_starts
        .partition_point(|&start| start <= y)
        .saturating_sub(1);
    let extent = |starts: &[f32], bottom: f32| {
        let top = starts.get(line).copied().unwrap_or(bottom);
        (
            top,
            starts.get(line + 1).copied().unwrap_or(bottom).max(top),
        )
    };
    let (top, bottom) = extent(&old_starts, old.rect.bottom());
    let fraction = if bottom > top {
        ((y - top) / (bottom - top)).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (top, bottom) = extent(&new_starts, new.rect.bottom());
    top + fraction * (bottom - top)
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
    /// The pane's wrap width (infinite when it doesn't wrap), for the removed lines too.
    pub wrap_width: f32,
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
        let band = |top: f32, bottom: f32| Rect::from_x_y_ranges(self.clip.x_range(), top..=bottom);
        // Line tints go under everything else.
        let mut tints = Vec::new();
        for (index, change) in self.decor.changes.iter().enumerate() {
            let (top, bottom) = rows.span(index, change, self.side, self.inline);
            if bottom < visible.min || top > visible.max {
                continue;
            }
            let (start, lines, words) = change.lines(self.side);
            // One tint per run of lines with no filler between them.
            let mut run: Option<(f32, f32)> = None;
            for k in 0..lines.len() {
                let line_top = rows.top(start + k);
                let line_bottom = line_top + rows.lines.height(start + k);
                run = match run {
                    Some((run_top, run_bottom)) if line_top - run_bottom < 0.5 => {
                        Some((run_top, line_bottom))
                    }
                    Some((run_top, run_bottom)) => {
                        tints.push(Shape::rect_filled(band(run_top, run_bottom), 0.0, fill));
                        Some((line_top, line_bottom))
                    }
                    None => Some((line_top, line_bottom)),
                };
                if line_bottom < visible.min || line_top > visible.max {
                    continue;
                }
                if let Some(ranges) = words.get(k) {
                    word_shapes(&mut out, rows, start + k, &lines[k], ranges, word_fill);
                }
                // Filler below a line where the other side is taller (more lines, or more
                // wrapped rows), like VS Code's hatched alignment zones.
                if !self.inline {
                    let next = if k + 1 < lines.len() {
                        rows.top(start + k + 1)
                    } else {
                        bottom
                    };
                    if next - line_bottom > 0.5 {
                        hatch(&mut out, band(line_bottom, next));
                    }
                }
            }
            if let Some((run_top, run_bottom)) = run {
                tints.push(Shape::rect_filled(band(run_top, run_bottom), 0.0, fill));
            }
            if self.inline {
                if let (Side::New, Some(zones)) = (self.side, &self.zones) {
                    self.zone_shapes(&mut out, painter, rows, (index, change), zones, font);
                }
            } else if lines.is_empty() {
                // This side has none of the change's lines: all of it is filler.
                hatch(&mut out, band(top, bottom));
            }
        }
        tints.extend(out);
        tints
    }

    /// The removed lines of `change`, above its new lines.
    fn zone_shapes(
        &self,
        out: &mut Vec<Shape>,
        painter: &Painter,
        rows: &PaneRows<'_>,
        (index, change): (usize, &Change),
        zones: &ZoneText<'_>,
        font: &FontId,
    ) {
        if change.old_lines.is_empty() {
            return;
        }
        let (top, _) = rows.span(index, change, Side::New, true);
        let height = rows
            .alignment
            .zone_height(index, change.old_lines.len(), rows.row_h);
        out.push(Shape::rect_filled(
            Rect::from_x_y_ranges(self.clip.x_range(), top..=top + height),
            0.0,
            c_diff_del_bg(),
        ));
        let x = rows.origin.x + rows.galley.rows.first().map_or(0.0, |row| row.pos.x);
        for (k, y, height) in rows.zone_lines(index, change) {
            let text = &change.old_lines[k];
            if y + height < self.clip.top() || y > self.clip.bottom() || text.is_empty() {
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
                zones.wrap_width,
            );
            let galley = painter.fonts_mut(|fonts| zone_galley(job, |job| fonts.layout_job(job)));
            out.push(Shape::galley(pos2(x, y), galley, c_text()));
        }
    }
}

/// A removed line of the inline view, wrapped (by `job`'s width) like the editor's lines.
pub(crate) fn zone_galley(
    job: LayoutJob,
    mut lay_out: impl FnMut(LayoutJob) -> Arc<Galley>,
) -> Arc<Galley> {
    if job.wrap.max_width.is_finite() && editor_wrap_indent() {
        super::super::line_layout::hanging_paragraph(job, &mut lay_out)
    } else {
        lay_out(job)
    }
}

/// Height of a removed line of the inline view: one `row_h` per row it wraps into.
pub(crate) fn zone_line_height(
    text: &str,
    font: &FontId,
    wrap_width: f32,
    row_h: f32,
    lay_out: impl FnMut(LayoutJob) -> Arc<Galley>,
) -> f32 {
    if !wrap_width.is_finite() || text.is_empty() {
        return row_h;
    }
    let galley = zone_galley(
        line_job(text, None, &[], Color32::TRANSPARENT, font, wrap_width),
        lay_out,
    );
    galley.rows.len().max(1) as f32 * row_h
}

/// Highlights of the changed `ranges` (bytes of `text`) of `line`, on each row it wraps into.
fn word_shapes(
    out: &mut Vec<Shape>,
    rows: &PaneRows<'_>,
    line: usize,
    text: &str,
    ranges: &[Range<usize>],
    fill: Color32,
) {
    let chars = |byte: usize| text[..byte.min(text.len())].chars().count();
    let ranges: Vec<(usize, usize)> = ranges
        .iter()
        .map(|range| (chars(range.start), chars(range.end)))
        .collect();
    // Chars of the line on the rows before this one.
    let mut offset = 0;
    for row in rows.galley.rows.iter().skip(rows.lines.first_row(line)) {
        let count = row.row.char_count_excluding_newline().0;
        let (x, y) = (rows.origin.x + row.pos.x, rows.origin.y + row.pos.y);
        for &(a, b) in &ranges {
            let (a, b) = (a.max(offset), b.min(offset + count));
            if a < b {
                out.push(Shape::rect_filled(
                    Rect::from_min_max(
                        pos2(x + row.x_offset(CharIndex(a - offset)), y),
                        pos2(
                            x + row.x_offset(CharIndex(b - offset)),
                            y + row.rect().height(),
                        ),
                    ),
                    0.0,
                    fill,
                ));
            }
        }
        offset += count;
        if row.ends_with_newline {
            break;
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
/// offset in it) and a background on `words`, wrapped at `wrap_width`.
pub(crate) fn line_job(
    text: &str,
    colored: Option<(&LayoutJob, usize)>,
    words: &[Range<usize>],
    word_bg: Color32,
    font: &FontId,
    wrap_width: f32,
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
            max_width: if wrap_width.is_finite() {
                wrap_width.round()
            } else {
                wrap_width
            },
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
