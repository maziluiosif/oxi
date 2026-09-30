//! Incremental whole-document layout for the editor's `TextEdit`.
//!
//! `TextEdit` needs a galley for the complete text. egui's own cache lays each paragraph out once,
//! but on every edit it still hashes the whole document and copies, hashes, and looks up every
//! paragraph again: ~10 ms per keystroke at 40k lines. Here the paragraph galleys of the previous
//! text are kept, a paragraph whose text is unchanged reuses its galley, and only edited
//! paragraphs are laid out. The result is assembled with the same [`Galley::concat`] egui uses, so
//! rows, positions and cursor mapping are identical.

use std::ops::Range;
use std::sync::Arc;

use eframe::egui::{self, Galley, text::LayoutJob};

/// The paragraph galleys of the last text laid out, and what they were laid out for.
pub(crate) struct LineLayout {
    text: String,
    paragraphs: Vec<Range<usize>>,
    galleys: Vec<Arc<Galley>>,
    key: LayoutKey,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct LayoutKey {
    wrap_width_bits: u32,
    pixels_per_point_bits: u32,
    fonts_generation: u64,
}

/// Paragraphs as egui splits them: on `\n`, excluding it, except that a final `\n` stays in the
/// last paragraph (which then gets its trailing empty row).
fn paragraph_ranges(text: &str, capacity: usize) -> Vec<Range<usize>> {
    let mut paragraphs = Vec::with_capacity(capacity);
    let mut start = 0;
    for newline in memchr::memchr_iter(b'\n', text.as_bytes()) {
        paragraphs.push(start..newline);
        start = newline + 1;
    }
    if start < text.len() {
        paragraphs.push(start..text.len());
    } else if let Some(last) = paragraphs.last_mut() {
        // The text ends with `\n`: egui keeps it in the last paragraph.
        last.end += 1;
    }
    paragraphs
}

/// The job for one paragraph, as egui derives it from a single-section document job.
fn paragraph_job(job: &LayoutJob, text: &str, first: bool) -> LayoutJob {
    let section = &job.sections[0];
    LayoutJob {
        text: text.to_owned(),
        sections: vec![egui::text::LayoutSection {
            leading_space: if first { section.leading_space } else { 0.0 },
            byte_range: egui::text::ByteIndex(0)..egui::text::ByteIndex(text.len()),
            format: section.format.clone(),
        }],
        wrap: job.wrap.clone(),
        first_row_min_height: if first { job.first_row_min_height } else { 0.0 },
        break_on_newline: job.break_on_newline,
        halign: job.halign,
        justify: job.justify,
        round_output_to_gui: job.round_output_to_gui,
        keep_trailing_whitespace: job.keep_trailing_whitespace,
    }
}

/// Lay out `job` (one section covering all of its text), reusing the paragraphs of the previous
/// call in `cache`. `lay_out` lays out a single paragraph job.
pub(crate) fn layout(
    cache: &mut Option<LineLayout>,
    mut job: LayoutJob,
    pixels_per_point: f32,
    mut lay_out: impl FnMut(LayoutJob) -> Arc<Galley>,
) -> Arc<Galley> {
    // egui rounds the wrap width before laying out; match it so paragraphs hit its cache and the
    // merged galley reports the same width.
    if job.wrap.max_width.is_finite() {
        job.wrap.max_width = job.wrap.max_width.round();
    }
    if job.sections.len() != 1 || !job.break_on_newline || job.wrap.max_rows != usize::MAX {
        *cache = None;
        return lay_out(job);
    }
    let key = LayoutKey {
        wrap_width_bits: job.wrap.max_width.to_bits(),
        pixels_per_point_bits: pixels_per_point.to_bits(),
        fonts_generation: crate::theme::fonts_generation(),
    };
    let previous = cache.take().filter(|previous| previous.key == key);

    let paragraphs = paragraph_ranges(
        &job.text,
        previous
            .as_ref()
            .map_or(0, |previous| previous.paragraphs.len() + 1),
    );
    // Bytes shared with the previous text at the start and at the end. A paragraph whose text
    // and closing (or opening) newline lie inside them is unchanged, with no need to compare it.
    let (new_len, old_len) = (
        job.text.len(),
        previous.as_ref().map_or(0, |previous| previous.text.len()),
    );
    let (prefix, suffix) = previous.as_ref().map_or((0, 0), |previous| {
        let prefix =
            crate::text_diff::common_prefix_len(previous.text.as_bytes(), job.text.as_bytes());
        let suffix = crate::text_diff::common_suffix_len(
            previous.text.as_bytes(),
            job.text.as_bytes(),
            old_len.min(new_len) - prefix,
        );
        (prefix, suffix)
    });
    let mut galleys = Vec::with_capacity(paragraphs.len());
    for (index, range) in paragraphs.iter().enumerate() {
        if let Some(previous) = &previous {
            // Its newline is shared and is not the old text's last byte (which egui folds into
            // the last paragraph), so the old paragraph at the same index is identical.
            if range.end < prefix && range.end + 1 < old_len {
                galleys.push(Arc::clone(&previous.galleys[index]));
                continue;
            }
            // The newline before it is shared, so is everything after: same paragraph, counted
            // from the end.
            if range.start > new_len - suffix {
                let old = previous.paragraphs.len() + index - paragraphs.len();
                galleys.push(Arc::clone(&previous.galleys[old]));
                continue;
            }
        }
        let text = &job.text[range.clone()];
        let reused = previous.as_ref().and_then(|previous| {
            // Paragraphs before an edit keep their index; those after it keep their distance
            // from the end. Either way the text must match exactly.
            let same = |old: usize| {
                (previous.text.get(previous.paragraphs[old].clone()) == Some(text))
                    .then(|| Arc::clone(&previous.galleys[old]))
            };
            let from_start = (index < previous.paragraphs.len()).then_some(index);
            let from_end = (previous.paragraphs.len() + index).checked_sub(paragraphs.len());
            // The first paragraph also carries the job's leading space and first-row height.
            from_start.and_then(same).or_else(|| {
                from_end
                    .filter(|old| (*old == 0) == (index == 0))
                    .and_then(same)
            })
        });
        galleys.push(reused.unwrap_or_else(|| lay_out(paragraph_job(&job, text, index == 0))));
    }

    let text = job.text.clone();
    let merged = if galleys.len() == 1 && paragraphs[0] == (0..text.len()) {
        // A single paragraph: exactly what egui returns for it.
        Arc::clone(&galleys[0])
    } else if galleys.is_empty() {
        lay_out(job)
    } else {
        Arc::new(Galley::concat(Arc::new(job), &galleys, pixels_per_point))
    };
    *cache = Some(LineLayout {
        text,
        paragraphs,
        galleys,
        key,
    });
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(text: &str) -> LayoutJob {
        let mut job = LayoutJob::simple(
            text.to_owned(),
            egui::FontId::monospace(12.0),
            egui::Color32::TRANSPARENT,
            f32::INFINITY,
        );
        job.wrap.max_width = f32::INFINITY;
        job
    }

    fn rows(galley: &Galley) -> Vec<(egui::Pos2, usize, bool)> {
        galley
            .rows
            .iter()
            .map(|row| (row.pos, row.row.glyphs.len(), row.ends_with_newline))
            .collect()
    }

    /// Lay `text` out incrementally and from scratch, and compare the results.
    fn check(ctx: &egui::Context, cache: &mut Option<LineLayout>, text: &str) -> usize {
        let mut laid_out = 0;
        let mut incremental = None;
        let mut full = None;
        let _ = ctx.run_ui(Default::default(), |ui| {
            ui.fonts_mut(|fonts| {
                incremental = Some(layout(cache, job(text), 1.0, |job| {
                    laid_out += 1;
                    fonts.layout_job(job)
                }));
                full = Some(fonts.layout_job(job(text)));
            });
        });
        let (incremental, full) = (incremental.unwrap(), full.unwrap());
        assert_eq!(incremental.job.text, text);
        assert_eq!(rows(&incremental), rows(&full), "rows differ for {text:?}");
        assert_eq!(incremental.rect, full.rect);
        assert_eq!(incremental.end().index, full.end().index);
        laid_out
    }

    #[test]
    fn random_edits_match_a_full_layout() {
        let ctx = egui::Context::default();
        let mut cache = None;
        let mut text = "a\n".to_owned();
        check(&ctx, &mut cache, &text);
        check(&ctx, &mut cache, "a\nb");
        check(&ctx, &mut cache, "a\n");
        let pieces = ["x", "\n", "↑", "\n\n", "", "fn f() {}\n"];
        let mut seed = 0x2545_f491_4f6c_dd1d_u64;
        let mut next = |bound: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % bound.max(1) as u64) as usize
        };
        for _ in 0..300 {
            let boundaries: Vec<usize> = text
                .char_indices()
                .map(|(i, _)| i)
                .chain([text.len()])
                .collect();
            let start = boundaries[next(boundaries.len())];
            let end_choices: Vec<usize> =
                boundaries.iter().copied().filter(|b| *b >= start).collect();
            let end = end_choices[next(end_choices.len().min(4))];
            text.replace_range(start..end, pieces[next(pieces.len())]);
            check(&ctx, &mut cache, &text);
        }
    }

    #[test]
    fn splits_paragraphs_like_egui() {
        assert_eq!(paragraph_ranges("", 0), Vec::<Range<usize>>::new());
        assert_eq!(paragraph_ranges("a", 0), vec![0..1]);
        assert_eq!(paragraph_ranges("a\nb", 0), vec![0..1, 2..3]);
        assert_eq!(paragraph_ranges("a\nb\n", 0), vec![0..1, 2..4]);
        assert_eq!(paragraph_ranges("a\n\n", 0), vec![0..1, 2..3]);
        assert_eq!(paragraph_ranges("\n", 0), vec![0..1]);
    }

    #[test]
    fn edits_match_a_full_layout_and_only_relayout_changed_lines() {
        let ctx = egui::Context::default();
        let mut cache = None;
        let base: String = (0..200).map(|i| format!("line {i} ↑ text\n")).collect();
        assert_eq!(check(&ctx, &mut cache, &base), 200);

        // Typing in the middle of one line relays just that line.
        let typed = base.replacen("line 100 ", "line 100 x", 1);
        assert_eq!(check(&ctx, &mut cache, &typed), 1);
        // A new line shifts everything after it, which must still be reused.
        let split = typed.replacen("line 100 x", "line 100\n x", 1);
        assert!(check(&ctx, &mut cache, &split) <= 2);
        // Joining lines, deleting at the ends, and edge cases.
        for text in [
            base.replacen("line 50 ↑ text\nline 51", "line 50 ↑ textline 51", 1),
            base.trim_end().to_owned(),
            format!("{base}tail"),
            format!("head\n{base}"),
            "\n".to_owned(),
            "\n\n\n".to_owned(),
            "no newline".to_owned(),
            String::new(),
            base.clone(),
        ] {
            check(&ctx, &mut cache, &text);
        }
    }
}
