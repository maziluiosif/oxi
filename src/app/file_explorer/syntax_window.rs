//! Viewport-sized syntax layout for the editor.
//!
//! TextEdit keeps a transparent full-document galley for cursor and scroll geometry; the
//! colored text painted on top only needs the rows on screen. Laying out just a window of
//! logical lines keeps opening and editing large files from paying for a second
//! whole-document layout.

use std::ops::Range;

use eframe::egui::{self, Galley, Pos2, Rect};
use egui::text::{ByteIndex, LayoutJob, LayoutSection};

/// Logical lines on screen, and the wider window laid out around them so small scrolls can
/// reuse the cached syntax galley.
pub(super) struct LineWindow {
    pub visible: Range<usize>,
    pub padded: Range<usize>,
}

pub(super) fn line_window(galley: &Galley, galley_pos: Pos2, viewport: Rect) -> LineWindow {
    let mut line = 0usize;
    let mut first = None;
    let mut last = 0usize;
    for (index, placed) in galley.rows.iter().enumerate() {
        if index > 0 && galley.rows[index - 1].ends_with_newline {
            line += 1;
        }
        let rect = placed.rect().translate(galley_pos.to_vec2());
        if rect.bottom() < viewport.top() {
            continue;
        }
        if rect.top() > viewport.bottom() {
            break;
        }
        first.get_or_insert(line);
        last = line;
    }
    let first = first.unwrap_or(line);
    let last = last.max(first);
    // One screen of margin on each side.
    let span = last - first + 1;
    LineWindow {
        visible: first..last + 1,
        padded: first.saturating_sub(span)..last + 1 + span,
    }
}

/// Top of logical line `target` in galley coordinates (the galley's bottom if it is past
/// the end).
pub(super) fn line_top(galley: &Galley, target: usize) -> f32 {
    let mut line = 0usize;
    for (index, placed) in galley.rows.iter().enumerate() {
        if index > 0 && galley.rows[index - 1].ends_with_newline {
            line += 1;
        }
        if line == target {
            return placed.rect().top();
        }
    }
    galley.rect.bottom()
}

/// Byte range covering logical lines `lines` of `text`, including their trailing newlines.
pub(super) fn line_byte_range(text: &str, lines: &Range<usize>) -> Range<usize> {
    let mut start = (lines.start == 0).then_some(0);
    let mut end = text.len();
    for (newline, (index, _)) in text.match_indices('\n').enumerate() {
        // Logical line `newline + 1` begins right after this newline.
        if newline + 1 == lines.start {
            start = Some(index + 1);
        }
        if newline + 1 == lines.end {
            end = index + 1;
            break;
        }
    }
    let start = start.unwrap_or(text.len());
    start..end.max(start)
}

/// The part of `job` covering logical lines `lines`, with sections clipped and rebased.
/// Wrapping is per line, so its rows match the full layout of the same lines exactly.
pub(super) fn slice_job(job: &LayoutJob, lines: &Range<usize>) -> LayoutJob {
    let bytes = line_byte_range(&job.text, lines);
    let first = job
        .sections
        .partition_point(|section| section.byte_range.end.0 <= bytes.start);
    let mut sections = Vec::new();
    for section in &job.sections[first..] {
        let (start, end) = (section.byte_range.start.0, section.byte_range.end.0);
        if start >= bytes.end {
            break;
        }
        let (start, end) = (start.max(bytes.start), end.min(bytes.end));
        if start < end {
            sections.push(LayoutSection {
                leading_space: 0.0,
                byte_range: ByteIndex(start - bytes.start)..ByteIndex(end - bytes.start),
                format: section.format.clone(),
            });
        }
    }
    LayoutJob {
        text: job.text[bytes].to_owned(),
        sections,
        wrap: job.wrap.clone(),
        first_row_min_height: job.first_row_min_height,
        break_on_newline: job.break_on_newline,
        halign: job.halign,
        justify: job.justify,
        round_output_to_gui: job.round_output_to_gui,
        keep_trailing_whitespace: job.keep_trailing_whitespace,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Color32, FontId, text::TextFormat};

    fn job(text: &str, cuts: &[usize]) -> LayoutJob {
        let mut job = LayoutJob {
            text: text.to_owned(),
            ..Default::default()
        };
        let mut start = 0;
        for &end in cuts.iter().chain(std::iter::once(&text.len())) {
            job.sections.push(LayoutSection {
                leading_space: 0.0,
                byte_range: ByteIndex(start)..ByteIndex(end),
                format: TextFormat::simple(
                    FontId::monospace(12.0),
                    Color32::from_gray(start as u8),
                ),
            });
            start = end;
        }
        job
    }

    #[test]
    fn line_byte_ranges_include_trailing_newlines() {
        let text = "a\nbb\nccc";
        assert_eq!(line_byte_range(text, &(0..1)), 0..2);
        assert_eq!(line_byte_range(text, &(1..2)), 2..5);
        assert_eq!(line_byte_range(text, &(1..10)), 2..8);
        assert_eq!(line_byte_range(text, &(5..9)), 8..8);
    }

    #[test]
    fn slice_clips_and_rebases_sections() {
        // Sections: "a\nb" | "b\ncc" | "c"
        let full = job("a\nbb\nccc", &[3, 7]);
        let sliced = slice_job(&full, &(1..2));
        assert_eq!(sliced.text, "bb\n");
        let ranges: Vec<_> = sliced
            .sections
            .iter()
            .map(|s| s.byte_range.start.0..s.byte_range.end.0)
            .collect();
        assert_eq!(ranges, vec![0..1, 1..3]);
        assert_eq!(sliced.sections[0].format, full.sections[0].format);
        assert_eq!(sliced.sections[1].format, full.sections[1].format);
    }

    #[test]
    fn slice_past_the_end_is_empty() {
        let sliced = slice_job(&job("one\ntwo", &[]), &(4..8));
        assert!(sliced.text.is_empty());
        assert!(sliced.sections.is_empty());
    }
}
