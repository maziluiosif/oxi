//! "N files changed" card at the bottom of a chat that changed the workspace: one consolidated
//! summary of every file the chat modified, with net line counts, a diff per file (against the
//! snapshot taken before the chat touched it), cross-agent review, and reverting one file or all
//! of the chat's changes.
//!
//! Per-turn snapshots stay on each message (for message-level rewind and persistence), while the
//! card consolidates them into a single net diff computed off the UI thread; nothing here runs
//! git, or asks an SSH host, on the UI thread.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::mpsc::{Receiver, TryRecvError};

use eframe::egui::{self, FontId, RichText, Sense, Ui};

use super::OxiApp;
use crate::git::{CompareFile, GitOp};
use crate::model::{ChatMessage, TurnChanges};
use crate::settings::LlmProviderKind;
use crate::theme::*;

/// Consolidated workspace changes across all completed turns of a chat (in its active repo).
#[derive(Clone, Debug)]
pub(crate) struct ChatChanges {
    pub repo: String,
    /// `(before, after)` snapshot pairs in chronological order (non-empty, each `before != after`).
    pub turns: Vec<(String, String)>,
    /// Pre-computed file list when there is a single turn that already persisted its `files`.
    pub single_turn_files: Option<Vec<CompareFile>>,
    /// Agent that made the latest change in `turns`, when known.
    pub author: Option<LlmProviderKind>,
}

impl ChatChanges {
    #[cfg(test)]
    pub(crate) fn from_single(
        changes: &TurnChanges,
        author: Option<LlmProviderKind>,
    ) -> Option<Self> {
        if changes.before == changes.after {
            return None;
        }
        Some(Self {
            repo: changes.repo.clone(),
            turns: vec![(changes.before.clone(), changes.after.clone())],
            single_turn_files: changes.files.clone(),
            author,
        })
    }

    pub(crate) fn before(&self) -> &str {
        &self.turns[0].0
    }

    pub(crate) fn after(&self) -> &str {
        &self.turns[self.turns.len() - 1].1
    }

    fn as_turn_changes(&self, files: Option<Vec<CompareFile>>) -> TurnChanges {
        TurnChanges {
            repo: self.repo.clone(),
            before: self.before().to_string(),
            after: self.after().to_string(),
            files,
        }
    }
}

/// Collect every completed turn's changes in `messages` into a single consolidated chat summary.
/// When a chat has multiple sub-turns (e.g. background-task wakeups) or multiple user turns, they
/// fold into one net card instead of stacking several cards.
pub(crate) fn collect_chat_changes(messages: &[ChatMessage]) -> Option<ChatChanges> {
    let entries: Vec<(&TurnChanges, Option<LlmProviderKind>)> = messages
        .iter()
        .filter(|m| !m.streaming)
        .filter_map(|m| {
            let changes = m.changes.as_deref()?;
            if changes.before == changes.after {
                return None;
            }
            Some((changes, m.route.as_deref().map(|r| r.provider)))
        })
        .collect();
    let (last, last_author) = *entries.last()?;
    let repo = last.repo.clone();
    let matching: Vec<&TurnChanges> = entries
        .iter()
        .filter(|(c, _)| c.repo == repo)
        .map(|(c, _)| *c)
        .collect();
    let turns: Vec<(String, String)> = matching
        .iter()
        .map(|c| (c.before.clone(), c.after.clone()))
        .collect();
    if turns.first().map(|(b, _)| b) == turns.last().map(|(_, a)| a) {
        return None;
    }
    let single_turn_files = (matching.len() == 1)
        .then(|| matching[0].files.clone())
        .flatten();
    Some(ChatChanges {
        repo,
        turns,
        single_turn_files,
        author: last_author,
    })
}

/// Compute the net changed file list for `turns` off the UI thread.
/// Contiguous snapshot chains (`t0..t1`, `t1..t2`, …) reduce to one `t0..tN` tree diff; when the
/// user edited files between turns (`t1 != t2_before`), unrelated manual edits are excluded by
/// intersecting with the paths actually touched by the agent turns.
fn compute_chat_changed_files(
    repo: &Path,
    turns: &[(String, String)],
) -> Result<Vec<CompareFile>, String> {
    let Some((first_before, _)) = turns.first() else {
        return Ok(Vec::new());
    };
    let Some((_, last_after)) = turns.last() else {
        return Ok(Vec::new());
    };
    if first_before == last_after {
        return Ok(Vec::new());
    }
    if turns.len() == 1 || turns.windows(2).all(|w| w[0].1 == w[1].0) {
        return crate::git::checkpoint::changed_files(repo, first_before, last_after);
    }
    let mut touched = HashSet::new();
    for (before, after) in turns {
        for file in crate::git::checkpoint::changed_files(repo, before, after)? {
            touched.insert(file.path);
        }
    }
    let net = crate::git::checkpoint::changed_files(repo, first_before, last_after)?;
    Ok(net
        .into_iter()
        .filter(|f| touched.contains(&f.path))
        .collect())
}

/// Per-turn view state, keyed by the turn's `before..after` trees.
#[derive(Default)]
pub(crate) struct TurnChangesCache {
    entries: HashMap<String, TurnEntry>,
    /// Background diffs and reverts, applied by [`OxiApp::drain_turn_changes`].
    jobs: Vec<Job>,
}

enum Job {
    Load {
        key: String,
        rx: Receiver<Result<Vec<CompareFile>, String>>,
    },
    Revert {
        key: String,
        only: Option<String>,
        rx: Receiver<Result<(), String>>,
    },
}

struct TurnEntry {
    /// `None` while the list is computed in the background.
    files: Option<Result<Vec<CompareFile>, String>>,
    expanded: bool,
    /// Paths put back from this card ("*" for the whole turn).
    reverted: HashSet<String>,
    /// A revert from this card is running.
    reverting: bool,
}

/// Run `work` on a background thread; its result arrives on the returned channel.
fn spawn<T: Send + 'static>(
    ctx: &egui::Context,
    work: impl FnOnce() -> T + Send + 'static,
) -> Receiver<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let _ = tx.send(work());
        ctx.request_repaint();
    });
    rx
}

/// Inset of the header row from the card's top, right and bottom edges.
const CARD_INSET: f32 = 4.0;
/// The header row's height, keeping the card compact and proportional to tool pills.
const HEADER_H: f32 = 24.0;
const FILE_ROW_H: f32 = SIDEBAR_ROW_H;
/// Chevron width plus its gap, so file rows start under "N files changed".
const FILE_ROW_INDENT: f32 = 14.0;

fn cache_key(changes: &TurnChanges) -> String {
    format!("{}..{}", changes.before, changes.after)
}

fn chat_cache_key(changes: &ChatChanges) -> String {
    if changes.turns.len() == 1 {
        format!("{}..{}", changes.before(), changes.after())
    } else {
        let mut out = String::new();
        for (i, (b, a)) in changes.turns.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(b);
            out.push_str("..");
            out.push_str(a);
        }
        out
    }
}

enum CardAction {
    Toggle,
    OpenDiff(String),
    RevertFile(String),
    RevertAll,
    Review(LlmProviderKind),
}

impl OxiApp {
    /// Render a single `TurnChanges` card (kept as a convenience wrapper for tests).
    #[cfg(test)]
    pub(crate) fn render_turn_changes(
        &mut self,
        ui: &mut Ui,
        changes: &TurnChanges,
        author: Option<LlmProviderKind>,
    ) {
        if let Some(chat) = ChatChanges::from_single(changes, author) {
            self.render_chat_changes(ui, &chat);
        }
    }

    /// Render the single consolidated "N files changed" card for the active chat.
    pub(crate) fn render_chat_changes(&mut self, ui: &mut Ui, changes: &ChatChanges) {
        if changes.turns.is_empty() || changes.before() == changes.after() {
            return;
        }
        let key = chat_cache_key(changes);
        if !self.conv.transcript.turn_changes.entries.contains_key(&key) {
            let files = changes.single_turn_files.clone().map(Ok);
            if files.is_none() {
                let repo = self.turn_repo(&changes.repo);
                let turns = changes.turns.clone();
                let rx = spawn(ui.ctx(), move || compute_chat_changed_files(&repo, &turns));
                self.conv.transcript.turn_changes.jobs.push(Job::Load {
                    key: key.clone(),
                    rx,
                });
            }
            self.conv.transcript.turn_changes.entries.insert(
                key.clone(),
                TurnEntry {
                    files,
                    expanded: false,
                    reverted: HashSet::new(),
                    reverting: false,
                },
            );
        }
        let entry = &self.conv.transcript.turn_changes.entries[&key];
        let files = match &entry.files {
            Some(Ok(files)) if !files.is_empty() => files.clone(),
            None => {
                render_loading_card(ui);
                return;
            }
            // Snapshot gone (gc'd) or nothing changed: no card.
            _ => return,
        };
        let expanded = entry.expanded;
        let all_reverted = entry.reverted.contains("*");
        let reverted = entry.reverted.clone();
        let reverting = entry.reverting;
        let author = changes.author;
        let reviewers = if all_reverted {
            Vec::new()
        } else {
            let session = self.active_session_key();
            self.review_candidates(session, author)
        };
        let busy = self.active_waiting_response();
        let (added, deleted) = files
            .iter()
            .fold((0, 0), |(a, d), f| (a + f.added, d + f.deleted));

        let mut action = None;
        egui::Frame::new()
            .fill(c_md_code_block_bg())
            .stroke(egui::Stroke::new(1.0, c_border()))
            .corner_radius(crate::theme::RADIUS_CHIP)
            .inner_margin(egui::Margin {
                left: 10,
                right: 6,
                top: CARD_INSET as i8,
                bottom: CARD_INSET as i8,
            })
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let (header_rect, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), HEADER_H),
                    Sense::hover(),
                );
                let header_bg_slot = ui.painter().add(egui::Shape::Noop);
                let mut right_edge = header_rect.right();

                // Lay out right-side action buttons first so we know their exact bounds and can
                // treat the entire rest of the header bar (text, +N/-M, chevron, and open space)
                // as one clickable expand/collapse surface.
                ui.scope_builder(
                    egui::UiBuilder::new()
                        .max_rect(header_rect)
                        .layout(egui::Layout::right_to_left(egui::Align::Center)),
                    |ui| {
                        ui.spacing_mut().item_spacing.x = 5.0;
                        let has_reviewer = !reviewers.is_empty();
                        if all_reverted || reverting {
                            let r = ui.label(
                                RichText::new(if reverting {
                                    "Reverting…"
                                } else {
                                    "Reverted"
                                })
                                .size(FS_TINY)
                                .color(c_text_muted()),
                            );
                            right_edge = right_edge.min(r.rect.left());
                        } else {
                            let revert = crate::ui::chrome::mini_button_icon_danger(
                                ui, ICON_UNDO, "Revert", true,
                            )
                            .on_hover_text("Put back every file changed in this chat");
                            right_edge = right_edge.min(revert.rect.left());
                            if revert.clicked() {
                                action = Some(CardAction::RevertAll);
                            }
                        }
                        if has_reviewer && !all_reverted && !reverting {
                            let (sep_rect, _) =
                                ui.allocate_exact_size(egui::vec2(5.0, 14.0), Sense::hover());
                            ui.painter().vline(
                                sep_rect.center().x,
                                sep_rect.y_range(),
                                egui::Stroke::new(1.0, c_border_subtle()),
                            );
                            right_edge = right_edge.min(sep_rect.left());
                        }
                        if let Some(&reviewer) = reviewers.first() {
                            let label = crate::app::composer::composer_provider_label;
                            if reviewers.len() > 1 {
                                let picker_id = ui.id().with(("review_agent_picker", &key));
                                let picker_btn = crate::ui::chrome::mini_button_icon_enabled(
                                    ui,
                                    ICON_ANGLE_DOWN,
                                    "",
                                    !busy,
                                )
                                .on_hover_text("Choose another agent to review these changes");
                                right_edge = right_edge.min(picker_btn.rect.left());
                                egui::Popup::from_toggle_button_response(&picker_btn)
                                    .id(picker_id)
                                    .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                                    .show(|ui| {
                                        ui.set_min_width(150.0);
                                        for &other in &reviewers {
                                            if ui
                                                .button(format!("Review with {}", label(other)))
                                                .clicked()
                                            {
                                                action = Some(CardAction::Review(other));
                                                ui.close();
                                            }
                                        }
                                    });
                            }
                            let button = crate::ui::chrome::mini_button_icon_enabled(
                                ui,
                                ICON_SEARCH,
                                &format!("Review with {}", label(reviewer)),
                                !busy,
                            )
                            .on_hover_text(if reviewers.len() > 1 {
                                "Ask another agent to review these changes (click ▾ to choose another)"
                            } else {
                                "Ask another agent to review these changes in this chat"
                            });
                            right_edge = right_edge.min(button.rect.left());
                            if button.clicked() {
                                action = Some(CardAction::Review(reviewer));
                            }
                            if !busy {
                                button.context_menu(|ui| {
                                    for &other in &reviewers {
                                        if ui
                                            .button(format!("Review with {}", label(other)))
                                            .clicked()
                                        {
                                            action = Some(CardAction::Review(other));
                                            ui.close();
                                        }
                                    }
                                });
                            }
                        }
                    },
                );

                let toggle_rect = egui::Rect::from_min_max(
                    header_rect.min,
                    egui::pos2((right_edge - 6.0).max(header_rect.left()), header_rect.bottom()),
                );
                let toggle_resp = ui.interact(
                    toggle_rect,
                    ui.id().with(("turn_changes_toggle", &key)),
                    Sense::click(),
                );
                let title_text = format!(
                    "{} file{} changed",
                    files.len(),
                    if files.len() == 1 { "" } else { "s" }
                );
                toggle_resp.widget_info(|| {
                    egui::WidgetInfo::selected(
                        egui::WidgetType::CollapsingHeader,
                        ui.is_enabled(),
                        expanded,
                        &title_text,
                    )
                });
                let header_hovered = toggle_resp.hovered() || toggle_resp.has_focus();
                if header_hovered {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    ui.painter().set(
                        header_bg_slot,
                        egui::Shape::rect_filled(
                            toggle_rect.expand2(egui::vec2(4.0, 0.0)),
                            egui::CornerRadius::same(RADIUS_ROW),
                            c_row_hover(),
                        ),
                    );
                }
                crate::ui::chrome::keyboard_focus_ring(
                    ui,
                    &toggle_resp,
                    egui::CornerRadius::same(RADIUS_ROW),
                );

                ui.scope_builder(
                    egui::UiBuilder::new()
                        .max_rect(toggle_rect)
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                    |ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        let chevron = if expanded {
                            ICON_ANGLE_DOWN
                        } else {
                            ICON_CHEVRON_RIGHT
                        };
                        ui.add(
                            egui::Label::new(
                                RichText::new(chevron)
                                    .font(FontId::new(FS_TINY - 1.5, icon_font()))
                                    .color(if header_hovered {
                                        c_text_muted()
                                    } else {
                                        c_text_faint()
                                    }),
                            )
                            .selectable(false),
                        );
                        ui.add(
                            egui::Label::new(
                                RichText::new(&title_text)
                                    .size(FS_SMALL)
                                    .color(if header_hovered { c_text_strong() } else { c_text() }),
                            )
                            .selectable(false),
                        );
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!("+{added}"))
                                    .font(FontId::monospace(FS_TINY))
                                    .color(c_diff_add_fg()),
                            )
                            .selectable(false),
                        );
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!("-{deleted}"))
                                    .font(FontId::monospace(FS_TINY))
                                    .color(c_diff_del_fg()),
                            )
                            .selectable(false),
                        );
                    },
                );

                if action.is_none() && toggle_resp.clicked() {
                    action = Some(CardAction::Toggle);
                }

                if expanded {
                    ui.add_space(4.0);
                    for file in &files {
                        let file_reverted =
                            all_reverted || reverting || reverted.contains(&file.path);
                        if let Some(a) = render_file_row(ui, file, file_reverted) {
                            action = Some(a);
                        }
                    }
                }
            });
        // Breathing room before the composer.
        ui.add_space(12.0);

        match action {
            Some(CardAction::Toggle) => {
                if let Some(entry) = self.conv.transcript.turn_changes.entries.get_mut(&key) {
                    entry.expanded = !entry.expanded;
                }
            }
            Some(CardAction::OpenDiff(path)) => {
                let base = format!(
                    "{}{}",
                    crate::git::checkpoint::TURN_BASE_PREFIX,
                    changes.before()
                );
                let source = crate::app::file_explorer::DiffSource::Compare {
                    base: base.clone(),
                    old_path: None,
                };
                if !self.open_diff_editor(&path, source, None) {
                    self.request(GitOp::ShowCompareDiff {
                        base,
                        path,
                        old_path: None,
                    });
                    self.conv.diff_view.open = true;
                    self.conv.editor.diff_tab_active = true;
                }
            }
            Some(CardAction::RevertFile(path)) => {
                self.revert_chat_changes(ui.ctx(), changes, &key, Some(path));
            }
            Some(CardAction::RevertAll) => self.revert_chat_changes(ui.ctx(), changes, &key, None),
            Some(CardAction::Review(reviewer)) => {
                let merged = changes.as_turn_changes(Some(files));
                self.review_turn_changes(&merged, author, reviewer)
            }
            None => {}
        }
    }

    /// Put back the files of a chat (or one of them) in the background; the card shows the
    /// outcome once [`Self::drain_turn_changes`] sees it.
    fn revert_chat_changes(
        &mut self,
        ctx: &egui::Context,
        changes: &ChatChanges,
        key: &str,
        only: Option<String>,
    ) {
        let cache = &mut self.conv.transcript.turn_changes;
        let Some(entry) = cache.entries.get_mut(key) else {
            return;
        };
        if entry.reverting {
            return;
        }
        entry.reverting = true;
        let repo = self.turn_repo(&changes.repo);
        let turns = changes.turns.clone();
        let path = only.clone();
        let rx = spawn(ctx, move || {
            crate::git::checkpoint::restore_turns(&repo, &turns, path.as_deref())
        });
        self.conv.transcript.turn_changes.jobs.push(Job::Revert {
            key: key.to_string(),
            only,
            rx,
        });
    }

    /// Apply finished background diffs and reverts. A diffed file list is also stored on the
    /// turns it belongs to, so the chat's next save keeps it.
    pub(crate) fn drain_turn_changes(&mut self) {
        let cache = &mut self.conv.transcript.turn_changes;
        if cache.jobs.is_empty() {
            return;
        }
        let mut loaded = Vec::new();
        let mut reverts = Vec::new();
        cache.jobs.retain(|job| match job {
            Job::Load { key, rx } => {
                let result = match rx.try_recv() {
                    Ok(result) => result,
                    Err(TryRecvError::Empty) => return true,
                    Err(TryRecvError::Disconnected) => Err("The diff stopped".to_string()),
                };
                loaded.push((key.clone(), result));
                false
            }
            Job::Revert { key, only, rx } => {
                let result = match rx.try_recv() {
                    Ok(result) => result,
                    Err(TryRecvError::Empty) => return true,
                    Err(TryRecvError::Disconnected) => Err("The revert stopped".to_string()),
                };
                reverts.push((key.clone(), only.clone(), result));
                false
            }
        });
        for (key, result) in loaded {
            if let Ok(files) = &result {
                for workspace in &mut self.conv.workspaces {
                    for session in &mut workspace.sessions {
                        for message in &mut session.messages {
                            if let Some(changes) = message.changes.as_deref_mut()
                                && changes.files.is_none()
                                && cache_key(changes) == key
                            {
                                changes.files = Some(files.clone());
                            }
                        }
                    }
                }
            }
            if let Some(entry) = self.conv.transcript.turn_changes.entries.get_mut(&key) {
                entry.files = Some(result);
            }
        }
        for (key, only, result) in reverts {
            if let Some(entry) = self.conv.transcript.turn_changes.entries.get_mut(&key) {
                entry.reverting = false;
                if result.is_ok() {
                    entry.reverted.insert(only.unwrap_or_else(|| "*".into()));
                }
            }
            match result {
                Ok(()) => {
                    self.conv.explorer.cache.invalidate();
                    self.request(GitOp::Refresh);
                    self.notify_composer("Changes reverted.");
                }
                Err(e) => self.notify_composer(format!("Revert failed: {e}")),
            }
        }
    }
}

/// Placeholder while an older turn's file list is diffed in the background.
fn render_loading_card(ui: &mut Ui) {
    egui::Frame::new()
        .fill(c_bg_elevated())
        .stroke(egui::Stroke::new(1.0, c_border()))
        .corner_radius(crate::theme::RADIUS_CARD)
        .inner_margin(egui::Margin {
            left: 14,
            right: CARD_INSET as i8,
            top: CARD_INSET as i8,
            bottom: CARD_INSET as i8,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.set_min_height(HEADER_H);
                crate::theme::small_spinner(ui);
                ui.label(
                    RichText::new("Changed files")
                        .size(FS_SMALL)
                        .color(c_text_muted()),
                );
            });
        });
    ui.add_space(12.0);
}

fn render_file_row(ui: &mut Ui, file: &CompareFile, reverted: bool) -> Option<CardAction> {
    let mut action = None;
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), FILE_ROW_H), Sense::click());
    let row_hot = ui
        .input(|i| i.pointer.hover_pos())
        .is_some_and(|p| rect.contains(p));
    let mut revert_hovered = false;
    let mut revert_clicked = false;
    let bg_slot = ui.painter().add(egui::Shape::Noop);
    let inner = egui::Rect::from_min_max(
        egui::pos2(rect.left() + FILE_ROW_INDENT, rect.top()),
        egui::pos2(rect.right() - 4.0, rect.bottom()),
    );
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(inner)
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
        |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if !reverted {
                let undo =
                    crate::ui::chrome::icon_button_inline(ui, ICON_UNDO, FS_TINY, c_text_muted())
                        .on_hover_text("Revert this file");
                revert_hovered = undo.hovered();
                if undo.clicked() {
                    revert_clicked = true;
                    action = Some(CardAction::RevertFile(file.path.clone()));
                }
            }
            if file.deleted > 0 {
                ui.add(
                    egui::Label::new(
                        RichText::new(format!("−{}", file.deleted))
                            .font(FontId::monospace(FS_TINY))
                            .color(c_danger()),
                    )
                    .selectable(false),
                );
            }
            if file.added > 0 {
                ui.add(
                    egui::Label::new(
                        RichText::new(format!("+{}", file.added))
                            .font(FontId::monospace(FS_TINY))
                            .color(c_success()),
                    )
                    .selectable(false),
                );
            }
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.add(
                    egui::Label::new(
                        RichText::new(file.status.to_string())
                            .font(FontId::monospace(FS_TINY))
                            .color(crate::app::file_explorer::git_status_color(file.status)),
                    )
                    .selectable(false),
                );
                let name = RichText::new(&file.path).size(FS_SMALL);
                let name = if reverted || file.status == 'D' {
                    name.strikethrough().color(c_text_muted())
                } else {
                    name.color(c_text())
                };
                ui.add(egui::Label::new(name).truncate().selectable(false));
            });
        },
    );
    if !reverted && row_hot && !revert_hovered {
        ui.painter().set(
            bg_slot,
            egui::Shape::rect_filled(rect, egui::CornerRadius::same(RADIUS_ROW), c_row_hover()),
        );
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let response = response.on_hover_text("Show the diff against the files before this chat");
    if !revert_clicked && !revert_hovered && !reverted && response.clicked() {
        action = Some(CardAction::OpenDiff(file.path.clone()));
    }
    action
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AssistantBlock, ChatMessage};

    #[test]
    fn older_turns_are_diffed_off_the_ui_thread_and_keep_the_result() {
        crate::secrets::use_mock_store();
        let root = crate::fsutil::unique_temp_path(&std::env::temp_dir(), "oxi-turn-card", "d");
        std::fs::create_dir_all(&root).unwrap();
        git2::Repository::init(&root).unwrap();
        std::fs::write(root.join("a.txt"), "one\n").unwrap();
        let before = crate::git::checkpoint::snapshot(&root).unwrap();
        std::fs::write(root.join("a.txt"), "one\ntwo\n").unwrap();
        let after = crate::git::checkpoint::snapshot(&root).unwrap();
        let changes = TurnChanges {
            repo: before.repo.to_string_lossy().into_owned(),
            before: before.tree,
            after: after.tree,
            files: None,
        };

        let mut app = OxiApp::new();
        let mut reply = ChatMessage::streaming_assistant(None);
        reply.blocks.push(AssistantBlock::Answer("done".into()));
        reply.finish_streaming();
        reply.changes = Some(Box::new(changes.clone()));
        app.active_session_mut().messages = vec![reply];

        let ctx = egui::Context::default();
        let frame = |app: &mut OxiApp| {
            let _ = ctx.run_ui(Default::default(), |ui| {
                app.render_turn_changes(ui, &changes, None);
            });
        };
        frame(&mut app);
        assert!(
            app.conv.transcript.turn_changes.entries[&cache_key(&changes)]
                .files
                .is_none(),
            "the first frame only starts the diff"
        );
        for _ in 0..500 {
            app.drain_turn_changes();
            if app.conv.transcript.turn_changes.jobs.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let saved = app.active_session().messages[0]
            .changes
            .as_deref()
            .and_then(|c| c.files.clone())
            .expect("the diffed list is stored on the turn");
        assert_eq!(saved.len(), 1);
        assert_eq!((saved[0].path.as_str(), saved[0].added), ("a.txt", 1));
        let json =
            serde_json::to_value(app.active_session().messages[0].changes.as_deref()).unwrap();
        assert_eq!(json["files"][0]["path"], "a.txt");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn chat_changes_consolidates_multiple_turns_into_one_net_diff() {
        crate::secrets::use_mock_store();
        let root = crate::fsutil::unique_temp_path(&std::env::temp_dir(), "oxi-chat-changes", "d");
        std::fs::create_dir_all(&root).unwrap();
        git2::Repository::init(&root).unwrap();
        std::fs::write(root.join("a.txt"), "base\n").unwrap();
        std::fs::write(root.join("unrelated.txt"), "keep\n").unwrap();
        let t0 = crate::git::checkpoint::snapshot(&root).unwrap();

        // Turn 1: edits a.txt and creates temp.txt
        std::fs::write(root.join("a.txt"), "base\nturn1\n").unwrap();
        std::fs::write(root.join("temp.txt"), "tmp\n").unwrap();
        let t1 = crate::git::checkpoint::snapshot(&root).unwrap();

        // User edits unrelated.txt between turns
        std::fs::write(root.join("unrelated.txt"), "manual user edit\n").unwrap();
        let t2_before = crate::git::checkpoint::snapshot(&root).unwrap();

        // Turn 2: updates a.txt again, adds b.txt, removes temp.txt
        std::fs::write(root.join("a.txt"), "base\nturn1\nturn2\n").unwrap();
        std::fs::write(root.join("b.txt"), "new file\n").unwrap();
        std::fs::remove_file(root.join("temp.txt")).unwrap();
        let t2_after = crate::git::checkpoint::snapshot(&root).unwrap();

        let mk = |before: &str, after: &str| {
            let mut m = ChatMessage::streaming_assistant(None);
            m.blocks.push(AssistantBlock::Answer("ok".into()));
            m.finish_streaming();
            m.changes = Some(Box::new(TurnChanges {
                repo: t0.repo.to_string_lossy().into_owned(),
                before: before.into(),
                after: after.into(),
                files: None,
            }));
            m
        };
        let messages = vec![mk(&t0.tree, &t1.tree), mk(&t2_before.tree, &t2_after.tree)];
        let consolidated = collect_chat_changes(&messages).expect("consolidated chat changes");
        assert_eq!(consolidated.turns.len(), 2);
        assert_eq!(consolidated.before(), t0.tree);
        assert_eq!(consolidated.after(), t2_after.tree);

        let files = compute_chat_changed_files(&t0.repo, &consolidated.turns).unwrap();
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        // Net result: a.txt (+2), b.txt (+1); temp.txt cancelled out and unrelated.txt excluded.
        assert_eq!(paths, vec!["a.txt", "b.txt"]);
        assert_eq!(files[0].added, 2);
        assert_eq!(files[1].added, 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn changed_files_header_only_toggles_on_unobstructed_clicks() {
        crate::secrets::use_mock_store();
        let changes = TurnChanges {
            repo: "/tmp/demo".into(),
            before: "aaa".into(),
            after: "bbb".into(),
            files: Some(vec![CompareFile {
                path: "src/main.rs".into(),
                old_path: None,
                status: 'M',
                added: 4,
                deleted: 1,
            }]),
        };
        let key = cache_key(&changes);
        let overlay_id = egui::Id::new("test_tc_overlay");
        let mut app = OxiApp::new();
        let mut setup = false;
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(640.0, 200.0))
            .build_ui(move |ui| {
                if !setup {
                    crate::theme::apply_theme(ui.ctx(), "dark");
                    setup = true;
                    return;
                }
                let card_rect = ui
                    .vertical(|ui| {
                        app.render_turn_changes(ui, &changes, None);
                    })
                    .response
                    .rect;
                let expanded = app
                    .conv
                    .transcript
                    .turn_changes
                    .entries
                    .get(&key)
                    .is_some_and(|e| e.expanded);
                ui.ctx().data_mut(|d| {
                    d.insert_temp(egui::Id::new("test_tc_expanded"), expanded);
                    d.insert_temp(egui::Id::new("test_tc_rect"), card_rect);
                });
                if ui.ctx().data(|d| d.get_temp::<bool>(overlay_id)) == Some(true) {
                    egui::Modal::new(overlay_id).show(ui.ctx(), |ui| {
                        ui.label("Blocking dialog");
                    });
                }
            });
        harness.run_steps(3);
        let is_expanded = |h: &egui_kittest::Harness<'_>| {
            h.ctx
                .data(|d| d.get_temp::<bool>(egui::Id::new("test_tc_expanded")))
                .unwrap_or(false)
        };
        let card_rect = harness
            .ctx
            .data(|d| d.get_temp::<egui::Rect>(egui::Id::new("test_tc_rect")))
            .expect("card rect recorded");
        assert!(!is_expanded(&harness), "starts collapsed");
        let click_at = |h: &mut egui_kittest::Harness<'_>, pos: egui::Pos2| {
            h.input_mut().events.extend([
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ]);
            h.run_steps(2);
        };
        // Click in the open space past "1 file changed +4 -1" (well left of the right actions).
        let open_header_pos = egui::pos2(card_rect.left() + 200.0, card_rect.top() + 16.0);
        click_at(&mut harness, open_header_pos);
        assert!(is_expanded(&harness), "clicking anywhere on header expands");
        // Click on the +4/-1 stats area to collapse back.
        let stats_pos = egui::pos2(card_rect.left() + 135.0, card_rect.top() + 16.0);
        click_at(&mut harness, stats_pos);
        assert!(!is_expanded(&harness), "second click collapses");

        harness.ctx.data_mut(|d| d.insert_temp(overlay_id, true));
        harness.run_steps(3);
        click_at(&mut harness, open_header_pos);
        assert!(!is_expanded(&harness), "a dialog blocks the click");
    }
}
