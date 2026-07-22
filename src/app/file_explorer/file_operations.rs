//! Explorer create, rename, and delete operations.

use std::path::Path;

use eframe::egui::{self, RichText, TextEdit, Ui};

use crate::theme::{FS_TINY, c_text_muted};

use super::super::{OxiApp, state::FileOperation};

impl OxiApp {
    pub(super) fn start_file_operation(&mut self, operation: FileOperation) {
        self.conv.editor.file_operation_name = match &operation {
            FileOperation::Rename(path) => path
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_default(),
            _ => String::new(),
        };
        self.conv.editor.file_operation = Some(operation);
        self.conv.editor.focus_file_operation_next_frame = true;
        self.conv.editor.error = None;
    }

    pub(super) fn render_file_operation(&mut self, ui: &mut Ui, operation: FileOperation) {
        let (label, destructive) = match operation {
            FileOperation::NewFile(_) => ("New file name", false),
            FileOperation::NewFolder(_) => ("New folder name", false),
            FileOperation::Rename(_) => ("Rename to", false),
            FileOperation::Delete(_) => ("Delete this path?", true),
        };
        ui.label(RichText::new(label).size(FS_TINY).color(c_text_muted()));
        if !destructive {
            let response = ui.add(
                TextEdit::singleline(&mut self.conv.editor.file_operation_name)
                    .id_salt("workspace_file_operation_name")
                    .desired_width(f32::INFINITY),
            );
            if std::mem::take(&mut self.conv.editor.focus_file_operation_next_frame) {
                response.request_focus();
            }
            if response.has_focus()
                && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
            {
                self.apply_file_operation(operation.clone());
            }
        }
        ui.horizontal(|ui| {
            if ui
                .button(if destructive { "Delete" } else { "Apply" })
                .clicked()
            {
                self.apply_file_operation(operation.clone());
            }
            if ui.button("Cancel").clicked() {
                self.conv.editor.file_operation = None;
            }
        });
    }

    fn apply_file_operation(&mut self, operation: FileOperation) {
        let name = self.conv.editor.file_operation_name.trim();
        let invalid_name =
            name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\']);
        let result = match operation {
            FileOperation::NewFile(parent) if !invalid_name => {
                let path = parent.join(name);
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path)
                    .map(|_| ())
            }
            FileOperation::NewFolder(parent) if !invalid_name => {
                std::fs::create_dir(parent.join(name))
            }
            FileOperation::Rename(path) if !invalid_name => {
                let destination = path.parent().unwrap_or(Path::new(".")).join(name);
                let renames_workspace_root =
                    path.as_path() == Path::new(&self.active_workspace().root_path);
                let result = std::fs::rename(&path, &destination);
                if result.is_ok() {
                    if renames_workspace_root {
                        self.active_workspace_mut().root_path = destination.display().to_string();
                        self.sync_workspaces_to_settings();
                    }
                    for document in &mut self.conv.editor.documents {
                        if document.path == path {
                            document.path = destination.clone();
                        } else if let Ok(relative) = document.path.strip_prefix(&path) {
                            document.path = destination.join(relative);
                        }
                    }
                    let expanded_descendants = self
                        .conv
                        .explorer_expanded
                        .iter()
                        .filter_map(|expanded| {
                            expanded
                                .strip_prefix(&path)
                                .ok()
                                .map(|relative| destination.join(relative))
                        })
                        .collect::<Vec<_>>();
                    self.conv
                        .explorer_expanded
                        .retain(|expanded| !expanded.starts_with(&path));
                    self.conv.explorer_expanded.extend(expanded_descendants);
                }
                result
            }
            FileOperation::Delete(path) => {
                if self
                    .conv
                    .editor
                    .documents
                    .iter()
                    .any(|document| document.path == path && document.is_dirty())
                {
                    self.conv.editor.error =
                        Some("Save or close the modified file before deleting it.".into());
                    return;
                }
                let result = if path.is_dir() {
                    std::fs::remove_dir(&path)
                } else {
                    std::fs::remove_file(&path)
                };
                if result.is_ok() {
                    self.remove_editor_path(&path);
                }
                result
            }
            _ => {
                self.conv.editor.error = Some("Enter a valid name without path separators.".into());
                return;
            }
        };
        match result {
            Ok(()) => {
                self.conv.editor.file_operation = None;
                self.conv.editor.error = None;
            }
            Err(error) => self.conv.editor.error = Some(format!("File operation failed: {error}")),
        }
    }

    fn remove_editor_path(&mut self, path: &Path) {
        self.conv
            .editor
            .documents
            .retain(|document| !document.path.starts_with(path));
        self.conv.editor.active = if self.conv.editor.documents.is_empty() {
            None
        } else {
            Some(
                self.conv
                    .editor
                    .active
                    .unwrap_or(0)
                    .min(self.conv.editor.documents.len() - 1),
            )
        };
    }
}
