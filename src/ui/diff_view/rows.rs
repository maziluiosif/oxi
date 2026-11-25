//! Row layout: the visible rows of a diff (file headers, folds, split or inline lines).

use super::*;

pub(super) fn row_file(row: Row) -> Option<usize> {
    match row {
        Row::Commit => None,
        Row::File(file)
        | Row::Note(file)
        | Row::Gap { file, .. }
        | Row::Fold { file, .. }
        | Row::Split { file, .. }
        | Row::Inline { file, .. } => Some(file),
    }
}

/// The rows of `model`. `line_rows(file, item, pane)` is how many rows a line wraps into in
/// `pane` (0 / 1: split left / right, 2: inline); a split row is as tall as its taller side.
pub(super) fn build_rows(
    model: &DiffModel,
    split: bool,
    collapse: bool,
    expanded: &HashSet<(usize, usize)>,
    collapsed_files: &HashSet<usize>,
    line_rows: &mut dyn FnMut(usize, usize, usize) -> usize,
) -> RowLayout {
    let line_h = line_height();
    let mut row_h = |row: Row| match row {
        Row::Split { file, left, right } => {
            let left = left.map_or(0, |item| line_rows(file, item, 0));
            let right = right.map_or(0, |item| line_rows(file, item, 1));
            left.max(right).max(1) as f32 * line_h
        }
        Row::Inline { file, item } => line_rows(file, item, 2).max(1) as f32 * line_h,
        _ => line_h,
    };
    let mut out = RowLayout::default();
    let push = |out: &mut RowLayout, row: Row, h: f32| {
        out.rows.push(row);
        out.ys.push(out.total);
        out.total += h;
    };
    if let Some(commit) = &model.commit {
        push(&mut out, Row::Commit, commit_height(commit));
    }
    let headers = model.files.len() > 1 || model.commit.is_some();
    for (f, file) in model.files.iter().enumerate() {
        if headers {
            push(&mut out, Row::File(f), FILE_HEADER_H);
            if collapsed_files.contains(&f) {
                continue;
            }
        }
        if file.binary || file.truncated && file.items.is_empty() || file.items.is_empty() {
            push(&mut out, Row::Note(f), BAND_H);
            continue;
        }
        // New and deleted files have nothing to compare against: one pane reads better.
        let split = split && !matches!(file.status(), 'A' | 'D');
        let line_row = |item: usize| {
            if split {
                Row::Split {
                    file: f,
                    left: Some(item),
                    right: Some(item),
                }
            } else {
                Row::Inline { file: f, item }
            }
        };
        let n = file.items.len();
        let is_gap = |i: usize| matches!(file.items.get(i), Some(Item::Gap { .. }));
        let mut i = 0;
        while i < n {
            match file.kind(i) {
                None => {
                    push(&mut out, Row::Gap { file: f, item: i }, BAND_H);
                    i += 1;
                }
                Some(Kind::Context) => {
                    let start = i;
                    while i < n && file.kind(i) == Some(Kind::Context) {
                        i += 1;
                    }
                    let lead = if start == 0 || is_gap(start - 1) {
                        0
                    } else {
                        CONTEXT
                    };
                    let trail = if i == n || is_gap(i) { 0 } else { CONTEXT };
                    let fold_start = (start + lead).min(i);
                    let fold_end = i.saturating_sub(trail).max(fold_start);
                    out.foldable |= fold_end - fold_start >= MIN_FOLD;
                    let fold = collapse
                        && fold_end - fold_start >= MIN_FOLD
                        && !expanded.contains(&(f, fold_start));
                    if fold {
                        for item in start..fold_start {
                            let row = line_row(item);
                            push(&mut out, row, row_h(row));
                        }
                        push(
                            &mut out,
                            Row::Fold {
                                file: f,
                                start: fold_start,
                                end: fold_end,
                            },
                            BAND_H,
                        );
                        for item in fold_end..i {
                            let row = line_row(item);
                            push(&mut out, row, row_h(row));
                        }
                    } else {
                        for item in start..i {
                            let row = line_row(item);
                            push(&mut out, row, row_h(row));
                        }
                    }
                }
                Some(_) => {
                    let start = i;
                    while i < n && file.kind(i) == Some(Kind::Removed) {
                        i += 1;
                    }
                    let mid = i;
                    while i < n && file.kind(i) == Some(Kind::Added) {
                        i += 1;
                    }
                    let change_y = out.total;
                    if split {
                        for k in 0..(mid - start).max(i - mid) {
                            let row = Row::Split {
                                file: f,
                                left: (start + k < mid).then_some(start + k),
                                right: (mid + k < i).then_some(mid + k),
                            };
                            push(&mut out, row, row_h(row));
                        }
                    } else {
                        for item in start..i {
                            let row = Row::Inline { file: f, item };
                            push(&mut out, row, row_h(row));
                        }
                    }
                    out.changes.push(Change {
                        y: change_y,
                        h: out.total - change_y,
                        removed: mid > start,
                        added: i > mid,
                    });
                }
            }
        }
    }
    out
}
