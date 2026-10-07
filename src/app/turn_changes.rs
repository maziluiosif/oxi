//! "N files changed" card under an agent turn that changed the workspace: the files with their
//! line counts, a diff per file (against the snapshot taken before the turn, with per-block
//! revert), and reverting one file or the whole turn.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use eframe::egui::{self, FontId, RichText, Sense, Ui};

use super::OxiApp;
use crate::git::{CompareFile, GitOp};
use crate::model::TurnChanges;
use crate::theme::*;

/// Per-turn view state, keyed by the turn's `before..after` trees.
#[derive(Default)]
pub(crate) struct TurnChangesCache {
    entries: HashMap<String, TurnEntry>,
}

struct TurnEntry {
    files: Result<Vec<CompareFile>, String>,
    expanded: bool,
    /// Paths put back from this card ("*" for the whole turn).
    reverted: HashSet<String>,
}

fn cache_key(changes: &TurnChanges) -> String {
    format!("{}..{}", changes.before, changes.after)
}

enum CardAction {
    Toggle,
    OpenDiff(String),
    RevertFile(String),
    RevertAll,
}

impl OxiApp {
    pub(crate) fn render_turn_changes(&mut self, ui: &mut Ui, changes: &TurnChanges) {
        let key = cache_key(changes);
        let entry = self
            .conv
            .transcript
            .turn_changes
            .entries
            .entry(key.clone())
            .or_insert_with(|| TurnEntry {
                files: crate::git::checkpoint::changed_files(
                    Path::new(&changes.repo),
                    &changes.before,
                    &changes.after,
                ),
                expanded: false,
                reverted: HashSet::new(),
            });
        let files = match &entry.files {
            Ok(files) if !files.is_empty() => files.clone(),
            // Snapshot gone (gc'd) or nothing changed: no card.
            _ => return,
        };
        let expanded = entry.expanded;
        let all_reverted = entry.reverted.contains("*");
        let reverted = entry.reverted.clone();
        let (added, deleted) = files
            .iter()
            .fold((0, 0), |(a, d), f| (a + f.added, d + f.deleted));

        let mut action = None;
        ui.add_space(4.0);
        egui::Frame::new()
            .fill(c_bg_elevated())
            .stroke(egui::Stroke::new(1.0, c_border()))
            .corner_radius(crate::theme::RADIUS_PANEL)
            .inner_margin(egui::Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let chevron = if expanded {
                        ICON_ANGLE_DOWN
                    } else {
                        ICON_CHEVRON_RIGHT
                    };
                    let icon = ui
                        .add(
                            egui::Label::new(
                                RichText::new(chevron)
                                    .font(FontId::new(FS_TINY, icon_font()))
                                    .color(c_text_muted()),
                            )
                            .sense(Sense::click())
                            .selectable(false),
                        )
                        .on_hover_cursor(egui::CursorIcon::PointingHand);
                    let header = ui
                        .add(
                            egui::Label::new(
                                RichText::new(format!(
                                    "{} file{} changed",
                                    files.len(),
                                    if files.len() == 1 { "" } else { "s" }
                                ))
                                .size(FS_SMALL)
                                .color(c_text()),
                            )
                            .sense(Sense::click())
                            .selectable(false),
                        )
                        .on_hover_cursor(egui::CursorIcon::PointingHand);
                    if header.clicked() || icon.clicked() {
                        action = Some(CardAction::Toggle);
                    }
                    ui.label(
                        RichText::new(format!("+{added}"))
                            .font(FontId::monospace(FS_TINY))
                            .color(c_success()),
                    );
                    ui.label(
                        RichText::new(format!("−{deleted}"))
                            .font(FontId::monospace(FS_TINY))
                            .color(c_danger()),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if all_reverted {
                            ui.label(
                                RichText::new("Reverted")
                                    .size(FS_TINY)
                                    .color(c_text_muted()),
                            );
                        } else if crate::ui::chrome::ghost_button_icon(
                            ui, ICON_UNDO, "Revert", true,
                        )
                        .on_hover_text("Put back every file this response changed")
                        .clicked()
                        {
                            action = Some(CardAction::RevertAll);
                        }
                    });
                });
                if expanded {
                    ui.add_space(2.0);
                    for file in &files {
                        let file_reverted = all_reverted || reverted.contains(&file.path);
                        if let Some(a) = render_file_row(ui, file, file_reverted) {
                            action = Some(a);
                        }
                    }
                }
            });

        match action {
            Some(CardAction::Toggle) => {
                if let Some(entry) = self.conv.transcript.turn_changes.entries.get_mut(&key) {
                    entry.expanded = !entry.expanded;
                }
            }
            Some(CardAction::OpenDiff(path)) => {
                self.request(GitOp::ShowCompareDiff {
                    base: format!(
                        "{}{}",
                        crate::git::checkpoint::TURN_BASE_PREFIX,
                        changes.before
                    ),
                    path,
                    old_path: None,
                });
                self.conv.diff_view.open = true;
                self.conv.editor.diff_tab_active = true;
            }
            Some(CardAction::RevertFile(path)) => {
                self.revert_turn_changes(changes, &key, Some(path));
            }
            Some(CardAction::RevertAll) => self.revert_turn_changes(changes, &key, None),
            None => {}
        }
    }

    fn revert_turn_changes(&mut self, changes: &TurnChanges, key: &str, only: Option<String>) {
        let result = crate::git::checkpoint::restore(
            Path::new(&changes.repo),
            &changes.before,
            &changes.after,
            only.as_deref(),
        );
        match result {
            Ok(()) => {
                if let Some(entry) = self.conv.transcript.turn_changes.entries.get_mut(key) {
                    entry.reverted.insert(only.unwrap_or_else(|| "*".into()));
                }
                self.conv.explorer.cache.invalidate();
                self.request(GitOp::Refresh);
                self.notify_composer("Changes reverted.");
            }
            Err(e) => self.notify_composer(format!("Revert failed: {e}")),
        }
    }
}

fn render_file_row(ui: &mut Ui, file: &CompareFile, reverted: bool) -> Option<CardAction> {
    let mut action = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(
            RichText::new(file.status.to_string())
                .font(FontId::monospace(FS_TINY))
                .color(crate::app::file_explorer::git_status_color(file.status)),
        );
        let name = RichText::new(&file.path).size(FS_SMALL);
        let name = if reverted || file.status == 'D' {
            name.strikethrough().color(c_text_muted())
        } else {
            name.color(c_text())
        };
        let row = ui
            .add(
                egui::Label::new(name)
                    .truncate()
                    .sense(Sense::click())
                    .selectable(false),
            )
            .on_hover_text("Show the diff against the files before this response")
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if row.clicked() && !reverted {
            action = Some(CardAction::OpenDiff(file.path.clone()));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if !reverted
                && ui
                    .add(
                        egui::Button::new(
                            RichText::new(ICON_UNDO)
                                .font(FontId::new(FS_TINY, icon_font()))
                                .color(c_text_muted()),
                        )
                        .frame(false),
                    )
                    .on_hover_text("Revert this file")
                    .clicked()
            {
                action = Some(CardAction::RevertFile(file.path.clone()));
            }
            if file.deleted > 0 {
                ui.label(
                    RichText::new(format!("−{}", file.deleted))
                        .font(FontId::monospace(FS_TINY))
                        .color(c_danger()),
                );
            }
            if file.added > 0 {
                ui.label(
                    RichText::new(format!("+{}", file.added))
                        .font(FontId::monospace(FS_TINY))
                        .color(c_success()),
                );
            }
        });
    });
    action
}
