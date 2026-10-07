//! VS Code's "quick diff" for the editor gutter: clicking a change marker opens a peek under
//! the change with the lines it replaced, a Revert button (one undoable edit in the buffer)
//! and previous/next navigation.
//!
//! While a peek has been opened, the document's gutter markers come from comparing the live
//! buffer with the base text, so a revert or an edit updates them at once instead of after the
//! next save and Git refresh. The base is `HEAD`, or the merge base while the Compare tab shows.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};

use eframe::egui::{self, Align, FontId, Layout, Margin, RichText, Stroke, Ui};

use crate::git::{GitLineChange, GitLineKind, TextHunk};
use crate::theme::*;

use super::super::OxiApp;

/// Diff lines shown in the peek before it scrolls.
const PEEK_MAX_LINES: usize = 14;
const MARKER_HIT_W: f32 = 10.0;

pub(crate) struct QuickDiff {
    path: PathBuf,
    compare_base: Option<String>,
    /// Newest commit when the base was read: a commit makes it stale.
    head: Option<String>,
    base: String,
    revision: Option<u64>,
    hunks: Vec<TextHunk>,
    markers: Vec<GitLineChange>,
    /// 0-based line the open peek belongs to.
    peek: Option<usize>,
}

impl QuickDiff {
    fn refresh(&mut self, revision: u64, content: &str) {
        if self.revision == Some(revision) {
            return;
        }
        self.revision = Some(revision);
        self.hunks = crate::git::text_hunks(&self.base, content);
        let last_line = content.split('\n').count().saturating_sub(1);
        self.markers.clear();
        for hunk in &self.hunks {
            let start = hunk.new_start.saturating_sub(1);
            if hunk.new_lines.is_empty() {
                self.markers.push(GitLineChange {
                    line: start.min(last_line),
                    kind: GitLineKind::Deleted,
                });
                continue;
            }
            let kind = if hunk.old_lines.is_empty() {
                GitLineKind::Added
            } else {
                GitLineKind::Modified
            };
            self.markers.extend(
                (start..start + hunk.new_lines.len()).map(|line| GitLineChange { line, kind }),
            );
        }
        self.markers.sort_by_key(|marker| marker.line);
        self.markers.dedup_by_key(|marker| marker.line);
    }

    pub(crate) fn close_peek(&mut self) {
        self.peek = None;
    }

    /// The hunk whose marker is on `line`.
    fn hunk_at(&self, line: usize) -> Option<usize> {
        let last_line = self
            .markers
            .last()
            .map_or(0, |marker| marker.line)
            .max(line);
        self.hunks.iter().position(|hunk| {
            let start = hunk.new_start.saturating_sub(1);
            if hunk.new_lines.is_empty() {
                start.min(last_line) == line
            } else {
                (start..start + hunk.new_lines.len()).contains(&line)
            }
        })
    }
}

fn hunk_line(hunk: &TextHunk) -> usize {
    hunk.new_start.saturating_sub(1)
}

/// A marker click waiting for its Git base, read off the UI thread: resolving a merge base or
/// opening a large repository can take longer than a frame.
pub(crate) struct PendingQuickDiff {
    path: PathBuf,
    line: usize,
    compare_base: Option<String>,
    rx: Receiver<Result<String, String>>,
}

enum PeekAction {
    Revert(usize),
    Go(usize),
    Close,
}

impl OxiApp {
    /// The Compare tab's base while it drives the gutter, like `git_gutter_line_changes`.
    fn quick_diff_compare_base(&self) -> Option<String> {
        if !self.conv.git_ui.open || self.conv.git_ui.tab != crate::app::git_panel::GitTab::Compare
        {
            return None;
        }
        self.conv
            .git_ui
            .compare
            .data
            .as_ref()
            .filter(|data| data.error.is_none())
            .map(|data| data.base.clone())
    }

    /// Live gutter markers for document `index`, while a quick diff is active for it.
    pub(crate) fn quick_diff_markers(&mut self, index: usize) -> Option<Vec<GitLineChange>> {
        self.poll_quick_diff();
        let compare_base = self.quick_diff_compare_base();
        let head = self.conv.git.log.first().map(|commit| commit.hash.clone());
        let document = self.conv.editor.documents.get(index)?;
        let quick = self.conv.editor.quick_diff.as_mut()?;
        if quick.path != document.path {
            return None;
        }
        // Saved and closed: Git's own markers are accurate again.
        let idle = quick.peek.is_none() && !document.is_dirty();
        if idle || quick.compare_base != compare_base || quick.head != head {
            self.conv.editor.quick_diff = None;
            return None;
        }
        quick.refresh(document.content_revision, &document.content);
        Some(quick.markers.clone())
    }

    /// Where document `index` sits in the repository, for [`crate::git::base_text`].
    fn quick_diff_source(&self, index: usize) -> Option<(PathBuf, String)> {
        let document = self.conv.editor.documents.get(index)?;
        let root = PathBuf::from(&self.active_workspace().root_path);
        let relative = document.path.strip_prefix(&root).ok()?;
        Some((
            document.path.clone(),
            relative.to_string_lossy().replace('\\', "/"),
        ))
    }

    /// Read the base in the background, then open the peek for the change on `line`.
    fn request_quick_diff(&mut self, ctx: &egui::Context, index: usize, line: usize) {
        let Some((path, relative)) = self.quick_diff_source(index) else {
            return;
        };
        let compare_base = self.quick_diff_compare_base();
        let root = self.active_workspace().root_path.clone();
        let base = compare_base.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(crate::git::base_text(&root, &relative, base.as_deref()));
            ctx.request_repaint();
        });
        self.conv.editor.quick_diff_pending = Some(PendingQuickDiff {
            path,
            line,
            compare_base,
            rx,
        });
    }

    /// Open the peek of a finished [`Self::request_quick_diff`].
    fn poll_quick_diff(&mut self) {
        let Some(pending) = self.conv.editor.quick_diff_pending.as_ref() else {
            return;
        };
        let result = match pending.rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("the Git reader stopped".to_string()),
        };
        let Some(pending) = self.conv.editor.quick_diff_pending.take() else {
            return;
        };
        match result {
            Ok(base) => {
                self.install_quick_diff(&pending.path, pending.line, pending.compare_base, base);
            }
            Err(error) => {
                self.conv.editor.error = Some(format!("Cannot compare with Git: {error}"));
            }
        }
    }

    /// Read the base on the calling thread and open the peek for the change on `line`.
    #[cfg(test)]
    pub(crate) fn open_quick_diff(&mut self, index: usize, line: usize) {
        let Some((path, relative)) = self.quick_diff_source(index) else {
            return;
        };
        let compare_base = self.quick_diff_compare_base();
        match crate::git::base_text(
            &self.active_workspace().root_path,
            &relative,
            compare_base.as_deref(),
        ) {
            Ok(base) => self.install_quick_diff(&path, line, compare_base, base),
            Err(error) => {
                self.conv.editor.error = Some(format!("Cannot compare with Git: {error}"));
            }
        }
    }

    fn install_quick_diff(
        &mut self,
        path: &std::path::Path,
        line: usize,
        compare_base: Option<String>,
        base: String,
    ) {
        // The tab may have been closed while the base was read.
        let Some(document) = self.conv.editor.documents.iter().find(|d| d.path == path) else {
            return;
        };
        let mut quick = QuickDiff {
            path: document.path.clone(),
            compare_base,
            head: self.conv.git.log.first().map(|commit| commit.hash.clone()),
            base,
            revision: None,
            hunks: Vec::new(),
            markers: Vec::new(),
            peek: None,
        };
        quick.refresh(document.content_revision, &document.content);
        quick.peek = quick
            .hunk_at(line)
            .map(|hunk| hunk_line(&quick.hunks[hunk]));
        self.conv.editor.quick_diff = Some(quick);
    }

    /// Marker clicks in the gutter, and the peek of the open change.
    pub(super) fn quick_diff_ui(
        &mut self,
        ui: &mut Ui,
        index: usize,
        gutter_rect: egui::Rect,
        visible: &[(usize, f32)],
        markers: &[GitLineChange],
    ) {
        let line_h = FS_SMALL * 1.35;
        let strip = egui::Rect::from_min_max(
            gutter_rect.left_top(),
            egui::pos2(gutter_rect.left() + MARKER_HIT_W, gutter_rect.bottom()),
        )
        .intersect(ui.clip_rect());
        let response = ui.interact(
            strip,
            ui.id().with("quick_diff_strip"),
            egui::Sense::click(),
        );
        let line_at = |y: f32| {
            visible
                .iter()
                .find(|(_, center)| (y - center).abs() <= line_h * 0.5)
                .map(|(line, _)| *line)
        };
        let marked = response
            .hover_pos()
            .and_then(|pos| line_at(pos.y))
            .filter(|line| markers.iter().any(|marker| marker.line == *line));
        if let Some(line) = marked {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            if response.clicked() {
                let open = self
                    .conv
                    .editor
                    .quick_diff
                    .as_ref()
                    .and_then(|quick| quick.peek.and_then(|peek| quick.hunk_at(peek)));
                let clicked = self
                    .conv
                    .editor
                    .quick_diff
                    .as_ref()
                    .and_then(|quick| quick.hunk_at(line));
                if open.is_some() && open == clicked {
                    // A second click on the same change closes its peek.
                    if let Some(quick) = self.conv.editor.quick_diff.as_mut() {
                        quick.close_peek();
                    }
                } else {
                    self.request_quick_diff(&ui.ctx().clone(), index, line);
                }
            }
        }

        let Some(document) = self.conv.editor.documents.get(index) else {
            return;
        };
        let Some(quick) = self
            .conv
            .editor
            .quick_diff
            .as_ref()
            .filter(|quick| quick.path == document.path)
        else {
            return;
        };
        let Some(peek) = quick.peek else { return };
        let Some(current) = quick.hunk_at(peek) else {
            // The change is gone (reverted or edited away).
            if let Some(quick) = self.conv.editor.quick_diff.as_mut() {
                quick.close_peek();
            }
            return;
        };
        let hunk = &quick.hunks[current];
        // Under the change's last line on screen: a change taller than the viewport still
        // gets its peek instead of one anchored below the fold.
        let first = hunk_line(hunk);
        let last = first + hunk.new_lines.len().saturating_sub(1);
        let Some(&(_, center)) = visible
            .iter()
            .filter(|(line, _)| (first..=last).contains(line))
            .max_by_key(|(line, _)| *line)
        else {
            return;
        };
        if ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            if let Some(quick) = self.conv.editor.quick_diff.as_mut() {
                quick.close_peek();
            }
            return;
        }

        let left = gutter_rect.right() + 6.0;
        // Let the foreground peek use the minimap's space too, leaving room for the
        // frame margins and scrollbar so longer source lines fit without scrolling.
        let width = (ui.clip_rect().right() - left - 24.0).clamp(260.0, 1280.0);
        let total = quick.hunks.len();
        let base_label = quick.compare_base.as_deref().unwrap_or("HEAD").to_owned();
        let mut action = None;
        egui::Area::new(ui.id().with("quick_diff_peek"))
            .order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(left, center + line_h * 0.5 + 2.0))
            .show(ui.ctx(), |ui| {
                egui::Frame::new()
                    .fill(c_bg_elevated())
                    .stroke(Stroke::new(1.0, c_border()))
                    .corner_radius(RADIUS_CHIP)
                    .inner_margin(Margin::symmetric(8, 6))
                    .show(ui, |ui| {
                        ui.set_width(width);
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            ui.label(
                                RichText::new(format!("Change {} of {total}", current + 1))
                                    .size(FS_SMALL)
                                    .color(c_text()),
                            );
                            ui.label(
                                RichText::new(format!("since {base_label}"))
                                    .size(FS_TINY)
                                    .color(c_text_faint()),
                            );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.spacing_mut().item_spacing.x = 2.0;
                                if crate::ui::chrome::icon_button_plain(ui, ICON_CLOSE, 20.0, false)
                                    .on_hover_text("Close (Esc)")
                                    .clicked()
                                {
                                    action = Some(PeekAction::Close);
                                }
                                if crate::ui::chrome::icon_button_plain(
                                    ui,
                                    ICON_ANGLE_DOWN,
                                    20.0,
                                    false,
                                )
                                .on_hover_text("Next change")
                                .clicked()
                                {
                                    action = Some(PeekAction::Go((current + 1) % total));
                                }
                                if crate::ui::chrome::icon_button_plain(
                                    ui,
                                    ICON_ANGLE_UP,
                                    20.0,
                                    false,
                                )
                                .on_hover_text("Previous change")
                                .clicked()
                                {
                                    action = Some(PeekAction::Go((current + total - 1) % total));
                                }
                                ui.add_space(6.0);
                                if crate::ui::chrome::ghost_button_icon(
                                    ui, ICON_UNDO, "Revert", false,
                                )
                                .on_hover_text(format!(
                                    "Put back the {base_label} version of these lines (undo with Cmd/Ctrl+Z)"
                                ))
                                .clicked()
                                {
                                    action = Some(PeekAction::Revert(current));
                                }
                            });
                        });
                        ui.add_space(4.0);
                        egui::ScrollArea::both()
                            .id_salt("quick_diff_lines")
                            .max_height(line_h * PEEK_MAX_LINES as f32)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                ui.spacing_mut().item_spacing.y = 0.0;
                                for (sign, lines, fill, fg) in [
                                    ('−', &hunk.old_lines, c_diff_del_bg(), c_diff_del_fg()),
                                    ('+', &hunk.new_lines, c_diff_add_bg(), c_diff_add_fg()),
                                ] {
                                    for line in lines.iter() {
                                        diff_line(ui, sign, line, fill, fg, line_h);
                                    }
                                }
                            });
                    });
            });

        match action {
            Some(PeekAction::Close) => {
                if let Some(quick) = self.conv.editor.quick_diff.as_mut() {
                    quick.close_peek();
                }
            }
            Some(PeekAction::Go(target)) => self.go_to_quick_diff_hunk(index, target, 0),
            Some(PeekAction::Revert(target)) => {
                let Some((edit, shift, count)) =
                    self.conv.editor.quick_diff.as_ref().map(|quick| {
                        let hunk = &quick.hunks[target];
                        let edit = crate::git::BlockEdit {
                            path: String::new(),
                            target: crate::git::BlockTarget::WorkTree,
                            start: hunk.new_start,
                            expected: hunk.new_lines.clone(),
                            replacement: hunk.old_lines.clone(),
                        };
                        let shift = hunk.old_lines.len() as isize - hunk.new_lines.len() as isize;
                        (edit, shift, quick.hunks.len())
                    })
                else {
                    return;
                };
                match self.edit_document_block(ui.ctx(), index, &edit) {
                    // Continue with the change that followed, like VS Code.
                    Ok(()) if count > 1 => {
                        let next = if target + 1 < count { target + 1 } else { 0 };
                        self.go_to_quick_diff_hunk(
                            index,
                            next,
                            if next > target { shift } else { 0 },
                        );
                    }
                    Ok(()) => {
                        if let Some(quick) = self.conv.editor.quick_diff.as_mut() {
                            quick.peek = None;
                        }
                    }
                    Err(error) => self.conv.editor.error = Some(error),
                }
            }
            None => {}
        }
    }

    /// Open the peek on hunk `target` (as numbered before an edit that moved later lines by
    /// `shift`) and bring it into view.
    fn go_to_quick_diff_hunk(&mut self, index: usize, target: usize, shift: isize) {
        let Some(quick) = self.conv.editor.quick_diff.as_mut() else {
            return;
        };
        let Some(hunk) = quick.hunks.get(target) else {
            quick.peek = None;
            return;
        };
        let line = hunk_line(hunk).saturating_add_signed(shift);
        quick.peek = Some(line);
        let Some(document) = self.conv.editor.documents.get(index) else {
            return;
        };
        let byte: usize = document
            .content
            .split_inclusive('\n')
            .take(line)
            .map(str::len)
            .sum();
        self.conv.editor.navigation_target = Some((document.path.clone(), byte..byte));
    }
}

fn diff_line(ui: &mut Ui, sign: char, text: &str, fill: egui::Color32, fg: egui::Color32, h: f32) {
    let font = FontId::monospace(FS_SMALL);
    let galley = ui
        .painter()
        .layout_no_wrap(text.replace('\t', "    "), font.clone(), c_text());
    let width = (galley.size().x + 28.0).max(ui.available_width());
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, h), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, fill);
    painter.text(
        egui::pos2(rect.left() + 8.0, rect.center().y),
        egui::Align2::CENTER_CENTER,
        sign,
        font,
        fg,
    );
    painter.galley(
        egui::pos2(rect.left() + 20.0, rect.center().y - galley.size().y / 2.0),
        galley,
        c_text(),
    );
}
