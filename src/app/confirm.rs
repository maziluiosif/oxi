//! Shared confirmation flow for destructive actions: one pending [`ConfirmAction`]
//! app-wide, rendered as a centered modal over everything (see `chrome::confirm_modal`).

use eframe::egui;

use eframe::egui::RichText;

use crate::git::worktree::RemovalRisk;
use crate::theme::{FS_SMALL, FS_TINY, c_error_fg, c_text, c_text_muted, c_warning_fg};
use crate::ui::chrome::{ModalOutcome, confirm_modal};

use super::OxiApp;
use super::rewind::RewindCheck;
use super::state::ConfirmAction;
use super::worktrees::WorktreeRiskCheck;

impl OxiApp {
    /// Queue a destructive action for confirmation. The modal renders on top of the
    /// whole window at the end of the frame.
    pub(crate) fn request_confirm(&mut self, mut action: ConfirmAction) {
        // Removing the work tree too is the default when nothing else in it would go: its
        // last chat, or the whole work tree workspace.
        let default_remove = match action {
            ConfirmAction::DeleteSession { wi, .. } => self
                .conv
                .workspaces
                .get(wi)
                .is_some_and(|w| w.worktree.is_some() && w.sessions.len() == 1),
            ConfirmAction::DeleteWorkspace { wi, .. } => self
                .conv
                .workspaces
                .get(wi)
                .is_some_and(|w| w.worktree.is_some()),
            _ => false,
        };
        if let Some(flag) = remove_worktree_flag(&mut action) {
            *flag = default_remove;
        }
        // Find out what removing the work tree would lose while the user reads the dialog.
        match action {
            ConfirmAction::DeleteSession { wi, .. }
            | ConfirmAction::DeleteWorkspace { wi, .. }
            | ConfirmAction::RemoveWorktree { wi } => self.start_worktree_risk_check(wi),
            _ => self.conv.worktree_risk = None,
        }
        self.conv.rewind_check = None;
        if let ConfirmAction::Rewind {
            wi,
            si,
            index,
            ref mut restore_files,
        } = action
        {
            let ctx = self.conv.git_ctx.clone();
            let key = self.session_key(wi, si);
            self.start_rewind_check(&ctx, key, index);
            *restore_files = self
                .conv
                .rewind_check
                .as_ref()
                .is_some_and(|c| c.files.any());
        }
        self.conv.confirm_prompt = Some(action);
    }

    /// True while the confirmation modal is up — used to suppress Enter-to-send in
    /// the composer, which would otherwise fire together with Enter-to-confirm.
    pub(crate) fn confirm_prompt_open(&self) -> bool {
        self.conv.confirm_prompt.is_some() || self.conv.editor.prompt.is_some()
    }

    pub(crate) fn render_confirm_prompt(&mut self, ctx: &egui::Context) {
        let Some(action) = self.conv.confirm_prompt.clone() else {
            return;
        };
        self.poll_worktree_risk();
        self.poll_rewind_check();

        let (title, body, note, confirm_label) = self.confirm_texts(&action);
        let option_label = self.confirm_option_label(&action);
        let mut option_value = matches!(
            action,
            ConfirmAction::DeleteSession {
                remove_worktree: true,
                ..
            } | ConfirmAction::DeleteWorkspace {
                remove_worktree: true,
                ..
            } | ConfirmAction::Rewind {
                restore_files: true,
                ..
            }
        );
        let rewind = self.conv.rewind_check.take().filter(|c| match action {
            ConfirmAction::Rewind { wi, si, index, .. } => {
                c.key == self.session_key(wi, si) && c.index == index
            }
            _ => false,
        });
        let rewind_ready =
            |restore: bool| !restore || rewind.as_ref().is_some_and(|c| c.files_ready());
        let worktree_option = matches!(
            action,
            ConfirmAction::DeleteSession { .. } | ConfirmAction::DeleteWorkspace { .. }
        );
        let removes_worktree = matches!(action, ConfirmAction::RemoveWorktree { .. })
            || (worktree_option && option_value && option_label.is_some());
        let check_root = match action {
            ConfirmAction::DeleteSession { wi, .. }
            | ConfirmAction::DeleteWorkspace { wi, .. }
            | ConfirmAction::RemoveWorktree { wi } => {
                self.conv.workspaces.get(wi).map(|w| w.root_path.clone())
            }
            _ => None,
        };
        let mut check = self
            .conv
            .worktree_risk
            .take()
            .filter(|c| Some(&c.root) == check_root.as_ref());
        let restores_files = matches!(action, ConfirmAction::Rewind { .. }) && option_value;
        let confirm_enabled = (!removes_worktree
            || check.as_ref().is_some_and(|c| c.allows_removal()))
            && rewind_ready(restores_files && option_label.is_some());
        let outcome = confirm_modal(
            ctx,
            "app_confirm_prompt",
            &title,
            &body,
            note.as_deref(),
            |ui| {
                if let Some(label) = option_label.as_deref() {
                    ui.add_space(10.0);
                    ui.checkbox(
                        &mut option_value,
                        RichText::new(label).size(FS_SMALL).color(c_text()),
                    );
                }
                if let Some(rewind) = rewind.as_ref() {
                    render_rewind_files(ui, rewind, option_value);
                    return;
                }
                let removing = matches!(action, ConfirmAction::RemoveWorktree { .. })
                    || (worktree_option && option_value && option_label.is_some());
                if removing && let Some(check) = check.as_mut() {
                    render_worktree_risk(ui, check);
                }
            },
            &confirm_label,
            confirm_enabled,
        );
        if check.as_ref().is_some_and(|c| c.result.is_none())
            || rewind
                .as_ref()
                .is_some_and(|c| c.plan.is_none() && !c.files.turns.is_empty())
        {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let options = check.as_ref().map(|c| c.options()).unwrap_or_default();
        self.conv.worktree_risk = check;
        self.conv.rewind_check = rewind;
        if let Some(flag) = self.conv.confirm_prompt.as_mut().and_then(option_flag) {
            *flag = option_value && option_label.is_some();
        }
        match outcome {
            ModalOutcome::Open => {}
            ModalOutcome::Cancelled => {
                self.conv.confirm_prompt = None;
                self.conv.worktree_risk = None;
                self.conv.rewind_check = None;
            }
            ModalOutcome::Confirmed => {
                if let Some(action) = self.conv.confirm_prompt.take() {
                    self.conv.worktree_risk = None;
                    self.conv.rewind_check = None;
                    self.execute_confirmed(ctx, action, options);
                }
            }
        }
    }

    /// Checkbox offered next to the action, if any: deleting a chat in a work tree can take
    /// the work tree with it, unless another of its chats is still running; so can removing
    /// the work tree workspace (the sidebar only offers that while none of its chats run).
    fn confirm_option_label(&self, action: &ConfirmAction) -> Option<String> {
        if let ConfirmAction::Rewind { .. } = action {
            let check = self.conv.rewind_check.as_ref()?;
            if !check.files.any() {
                return None;
            }
            return Some(match check.files.files {
                0 => "Also restore the files these responses changed".to_string(),
                1 => "Also restore the 1 file these responses changed".to_string(),
                n => format!("Also restore the {n} files these responses changed"),
            });
        }
        if let ConfirmAction::DeleteWorkspace { wi, .. } = action {
            let info = self.conv.workspaces.get(*wi)?.worktree.as_ref()?;
            return Some(format!(
                "Also delete the work tree folder and branch \u{201c}{}\u{201d}",
                info.branch
            ));
        }
        let ConfirmAction::DeleteSession { wi, si, .. } = action else {
            return None;
        };
        let workspace = self.conv.workspaces.get(*wi)?;
        let info = workspace.worktree.as_ref()?;
        let others = workspace.sessions.len().saturating_sub(1);
        if (0..workspace.sessions.len()).any(|i| i != *si && self.session_row_is_running(*wi, i)) {
            return None;
        }
        Some(match others {
            0 => format!("Also remove the work tree \u{201c}{}\u{201d}", info.branch),
            1 => format!(
                "Also remove the work tree \u{201c}{}\u{201d} and its other chat",
                info.branch
            ),
            n => format!(
                "Also remove the work tree \u{201c}{}\u{201d} and its {n} other chats",
                info.branch
            ),
        })
    }

    fn confirm_texts(&self, action: &ConfirmAction) -> (String, String, Option<String>, String) {
        match action {
            ConfirmAction::DeleteSession {
                wi,
                si,
                remove_worktree,
            } => {
                let name = self
                    .conv
                    .workspaces
                    .get(*wi)
                    .and_then(|w| w.sessions.get(*si))
                    .map(|s| s.title.clone())
                    .unwrap_or_else(|| "this chat".to_string());
                let body = if *remove_worktree {
                    format!(
                        "\u{201c}{name}\u{201d} and its history will be deleted, together with \
                         the work tree."
                    )
                } else {
                    format!("\u{201c}{name}\u{201d} and its history will be deleted.")
                };
                (
                    "Delete chat?".to_string(),
                    body,
                    Some("This action cannot be undone.".to_string()),
                    "Delete".to_string(),
                )
            }
            ConfirmAction::DeleteWorkspace {
                wi,
                remove_worktree,
            } => {
                let name = self
                    .conv
                    .workspaces
                    .get(*wi)
                    .map(|w| crate::theme::workspace_sidebar_label(&w.root_path))
                    .unwrap_or_else(|| "this workspace".to_string());
                if *remove_worktree {
                    return (
                        "Remove workspace?".to_string(),
                        format!(
                            "\u{201c}{name}\u{201d} will be removed from the sidebar, and its \
                             work tree deleted."
                        ),
                        Some("This action cannot be undone.".to_string()),
                        "Remove".to_string(),
                    );
                }
                (
                    "Remove workspace?".to_string(),
                    format!(
                        "\u{201c}{name}\u{201d} will be removed from the sidebar. Files on \
                         disk are kept; re-add the folder to see its chats again."
                    ),
                    None,
                    "Remove".to_string(),
                )
            }
            ConfirmAction::RemoveWorktree { wi } => {
                let branch = self
                    .conv
                    .workspaces
                    .get(*wi)
                    .and_then(|w| w.worktree.as_ref())
                    .map(|w| w.branch.clone())
                    .unwrap_or_default();
                (
                    "Remove work tree?".to_string(),
                    format!("The work tree of \u{201c}{branch}\u{201d} will be deleted."),
                    Some("This action cannot be undone.".to_string()),
                    "Remove".to_string(),
                )
            }
            ConfirmAction::Rewind { wi, si, index, .. } => {
                let later = self
                    .conv
                    .workspaces
                    .get(*wi)
                    .and_then(|w| w.sessions.get(*si))
                    .map_or(0, |s| s.messages.len().saturating_sub(*index + 1));
                (
                    "Rewind chat?".to_string(),
                    format!(
                        "The chat goes back to before this message: it and {} after it are \
                         removed, and the message returns to the composer to edit and send again.",
                        if later == 1 {
                            "the reply".to_string()
                        } else {
                            format!("{later} messages")
                        }
                    ),
                    None,
                    "Rewind".to_string(),
                )
            }
            ConfirmAction::GitDiscard { paths } => {
                let body = if let [path] = paths.as_slice() {
                    format!("This will permanently discard changes in {path}.")
                } else {
                    format!(
                        "This will permanently discard changes in {} files.",
                        paths.len()
                    )
                };
                (
                    "Discard changes?".to_string(),
                    body,
                    Some("This action cannot be undone.".to_string()),
                    "Discard".to_string(),
                )
            }
            ConfirmAction::GitForceDeleteBranch { name } => (
                "Delete unmerged branch?".to_string(),
                format!(
                    "\u{201c}{name}\u{201d} has commits that are not in the current branch \
                     or its upstream. Deleting it drops them."
                ),
                Some("This action cannot be undone.".to_string()),
                "Delete".to_string(),
            ),
            ConfirmAction::DeleteLocalModel { id } => (
                "Delete model?".to_string(),
                format!("{id} will be deleted from disk."),
                None,
                "Delete".to_string(),
            ),
            ConfirmAction::DeleteRemoteModel { id, .. } => (
                "Delete remote model?".to_string(),
                format!("{id} will be deleted from the SSH host."),
                None,
                "Delete".to_string(),
            ),
            ConfirmAction::DeleteVoiceModel { id } => (
                "Delete voice model?".to_string(),
                format!("{id} will be deleted from disk."),
                None,
                "Delete".to_string(),
            ),
            ConfirmAction::ReplaceInFiles { matches, files } => (
                "Replace in files?".to_string(),
                format!(
                    "Replace {matches} {} in {files} {}. Files open in the editor are changed \
                     there and left unsaved; the others are written to disk.",
                    if *matches == 1 { "match" } else { "matches" },
                    if *files == 1 { "file" } else { "files" },
                ),
                Some("Files written to disk cannot be restored with Undo.".to_string()),
                "Replace".to_string(),
            ),
        }
    }

    fn execute_confirmed(
        &mut self,
        ctx: &egui::Context,
        action: ConfirmAction,
        worktree: crate::git::worktree::RemoveOptions,
    ) {
        match action {
            ConfirmAction::DeleteSession {
                wi,
                si,
                remove_worktree,
            } => {
                // Sessions can only be deleted from the active workspace (the sidebar
                // enforces this); guard anyway in case state moved under the modal.
                if wi == self.conv.active_workspace {
                    self.delete_session(si);
                    if remove_worktree {
                        self.remove_worktree(wi, worktree);
                    }
                }
            }
            ConfirmAction::DeleteWorkspace {
                wi,
                remove_worktree: true,
            } => self.remove_worktree(wi, worktree),
            ConfirmAction::DeleteWorkspace { wi, .. } => self.delete_workspace(wi),
            ConfirmAction::RemoveWorktree { wi } => self.remove_worktree(wi, worktree),
            ConfirmAction::Rewind {
                wi,
                si,
                index,
                restore_files,
            } => {
                let key = self.session_key(wi, si);
                self.start_rewind(ctx, key, index, restore_files);
            }
            ConfirmAction::GitDiscard { paths } => {
                self.request(crate::git::GitOp::Discard(paths));
            }
            ConfirmAction::GitForceDeleteBranch { name } => {
                self.request(crate::git::GitOp::DeleteBranch { name, force: true });
            }
            ConfirmAction::DeleteLocalModel { id } => {
                if let Err(e) = crate::local_models::remove_downloaded(&id) {
                    self.conv.local_models.runtime_status = Some(format!("Delete failed: {e}"));
                }
                self.conv.local_models.downloaded = crate::local_models::load_manifest().models;
            }
            ConfirmAction::DeleteRemoteModel { id, path } => {
                self.spawn_remote_model_delete(ctx, id, path);
            }
            ConfirmAction::DeleteVoiceModel { id } => {
                if let Err(e) = crate::voice_models::remove_downloaded(&id) {
                    self.conv.voice_ui.download_error = Some(format!("Delete failed: {e}"));
                }
                self.conv.voice_ui.downloaded = crate::voice_models::load_manifest().models;
                if self.conv.settings.dictation.model_id.as_deref() == Some(id.as_str()) {
                    self.conv.settings.dictation.model_id = None;
                }
            }
            ConfirmAction::ReplaceInFiles { .. } => self.replace_in_files(),
        }
    }
}

/// The "also remove the work tree" flag of actions that offer it.
fn remove_worktree_flag(action: &mut ConfirmAction) -> Option<&mut bool> {
    match action {
        ConfirmAction::DeleteSession {
            remove_worktree, ..
        }
        | ConfirmAction::DeleteWorkspace {
            remove_worktree, ..
        } => Some(remove_worktree),
        _ => None,
    }
}

/// The checkbox value of actions that offer one.
fn option_flag(action: &mut ConfirmAction) -> Option<&mut bool> {
    match action {
        ConfirmAction::Rewind { restore_files, .. } => Some(restore_files),
        other => remove_worktree_flag(other),
    }
}

/// What restoring the files of a rewind does, and why it can't when it can't.
fn render_rewind_files(ui: &mut egui::Ui, check: &RewindCheck, restore: bool) {
    let note = |ui: &mut egui::Ui, text: String, color: egui::Color32| {
        ui.add_space(6.0);
        ui.label(RichText::new(text).size(FS_TINY).color(color));
    };
    if check.files.unrestorable > 0 {
        note(
            ui,
            format!(
                "{} changed files with no snapshot to restore from; those changes stay.",
                if check.files.unrestorable == 1 {
                    "1 later response".to_string()
                } else {
                    format!("{} later responses", check.files.unrestorable)
                }
            ),
            c_warning_fg(),
        );
    }
    if !restore || check.files.turns.is_empty() {
        return;
    }
    match &check.plan {
        None => {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                crate::theme::small_spinner(ui);
                ui.label(
                    RichText::new("Checking the files for later edits\u{2026}")
                        .size(FS_TINY)
                        .color(c_text_muted()),
                );
            });
        }
        Some(Err(e)) => note(
            ui,
            format!("The files can't be restored: {e}"),
            c_error_fg(),
        ),
        Some(Ok(plan)) if !plan.conflicts.is_empty() => note(
            ui,
            format!(
                "Changed since, so restoring would overwrite them: {}. Rewind the chat only, \
                 or undo those edits first.",
                names(&plan.conflicts)
            ),
            c_error_fg(),
        ),
        Some(Ok(plan)) => note(
            ui,
            format!(
                "{} back to how {} before this message: {}.",
                plural(plan.paths.len(), "file goes", "files go"),
                if plan.paths.len() == 1 {
                    "it was"
                } else {
                    "they were"
                },
                names(&plan.paths)
            ),
            c_text_muted(),
        ),
    }
}

/// Names listed per kind of lost work before "and N more".
const RISK_NAMES_SHOWN: usize = 4;

fn names(items: &[String]) -> String {
    let mut shown = items
        .iter()
        .take(RISK_NAMES_SHOWN)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if items.len() > RISK_NAMES_SHOWN {
        shown.push_str(&format!(", and {} more", items.len() - RISK_NAMES_SHOWN));
    }
    shown
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// What deleting the work tree loses, with the explicit choices to keep the branch or to
/// delete anyway.
fn render_worktree_risk(ui: &mut egui::Ui, check: &mut WorktreeRiskCheck) {
    ui.add_space(8.0);
    let risk = match &check.result {
        None => {
            ui.horizontal(|ui| {
                crate::theme::small_spinner(ui);
                ui.label(
                    RichText::new("Checking the work tree for unsaved work\u{2026}")
                        .size(FS_SMALL)
                        .color(c_text_muted()),
                );
            });
            return;
        }
        Some(Err(e)) => {
            ui.label(
                RichText::new(format!("Could not check what would be lost: {e}"))
                    .size(FS_SMALL)
                    .color(c_error_fg()),
            );
            ui.checkbox(
                &mut check.force,
                RichText::new("Delete the work tree anyway")
                    .size(FS_SMALL)
                    .color(c_text()),
            );
            return;
        }
        Some(Ok(risk)) => risk.clone(),
    };
    if !risk.loses_files() && !risk.loses_commits() {
        ui.label(
            RichText::new(
                "Nothing would be lost: no uncommitted files, and every commit of the branch \
                 is on another branch.",
            )
            .size(FS_SMALL)
            .color(c_text_muted()),
        );
        return;
    }
    let commits_lost = risk.loses_commits() && (!check.keep_branch || risk.detached);
    if risk.loses_files() || commits_lost {
        ui.label(
            RichText::new("Deleting it loses:")
                .size(FS_SMALL)
                .color(c_warning_fg()),
        );
    }
    let line = |ui: &mut egui::Ui, text: String| {
        ui.label(
            RichText::new(format!("\u{2022} {text}"))
                .size(FS_TINY)
                .color(c_text()),
        );
    };
    if !risk.modified.is_empty() {
        line(
            ui,
            format!(
                "{} with uncommitted changes: {}",
                plural(risk.modified.len(), "file", "files"),
                names(&risk.modified)
            ),
        );
    }
    if !risk.untracked.is_empty() {
        line(
            ui,
            format!(
                "{}: {}",
                plural(risk.untracked.len(), "untracked file", "untracked files"),
                names(&risk.untracked)
            ),
        );
    }
    if commits_lost {
        line(ui, unmerged_line(&risk));
    }
    if risk.loses_commits() && !risk.detached {
        ui.add_space(4.0);
        ui.checkbox(
            &mut check.keep_branch,
            RichText::new(format!(
                "Keep the branch \u{201c}{}\u{201d} and its commits",
                risk.branch
            ))
            .size(FS_SMALL)
            .color(c_text()),
        );
        if check.keep_branch {
            ui.label(
                RichText::new(format!("Kept: {}", unmerged_line(&risk)))
                    .size(FS_TINY)
                    .color(c_text_muted()),
            );
        }
    }
    if risk.blocks(check.options()) {
        ui.add_space(4.0);
        ui.checkbox(
            &mut check.force,
            RichText::new("Delete anyway and lose the work listed above")
                .size(FS_SMALL)
                .color(c_text()),
        );
    } else {
        check.force = false;
    }
}

fn unmerged_line(risk: &RemovalRisk) -> String {
    let count = if risk.more_unmerged {
        format!("{}+ commits", risk.unmerged.len())
    } else {
        plural(risk.unmerged.len(), "commit", "commits")
    };
    let place = if risk.detached {
        "on a detached HEAD".to_string()
    } else {
        format!("only on \u{201c}{}\u{201d}", risk.branch)
    };
    format!("{count} {place}: {}", names(&risk.unmerged))
}
