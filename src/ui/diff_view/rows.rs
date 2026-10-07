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

pub(super) fn build_rows(
    model: &DiffModel,
    split: bool,
    collapse: bool,
    expanded: &HashSet<(usize, usize)>,
    collapsed_files: &HashSet<usize>,
) -> RowLayout {
    let line_h = line_height();
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
                            push(&mut out, line_row(item), line_h);
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
                            push(&mut out, line_row(item), line_h);
                        }
                    } else {
                        for item in start..i {
                            push(&mut out, line_row(item), line_h);
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
                            push(
                                &mut out,
                                Row::Split {
                                    file: f,
                                    left: (start + k < mid).then_some(start + k),
                                    right: (mid + k < i).then_some(mid + k),
                                },
                                line_h,
                            );
                        }
                    } else {
                        for item in start..i {
                            push(&mut out, Row::Inline { file: f, item }, line_h);
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
