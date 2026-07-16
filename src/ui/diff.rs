//! Shared unified-diff colorization. Single source of truth for both the git panel's
//! full-area diff viewer and the transcript's tool/edit diff bodies, so `+`/`-` lines,
//! `+++`/`---` file headers, and `@@` hunk headers read identically everywhere.

use eframe::egui::text::{LayoutJob, LayoutSection, TextFormat, TextWrapping};
use eframe::egui::{Color32, FontId};

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
