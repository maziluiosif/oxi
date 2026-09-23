//! Shared unified-diff colorization. Single source of truth for both the git panel's
//! full-area diff viewer and the transcript's tool/edit diff bodies, so `+`/`-` lines,
//! `+++`/`---` file headers, and `@@` hunk headers read identically everywhere.

use eframe::egui::text::{LayoutJob, LayoutSection, TextFormat, TextWrapping};
use eframe::egui::{self, Color32, FontId, Id, Rect, Shape, Stroke, Ui};

#[derive(Clone, Copy)]
enum ChatDiffLineKind {
    Header,
    Context,
    Added,
    Removed,
    Empty,
}

struct ChatDiffRow {
    left: String,
    right: String,
    left_no: Option<usize>,
    right_no: Option<usize>,
    left_kind: ChatDiffLineKind,
    right_kind: ChatDiffLineKind,
}

use crate::theme::*;

/// Colorize a unified diff into a wrapped monospace [`LayoutJob`] at `FS_CODE`.
pub fn diff_layout_job(text: &str, wrap_width: f32) -> LayoutJob {
    let mut job = LayoutJob {
        wrap: TextWrapping {
            max_width: wrap_width,
            ..Default::default()
        },
        break_on_newline: true,
        ..Default::default()
    };

    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        let start = job.text.len();
        job.text.push_str(line);
        // Keep the newline inside this section's byte range — egui only lays out
        // bytes covered by a section, so a newline left in a gap gets dropped.
        if lines.peek().is_some() {
            job.text.push('\n');
        }
        let end = job.text.len();
        let (color, background) = if line.starts_with("+++") || line.starts_with("---") {
            (c_text(), c_bg_elevated())
        } else if line.starts_with('+') {
            (c_diff_add_fg(), c_diff_add_bg())
        } else if line.starts_with('-') {
            (c_diff_del_fg(), c_diff_del_bg())
        } else if line.starts_with("@@") {
            (c_accent(), Color32::TRANSPARENT)
        } else {
            (c_text_muted(), Color32::TRANSPARENT)
        };
        job.sections.push(LayoutSection {
            leading_space: 0.0,
            byte_range: eframe::egui::text::ByteIndex(start)..eframe::egui::text::ByteIndex(end),
            format: TextFormat {
                font_id: FontId::monospace(FS_CODE),
                color,
                background,
                ..Default::default()
            },
        });
    }

    job
}

/// Show a unified diff as aligned old/new columns inside the transcript: faint line numbers,
/// full-width row tints and a hairline between the two sides. Wrapping is deliberately disabled
/// so one source row always occupies exactly one visual row in both columns; each column scrolls
/// horizontally on its own.
pub fn show_split_chat_diff(
    ui: &mut Ui,
    text: &str,
    max_rows: Option<usize>,
    id: Id,
    selectable: bool,
) {
    let rows = split_chat_diff_rows_limited(text, max_rows);
    if rows.is_empty() {
        return;
    }
    let digits = rows
        .iter()
        .flat_map(|row| [row.left_no, row.right_no])
        .flatten()
        .max()
        .unwrap_or(1)
        .to_string()
        .len();
    const GAP: f32 = 16.0;
    ui.spacing_mut().item_spacing.x = GAP;
    let top = ui.cursor().top();
    let mut divider_x = None;
    ui.columns(2, |columns| {
        for (index, column) in columns.iter_mut().enumerate() {
            let left = index == 0;
            if left {
                divider_x = Some(column.max_rect().right() + GAP / 2.0);
            }
            egui::ScrollArea::horizontal()
                .id_salt(id.with(left))
                .auto_shrink([false, true])
                .show(column, |ui| {
                    let backgrounds = ui.painter().add(Shape::Noop);
                    let text_top = ui.cursor().top();
                    let job = chat_diff_column_job(&rows, left, digits);
                    if selectable {
                        crate::theme::selectable_text_job(ui, job);
                    } else {
                        ui.add(
                            egui::Label::new(job)
                                .wrap_mode(egui::TextWrapMode::Extend)
                                .selectable(false),
                        );
                    }
                    let used = ui.min_rect();
                    let row_h = (used.bottom() - text_top) / rows.len() as f32;
                    let x = egui::Rangef::new(
                        used.left().min(ui.clip_rect().left()),
                        used.right().max(ui.clip_rect().right()),
                    );
                    let shapes = rows
                        .iter()
                        .enumerate()
                        .filter_map(|(i, row)| {
                            let fill = match if left { row.left_kind } else { row.right_kind } {
                                ChatDiffLineKind::Added => c_diff_add_bg(),
                                ChatDiffLineKind::Removed => c_diff_del_bg(),
                                _ => return None,
                            };
                            let y0 = text_top + i as f32 * row_h;
                            let rect = Rect::from_x_y_ranges(x, y0..=y0 + row_h);
                            Some(Shape::rect_filled(rect, 0.0, fill))
                        })
                        .collect();
                    ui.painter().set(backgrounds, Shape::Vec(shapes));
                });
        }
    });
    if let Some(x) = divider_x {
        let bottom = ui.min_rect().bottom();
        ui.painter()
            .vline(x, top..=bottom, Stroke::new(1.0, c_border_subtle()));
    }
}

fn split_chat_diff_rows_limited(text: &str, max_rows: Option<usize>) -> Vec<ChatDiffRow> {
    let mut rows = split_chat_diff_rows(text);
    if let Some(limit) = max_rows
        && rows.len() > limit
    {
        rows.truncate(limit);
        rows.push(ChatDiffRow {
            left: "… more changes".to_string(),
            right: "click to show all".to_string(),
            left_no: None,
            right_no: None,
            left_kind: ChatDiffLineKind::Header,
            right_kind: ChatDiffLineKind::Header,
        });
    }
    rows
}

fn chat_diff_column_job(rows: &[ChatDiffRow], left: bool, digits: usize) -> LayoutJob {
    let mut job = LayoutJob {
        wrap: TextWrapping {
            max_width: f32::INFINITY,
            ..Default::default()
        },
        break_on_newline: true,
        ..Default::default()
    };
    let font = FontId::monospace(FS_CODE);
    for (index, row) in rows.iter().enumerate() {
        let (text, number, kind) = if left {
            (&row.left, row.left_no, row.left_kind)
        } else {
            (&row.right, row.right_no, row.right_kind)
        };
        let gutter = match (number, kind) {
            (Some(n), _) => format!("{n:>digits$}  "),
            (None, ChatDiffLineKind::Header) => String::new(),
            (None, _) => " ".repeat(digits + 2),
        };
        let number_color = match kind {
            ChatDiffLineKind::Added => c_diff_add_fg().gamma_multiply(0.7),
            ChatDiffLineKind::Removed => c_diff_del_fg().gamma_multiply(0.7),
            _ => c_text_faint(),
        };
        job.append(&gutter, 0.0, TextFormat::simple(font.clone(), number_color));
        let color = match kind {
            ChatDiffLineKind::Added => c_diff_add_fg(),
            ChatDiffLineKind::Removed => c_diff_del_fg(),
            ChatDiffLineKind::Header | ChatDiffLineKind::Empty => c_text_faint(),
            ChatDiffLineKind::Context => c_text_muted(),
        };
        let mut line = text.clone();
        if index + 1 < rows.len() {
            line.push('\n');
        }
        job.append(&line, 0.0, TextFormat::simple(font.clone(), color));
    }
    job
}

fn split_chat_diff_rows(text: &str) -> Vec<ChatDiffRow> {
    let lines: Vec<&str> = text.lines().collect();
    let mut rows = Vec::new();
    let (mut old_line, mut new_line) = (1usize, 1usize);
    let mut index = 0usize;
    while index < lines.len() {
        let line = lines[index];
        if line.starts_with("--- ") || line.starts_with("+++ ") {
            index += 1;
            continue;
        }
        if line.starts_with("@@") {
            if let Some((old, new)) = hunk_starts(line) {
                old_line = old;
                new_line = new;
            }
            index += 1;
            continue;
        }
        if line.starts_with('-') && !line.starts_with("---") {
            let removed_start = index;
            while index < lines.len()
                && lines[index].starts_with('-')
                && !lines[index].starts_with("---")
            {
                index += 1;
            }
            let added_start = index;
            while index < lines.len()
                && lines[index].starts_with('+')
                && !lines[index].starts_with("+++")
            {
                index += 1;
            }
            let removed = &lines[removed_start..added_start];
            let added = &lines[added_start..index];
            for pair in 0..removed.len().max(added.len()) {
                let left_no = removed.get(pair).map(|_| {
                    old_line += 1;
                    old_line - 1
                });
                let right_no = added.get(pair).map(|_| {
                    new_line += 1;
                    new_line - 1
                });
                rows.push(ChatDiffRow {
                    left: removed
                        .get(pair)
                        .map(|l| l[1..].to_string())
                        .unwrap_or_default(),
                    right: added
                        .get(pair)
                        .map(|l| l[1..].to_string())
                        .unwrap_or_default(),
                    left_no,
                    right_no,
                    left_kind: if pair < removed.len() {
                        ChatDiffLineKind::Removed
                    } else {
                        ChatDiffLineKind::Empty
                    },
                    right_kind: if pair < added.len() {
                        ChatDiffLineKind::Added
                    } else {
                        ChatDiffLineKind::Empty
                    },
                });
            }
            continue;
        }
        if let Some(content) = line.strip_prefix('+') {
            rows.push(ChatDiffRow {
                left: String::new(),
                right: content.to_string(),
                left_no: None,
                right_no: Some(new_line),
                left_kind: ChatDiffLineKind::Empty,
                right_kind: ChatDiffLineKind::Added,
            });
            new_line += 1;
        } else if let Some(content) = line.strip_prefix(' ') {
            rows.push(ChatDiffRow {
                left: content.to_string(),
                right: content.to_string(),
                left_no: Some(old_line),
                right_no: Some(new_line),
                left_kind: ChatDiffLineKind::Context,
                right_kind: ChatDiffLineKind::Context,
            });
            old_line += 1;
            new_line += 1;
        }
        index += 1;
    }
    rows
}

fn hunk_starts(line: &str) -> Option<(usize, usize)> {
    let mut parts = line.split_whitespace();
    (parts.next()? == "@@").then_some(())?;
    let old = parts
        .next()?
        .strip_prefix('-')?
        .split(',')
        .next()?
        .parse()
        .ok()?;
    let new = parts
        .next()?
        .strip_prefix('+')?
        .split(',')
        .next()?
        .parse()
        .ok()?;
    Some((old, new))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_chat_diff_pairs_rows_and_aligns_line_numbers() {
        let rows = split_chat_diff_rows(
            "--- a/f\n+++ b/f\n@@ -10,3 +10,3 @@\n-old one\n-old two\n+new one\n+new two\n same",
        );
        assert_eq!(rows.len(), 3);
        assert_eq!(
            (rows[0].left_no, rows[0].left.as_str()),
            (Some(10), "old one")
        );
        assert_eq!(
            (rows[0].right_no, rows[0].right.as_str()),
            (Some(10), "new one")
        );
        assert_eq!(
            (rows[1].left_no, rows[1].left.as_str()),
            (Some(11), "old two")
        );
        assert_eq!(
            (rows[1].right_no, rows[1].right.as_str()),
            (Some(11), "new two")
        );
        assert_eq!((rows[2].left_no, rows[2].left.as_str()), (Some(12), "same"));
        assert_eq!(
            (rows[2].right_no, rows[2].right.as_str()),
            (Some(12), "same")
        );
    }

    #[test]
    fn split_chat_diff_jobs_disable_soft_wrapping() {
        let rows = split_chat_diff_rows_limited("@@ -1 +1 @@\n-old\n+new", None);
        assert!(
            chat_diff_column_job(&rows, true, 1)
                .wrap
                .max_width
                .is_infinite()
        );
        assert!(
            chat_diff_column_job(&rows, false, 1)
                .wrap
                .max_width
                .is_infinite()
        );
    }

    #[test]
    fn split_chat_diff_gutter_right_aligns_numbers() {
        let rows = split_chat_diff_rows("@@ -9,2 +9,2 @@\n same\n+added");
        let job = chat_diff_column_job(&rows, false, 2);
        assert!(
            job.text.starts_with(" 9  same\n10  added"),
            "{:?}",
            job.text
        );
    }
}
