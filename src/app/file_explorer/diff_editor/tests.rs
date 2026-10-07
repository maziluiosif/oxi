use super::decor::{DiffDecor, Gap, Side};

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
    // The base gets one filler row after its two changed lines; the new side one where the
    // removed line was.
    assert_eq!(decor.gaps(Side::Old, false), [Gap { line: 3, rows: 1 }]);
    assert_eq!(decor.gaps(Side::New, false), [Gap { line: 5, rows: 1 }]);
    // Inline: each change's removed lines sit above its new lines.
    assert_eq!(
        decor.gaps(Side::New, true),
        [Gap { line: 1, rows: 2 }, Gap { line: 5, rows: 1 }]
    );
}

#[test]
fn deletions_at_the_end_get_a_gap_after_the_last_line() {
    let decor = DiffDecor::compute("a\nb\nc\n", "a\n");
    assert_eq!(decor.gaps(Side::New, false), [Gap { line: 1, rows: 2 }]);
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
    let gaps = [Gap { line: 1, rows: 2 }, Gap { line: 3, rows: 1 }];
    let gapped = super::apply_gaps(&galley, 0, &gaps, row_h);
    let tops: Vec<f32> = gapped.rows.iter().map(|row| row.pos.y).collect();
    assert_eq!(tops, [0.0, 3.0 * row_h, 4.0 * row_h]);
    // The gap after the last line only makes the galley taller.
    assert_eq!(gapped.rect.height(), galley.rect.height() + 3.0 * row_h);
    // A window starting at line 1 is placed at that line's top: its own gap is excluded.
    let window = super::apply_gaps(&galley, 1, &gaps, row_h);
    assert_eq!(window.rows[1].pos.y, galley.rows[1].pos.y);
}
