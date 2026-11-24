//! Explorer create, rename, and delete operations.

use std::io;
use std::path::Path;

use eframe::egui::{self, RichText, Ui};

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
            FileOperation::Delete(_) => ("Delete this file?", true),
            FileOperation::DeleteFolder(_) => ("Delete this folder and everything in it?", true),
        };
        ui.label(RichText::new(label).size(FS_TINY).color(c_text_muted()));
        if !destructive {
            let response = crate::ui::chrome::settings_text_field(
                ui,
                &mut self.conv.editor.file_operation_name,
                "",
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
            let apply = if destructive {
                crate::ui::chrome::ghost_button(ui, "Delete", true)
            } else {
                crate::ui::chrome::primary_button(ui, "Apply")
            };
            if apply.clicked() {
                self.apply_file_operation(operation.clone());
            }
            if crate::ui::chrome::ghost_button(ui, "Cancel", false).clicked() {
                self.conv.editor.file_operation = None;
            }
        });
    }

    pub(super) fn apply_file_operation(&mut self, operation: FileOperation) {
        let name = self.conv.editor.file_operation_name.trim();
        let invalid_name =
            name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\']);
        let folder = matches!(operation, FileOperation::DeleteFolder(_));
        let result = match operation {
            FileOperation::NewFile(parent) if !invalid_name => {
                crate::vfs::create_new_file(&parent.join(name))
            }
            FileOperation::NewFolder(parent) if !invalid_name => {
                crate::vfs::create_dir(&parent.join(name))
            }
            FileOperation::Rename(path) if !invalid_name => {
                let destination = path.parent().unwrap_or(Path::new(".")).join(name);
                let renames_workspace_root =
                    path.as_path() == Path::new(&self.active_workspace().root_path);
                let result = crate::vfs::rename(&path, &destination);
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
                        .explorer
                        .expanded
                        .iter()
                        .filter_map(|expanded| {
                            expanded
                                .strip_prefix(&path)
                                .ok()
                                .map(|relative| destination.join(relative))
                        })
                        .collect::<Vec<_>>();
                    self.conv
                        .explorer
                        .expanded
                        .retain(|expanded| !expanded.starts_with(&path));
                    self.conv.explorer.expanded.extend(expanded_descendants);
                }
                result
            }
            FileOperation::Delete(path) | FileOperation::DeleteFolder(path) => {
                // Open documents keep resolved paths. Resolve only the parent, so a symlink being
                // deleted is not followed.
                let target = path
                    .parent()
                    .zip(path.file_name())
                    .and_then(|(parent, name)| {
                        crate::vfs::canonicalize(parent).ok().map(|p| p.join(name))
                    })
                    .unwrap_or_else(|| path.clone());
                // A folder takes every open file under it along, so none of them may be unsaved.
                if self
                    .conv
                    .editor
                    .documents
                    .iter()
                    .any(|document| document.is_dirty() && document.path.starts_with(&target))
                {
                    self.conv.editor.error = Some(if folder {
                        "Save or close the modified files in this folder before deleting it.".into()
                    } else {
                        "Save or close the modified file before deleting it.".into()
                    });
                    return;
                }
                let result = if folder {
                    crate::vfs::remove_dir_all(&path)
                } else {
                    crate::vfs::remove_file(&path)
                };
                match &result {
                    Ok(()) => self.forget_deleted_path(&path, &target),
                    // A folder can be partly deleted before the error.
                    Err(_) if folder => {
                        self.conv.explorer.cache.invalidate();
                        self.close_documents_where(|document| {
                            document.starts_with(&target)
                                && crate::vfs::metadata(document)
                                    .is_err_and(|error| error.kind() == io::ErrorKind::NotFound)
                        });
                    }
                    Err(_) => {}
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
                self.conv.explorer.cache.invalidate();
            }
            Err(error) => self.conv.editor.error = Some(format!("File operation failed: {error}")),
        }
    }

    /// `path` as the explorer lists it, `target` as open documents store it.
    fn forget_deleted_path(&mut self, path: &Path, target: &Path) {
        self.conv
            .explorer
            .expanded
            .retain(|expanded| !expanded.starts_with(path));
        self.close_documents_where(|document| document.starts_with(target));
    }

    fn close_documents_where(&mut self, close: impl Fn(&Path) -> bool) {
        let editor = &mut self.conv.editor;
        for index in (0..editor.documents.len()).rev() {
            if close(&editor.documents[index].path) {
                editor.remove_document(index);
            }
        }
    }
}
