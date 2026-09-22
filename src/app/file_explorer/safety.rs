//! Guard document loss at tab close, reload, and application exit.

use eframe::egui::{self, Align, Layout, RichText};

use super::super::{OxiApp, state::EditorPrompt};
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

    /// Drop a tab and keep typing in whichever document becomes visible.
    fn close_editor_document(&mut self, index: usize) {
        self.conv.editor.remove_document(index);
        if self.conv.editor.active.is_some() {
            self.conv.editor.focus_editor_next_frame = true;
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
            self.conv.settings_exit_prompt = None;
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
        let modal = egui::Modal::new(egui::Id::new("editor_unsaved_prompt")).show(ctx, |ui| {
            ui.set_width(460.0_f32.min(ctx.content_rect().width() - 48.0));
            ui.label(RichText::new(title).size(FS_H2).strong());
            ui.add_space(8.0);
            ui.label(description);
            if matches!(prompt, EditorPrompt::Exit) {
                egui::ScrollArea::vertical()
                    .max_height(140.0)
                    .show(ui, |ui| {
                        for document in self.conv.editor.documents.iter().filter(|d| d.is_dirty()) {
                            ui.label(
                                RichText::new(document.path.display().to_string()).size(FS_SMALL),
                            );
                        }
                        if self.settings_dirty() {
                            ui.label("Settings");
                        }
                    });
            }
            if let Some(error) = &self.conv.editor.error {
                ui.add_space(8.0);
                ui.label(RichText::new(error).color(c_error_fg()));
            }
            ui.add_space(16.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                save = primary_button(ui, primary).clicked();
                if let Some(label) = destructive {
                    discard = ghost_button(ui, label, true).clicked();
                }
                cancel = ghost_button(ui, "Cancel", false).clicked();
            });
        });
        // Enter is intentionally left to the focused button; it never silently discards edits.
        if cancel || modal.should_close() {
            self.conv.editor.prompt = None;
            return;
        }
        if !save && !discard {
            return;
        }
        if matches!(prompt, EditorPrompt::Exit) {
            if save {
                for index in 0..self.conv.editor.documents.len() {
                    if !self.conv.editor.documents[index].is_dirty() {
                        continue;
                    }
                    if self.conv.editor.documents[index].is_scratchpad {
                        self.autosave_scratchpad(index);
                        if self.conv.editor.documents[index].is_dirty() {
                            return;
                        }
                    } else if let Err(error) = self.save_editor_document(index, false) {
                        self.conv.editor.error = Some(format!(
                            "{}: {error} Cancel to review this file.",
                            self.conv.editor.documents[index].path.display()
                        ));
                        return;
                    }
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
            EditorPrompt::Exit => unreachable!(),
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
            EditorPrompt::Exit => unreachable!(),
        }
        self.conv.editor.prompt = None;
        self.conv.editor.error = None;
    }
}
