use super::decor::{DiffDecor, Gap, LineGeometry, Side};

const ROW: f32 = 10.0;

/// Gaps as (line, rows) for readable asserts.
fn rows(gaps: &[Gap]) -> Vec<(usize, f32)> {
    gaps.iter()
        .map(|gap| (gap.line, gap.height / ROW))
        .collect()
}

/// A pane of `lines` lines where `wrapped` lines take that many rows.
fn geometry(lines: usize, wrapped: &[(usize, usize)]) -> LineGeometry {
    LineGeometry::for_test(
        (0..lines)
            .map(|line| {
                wrapped
                    .iter()
                    .find(|(wrapped, _)| *wrapped == line)
                    .map_or(1, |(_, rows)| *rows)
            })
            .collect(),
        ROW,
    )
}

#[test]
fn modified_lines_pair_up_with_word_changes() {
    let decor = DiffDecor::compute("a\nlet x = 1;\nc\n", "a\nlet x = 2;\nc\n");
    assert_eq!(decor.changes.len(), 1);
    let change = &decor.changes[0];
    assert_eq!((change.old_start, change.new_start), (1, 1));
    assert_eq!(change.old_lines, ["let x = 1;"]);
    assert_eq!(change.new_lines, ["let x = 2;"]);
    // The changed word is the digit, byte 8 of both lines.
    assert_eq!(change.old_words.len(), 1);
    assert_eq!(change.old_words[0].first(), Some(&(8..9)));
    assert_eq!(change.new_words, change.old_words);
    assert_eq!((decor.added, decor.removed), (1, 1));
}

#[test]
fn split_gaps_align_both_sides() {
    // Two lines become three, then one line is removed.
    let decor = DiffDecor::compute(
        "keep\nold 1\nold 2\nkeep\ngone\nend\n",
        "keep\nnew 1\nnew 2\nnew 3\nkeep\nend\n",
    );
    assert_eq!(decor.changes.len(), 2);
    let aligned = decor.align(
        &LineGeometry::unwrapped(7, ROW),
        &LineGeometry::unwrapped(7, ROW),
    );
    // The base gets one filler row after its two changed lines; the new side one where the
    // removed line was.
    assert_eq!(rows(&aligned.old), [(3, 1.0)]);
    assert_eq!(rows(&aligned.new), [(5, 1.0)]);
    // Both sides see each change at the same height: rows 1..4, then 5..6.
    assert_eq!(aligned.spans, [(ROW, 4.0 * ROW), (5.0 * ROW, 6.0 * ROW)]);
    // Inline: each change's removed lines sit above its new lines.
    assert_eq!(
        rows(&decor.inline_alignment(|_, _| ROW).new),
        [(1, 2.0), (5, 1.0)]
    );
}

#[test]
fn deletions_at_the_end_get_a_gap_after_the_last_line() {
    let decor = DiffDecor::compute("a\nb\nc\n", "a\n");
    let aligned = decor.align(
        &LineGeometry::unwrapped(4, ROW),
        &LineGeometry::unwrapped(2, ROW),
    );
    assert_eq!(rows(&aligned.new), [(1, 2.0)]);
    assert!(aligned.old.is_empty());
    assert_eq!(decor.change_at_new_line(1), Some(0));
    assert!(decor.is_changed(Side::Old, 1));
    assert!(decor.is_changed(Side::Old, 2));
    assert!(!decor.is_changed(Side::Old, 0));
    assert!(!decor.is_changed(Side::New, 0));
}

#[test]
fn line_changes_mark_added_modified_and_removed_lines() {
    use crate::git::GitLineKind::*;
    let decor = DiffDecor::compute("a\nb\nc\nd\n", "a\nB\nc\nnew\n");
    let kinds: Vec<_> = super::line_changes(&decor, 5)
        .into_iter()
        .map(|change| (change.line, change.kind))
        .collect();
    assert_eq!(kinds, [(1, Modified), (3, Modified)]);
    let decor = DiffDecor::compute("a\nb\n", "a\n");
    let kinds: Vec<_> = super::line_changes(&decor, 2)
        .into_iter()
        .map(|change| (change.line, change.kind))
        .collect();
    assert_eq!(kinds, [(1, Deleted)]);
    let decor = DiffDecor::compute("a\n", "a\nb\n");
    let kinds: Vec<_> = super::line_changes(&decor, 3)
        .into_iter()
        .map(|change| (change.line, change.kind))
        .collect();
    assert_eq!(kinds, [(1, Added)]);
}

#[test]
fn gaps_move_later_rows_down() {
    use eframe::egui;
    let ctx = egui::Context::default();
    let mut galley = None;
    let _ = ctx.run_ui(Default::default(), |ui| {
        let job = egui::text::LayoutJob::simple(
            "a\nb\nc".to_owned(),
            egui::FontId::monospace(12.0),
            egui::Color32::WHITE,
            f32::INFINITY,
        );
        galley = Some(ui.fonts_mut(|fonts| fonts.layout_job(job)));
    });
    let galley = galley.unwrap();
    let row_h = galley.rows[0].rect().height();
    let gaps = [
        Gap {
            line: 1,
            height: 2.0 * row_h,
        },
        Gap {
            line: 3,
            height: row_h,
        },
    ];
    let gapped = super::apply_gaps(&galley, 0, &gaps);
    let tops: Vec<f32> = gapped.rows.iter().map(|row| row.pos.y).collect();
    assert_eq!(tops, [0.0, 3.0 * row_h, 4.0 * row_h]);
    // The gap after the last line only makes the galley taller.
    assert_eq!(gapped.rect.height(), galley.rect.height() + 3.0 * row_h);
    // A window starting at line 1 is placed at that line's top: its own gap is excluded.
    let window = super::apply_gaps(&galley, 1, &gaps);
    assert_eq!(window.rows[1].pos.y, galley.rows[1].pos.y);
}

#[test]
fn wrapped_unchanged_lines_get_space_on_the_side_that_wraps_less() {
    let decor = DiffDecor::compute("a\nlong\nb\nc\n", "a\nlong\nb\nC\n");
    // Line 1 is the same text but wraps into three rows on the new side only (its pane is
    // narrower): the base gets two rows after it, so `b` stays level.
    let aligned = decor.align(&geometry(5, &[]), &geometry(5, &[(1, 3)]));
    assert_eq!(rows(&aligned.old), [(2, 2.0)]);
    assert!(aligned.new.is_empty());
    // The change (`c` -> `C`) starts below both.
    assert_eq!(aligned.spans, [(5.0 * ROW, 6.0 * ROW)]);
}

#[test]
fn changed_lines_pair_up_however_they_wrap() {
    // Two lines become one; the first old line wraps into two rows, the new one into four.
    let decor = DiffDecor::compute("a\nold 1\nold 2\nz\n", "a\nnew\nz\n");
    let aligned = decor.align(&geometry(5, &[(1, 2)]), &geometry(4, &[(1, 4)]));
    // `old 1` is padded to the four rows of `new`, then `old 2` (one row) is matched by filler
    // on the new side at the end of the change.
    assert_eq!(rows(&aligned.old), [(2, 2.0)]);
    assert_eq!(rows(&aligned.new), [(2, 1.0)]);
    assert_eq!(aligned.spans, [(ROW, 6.0 * ROW)]);
}

#[test]
fn insertions_span_their_lines_on_both_sides() {
    let decor = DiffDecor::compute("a\nz\n", "a\nnew\nz\n");
    let aligned = decor.align(&geometry(3, &[]), &geometry(4, &[(1, 3)]));
    // The base holds the inserted line's three rows before `z`.
    assert_eq!(rows(&aligned.old), [(1, 3.0)]);
    assert_eq!(aligned.spans, [(ROW, 4.0 * ROW)]);
}

#[test]
fn line_geometry_maps_wrapped_rows_to_lines() {
    use eframe::egui;
    let ctx = egui::Context::default();
    let mut galley = None;
    let _ = ctx.run_ui(Default::default(), |ui| {
        let job = egui::text::LayoutJob::simple(
            "short\nthis line is long enough to wrap a few times\nend".to_owned(),
            egui::FontId::monospace(12.0),
            egui::Color32::WHITE,
            80.0,
        );
        galley = Some(ui.fonts_mut(|fonts| fonts.layout_job(job)));
    });
    let galley = galley.unwrap();
    let row_h = galley.rows[0].rect().height();
    // Rows start at the galley's top: change spans are measured from there.
    assert_eq!(galley.rows[0].pos.y, 0.0);
    let lines = LineGeometry::measure(&galley, row_h);
    assert!(lines.wraps());
    assert_eq!(lines.lines, 3);
    let wrapped_rows = galley.rows.len() - 2;
    assert!(wrapped_rows > 1);
    assert_eq!(lines.first_row(1), 1);
    assert_eq!(lines.first_row(2), 1 + wrapped_rows);
    assert_eq!(lines.line_of_row(wrapped_rows), 1);
    assert_eq!(lines.line_of_row(1 + wrapped_rows), 2);
    assert!((lines.height(1) - wrapped_rows as f32 * row_h).abs() < 0.01);
    // Unwrapped text stores nothing: row `n` is line `n`.
    let unwrapped = LineGeometry::unwrapped(3, row_h);
    assert!(!unwrapped.wraps());
    assert_eq!(unwrapped.first_row(2), 2);
}

#[test]
fn inline_zones_take_the_height_their_removed_lines_wrap_to() {
    // `old 1` wraps into three rows in its zone; `gone` takes one.
    let decor = DiffDecor::compute("a\nold 1\nb\ngone\nc\n", "a\nnew\nb\nc\n");
    let aligned = decor.inline_alignment(|_, text| if text == "old 1" { 3.0 * ROW } else { ROW });
    assert_eq!(rows(&aligned.new), [(1, 3.0), (3, 1.0)]);
    assert_eq!(aligned.zones, [vec![3.0 * ROW], vec![ROW]]);
    // The second zone starts below `a`, the first zone, `new` and `b`; `new` wraps in two.
    let lines = geometry(5, &[(1, 2)]);
    assert_eq!(aligned.change_top(1, &decor.changes[1], &lines), 7.0 * ROW);
    assert_eq!(aligned.change_top(0, &decor.changes[0], &lines), ROW);
}

#[test]
fn line_geometry_follows_where_egui_places_the_rows() {
    use eframe::egui;
    // At 2x, the default mono font's row height is 17.16 pt but its rows are placed every
    // 17 pt: measured by the font, the base side drifts ~0.16 pt a line from the editor's.
    let ctx = egui::Context::default();
    crate::theme::setup_style(&ctx);
    ctx.set_fonts(crate::theme::font_definitions(
        &crate::theme::FontSelection {
            ui: "default".into(),
            mono: "default".into(),
        },
    ));
    ctx.set_pixels_per_point(2.0);
    let mut laid = None;
    for _ in 0..2 {
        let _ = ctx.run_ui(Default::default(), |ui| {
            let font = crate::theme::editor_font();
            let row_h = ui.fonts_mut(|fonts| fonts.row_height(&font));
            let lay_out = |text: String, width: f32| {
                let job =
                    egui::text::LayoutJob::simple(text, font.clone(), egui::Color32::WHITE, width);
                ui.fonts_mut(|fonts| fonts.layout_job(job))
            };
            let wrapping =
                format!("{}{}", "line\n".repeat(300), "word ".repeat(60) + "\n").repeat(2);
            laid = Some((
                row_h,
                lay_out("line\n".repeat(600), f32::INFINITY),
                lay_out(wrapping, 300.0),
            ));
        });
    }
    let (font_row_h, unwrapped, wrapped) = laid.unwrap();
    let lines = LineGeometry::measure(&unwrapped, font_row_h);
    assert!(!lines.wraps());
    assert!((lines.row_h * 599.0 - unwrapped.rows[599].pos.y).abs() < 0.5);
    let lines = LineGeometry::measure(&wrapped, font_row_h);
    assert!(lines.wraps());
    let last = lines.lines - 2;
    let top: f32 = (0..last).map(|line| lines.height(line)).sum();
    assert!((top - wrapped.rows[lines.first_row(last)].pos.y).abs() < 0.5);
}

#[test]
fn sub_pixel_differences_add_up_to_a_gap_instead_of_vanishing() {
    // Each unchanged line is 0.2 taller on the new side: too little for a gap on its own.
    let decor = DiffDecor::compute(&"same\n".repeat(10), &"same\n".repeat(10));
    let old = LineGeometry::for_test(vec![1; 10], ROW);
    let new = LineGeometry::for_test(vec![1; 10], ROW + 0.2);
    let aligned = decor.align(&old, &new);
    let padded: f32 = aligned.old.iter().map(|gap| gap.height).sum();
    assert!(aligned.new.is_empty());
    // Ten lines apart by 2.0 in all: the base is padded by all but the last half pixel.
    assert!(padded > 1.5 && padded <= 2.0 + 1e-3, "{padded}");
}
