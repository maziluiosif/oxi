//! Shared unified-diff colorization. Single source of truth for both the git panel's
//! full-area diff viewer and the transcript's tool/edit diff bodies, so `+`/`-` lines,
//! `+++`/`---` file headers, and `@@` hunk headers read identically everywhere.

use eframe::egui::text::{LayoutJob, LayoutSection, TextFormat, TextWrapping};
use eframe::egui::{Color32, FontId};

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

/// Render the chat diff in aligned old/new columns. Wrapping is deliberately disabled so one
/// source row always occupies exactly one visual row in both columns.
pub fn split_chat_diff_layout_jobs(text: &str, max_rows: Option<usize>) -> (LayoutJob, LayoutJob) {
    let mut rows = split_chat_diff_rows(text);
    if let Some(limit) = max_rows
        && rows.len() > limit
    {
        rows.truncate(limit);
        rows.push(ChatDiffRow {
            left: "… more changes".to_string(),
            right: "… click to expand".to_string(),
            left_kind: ChatDiffLineKind::Header,
            right_kind: ChatDiffLineKind::Header,
        });
    }
    (
        chat_diff_column_job(&rows, true),
        chat_diff_column_job(&rows, false),
    )
}

fn chat_diff_column_job(rows: &[ChatDiffRow], left: bool) -> LayoutJob {
    let mut job = LayoutJob {
        wrap: TextWrapping {
            max_width: f32::INFINITY,
            ..Default::default()
        },
        break_on_newline: true,
        ..Default::default()
    };
    for (index, row) in rows.iter().enumerate() {
        let (text, kind) = if left {
            (&row.left, row.left_kind)
        } else {
            (&row.right, row.right_kind)
        };
        let start = job.text.len();
        job.text.push_str(text);
        if index + 1 < rows.len() {
            job.text.push('\n');
        }
        let end = job.text.len();
        let (color, background) = match kind {
            ChatDiffLineKind::Added => (c_diff_add_fg(), c_diff_add_bg()),
            ChatDiffLineKind::Removed => (c_diff_del_fg(), c_diff_del_bg()),
            ChatDiffLineKind::Header => (c_text(), c_bg_elevated()),
            ChatDiffLineKind::Context => (c_text_muted(), Color32::TRANSPARENT),
            ChatDiffLineKind::Empty => (c_text_faint(), Color32::TRANSPARENT),
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
                let left = removed.get(pair).map(|line| {
                    let text = format!("{old_line:<4}  {}", &line[1..]);
                    old_line += 1;
                    text
                });
                let right = added.get(pair).map(|line| {
                    let text = format!("{new_line:<4}  {}", &line[1..]);
                    new_line += 1;
                    text
                });
                rows.push(ChatDiffRow {
                    left: left.unwrap_or_default(),
                    right: right.unwrap_or_default(),
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
                right: format!("{new_line:<4}  {content}"),
                left_kind: ChatDiffLineKind::Empty,
                right_kind: ChatDiffLineKind::Added,
            });
            new_line += 1;
        } else if let Some(content) = line.strip_prefix(' ') {
            rows.push(ChatDiffRow {
                left: format!("{old_line:<4}  {content}"),
                right: format!("{new_line:<4}  {content}"),
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
        assert_eq!(rows[0].left, "10    old one");
        assert_eq!(rows[0].right, "10    new one");
        assert_eq!(rows[1].left, "11    old two");
        assert_eq!(rows[1].right, "11    new two");
        assert_eq!(rows[2].left, "12    same");
        assert_eq!(rows[2].right, "12    same");
    }

    #[test]
    fn split_chat_diff_jobs_disable_soft_wrapping() {
        let (left, right) = split_chat_diff_layout_jobs("@@ -1 +1 @@\n-old\n+new", None);
        assert!(left.wrap.max_width.is_infinite());
        assert!(right.wrap.max_width.is_infinite());
    }
}
