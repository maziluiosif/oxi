//! Shared confirmation flow for destructive actions: one pending [`ConfirmAction`]
//! app-wide, rendered as a centered modal over everything (see `chrome::confirm_modal`).

use eframe::egui;

use crate::ui::chrome::{ModalOutcome, confirm_modal};

use super::OxiApp;
use super::state::ConfirmAction;

impl OxiApp {
    /// Queue a destructive action for confirmation. The modal renders on top of the
    /// whole window at the end of the frame.
    pub(crate) fn request_confirm(&mut self, action: ConfirmAction) {
        self.conv.confirm_prompt = Some(action);
    }

    /// True while the confirmation modal is up — used to suppress Enter-to-send in
    /// the composer, which would otherwise fire together with Enter-to-confirm.
    pub(crate) fn confirm_prompt_open(&self) -> bool {
        self.conv.confirm_prompt.is_some()
    }

    pub(crate) fn render_confirm_prompt(&mut self, ctx: &egui::Context) {
        let Some(action) = self.conv.confirm_prompt.clone() else {
            return;
        };

        let (title, body, note, confirm_label) = self.confirm_texts(&action);
        match confirm_modal(
            ctx,
            "app_confirm_prompt",
            &title,
            &body,
            note.as_deref(),
            &confirm_label,
        ) {
            ModalOutcome::Open => {}
            ModalOutcome::Cancelled => self.conv.confirm_prompt = None,
            ModalOutcome::Confirmed => {
                self.conv.confirm_prompt = None;
                self.execute_confirmed(action);
            }
        }
    }

    fn confirm_texts(&self, action: &ConfirmAction) -> (String, String, Option<String>, String) {
        match action {
            ConfirmAction::DeleteSession { wi, si } => {
                let name = self
                    .conv
                    .workspaces
                    .get(*wi)
                    .and_then(|w| w.sessions.get(*si))
                    .map(|s| s.title.clone())
                    .unwrap_or_else(|| "this chat".to_string());
                (
                    "Delete chat?".to_string(),
                    format!("\u{201c}{name}\u{201d} and its history will be deleted."),
                    Some("This action cannot be undone.".to_string()),
                    "Delete".to_string(),
                )
            }
            ConfirmAction::DeleteWorkspace { wi } => {
                let name = self
                    .conv
                    .workspaces
                    .get(*wi)
                    .map(|w| crate::theme::workspace_sidebar_label(&w.root_path))
                    .unwrap_or_else(|| "this workspace".to_string());
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
            ConfirmAction::DeleteLocalModel { id } => (
                "Delete model?".to_string(),
                format!("{id} will be deleted from disk."),
                None,
                "Delete".to_string(),
            ),
            ConfirmAction::DeleteVoiceModel { id } => (
                "Delete voice model?".to_string(),
                format!("{id} will be deleted from disk."),
                None,
                "Delete".to_string(),
            ),
        }
    }

    fn execute_confirmed(&mut self, action: ConfirmAction) {
        match action {
            ConfirmAction::DeleteSession { wi, si } => {
                // Sessions can only be deleted from the active workspace (the sidebar
                // enforces this); guard anyway in case state moved under the modal.
                if wi == self.conv.active_workspace {
                    self.delete_session(si);
                }
            }
            ConfirmAction::DeleteWorkspace { wi } => self.delete_workspace(wi),
            ConfirmAction::GitDiscard { paths } => {
                self.request(crate::git::GitOp::Discard(paths));
            }
            ConfirmAction::DeleteLocalModel { id } => {
                if let Err(e) = crate::local_models::remove_downloaded(&id) {
                    self.conv.local_models.runtime_status = Some(format!("Delete failed: {e}"));
                }
                self.conv.local_models.downloaded = crate::local_models::load_manifest().models;
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
        }
    }
}
