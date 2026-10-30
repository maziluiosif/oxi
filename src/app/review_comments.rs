//! Comments on diff lines for the agent. In the editor's diff mode, a change's Comment button (or
//! Comment on selection, for any lines of the file) opens a note; the notes collect above the
//! composer and go out together with the next message, each with its file, its old/new line
//! numbers and the exact diff excerpt, so the agent can answer all of them in one response.

use std::fmt::Write as _;

use eframe::egui::{self, Frame, Margin, RichText, Stroke, Ui};

use super::OxiApp;
use super::file_explorer::DiffChange as Change;
use crate::model::UserAttachment;
use crate::theme::*;

/// Name of the attachment the comments are sent as.
pub(crate) const ATTACHMENT: &str = "Review comments";
/// The message sent with the comments when the composer is empty.
const DEFAULT_PROMPT: &str = "Please address my review comments on the diff.";

/// One note on a range of diff lines.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct ReviewComment {
    /// Workspace the file belongs to.
    pub root: String,
    /// Path relative to the workspace.
    pub path: String,
    /// What the diff compared with ("agent turn", "HEAD", a branch…).
    pub base: String,
    /// 1-based inclusive lines on the old side, if the comment covers any.
    pub old_lines: Option<(usize, usize)>,
    /// 1-based inclusive lines on the new side, if the comment covers any.
    pub new_lines: Option<(usize, usize)>,
    /// The commented lines as a unified diff excerpt (`-` old, `+` new, ` ` unchanged).
    pub excerpt: String,
    pub note: String,
}

impl ReviewComment {
    /// `src/x.rs:12-14`, by the new lines when there are any.
    pub(crate) fn location(&self) -> String {
        let range = |(a, b): (usize, usize)| {
            if a == b {
                a.to_string()
            } else {
                format!("{a}-{b}")
            }
        };
        match (self.new_lines, self.old_lines) {
            (Some(new), _) => format!("{}:{}", self.path, range(new)),
            (None, Some(old)) => format!("{} (removed lines {})", self.path, range(old)),
            (None, None) => self.path.clone(),
        }
    }
}

/// A comment being written: where it goes, and its text so far.
pub(crate) struct CommentDraft {
    pub target: ReviewComment,
    pub text: String,
    /// Where the note opened, in screen points.
    pub anchor: egui::Pos2,
}

/// 1-based inclusive line range of `count` lines from 0-based `start`.
fn range(start: usize, count: usize) -> Option<(usize, usize)> {
    (count > 0).then(|| (start + 1, start + count))
}

/// Target for a comment on one change of the diff.
pub(crate) fn change_target(root: &str, path: &str, base: &str, change: &Change) -> ReviewComment {
    let mut excerpt = String::new();
    for line in &change.old_lines {
        let _ = writeln!(excerpt, "-{line}");
    }
    for line in &change.new_lines {
        let _ = writeln!(excerpt, "+{line}");
    }
    ReviewComment {
        root: root.to_string(),
        path: path.to_string(),
        base: base.to_string(),
        old_lines: range(change.old_start, change.old_lines.len()),
        new_lines: range(change.new_start, change.new_lines.len()),
        excerpt,
        note: String::new(),
    }
}

/// Target for a comment on lines `first..=last` (0-based) of the new side, `content`. Changes
/// that touch those lines bring their removed lines along, so the excerpt is the exact diff.
pub(crate) fn lines_target(
    root: &str,
    path: &str,
    base: &str,
    content: &str,
    (first, last): (usize, usize),
    changes: &[Change],
) -> ReviewComment {
    let lines: Vec<&str> = content.split('\n').collect();
    let last = last.min(lines.len().saturating_sub(1));
    let touching: Vec<&Change> = changes
        .iter()
        .filter(|c| {
            let end = c.new_start + c.new_lines.len();
            // A pure removal sits between two new lines; it counts when inside the range.
            c.new_start <= last && (end > first || (c.new_lines.is_empty() && c.new_start > first))
        })
        .collect();
    let mut excerpt = String::new();
    let mut old: Option<(usize, usize)> = None;
    // A change that starts above the range still shows the lines it removed.
    for change in touching.iter().filter(|c| c.new_start < first) {
        for removed in &change.old_lines {
            let _ = writeln!(excerpt, "-{removed}");
        }
        if let Some((a, b)) = range(change.old_start, change.old_lines.len()) {
            old = Some((a, b));
        }
    }
    for line in first..=last {
        for change in touching.iter().filter(|c| c.new_start == line) {
            for removed in &change.old_lines {
                let _ = writeln!(excerpt, "-{removed}");
            }
            if let Some((a, b)) = range(change.old_start, change.old_lines.len()) {
                old = Some(old.map_or((a, b), |(x, y)| (x.min(a), y.max(b))));
            }
        }
        let added = touching
            .iter()
            .any(|c| (c.new_start..c.new_start + c.new_lines.len()).contains(&line));
        let _ = writeln!(
            excerpt,
            "{}{}",
            if added { '+' } else { ' ' },
            lines.get(line).copied().unwrap_or("")
        );
    }
    ReviewComment {
        root: root.to_string(),
        path: path.to_string(),
        base: base.to_string(),
        old_lines: old,
        new_lines: Some((first + 1, last + 1)),
        excerpt,
        note: String::new(),
    }
}

/// All comments as one message attachment.
pub(crate) fn format_comments(comments: &[ReviewComment]) -> String {
    let mut out = String::from(
        "Review comments the user wrote on lines of the diff. Address every one of them; line \
         numbers are 1-based.\n",
    );
    for (i, c) in comments.iter().enumerate() {
        let lines = |label: &str, range: Option<(usize, usize)>| {
            range.map(|(a, b)| {
                if a == b {
                    format!("{label} line {a}")
                } else {
                    format!("{label} lines {a}-{b}")
                }
            })
        };
        let place = [lines("new", c.new_lines), lines("old", c.old_lines)]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(", ");
        let _ = write!(
            out,
            "\n{}. {} ({place}; diff against {})\n```diff\n{}```\nComment: {}\n",
            i + 1,
            c.path,
            c.base,
            c.excerpt,
            c.note.trim()
        );
    }
    out
}

/// Where the comments not sent yet are kept between runs.
fn comments_path() -> std::path::PathBuf {
    crate::app_dirs::data_dir().join("review_comments.json")
}

/// The comments a previous run left unsent.
pub(crate) fn load_comments() -> Vec<ReviewComment> {
    std::fs::read(comments_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

impl OxiApp {
    /// Keep the unsent comments on disk (off the UI thread; they change a few times a minute
    /// at most).
    fn save_comments(&self) {
        let content = serde_json::to_vec_pretty(&self.conv.review_comments).unwrap_or_default();
        let _ = std::thread::Builder::new()
            .name("oxi-review-comments".into())
            .spawn(move || {
                if let Err(e) = crate::fsutil::write_atomic_private(&comments_path(), &content) {
                    log::warn!("review comments: could not save: {e}");
                }
            });
    }

    /// Comments on `path` (relative to the active workspace), for the diff's markers.
    pub(crate) fn comments_on(&self, path: &str) -> Vec<&ReviewComment> {
        let root = &self.active_workspace().root_path;
        self.conv
            .review_comments
            .iter()
            .filter(|c| &c.root == root && c.path == path)
            .collect()
    }

    /// Comments waiting to go out with the active workspace's next message.
    fn workspace_comments(&self) -> Vec<(usize, &ReviewComment)> {
        let root = &self.active_workspace().root_path;
        self.conv
            .review_comments
            .iter()
            .enumerate()
            .filter(|(_, c)| &c.root == root)
            .collect()
    }

    /// Whether comments wait for the active workspace's next message (they can be sent alone,
    /// with the default prompt).
    pub(crate) fn has_review_comments(&self) -> bool {
        !self.workspace_comments().is_empty()
    }

    /// Move the active workspace's comments into the composer's attachments, before sending.
    pub(crate) fn attach_review_comments(&mut self) {
        let root = self.active_workspace().root_path.clone();
        let (comments, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.conv.review_comments)
            .into_iter()
            .partition(|c| c.root == root);
        self.conv.review_comments = rest;
        if comments.is_empty() {
            return;
        }
        self.save_comments();
        self.conv.composer.pending_texts.push(UserAttachment::Text {
            name: format!("{ATTACHMENT} ({})", comments.len()),
            text: format_comments(&comments),
        });
        if self.conv.composer.input.trim().is_empty() {
            self.conv.composer.input = DEFAULT_PROMPT.to_string();
        }
    }

    /// Start a note on `target`, opened at `anchor`.
    pub(crate) fn open_comment_draft(&mut self, target: ReviewComment, anchor: egui::Pos2) {
        self.conv.comment_draft = Some(CommentDraft {
            target,
            text: String::new(),
            anchor,
        });
    }

    /// The note being written, floating where it was opened.
    pub(crate) fn render_comment_draft(&mut self, ctx: &egui::Context) {
        let Some(mut draft) = self.conv.comment_draft.take() else {
            return;
        };
        let mut add = false;
        let mut cancel = false;
        let screen = ctx.content_rect();
        let width = 360.0_f32.min(screen.width() - 32.0);
        let pos = egui::pos2(
            draft.anchor.x.min(screen.right() - width - 16.0).max(16.0),
            draft.anchor.y.min(screen.bottom() - 220.0).max(16.0),
        );
        egui::Area::new(egui::Id::new("diff_comment_draft"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .show(ctx, |ui| {
                Frame::new()
                    .fill(c_bg_elevated())
                    .stroke(Stroke::new(1.0, c_border()))
                    .corner_radius(RADIUS_CHIP)
                    .inner_margin(Margin::same(12))
                    .show(ui, |ui| {
                        ui.set_width(width);
                        ui.label(
                            RichText::new(format!("Comment on {}", draft.target.location()))
                                .size(FS_SMALL)
                                .color(c_text()),
                        );
                        ui.add_space(6.0);
                        let edit = ui.add(
                            egui::TextEdit::multiline(&mut draft.text)
                                .desired_rows(3)
                                .desired_width(f32::INFINITY)
                                .hint_text("What should the agent change here?"),
                        );
                        if !edit.has_focus() && draft.text.is_empty() {
                            edit.request_focus();
                        }
                        let submit = edit.has_focus()
                            && ui.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.command);
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("Sent with your next message")
                                    .size(FS_TINY)
                                    .color(c_text_faint()),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let ready = !draft.text.trim().is_empty();
                                    if ui
                                        .add_enabled(ready, egui::Button::new("Add comment"))
                                        .on_hover_text("Cmd/Ctrl+Enter")
                                        .clicked()
                                        || (submit && ready)
                                    {
                                        add = true;
                                    }
                                    if ui.button("Cancel").on_hover_text("Esc").clicked() {
                                        cancel = true;
                                    }
                                },
                            );
                        });
                    });
            });
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            cancel = true;
        }
        if add {
            let mut comment = draft.target;
            comment.note = draft.text.trim().to_string();
            self.conv.review_comments.push(comment);
            self.save_comments();
            self.notify_composer("Comment added; it goes out with your next message.");
        } else if !cancel {
            self.conv.comment_draft = Some(draft);
        }
    }

    /// The active workspace's comments above the composer, each removable.
    pub(super) fn render_review_comments(&mut self, ui: &mut Ui) {
        let comments: Vec<(usize, String, String)> = self
            .workspace_comments()
            .into_iter()
            .map(|(i, c)| (i, c.location(), c.note.clone()))
            .collect();
        if comments.is_empty() {
            return;
        }
        let mut remove = None;
        let mut clear = false;
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!(
                    "{} · sent with your next message",
                    if comments.len() == 1 {
                        "1 review comment".to_string()
                    } else {
                        format!("{} review comments", comments.len())
                    }
                ))
                .size(FS_SMALL)
                .color(c_text_muted()),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                clear = ui.small_button("Clear").clicked();
            });
        });
        for (index, location, note) in &comments {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(
                    RichText::new(location)
                        .font(egui::FontId::monospace(FS_TINY))
                        .color(c_accent()),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .small_button("×")
                        .on_hover_text("Remove this comment")
                        .clicked()
                    {
                        remove = Some(*index);
                    }
                    ui.add(
                        egui::Label::new(RichText::new(note).size(FS_SMALL).color(c_text()))
                            .truncate()
                            .selectable(false),
                    )
                    .on_hover_text(note);
                });
            });
        }
        ui.add_space(4.0);
        if clear {
            let root = self.active_workspace().root_path.clone();
            self.conv.review_comments.retain(|c| c.root != root);
            self.save_comments();
        } else if let Some(index) = remove {
            self.conv.review_comments.remove(index);
            self.save_comments();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(old_start: usize, old: &[&str], new_start: usize, new: &[&str]) -> Change {
        Change {
            old_start,
            old_lines: old.iter().map(|s| s.to_string()).collect(),
            new_start,
            new_lines: new.iter().map(|s| s.to_string()).collect(),
            old_words: Vec::new(),
            new_words: Vec::new(),
        }
    }

    #[test]
    fn a_change_comment_carries_both_sides_and_the_exact_lines() {
        let c = change_target(
            "/r",
            "src/a.rs",
            "agent turn",
            &change(9, &["let x = 1;"], 9, &["let x = 2;", "let y = 3;"]),
        );
        assert_eq!(c.old_lines, Some((10, 10)));
        assert_eq!(c.new_lines, Some((10, 11)));
        assert_eq!(c.excerpt, "-let x = 1;\n+let x = 2;\n+let y = 3;\n");
        assert_eq!(c.location(), "src/a.rs:10-11");
        let removal = change_target("/r", "b.rs", "HEAD", &change(4, &["gone"], 4, &[]));
        assert_eq!(removal.new_lines, None);
        assert_eq!(removal.location(), "b.rs (removed lines 5)");
    }

    #[test]
    fn selected_lines_include_the_changes_inside_them() {
        let content = "a\nb2\nc\nd\n";
        let changes = [
            change(1, &["b"], 1, &["b2"]),
            // `x` was removed between `c` and `d`.
            change(3, &["x"], 3, &[]),
        ];
        let c = lines_target("/r", "f.txt", "HEAD", content, (0, 3), &changes);
        assert_eq!(c.excerpt, " a\n-b\n+b2\n c\n-x\n d\n");
        assert_eq!(c.new_lines, Some((1, 4)));
        assert_eq!(c.old_lines, Some((2, 4)));
        let text = format_comments(&[ReviewComment {
            note: "rename b".into(),
            ..c
        }]);
        assert!(text.contains("1. f.txt (new lines 1-4, old lines 2-4; diff against HEAD)"));
        assert!(text.contains("```diff\n a\n-b\n+b2\n c\n-x\n d\n```\nComment: rename b"));
    }
}
