//! Guard document loss at tab close, reload, and application exit.

use eframe::egui::{self, Align, Layout, RichText};

use super::super::{
    OxiApp,
    state::{EditorPrompt, SidebarMode},
};
use crate::theme::*;
use crate::ui::chrome::{ghost_button, primary_button};

impl OxiApp {
    pub(crate) fn request_close_editor_tab(&mut self, index: usize) {
        if self.conv.editor.documents[index].is_scratchpad {
            self.autosave_scratchpad(index);
        }
        let document = &self.conv.editor.documents[index];
        if document.is_dirty() {
            self.conv.editor.prompt = Some(EditorPrompt::Close(document.path.clone()));
        } else {
            self.close_editor_document(index);
        }
    }

    /// Close every editor tab, including the git diff tab; unsaved documents ask first.
    pub(crate) fn request_close_all_editor_tabs(&mut self) {
        for index in 0..self.conv.editor.documents.len() {
            if self.conv.editor.documents[index].is_scratchpad {
                self.autosave_scratchpad(index);
            }
        }
        if self.conv.editor.documents.iter().any(|d| d.is_dirty()) {
            self.conv.editor.prompt = Some(EditorPrompt::CloseAll);
        } else {
            self.close_all_editor_documents();
        }
    }

    fn close_all_editor_documents(&mut self) {
        if self.conv.diff_view.open {
            self.close_editor_git_diff();
        }
        self.conv.editor.markdown_preview_active = false;
        while let Some(index) = self.conv.editor.documents.len().checked_sub(1) {
            self.close_editor_document(index);
        }
    }

    /// Save every dirty document; on failure leave the error for the open prompt and stop.
    fn save_dirty_editor_documents(&mut self) -> bool {
        for index in 0..self.conv.editor.documents.len() {
            if !self.conv.editor.documents[index].is_dirty() {
                continue;
            }
            if self.conv.editor.documents[index].is_scratchpad {
                self.autosave_scratchpad(index);
                if self.conv.editor.documents[index].is_dirty() {
                    return false;
                }
            } else if let Err(error) = self.save_editor_document(index, false) {
                self.conv.editor.error = Some(format!(
                    "{}: {error} Cancel to review this file.",
                    self.conv.editor.documents[index].path.display()
                ));
                return false;
            }
        }
        true
    }

    /// Drop a tab and focus the remaining document or the chat composer.
    fn close_editor_document(&mut self, index: usize) {
        self.conv.editor.remove_document(index);
        if self.conv.editor.active.is_some() {
            self.conv.editor.focus_editor_next_frame = true;
        } else if self.conv.editor.documents.is_empty()
            // The search results stay up: they are how the user moves to the next file.
            && self.conv.sidebar.mode != SidebarMode::Search
            && !(self.conv.editor.diff_tab_active
                && self.conv.diff_view.open
                && self.conv.git.diff.is_some())
        {
            self.conv.sidebar.mode = SidebarMode::Chats;
            self.focus_active_view_next_frame();
        }
    }

    pub(super) fn request_reload_editor_file(&mut self) {
        if let Some(document) = self.conv.editor.active_document() {
            if document.is_dirty() {
                self.conv.editor.prompt = Some(EditorPrompt::Reload(document.path.clone()));
            } else {
                self.reload_active_editor_file();
            }
        }
    }

    pub(crate) fn guard_editor_exit(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested())
            && !self.conv.editor.allow_exit
            && (self.conv.editor.documents.iter().any(|d| d.is_dirty()) || self.settings_dirty())
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.conv.settings_page.exit_prompt = None;
            self.conv.editor.prompt = Some(EditorPrompt::Exit);
        }
    }

    pub(crate) fn render_editor_prompt(&mut self, ctx: &egui::Context) {
        let Some(prompt) = self.conv.editor.prompt.clone() else {
            return;
        };
        let (title, description, primary, destructive) = match &prompt {
            EditorPrompt::Close(path) => (
                "Save changes before closing?",
                format!("{} has unsaved changes.", path.display()),
                "Save & close",
                Some("Don't save"),
            ),
            EditorPrompt::Reload(path) => (
                "Reload from disk?",
                format!(
                    "Your unsaved edits to {} will be replaced by the version on disk.",
                    path.display()
                ),
                "Reload",
                None,
            ),
            EditorPrompt::Overwrite { path, .. } => (
                "File changed on disk",
                format!(
                    "{} was changed or removed outside the editor. Overwrite replaces the disk version with your edits.",
                    path.display()
                ),
                "Overwrite",
                None,
            ),
            EditorPrompt::CloseAll => (
                "Save changes before closing?",
                "Some open files have unsaved changes.".into(),
                "Save all & close",
                Some("Don't save"),
            ),
            EditorPrompt::Exit => (
                "Save before quitting?",
                "Open files or settings have unsaved changes. Save them before closing oxi.".into(),
                "Save all & quit",
                Some("Quit without saving"),
            ),
        };
        let mut save = false;
        let mut discard = false;
        let mut cancel = false;
        let modal = egui::Modal::new(egui::Id::new("editor_unsaved_prompt"))
            .frame(egui::Frame::NONE)
            .backdrop_color(c_modal_backdrop())
            .show(ctx, |ui| {
                crate::ui::chrome::modal_frame(ui).show(ui, |ui| {
                    ui.set_width(460.0_f32.min(ctx.content_rect().width() - 48.0));
                    crate::ui::chrome::modal_title(ui, title);
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(description)
                            .size(FS_SMALL)
                            .color(c_text_muted()),
                    );
                    if matches!(prompt, EditorPrompt::Exit | EditorPrompt::CloseAll) {
                        ui.add_space(6.0);
                        egui::ScrollArea::vertical()
                            .max_height(140.0)
                            .show(ui, |ui| {
                                for document in
                                    self.conv.editor.documents.iter().filter(|d| d.is_dirty())
                                {
                                    ui.label(
                                        RichText::new(document.path.display().to_string())
                                            .size(FS_SMALL)
                                            .color(c_text()),
                                    );
                                }
                                if matches!(prompt, EditorPrompt::Exit) && self.settings_dirty() {
                                    ui.label(
                                        RichText::new("Settings").size(FS_SMALL).color(c_text()),
                                    );
                                }
                            });
                    }
                    if let Some(error) = &self.conv.editor.error {
                        ui.add_space(8.0);
                        crate::ui::chrome::alert_banner(ui, error, true);
                    }
                    ui.add_space(14.0);
                    let row = egui::vec2(ui.available_width(), 28.0);
                    ui.allocate_ui_with_layout(row, Layout::right_to_left(Align::Center), |ui| {
                        save = primary_button(ui, primary).clicked();
                        if let Some(label) = destructive {
                            discard = ghost_button(ui, label, true).clicked();
                        }
                        cancel = ghost_button(ui, "Cancel", false)
                            .on_hover_text("Esc")
                            .clicked();
                    });
                });
            });
        // Enter is intentionally left to the focused button; it never silently discards edits.
        if cancel || modal.should_close() {
            if matches!(prompt, EditorPrompt::Exit) {
                // A "Restart to update" was waiting on this quit.
                crate::update::cancel_relaunch();
            }
            self.conv.editor.prompt = None;
            return;
        }
        if !save && !discard {
            return;
        }
        if matches!(prompt, EditorPrompt::CloseAll) {
            if save && !self.save_dirty_editor_documents() {
                return;
            }
            self.conv.editor.prompt = None;
            self.conv.editor.error = None;
            self.close_all_editor_documents();
            return;
        }
        if matches!(prompt, EditorPrompt::Exit) {
            if save {
                if !self.save_dirty_editor_documents() {
                    return;
                }
                if self.settings_dirty()
                    && let Err(error) = self.conv.settings.save()
                {
                    self.conv.editor.error = Some(format!("Could not save settings: {error}"));
                    return;
                }
            }
            self.conv.editor.allow_exit = true;
            self.conv.editor.prompt = None;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        let path = match &prompt {
            EditorPrompt::Close(path)
            | EditorPrompt::Reload(path)
            | EditorPrompt::Overwrite { path, .. } => path,
            EditorPrompt::CloseAll | EditorPrompt::Exit => unreachable!(),
        };
        let Some(index) = self
            .conv
            .editor
            .documents
            .iter()
            .position(|d| &d.path == path)
        else {
            self.conv.editor.prompt = None;
            return;
        };
        match prompt {
            EditorPrompt::Close(path) => {
                if save {
                    if self.conv.editor.documents[index].is_scratchpad {
                        self.autosave_scratchpad(index);
                        if self.conv.editor.documents[index].is_dirty() {
                            return;
                        }
                    } else if let Err(error) = self.save_editor_document(index, false) {
                        self.conv.editor.error = Some(error);
                        if self.conv.editor.documents[index].externally_modified {
                            self.conv.editor.prompt = Some(EditorPrompt::Overwrite {
                                path,
                                close_after: true,
                            });
                        }
                        return;
                    }
                }
                self.close_editor_document(index);
            }
            EditorPrompt::Reload(_) => {
                self.conv.editor.active = Some(index);
                self.reload_active_editor_file();
                if self.conv.editor.error.is_some() {
                    return;
                }
            }
            EditorPrompt::Overwrite { close_after, .. } => {
                if let Err(error) = self.save_editor_document(index, true) {
                    self.conv.editor.error = Some(error);
                    return;
                }
                if close_after {
                    self.close_editor_document(index);
                }
            }
            EditorPrompt::CloseAll | EditorPrompt::Exit => unreachable!(),
        }
        self.conv.editor.prompt = None;
        self.conv.editor.error = None;
    }
}
