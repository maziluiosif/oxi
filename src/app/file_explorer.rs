//! Workspace file explorer and multi-tab text editor.

use eframe::egui::scroll_area::ScrollBarVisibility;
use eframe::egui::{
    self, Align, FontId, Frame, Layout, Margin, RichText, ScrollArea, TextEdit, Ui,
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use walkdir::WalkDir;

use crate::theme::*;
use crate::ui::chrome::icon_glyph_rich;

use super::state::FileOperation;
use super::{EditorDocument, OxiApp};

mod support;
pub(crate) use support::find_match_ranges;
use support::{
    apply_definition_underline, apply_search_highlights, file_icon, fuzzy_path_score,
    is_gitignored, language_for_path, load_gitignore_patterns, should_ignore,
};

const MAX_TEXT_FILE_BYTES: u64 = 2 * 1024 * 1024;
const ALWAYS_SKIPPED_DIRS: &[&str] = &[".git"];

#[derive(Default)]
pub struct EditorLayoutCache {
    revision: u64,
    wrap_width_bits: u32,
    pixels_per_point_bits: u32,
    geometry: Option<Arc<egui::Galley>>,
    syntax: Option<Arc<egui::Galley>>,
}

type EditorScrollOutput = egui::scroll_area::ScrollAreaOutput<(
    Vec<(usize, f32)>,
    bool,
    Option<(usize, usize)>,
    Option<usize>,
    Option<usize>,
    Option<usize>,
)>;

impl OxiApp {
    pub(crate) fn render_file_explorer(&mut self, ui: &mut Ui) {
        ui.set_min_width(ui.max_rect().width());
        let root = PathBuf::from(&self.active_workspace().root_path);
        let ignored = load_gitignore_patterns(&root);
        // Explorer decorations share the existing async Git worker with the source-control panel.
        let git_was_uninitialized = self.conv.git_rx.is_none();
        self.ensure_git_channels();
        if git_was_uninitialized {
            let _ = self
                .conv
                .git_tx
                .as_ref()
                .map(|tx| tx.send(crate::git::GitOp::Refresh));
        }

        // Whole row right-to-left: the icons stay pinned to the right edge at any
        // sidebar width, and the trailing label truncates instead of being overlapped
        // when the sidebar is dragged down to its minimum width.
        ui.horizontal(|ui| {
            // Match the conversations sidebar's 24 px search toolbar exactly so switching
            // modes does not move the first content row by a few pixels.
            ui.set_height(24.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if crate::ui::chrome::icon_button_plain(ui, ICON_SETTINGS, 20.0, false)
                    .on_hover_text("Open settings")
                    .clicked()
                {
                    self.open_settings_page();
                }
                if crate::ui::chrome::icon_button_plain(ui, ICON_REFRESH, 20.0, false)
                    .on_hover_text("Refresh and check files on disk")
                    .clicked()
                {
                    self.check_external_file_changes();
                    let _ = self
                        .conv
                        .git_tx
                        .as_ref()
                        .map(|tx| tx.send(crate::git::GitOp::Refresh));
                    ui.ctx().request_repaint();
                }
                if crate::ui::chrome::icon_button_plain(ui, ICON_FILE, 20.0, false)
                    .on_hover_text("New file")
                    .clicked()
                {
                    self.start_file_operation(FileOperation::NewFile(root.clone()));
                }
                if crate::ui::chrome::icon_button_plain(ui, ICON_FOLDER_PLUS, 20.0, false)
                    .on_hover_text("New folder")
                    .clicked()
                {
                    self.start_file_operation(FileOperation::NewFolder(root.clone()));
                }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.add(
                        egui::Label::new(
                            RichText::new("EXPLORER")
                                .size(FS_TINY)
                                .strong()
                                .color(c_text_muted()),
                        )
                        .truncate(),
                    );
                });
            });
        });
        // Keep the same toolbar-to-list rhythm as the conversations sidebar (8 px
        // gap plus the list's 1 px leading breathing room).
        ui.add_space(8.0);
        ui.add_space(1.0);

        let label = root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_else(|| root.to_str().unwrap_or("workspace"));
        let root_expanded = !self.conv.explorer_collapsed_roots.contains(&root);
        let (root_rect, root_response) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 22.0), egui::Sense::click());
        paint_explorer_row(ui, root_rect, root_response.hovered(), false);
        ui.scope_builder(
            egui::UiBuilder::new().max_rect(root_rect.shrink2(egui::vec2(4.0, 0.0))),
            |ui| {
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    ui.label(icon_glyph_rich(
                        if root_expanded {
                            ICON_ANGLE_DOWN
                        } else {
                            ICON_CHEVRON_RIGHT
                        },
                        FS_TINY,
                        c_text_faint(),
                    ));
                    ui.label(icon_glyph_rich(
                        if root_expanded {
                            ICON_FOLDER_OPEN
                        } else {
                            ICON_FOLDER
                        },
                        FS_SMALL,
                        c_text_muted(),
                    ));
                    ui.add(
                        egui::Label::new(
                            RichText::new(label)
                                .strong()
                                .size(FS_SMALL)
                                .color(c_sidebar_section()),
                        )
                        .truncate(),
                    );
                });
            },
        );
        let root_response = root_response.on_hover_text(root.display().to_string());
        if root_response.clicked() {
            if root_expanded {
                self.conv.explorer_collapsed_roots.insert(root.clone());
            } else {
                self.conv.explorer_collapsed_roots.remove(&root);
            }
        }
        root_response.context_menu(|ui| self.render_root_context_menu(ui, &root));
        ui.add_space(4.0);

        if let Some(operation) = self.conv.editor.file_operation.clone() {
            self.render_file_operation(ui, operation);
            ui.add_space(6.0);
        }

        if root_expanded {
            ScrollArea::vertical()
                .id_salt("workspace_file_explorer")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    self.render_explorer_directory(ui, &root, &root, &ignored, 1)
                });
        }

        if let Some(error) = self.conv.editor.error.clone() {
            ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                ui.label(RichText::new(error).size(FS_TINY).color(c_error_fg()));
            });
        }
    }

    fn render_explorer_directory(
        &mut self,
        ui: &mut Ui,
        root: &Path,
        directory: &Path,
        ignored: &[String],
        depth: usize,
    ) {
        let mut entries = match std::fs::read_dir(directory) {
            Ok(entries) => entries.filter_map(Result::ok).collect::<Vec<_>>(),
            Err(error) => {
                ui.label(RichText::new(format!("Cannot read folder: {error}")).size(FS_TINY));
                return;
            }
        };
        entries.sort_by_key(|entry| {
            let is_file = entry.file_type().map(|kind| kind.is_file()).unwrap_or(true);
            (is_file, entry.file_name().to_string_lossy().to_lowercase())
        });

        for entry in entries {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() && ALWAYS_SKIPPED_DIRS.contains(&name.as_str()) {
                continue;
            }
            let git_ignored = is_gitignored(root, &path, kind.is_dir(), ignored);
            let indent = depth as f32 * 14.0;
            if kind.is_dir() {
                let expanded = self.conv.explorer_expanded.contains(&path);
                let git_status = self.git_status_for_directory(root, &path);
                let (rect, response) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 22.0),
                    egui::Sense::click(),
                );
                paint_explorer_row(ui, rect, response.hovered(), false);
                ui.scope_builder(
                    egui::UiBuilder::new().max_rect(rect.shrink2(egui::vec2(4.0, 0.0))),
                    |ui| {
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            ui.spacing_mut().item_spacing.x = 3.0;
                            ui.add_space(indent);
                            let chevron = if expanded {
                                ICON_ANGLE_DOWN
                            } else {
                                ICON_CHEVRON_RIGHT
                            };
                            let folder = if expanded {
                                ICON_FOLDER_OPEN
                            } else {
                                ICON_FOLDER
                            };
                            ui.label(crate::ui::chrome::icon_glyph_rich(
                                chevron,
                                FS_TINY,
                                c_text_faint(),
                            ));
                            ui.label(crate::ui::chrome::icon_label_job(
                                folder,
                                &name,
                                FS_SMALL,
                                explorer_entry_color(c_text(), git_ignored),
                            ));
                            if let Some(status) = git_status {
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    // Keep clear of the floating scrollbar, which is
                                    // painted over the row's right edge.
                                    ui.add_space(10.0);
                                    ui.label(
                                        RichText::new(status)
                                            .monospace()
                                            .size(FS_TINY)
                                            .strong()
                                            .color(git_status_color(status)),
                                    );
                                });
                            }
                        });
                    },
                );
                if response.clicked() {
                    if expanded {
                        self.conv.explorer_expanded.remove(&path);
                    } else {
                        self.conv.explorer_expanded.insert(path.clone());
                    }
                }
                response.context_menu(|ui| self.render_path_context_menu(ui, &path, true));
                if expanded {
                    self.render_explorer_directory(ui, root, &path, ignored, depth + 1);
                }
            } else if kind.is_file() {
                // Ctrl/Cmd+P temporarily changes the active editor tab while previewing.
                // Keep the Explorer selection on the previously committed tab until the
                // picker result is accepted.
                let selected_index = if self.conv.editor.file_picker_open {
                    self.conv.editor.file_picker_previous_active
                } else {
                    self.conv.editor.active
                };
                let selected = selected_index
                    .and_then(|index| self.conv.editor.documents.get(index))
                    .is_some_and(|document| document.path == path);
                let git_status = self.git_status_for_path(root, &path);
                let (rect, response) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 22.0),
                    egui::Sense::click(),
                );
                paint_explorer_row(ui, rect, response.hovered(), selected);
                let (icon, color) = file_icon(&path);
                ui.scope_builder(
                    egui::UiBuilder::new().max_rect(rect.shrink2(egui::vec2(4.0, 0.0))),
                    |ui| {
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            ui.spacing_mut().item_spacing.x = 3.0;
                            ui.add_space(indent + 14.0);
                            ui.label(crate::ui::chrome::icon_label_job(
                                icon,
                                &name,
                                FS_SMALL,
                                explorer_entry_color(
                                    if selected {
                                        crate::theme::blend_color(color, c_text_strong(), 0.28)
                                    } else {
                                        color
                                    },
                                    git_ignored,
                                ),
                            ));
                            if let Some(status) = git_status {
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    // Keep clear of the floating scrollbar, which is
                                    // painted over the row's right edge.
                                    ui.add_space(10.0);
                                    ui.label(
                                        RichText::new(status)
                                            .monospace()
                                            .size(FS_TINY)
                                            .strong()
                                            .color(git_status_color(status)),
                                    );
                                });
                            }
                        });
                    },
                );
                if self
                    .conv
                    .editor
                    .explorer_reveal_pending
                    .as_deref()
                    .is_some_and(|pending| pending == path)
                {
                    response.scroll_to_me(Some(Align::Center));
                    self.conv.editor.explorer_reveal_pending = None;
                }
                let response = response.on_hover_text(path.display().to_string());
                if response.clicked() {
                    self.open_editor_file(path.clone());
                    self.conv.editor.focus_editor_next_frame = true;
                }
                response.context_menu(|ui| self.render_path_context_menu(ui, &path, false));
            }
        }
    }

    fn git_status_for_directory(&self, root: &Path, directory: &Path) -> Option<char> {
        let relative = directory
            .strip_prefix(root)
            .ok()?
            .to_string_lossy()
            .replace('\\', "/");
        let prefix = format!("{relative}/");
        self.conv
            .git
            .unstaged
            .iter()
            .chain(self.conv.git.staged.iter())
            .find(|entry| entry.path.starts_with(&prefix))
            .map(|entry| entry.status)
    }

    fn git_status_for_path(&self, root: &Path, path: &Path) -> Option<char> {
        let relative = path
            .strip_prefix(root)
            .ok()?
            .to_string_lossy()
            .replace('\\', "/");
        self.conv
            .git
            .unstaged
            .iter()
            .chain(self.conv.git.staged.iter())
            .find(|entry| entry.path == relative)
            .map(|entry| entry.status)
    }

    fn render_root_context_menu(&mut self, ui: &mut Ui, root: &Path) {
        ui.set_min_width(210.0);
        if ui.button("New File").clicked() {
            self.start_file_operation(FileOperation::NewFile(root.to_path_buf()));
            ui.close();
        }
        if ui.button("Rename...").clicked() {
            self.start_file_operation(FileOperation::Rename(root.to_path_buf()));
            ui.close();
        }
        if ui.button("Open Folder...").clicked() {
            self.open_workspace_folder();
            ui.close();
        }
        if ui.button("Copy Path").clicked() {
            ui.ctx().copy_text(root.display().to_string());
            ui.close();
        }
        ui.separator();
        if ui.button("New Folder...").clicked() {
            self.start_file_operation(FileOperation::NewFolder(root.to_path_buf()));
            ui.close();
        }
        if ui.button("Find in Folder...").clicked() {
            self.conv.editor.find_open = true;
            self.conv.editor.find_replace_open = false;
            self.conv.editor.focus_find_next_frame = true;
            ui.close();
        }
    }

    fn render_path_context_menu(&mut self, ui: &mut Ui, path: &Path, directory: bool) {
        ui.set_min_width(if directory { 210.0 } else { 170.0 });
        if directory {
            if ui.button("New File").clicked() {
                self.start_file_operation(FileOperation::NewFile(path.to_path_buf()));
                ui.close();
            }
            if ui.button("Rename...").clicked() {
                self.start_file_operation(FileOperation::Rename(path.to_path_buf()));
                ui.close();
            }
            if ui.button("Open Folder...").clicked() {
                reveal_path_in_file_manager(path);
                ui.close();
            }
            if ui.button("Copy Path").clicked() {
                ui.ctx().copy_text(path.display().to_string());
                ui.close();
            }
            ui.separator();
            if ui.button("New Folder...").clicked() {
                self.start_file_operation(FileOperation::NewFolder(path.to_path_buf()));
                ui.close();
            }
            if ui.button("Delete Folder").clicked() {
                self.start_file_operation(FileOperation::Delete(path.to_path_buf()));
                ui.close();
            }
            if ui.button("Find in Folder...").clicked() {
                self.conv.editor.find_open = true;
                self.conv.editor.find_replace_open = false;
                ui.close();
            }
        } else {
            if ui.button("Rename...").clicked() {
                self.start_file_operation(FileOperation::Rename(path.to_path_buf()));
                ui.close();
            }
            if ui.button("Delete File").clicked() {
                self.start_file_operation(FileOperation::Delete(path.to_path_buf()));
                ui.close();
            }
            if ui.button(reveal_label()).clicked() {
                reveal_path_in_file_manager(path);
                ui.close();
            }
            if ui.button("Copy Path").clicked() {
                ui.ctx().copy_text(path.display().to_string());
                ui.close();
            }
        }
    }

    fn start_file_operation(&mut self, operation: FileOperation) {
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

    fn render_file_operation(&mut self, ui: &mut Ui, operation: FileOperation) {
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

    fn reveal_editor_file_in_explorer(&mut self, path: &Path) {
        let root = PathBuf::from(&self.active_workspace().root_path);
        let safe_root = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
        let safe_path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if !safe_path.starts_with(&safe_root) {
            return;
        }

        let relative = safe_path.strip_prefix(&safe_root).unwrap_or(&safe_path);
        let explorer_path = root.join(relative);
        self.conv.explorer_collapsed_roots.remove(&root);
        let mut parent = explorer_path.parent();
        while let Some(directory) = parent {
            if directory == root {
                break;
            }
            self.conv.explorer_expanded.insert(directory.to_path_buf());
            parent = directory.parent();
        }
        self.conv.editor.explorer_reveal_pending = Some(explorer_path);
    }

    fn open_editor_file(&mut self, path: PathBuf) {
        self.open_editor_file_impl(path, true);
    }

    fn open_editor_file_impl(&mut self, path: PathBuf, reveal_in_explorer: bool) {
        let root = PathBuf::from(&self.active_workspace().root_path);
        let safe_root = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
        let safe_path = match std::fs::canonicalize(&path) {
            Ok(path) if path.starts_with(&safe_root) => path,
            _ => {
                self.conv.editor.error = Some("The file is outside the active workspace.".into());
                return;
            }
        };
        self.conv.sidebar_mode = super::state::SidebarMode::Explorer;
        self.conv.sidebar_open = true;
        self.conv.editor.hidden_active = None;
        if let Some(index) = self
            .conv
            .editor
            .documents
            .iter()
            .position(|document| document.path == safe_path)
        {
            self.conv.editor.active = Some(index);
            self.conv.editor.diff_tab_active = false;
            if reveal_in_explorer {
                self.reveal_editor_file_in_explorer(&safe_path);
            }
            return;
        }
        let metadata = match std::fs::metadata(&safe_path) {
            Ok(metadata) if metadata.len() <= MAX_TEXT_FILE_BYTES => metadata,
            Ok(_) => {
                self.conv.editor.error = Some("File is larger than the 2 MB editor limit.".into());
                return;
            }
            Err(error) => {
                self.conv.editor.error = Some(format!("Could not inspect file: {error}"));
                return;
            }
        };
        match std::fs::read_to_string(&safe_path) {
            Ok(content) => {
                self.conv.editor.documents.push(EditorDocument {
                    path: safe_path.clone(),
                    saved_content: content.clone(),
                    content,
                    disk_modified: metadata.modified().ok(),
                    externally_modified: false,
                    syntax_state: None,
                    content_revision: 0,
                    dirty: false,
                    layout_cache: EditorLayoutCache::default(),
                    minimap_cache: None,
                });
                self.conv.editor.active = Some(self.conv.editor.documents.len() - 1);
                self.conv.editor.error = None;
                self.conv.editor.show_diff = false;
                // An open git diff stays reachable as an editor tab; just show the file.
                self.conv.editor.diff_tab_active = false;
                if reveal_in_explorer {
                    self.reveal_editor_file_in_explorer(&safe_path);
                }
            }
            Err(error) => {
                self.conv.editor.error = Some(format!("Could not open text file: {error}"))
            }
        }
    }

    pub(crate) fn save_editor_file(&mut self) {
        let Some(document) = self.conv.editor.active_document_mut() else {
            return;
        };
        match std::fs::write(&document.path, document.content.as_bytes()) {
            Ok(()) => {
                document.saved_content.clone_from(&document.content);
                document.dirty = false;
                document.disk_modified = std::fs::metadata(&document.path)
                    .and_then(|metadata| metadata.modified())
                    .ok();
                document.externally_modified = false;
                self.conv.editor.error = None;
                let _ = self
                    .conv
                    .git_tx
                    .as_ref()
                    .map(|tx| tx.send(crate::git::GitOp::Refresh));
            }
            Err(error) => self.conv.editor.error = Some(format!("Could not save file: {error}")),
        }
    }

    fn check_external_file_changes(&mut self) {
        for document in &mut self.conv.editor.documents {
            let modified = std::fs::metadata(&document.path)
                .and_then(|metadata| metadata.modified())
                .ok();
            if modified.is_some()
                && document.disk_modified.is_some()
                && modified != document.disk_modified
            {
                document.externally_modified = true;
            }
        }
    }

    fn reload_active_editor_file(&mut self) {
        let Some(document) = self.conv.editor.active_document_mut() else {
            return;
        };
        match std::fs::read_to_string(&document.path) {
            Ok(content) => {
                document.content = content.clone();
                document.saved_content = content;
                document.content_revision = document.content_revision.wrapping_add(1);
                document.dirty = false;
                document.layout_cache = EditorLayoutCache::default();
                document.minimap_cache = None;
                document.disk_modified = std::fs::metadata(&document.path)
                    .and_then(|metadata| metadata.modified())
                    .ok();
                document.externally_modified = false;
                self.conv.editor.error = None;
            }
            Err(error) => self.conv.editor.error = Some(format!("Could not reload file: {error}")),
        }
    }

    pub(crate) fn open_file_picker(&mut self) {
        let root = PathBuf::from(&self.active_workspace().root_path);
        let ignored = load_gitignore_patterns(&root);
        self.conv.editor.file_picker_files = WalkDir::new(&root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|entry| {
                entry.path() == root
                    || !should_ignore(&root, entry.path(), entry.file_type().is_dir(), &ignored)
            })
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .map(|entry| entry.into_path())
            .collect();
        self.conv
            .editor
            .file_picker_files
            .sort_by_cached_key(|path| {
                path.strip_prefix(&root)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .to_ascii_lowercase()
            });
        self.conv.editor.file_picker_query.clear();
        self.conv.editor.file_picker_last_query.clear();
        self.conv.editor.file_picker_selected = 0;
        self.conv.editor.file_picker_preview = None;
        self.conv.editor.file_picker_previous_active = self.conv.editor.active;
        self.conv.editor.file_picker_previous_diff_active = self.conv.editor.diff_tab_active;
        self.conv.editor.file_picker_preview_created = false;
        self.conv.editor.file_picker_open = true;
    }

    fn preview_file_picker_path(&mut self, path: &Path) {
        if self
            .conv
            .editor
            .file_picker_preview
            .as_deref()
            .is_some_and(|preview| preview == path)
        {
            return;
        }

        if self.conv.editor.file_picker_preview_created
            && let Some(preview) = self.conv.editor.file_picker_preview.as_ref()
            && let Some(index) = self
                .conv
                .editor
                .documents
                .iter()
                .position(|document| &document.path == preview)
        {
            self.conv.editor.documents.remove(index);
        }

        let safe_path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let was_open = self
            .conv
            .editor
            .documents
            .iter()
            .any(|document| document.path == safe_path);
        self.conv.editor.file_picker_preview = None;
        self.conv.editor.file_picker_preview_created = false;
        self.open_editor_file_impl(path.to_path_buf(), false);
        if self
            .conv
            .editor
            .active_document()
            .is_some_and(|document| document.path == safe_path)
        {
            self.conv.editor.file_picker_preview = Some(safe_path);
            self.conv.editor.file_picker_preview_created = !was_open;
        }
    }

    fn clear_file_picker_preview(&mut self) {
        if self.conv.editor.file_picker_preview_created
            && let Some(preview) = self.conv.editor.file_picker_preview.as_ref()
            && let Some(index) = self
                .conv
                .editor
                .documents
                .iter()
                .position(|document| &document.path == preview)
        {
            self.conv.editor.documents.remove(index);
        }
        self.conv.editor.active = self
            .conv
            .editor
            .file_picker_previous_active
            .filter(|&index| index < self.conv.editor.documents.len());
        self.conv.editor.diff_tab_active = self.conv.editor.file_picker_previous_diff_active;
        self.conv.editor.file_picker_preview = None;
        self.conv.editor.file_picker_preview_created = false;
    }

    pub(crate) fn cancel_file_picker(&mut self) {
        self.clear_file_picker_preview();
        self.conv.editor.file_picker_open = false;
    }

    pub(crate) fn render_file_picker(&mut self, ctx: &egui::Context) {
        if !self.conv.editor.file_picker_open {
            return;
        }
        let root = PathBuf::from(&self.active_workspace().root_path);
        let query = self.conv.editor.file_picker_query.to_ascii_lowercase();
        if query != self.conv.editor.file_picker_last_query {
            self.conv.editor.file_picker_selected = 0;
            self.conv.editor.file_picker_last_query.clone_from(&query);
        }
        let mut ranked = self
            .conv
            .editor
            .file_picker_files
            .iter()
            .filter_map(|path| {
                let relative = path.strip_prefix(&root).unwrap_or(path);
                let display = relative.to_string_lossy().replace('\\', "/");
                fuzzy_path_score(&display, &query).map(|score| (score, display, path.clone()))
            })
            .collect::<Vec<_>>();
        ranked.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        let matches = ranked
            .into_iter()
            .take(100)
            .map(|(_, _, path)| path)
            .collect::<Vec<_>>();

        let (arrow_up, arrow_down, enter) = ctx.input_mut(|input| {
            (
                input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                input.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
            )
        });
        let keyboard_navigation = arrow_up || arrow_down;
        let pointer_moved = ctx.input(|input| input.pointer.delta() != egui::Vec2::ZERO);
        if !matches.is_empty() {
            if arrow_down {
                self.conv.editor.file_picker_selected =
                    (self.conv.editor.file_picker_selected + 1).min(matches.len() - 1);
            } else if arrow_up {
                self.conv.editor.file_picker_selected =
                    self.conv.editor.file_picker_selected.saturating_sub(1);
            }
            self.conv.editor.file_picker_selected =
                self.conv.editor.file_picker_selected.min(matches.len() - 1);
        } else {
            self.conv.editor.file_picker_selected = 0;
        }
        let mut selected = enter
            .then(|| matches.get(self.conv.editor.file_picker_selected).cloned())
            .flatten();
        let mut open = true;
        // Shrink for short result lists, but cap the picker so longer lists remain scrollable.
        let available = ctx.content_rect().size();
        let max_picker_height = 440.0_f32.min((available.y - 104.0).max(120.0));
        let row_height = 27.0; // 24 px row + the theme's 3 px item spacing.
        let picker_height =
            (88.0 + matches.len() as f32 * row_height).clamp(120.0, max_picker_height);
        let picker_size = egui::vec2(
            560.0_f32.min((available.x - 32.0).max(280.0)),
            picker_height,
        );
        egui::Window::new("Open file")
            .id(egui::Id::new("workspace_file_picker"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 72.0))
            .fixed_size(picker_size)
            .open(&mut open)
            .show(ctx, |ui| {
                let response = ui.add(
                    TextEdit::singleline(&mut self.conv.editor.file_picker_query)
                        .id_salt("workspace_file_picker_query")
                        .hint_text("Type a file name or path…")
                        .desired_width(f32::INFINITY),
                );
                if !response.has_focus() {
                    response.request_focus();
                }
                ui.add_space(6.0);
                // Fill the remaining window height even when every match fits. Otherwise the
                // scroll area's clip edge can land on the final row as selection repaints.
                ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .animated(false)
                    .show(ui, |ui| {
                        if matches.is_empty() {
                            ui.label(RichText::new("No matching files").color(c_text_muted()));
                        }
                        for (match_index, path) in matches.iter().enumerate() {
                            let relative = path.strip_prefix(&root).unwrap_or(path);
                            let response = ui.add_sized(
                                [ui.available_width(), ui.spacing().interact_size.y],
                                egui::Button::selectable(
                                    match_index == self.conv.editor.file_picker_selected,
                                    relative.to_string_lossy(),
                                )
                                // Keep the label left-aligned while the selectable area fills the row.
                                .right_text(()),
                            );
                            // Scrolling a hover-selected row every frame moves a different row under
                            // the stationary pointer, which changes selection again and causes a
                            // scroll/hover feedback loop. Only keyboard navigation needs auto-scroll.
                            if keyboard_navigation
                                && match_index == self.conv.editor.file_picker_selected
                            {
                                response.scroll_to_me(None);
                            }
                            // Keyboard navigation may scroll the list under the pointer. Do not let
                            // that same frame's hover state override the arrow-selected row, otherwise
                            // the selection can oscillate (most visibly on the last item).
                            if pointer_moved && !keyboard_navigation && response.hovered() {
                                self.conv.editor.file_picker_selected = match_index;
                            }
                            if response.clicked() {
                                selected = Some(path.clone());
                            }
                        }
                        // Keep the final row away from the scroll area's bottom clip boundary.
                        ui.add_space(2.0);
                    });
            });
        if let Some(path) = selected {
            // Enter/click promotes the temporary preview to a regular editor tab.
            self.preview_file_picker_path(&path);
            self.conv.editor.file_picker_preview = None;
            self.conv.editor.file_picker_preview_created = false;
            self.conv.editor.file_picker_open = false;
            self.reveal_editor_file_in_explorer(&path);
            // The file becomes a permanent tab: hand focus to the editor now that the
            // picker window (which owned keyboard focus) is gone.
            self.conv.editor.focus_editor_next_frame = true;
        } else if !open {
            self.cancel_file_picker();
        } else {
            self.conv.editor.file_picker_open = true;
            if !query.is_empty() {
                if let Some(path) = matches.get(self.conv.editor.file_picker_selected) {
                    self.preview_file_picker_path(path);
                } else {
                    self.clear_file_picker_preview();
                }
            } else {
                // Cmd/Ctrl+P initially lists files without changing the editor. Preview starts only
                // after the user types at least one character.
                self.clear_file_picker_preview();
            }
        }
    }

    pub(crate) fn render_text_editor(&mut self, ui: &mut Ui) {
        self.check_external_file_changes();
        self.render_editor_tabs(ui);
        if self.conv.editor.diff_tab_active
            && self.conv.diff_view_open
            && self.conv.git.diff.is_some()
        {
            self.render_editor_git_diff(ui);
            return;
        }
        if self.conv.editor.active_document().is_none() {
            return;
        }

        let external = self
            .conv
            .editor
            .active_document()
            .is_some_and(|document| document.externally_modified);
        if external {
            ui.horizontal(|ui| {
                ui.label(RichText::new("File changed on disk.").color(c_warning_fg()));
                if ui.button("Reload from disk").clicked() {
                    self.reload_active_editor_file();
                }
            });
        }
        if let Some(error) = self.conv.editor.error.clone() {
            ui.label(RichText::new(error).size(FS_SMALL).color(c_error_fg()));
        }

        if self.conv.editor.find_open {
            // Find floats over the bottom of the editor instead of participating in layout.
            // Opening/closing it therefore cannot resize the editor viewport or alter its scroll.
            let editor_rect = ui.available_rect_before_wrap();
            if self.conv.editor.show_diff {
                self.render_editor_diff(ui);
            } else {
                self.render_editor_body(ui);
            }
            let panel_height = if self.conv.editor.find_replace_open {
                67.0
            } else {
                32.0
            };
            let panel_rect = egui::Rect::from_min_size(
                egui::pos2(editor_rect.left(), editor_rect.bottom() - panel_height),
                egui::vec2(editor_rect.width(), panel_height),
            );
            ui.scope_builder(
                egui::UiBuilder::new()
                    .max_rect(panel_rect)
                    .sense(egui::Sense::hover()),
                |ui| self.render_find_replace(ui),
            );
        } else if self.conv.editor.show_diff {
            self.render_editor_diff(ui);
        } else {
            self.render_editor_body(ui);
        }
    }

    fn render_editor_tabs(&mut self, ui: &mut Ui) {
        let mut select = None;
        let mut close = None;
        let mut save = false;
        let mut reveal = false;
        let mut toggle_diff = false;
        let mut select_git_diff = false;
        let mut close_git_diff = false;
        let mut new_file = false;
        let mut navigate_back = false;
        let mut navigate_forward = false;
        let git_diff_tab = self.conv.diff_view_open && self.conv.git.diff.is_some();
        let git_diff_active = git_diff_tab && self.conv.editor.diff_tab_active;
        let can_go_back = !self.conv.editor.navigation_back.is_empty();
        let can_go_forward = !self.conv.editor.navigation_forward.is_empty();
        let tab_strip_width = (ui.available_width() - 126.0).max(80.0);

        Frame::new()
            .fill(c_bg_elevated_2())
            .inner_margin(Margin::symmetric(6, 0))
            .show(ui, |ui| {
                ui.set_height(34.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    let back = ui.add_enabled(
                        can_go_back,
                        egui::Button::new(icon_glyph_rich(
                            ICON_CHEVRON_LEFT,
                            FS_SMALL,
                            c_text_muted(),
                        ))
                        .frame(false)
                        .min_size(egui::vec2(24.0, 30.0)),
                    );
                    if back.clicked() {
                        navigate_back = true;
                    }
                    let forward = ui.add_enabled(
                        can_go_forward,
                        egui::Button::new(icon_glyph_rich(
                            ICON_CHEVRON_RIGHT,
                            FS_SMALL,
                            c_text_muted(),
                        ))
                        .frame(false)
                        .min_size(egui::vec2(24.0, 30.0)),
                    );
                    if forward.clicked() {
                        navigate_forward = true;
                    }

                    ScrollArea::horizontal()
                        .id_salt("editor_tabs")
                        .max_width(tab_strip_width)
                        .scroll_bar_visibility(ScrollBarVisibility::AlwaysHidden)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 0.0;
                                for (index, document) in
                                    self.conv.editor.documents.iter().enumerate()
                                {
                                    let name = document
                                        .path
                                        .file_name()
                                        .unwrap_or_default()
                                        .to_string_lossy();
                                    let label = if document.is_dirty() {
                                        format!("{name}  ●")
                                    } else {
                                        name.into_owned()
                                    };
                                    let active =
                                        !git_diff_active && self.conv.editor.active == Some(index);
                                    let font = FontId::proportional(FS_SMALL);
                                    let label_width = ui.fonts_mut(|fonts| {
                                        fonts
                                            .layout_no_wrap(label.clone(), font.clone(), c_text())
                                            .rect
                                            .width()
                                    });
                                    let tab_width = label_width + 42.0;
                                    let (rect, response) = ui.allocate_exact_size(
                                        egui::vec2(tab_width, 28.0),
                                        egui::Sense::click(),
                                    );
                                    // The close hit target overlaps the tab response. Test the full rectangle
                                    // so the name and close icon still share one hover surface.
                                    let hovered = ui.rect_contains_pointer(rect);
                                    let fill = if active {
                                        c_bg_main()
                                    } else if hovered {
                                        c_row_hover()
                                    } else {
                                        egui::Color32::TRANSPARENT
                                    };
                                    if fill != egui::Color32::TRANSPARENT {
                                        let mut fill_rect = rect;
                                        if active || hovered {
                                            // Active and hovered tabs share the same silhouette, extending through
                                            // the header's lower edge instead of looking like floating pills.
                                            fill_rect.max.y += RADIUS_ROW as f32 + 4.0;
                                        }
                                        ui.painter().rect_filled(
                                            fill_rect,
                                            egui::CornerRadius::same(RADIUS_ROW),
                                            fill,
                                        );
                                    }
                                    ui.painter().text(
                                        egui::pos2(rect.left() + 10.0, rect.center().y),
                                        egui::Align2::LEFT_CENTER,
                                        label,
                                        font,
                                        if active {
                                            c_text_strong()
                                        } else {
                                            c_text_muted()
                                        },
                                    );
                                    let close_rect = egui::Rect::from_center_size(
                                        egui::pos2(rect.right() - 13.0, rect.center().y),
                                        egui::vec2(22.0, rect.height()),
                                    );
                                    let close_response = ui.interact(
                                        close_rect,
                                        ui.id().with(("editor_tab_close", index)),
                                        egui::Sense::click(),
                                    );
                                    ui.painter().text(
                                        close_rect.center(),
                                        egui::Align2::CENTER_CENTER,
                                        ICON_CLOSE,
                                        FontId::new(FS_TINY, icon_font()),
                                        if close_response.hovered() {
                                            c_accent()
                                        } else {
                                            c_text_faint()
                                        },
                                    );
                                    if close_response.hovered() || hovered {
                                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                                    }
                                    if close_response.clicked() || response.middle_clicked() {
                                        close = Some(index);
                                    } else if response.clicked() {
                                        select = Some(index);
                                    }
                                    let response =
                                        response.on_hover_text(document.path.display().to_string());
                                    response.context_menu(|ui| {
                                        if ui.button("Save").clicked() {
                                            select = Some(index);
                                            save = true;
                                            ui.close();
                                        }
                                        if ui.button("Reveal in Explorer").clicked() {
                                            select = Some(index);
                                            reveal = true;
                                            ui.close();
                                        }
                                        if ui.button("Unsaved changes diff").clicked() {
                                            select = Some(index);
                                            toggle_diff = true;
                                            ui.close();
                                        }
                                        if ui.button("Close").clicked() {
                                            close = Some(index);
                                            ui.close();
                                        }
                                    });
                                }

                                // Git diff pseudo-tab: keeps the diff one click away from the
                                // editable file tabs instead of replacing the whole chat area.
                                if git_diff_tab {
                                    let title = self
                                        .conv
                                        .git
                                        .current_diff_path
                                        .as_deref()
                                        .map(|path| {
                                            path.rsplit_once('/').map_or(path, |(_, file)| file)
                                        })
                                        .unwrap_or("diff")
                                        .to_owned();
                                    let label = format!("Diff: {title}");
                                    let font = FontId::proportional(FS_SMALL);
                                    let label_width = ui.fonts_mut(|fonts| {
                                        fonts
                                            .layout_no_wrap(label.clone(), font.clone(), c_text())
                                            .rect
                                            .width()
                                    });
                                    let tab_width = label_width + 42.0;
                                    let (rect, response) = ui.allocate_exact_size(
                                        egui::vec2(tab_width, 28.0),
                                        egui::Sense::click(),
                                    );
                                    let hovered = ui.rect_contains_pointer(rect);
                                    let fill = if git_diff_active {
                                        c_bg_main()
                                    } else if hovered {
                                        c_row_hover()
                                    } else {
                                        egui::Color32::TRANSPARENT
                                    };
                                    if fill != egui::Color32::TRANSPARENT {
                                        let mut fill_rect = rect;
                                        if git_diff_active || hovered {
                                            fill_rect.max.y += RADIUS_ROW as f32 + 4.0;
                                        }
                                        ui.painter().rect_filled(
                                            fill_rect,
                                            egui::CornerRadius::same(RADIUS_ROW),
                                            fill,
                                        );
                                    }
                                    ui.painter().text(
                                        egui::pos2(rect.left() + 10.0, rect.center().y),
                                        egui::Align2::LEFT_CENTER,
                                        label,
                                        font,
                                        if git_diff_active {
                                            c_text_strong()
                                        } else {
                                            c_text_muted()
                                        },
                                    );
                                    let close_rect = egui::Rect::from_center_size(
                                        egui::pos2(rect.right() - 13.0, rect.center().y),
                                        egui::vec2(22.0, rect.height()),
                                    );
                                    let close_response = ui.interact(
                                        close_rect,
                                        ui.id().with("editor_git_diff_tab_close"),
                                        egui::Sense::click(),
                                    );
                                    ui.painter().text(
                                        close_rect.center(),
                                        egui::Align2::CENTER_CENTER,
                                        ICON_CLOSE,
                                        FontId::new(FS_TINY, icon_font()),
                                        if close_response.hovered() {
                                            c_accent()
                                        } else {
                                            c_text_faint()
                                        },
                                    );
                                    if close_response.hovered() || hovered {
                                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                                    }
                                    if close_response.clicked() || response.middle_clicked() {
                                        close_git_diff = true;
                                    } else if response.clicked() {
                                        select_git_diff = true;
                                    }
                                    if let Some(path) = self.conv.git.current_diff_path.as_deref() {
                                        response.on_hover_text(path);
                                    }
                                }
                            });
                        });

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.menu_button(
                            RichText::new("▾").size(FS_SMALL).color(c_text_muted()),
                            |ui| {
                                ui.set_min_width(180.0);
                                for (index, document) in
                                    self.conv.editor.documents.iter().enumerate()
                                {
                                    let name = document
                                        .path
                                        .file_name()
                                        .unwrap_or_default()
                                        .to_string_lossy();
                                    if ui
                                        .selectable_label(
                                            !git_diff_active
                                                && self.conv.editor.active == Some(index),
                                            name,
                                        )
                                        .clicked()
                                    {
                                        select = Some(index);
                                        ui.close();
                                    }
                                }
                            },
                        )
                        .response
                        .on_hover_text("Open tabs");
                        if ui
                            .add(
                                egui::Button::new(icon_glyph_rich(
                                    ICON_PLUS,
                                    FS_SMALL,
                                    c_text_muted(),
                                ))
                                .frame(false)
                                .min_size(egui::vec2(24.0, 30.0)),
                            )
                            .on_hover_text("New file")
                            .clicked()
                        {
                            new_file = true;
                        }
                    });
                });
            });
        if new_file {
            let root = PathBuf::from(&self.active_workspace().root_path);
            self.start_file_operation(FileOperation::NewFile(root));
        }
        if navigate_back {
            self.navigate_editor_history(false);
        } else if navigate_forward {
            self.navigate_editor_history(true);
        }
        if let Some(index) = select {
            self.conv.editor.active = Some(index);
            self.conv.editor.diff_tab_active = false;
            self.conv.editor.focus_editor_next_frame = true;
            if let Some(path) = self
                .conv
                .editor
                .documents
                .get(index)
                .map(|document| document.path.clone())
            {
                self.reveal_editor_file_in_explorer(&path);
            }
        }
        if select_git_diff {
            self.conv.editor.diff_tab_active = true;
        }
        if close_git_diff {
            self.close_editor_git_diff();
        }
        if save {
            self.save_editor_file();
        }
        if reveal {
            self.reveal_active_file();
        }
        if toggle_diff {
            self.conv.editor.show_diff = !self.conv.editor.show_diff;
        }
        if let Some(index) = close {
            if self.conv.editor.documents[index].is_dirty() {
                self.conv.editor.error = Some("Save the file before closing its tab.".into());
            } else {
                self.conv.editor.documents.remove(index);
                self.conv.editor.active = if self.conv.editor.documents.is_empty() {
                    None
                } else {
                    Some(index.min(self.conv.editor.documents.len() - 1))
                };
                if self.conv.editor.active.is_some() {
                    self.conv.editor.focus_editor_next_frame = true;
                }
            }
        }
    }

    fn render_find_replace(&mut self, ui: &mut Ui) {
        let query_changed = self.conv.editor.find_query != self.conv.editor.find_last_query
            || self.conv.editor.find_case_sensitive != self.conv.editor.find_last_case_sensitive;
        if query_changed {
            self.conv
                .editor
                .find_last_query
                .clone_from(&self.conv.editor.find_query);
            self.conv.editor.find_last_case_sensitive = self.conv.editor.find_case_sensitive;
            self.conv.editor.find_active_match = 0;
            // Typing previews and reveals the first result, but deliberately does not update
            // the editor TextEdit's cursor state: doing so can steal focus from the Find input.
            self.conv.editor.find_has_navigated = !self.conv.editor.find_query.is_empty();
            self.conv.editor.find_select_pending = false;
            self.conv.editor.find_reveal_pending = !self.conv.editor.find_query.is_empty();
            self.conv.editor.find_focus_editor_pending = false;
        }
        let ranges = self
            .conv
            .editor
            .active_document()
            .map(|document| {
                find_match_ranges(
                    &document.content,
                    &self.conv.editor.find_query,
                    self.conv.editor.find_case_sensitive,
                )
            })
            .unwrap_or_default();
        if ranges.is_empty() {
            self.conv.editor.find_active_match = 0;
        } else {
            self.conv.editor.find_active_match %= ranges.len();
        }

        let mut previous = false;
        let mut next = false;
        let mut replace_one = false;
        let mut replace_all = false;
        let mut close = false;
        Frame::new()
            .fill(c_bg_elevated_2())
            .stroke(egui::Stroke::new(1.0, c_border_subtle()))
            .inner_margin(Margin {
                left: 12,
                right: 12,
                top: 2,
                bottom: 2,
            })
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing = egui::vec2(8.0, 7.0);
                const TOGGLE_WIDTH: f32 = 56.0;
                const LABEL_WIDTH: f32 = 64.0;
                const ACTION_WIDTH: f32 = 92.0;
                const CLOSE_WIDTH: f32 = 28.0;
                const ROW_HEIGHT: f32 = 28.0;
                const FIELD_HEIGHT: f32 = 24.0;
                let input_width = (ui.available_width()
                    - TOGGLE_WIDTH
                    - 32.0
                    - LABEL_WIDTH
                    - ACTION_WIDTH * 2.0
                    - CLOSE_WIDTH
                    - 7.0 * 8.0)
                    .max(80.0);

                ui.horizontal(|ui| {
                    let replace_toggle = ui
                        .add_sized(
                            [TOGGLE_WIDTH, 28.0],
                            egui::Button::selectable(self.conv.editor.find_replace_open, "AB"),
                        )
                        .on_hover_text("Show replace field");
                    if replace_toggle.clicked() {
                        self.conv.editor.find_replace_open = !self.conv.editor.find_replace_open;
                    }
                    let case_toggle = ui
                        .add_sized(
                            [32.0, ROW_HEIGHT],
                            egui::Button::selectable(self.conv.editor.find_case_sensitive, "Aa"),
                        )
                        .on_hover_text(if self.conv.editor.find_case_sensitive {
                            "Case sensitive"
                        } else {
                            "Case insensitive"
                        });
                    if case_toggle.clicked() {
                        self.conv.editor.find_case_sensitive =
                            !self.conv.editor.find_case_sensitive;
                    }
                    ui.allocate_ui_with_layout(
                        egui::vec2(LABEL_WIDTH, ROW_HEIGHT),
                        Layout::right_to_left(Align::Center),
                        |ui| {
                            ui.label(RichText::new("Find:").size(FS_SMALL).color(c_text_muted()));
                        },
                    );
                    let find_response = ui
                        .allocate_ui_with_layout(
                            egui::vec2(input_width, ROW_HEIGHT),
                            Layout::left_to_right(Align::Center),
                            |ui| {
                                ui.add_sized(
                                    [input_width, FIELD_HEIGHT],
                                    TextEdit::singleline(&mut self.conv.editor.find_query)
                                        .id_salt("workspace_editor_find")
                                        .margin(Margin::symmetric(6, 2))
                                        .hint_text("Find"),
                                )
                            },
                        )
                        .inner;
                    if query_changed || std::mem::take(&mut self.conv.editor.focus_find_next_frame)
                    {
                        find_response.request_focus();
                    }
                    next = ui
                        .add_sized([ACTION_WIDTH, 28.0], egui::Button::new("Find"))
                        .clicked();
                    previous = ui
                        .add_sized([ACTION_WIDTH, 28.0], egui::Button::new("Find Prev"))
                        .clicked();
                    close = ui
                        .add_sized(
                            [CLOSE_WIDTH, 28.0],
                            egui::Button::new(icon_glyph_rich(ICON_CLOSE, FS_TINY, c_text_muted()))
                                .frame(false),
                        )
                        .on_hover_text("Close find")
                        .clicked();

                    // A single-line TextEdit gives up focus when Enter is pressed. Checking
                    // `lost_focus` is therefore essential; `has_focus` alone misses the exact
                    // frame carrying Enter and neither navigation nor focus restoration runs.
                    let enter_while_editing = (find_response.has_focus()
                        || find_response.lost_focus())
                        && ui.input(|input| input.key_pressed(egui::Key::Enter));
                    if enter_while_editing {
                        if ui.input(|input| input.modifiers.shift) {
                            previous = true;
                        } else {
                            next = true;
                        }
                    }
                });

                if self.conv.editor.find_replace_open {
                    ui.horizontal(|ui| {
                        ui.allocate_exact_size(
                            egui::vec2(TOGGLE_WIDTH + 32.0 + 8.0, ROW_HEIGHT),
                            egui::Sense::hover(),
                        );
                        ui.allocate_ui_with_layout(
                            egui::vec2(LABEL_WIDTH, ROW_HEIGHT),
                            Layout::right_to_left(Align::Center),
                            |ui| {
                                ui.label(
                                    RichText::new("Replace:")
                                        .size(FS_SMALL)
                                        .color(c_text_muted()),
                                );
                            },
                        );
                        ui.allocate_ui_with_layout(
                            egui::vec2(input_width, ROW_HEIGHT),
                            Layout::left_to_right(Align::Center),
                            |ui| {
                                ui.add_sized(
                                    [input_width, FIELD_HEIGHT],
                                    TextEdit::singleline(&mut self.conv.editor.replace_query)
                                        .margin(Margin::symmetric(6, 2))
                                        .hint_text("Replace"),
                                );
                            },
                        );
                        replace_one = ui
                            .add_sized([ACTION_WIDTH, 28.0], egui::Button::new("Replace"))
                            .clicked();
                        replace_all = ui
                            .add_sized([ACTION_WIDTH, 28.0], egui::Button::new("Replace All"))
                            .clicked();
                    });
                }
            });

        if next || previous {
            // A single-line TextEdit releases focus on Enter, including when there are no matches.
            // Always return focus to Find after a navigation attempt so repeated searches and query
            // edits continue to work without requiring another click.
            self.conv.editor.focus_find_next_frame = true;
        }
        if !ranges.is_empty() && (next || previous) {
            self.conv.editor.find_active_match = if previous {
                if self.conv.editor.find_has_navigated {
                    self.conv
                        .editor
                        .find_active_match
                        .checked_sub(1)
                        .unwrap_or(ranges.len() - 1)
                } else {
                    ranges.len() - 1
                }
            } else if self.conv.editor.find_has_navigated {
                (self.conv.editor.find_active_match + 1) % ranges.len()
            } else {
                0
            };
            self.conv.editor.find_has_navigated = true;
            // While Find is open, navigation changes the active highlight and scroll only.
            // The editor caret is committed when Find closes, avoiding a competing TextEdit
            // cursor update that could take keyboard focus from the query field.
            self.conv.editor.find_select_pending = false;
            self.conv.editor.find_reveal_pending = true;
            self.conv.editor.find_focus_editor_pending = false;
        }
        if (replace_one || replace_all) && !ranges.is_empty() {
            let replacement = self.conv.editor.replace_query.clone();
            let active = self.conv.editor.find_active_match;
            if let Some(document) = self.conv.editor.active_document_mut() {
                if replace_all {
                    for range in ranges.iter().rev() {
                        document.content.replace_range(range.clone(), &replacement);
                    }
                } else {
                    document
                        .content
                        .replace_range(ranges[active].clone(), &replacement);
                }
                document.content_revision = document.content_revision.wrapping_add(1);
                document.dirty = document.content != document.saved_content;
                document.layout_cache = EditorLayoutCache::default();
                document.minimap_cache = None;
            }
            if replace_all {
                self.conv.editor.find_active_match = 0;
            }
            self.conv.editor.find_select_pending = false;
            self.conv.editor.find_reveal_pending = true;
        }
        if close {
            self.conv.editor.find_open = false;
            // Apply the current result on the following editor frame, then return focus.
            self.conv.editor.find_select_pending = self.conv.editor.find_has_navigated;
            self.conv.editor.find_focus_editor_pending = true;
        }
    }

    fn render_editor_body(&mut self, ui: &mut Ui) {
        let Some(index) = self.conv.editor.active else {
            return;
        };
        let extension = language_for_path(&self.conv.editor.documents[index].path).to_owned();
        let navigation_range = self
            .conv
            .editor
            .navigation_target
            .as_ref()
            .filter(|(path, _)| path == &self.conv.editor.documents[index].path)
            .map(|(_, range)| range.clone());
        if navigation_range.is_some() {
            self.conv.editor.navigation_target = None;
        }
        let goto_definition_requested =
            std::mem::take(&mut self.conv.editor.goto_definition_requested);
        // Keep match geometry for one extra frame while Find closes so Escape/X can
        // apply the current match caret before the panel disappears.
        let find_ranges = if self.conv.editor.find_open
            || self.conv.editor.find_select_pending
            || self.conv.editor.find_focus_editor_pending
        {
            find_match_ranges(
                &self.conv.editor.documents[index].content,
                &self.conv.editor.find_query,
                self.conv.editor.find_case_sensitive,
            )
        } else {
            Vec::new()
        };
        let active_find_match = (!find_ranges.is_empty()).then(|| {
            self.conv
                .editor
                .find_active_match
                .min(find_ranges.len() - 1)
        });
        let select_find_match = self.conv.editor.find_select_pending && active_find_match.is_some();
        let reveal_find_match = (select_find_match || self.conv.editor.find_reveal_pending)
            && active_find_match.is_some();
        let focus_editor_for_find = std::mem::take(&mut self.conv.editor.find_focus_editor_pending);
        let focus_editor_requested =
            focus_editor_for_find || std::mem::take(&mut self.conv.editor.focus_editor_next_frame);
        self.conv.editor.find_select_pending = false;
        self.conv.editor.find_reveal_pending = false;
        let logical_line_count = self.conv.editor.documents[index]
            .minimap_cache
            .as_ref()
            .map_or_else(
                || {
                    self.conv.editor.documents[index]
                        .content
                        .bytes()
                        .filter(|byte| *byte == b'\n')
                        .count()
                        + 1
                },
                |geometry| geometry.line_count,
            );
        let gutter_digits = logical_line_count.to_string().len().max(2) as f32;
        let digit_width = ui.fonts_mut(|fonts| {
            fonts
                .glyph_width(&FontId::monospace(FS_SMALL), '0')
                .max(FS_SMALL * 0.5)
        });
        const GIT_MARKER_WIDTH: f32 = 2.0;
        // Keep the line numbers at their original position while placing the slimmer Git
        // marker flush against the editor container's left boundary.
        const GUTTER_LEFT_PADDING: f32 = 10.0;
        const GUTTER_RIGHT_PADDING: f32 = 12.0;
        let gutter_width = gutter_digits * digit_width
            + GUTTER_LEFT_PADDING
            + GUTTER_RIGHT_PADDING
            + GIT_MARKER_WIDTH;
        let root = PathBuf::from(&self.active_workspace().root_path);
        let relative_path = self.conv.editor.documents[index]
            .path
            .strip_prefix(&root)
            .unwrap_or(&self.conv.editor.documents[index].path)
            .to_string_lossy()
            .replace('\\', "/");
        let disk_git_line_changes = self
            .conv
            .git
            .line_changes
            .get(&relative_path)
            .cloned()
            .unwrap_or_default();
        // Git itself only sees the saved file. When editing changes the number of lines,
        // project those disk-based markers onto the in-memory buffer so the gutter follows
        // inserted/deleted newlines without doing Git work on every keystroke.
        let git_line_changes = if self.conv.editor.documents[index].is_dirty() {
            live_git_line_changes(
                &disk_git_line_changes,
                &self.conv.editor.documents[index].saved_content,
                &self.conv.editor.documents[index].content,
            )
        } else {
            disk_git_line_changes
        };
        const MINIMAP_WIDTH: f32 = 96.0;
        let editor_view_size = ui.available_size();
        let mut goto_definition_byte = None;
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;

            // The gutter is fixed, like Sublime's: horizontal scrolling only moves source text.
            let (_, gutter_rect) =
                ui.allocate_space(egui::vec2(gutter_width, editor_view_size.y.max(24.0)));
            ui.painter().vline(
                gutter_rect.right(),
                gutter_rect.y_range(),
                egui::Stroke::new(1.0, c_border_subtle()),
            );

            const MINIMAP_SCROLLBAR_WIDTH: f32 = 10.0;
            let editor_view_width =
                (editor_view_size.x - gutter_width - MINIMAP_WIDTH - MINIMAP_SCROLLBAR_WIDTH)
                    .max(80.0);
            let scroll_output = ui
                .vertical(|ui| {
                    ui.set_width(editor_view_width);
                    ui.set_height(editor_view_size.y.max(24.0));
                    ScrollArea::both()
                        .id_salt("text_editor_scroll")
                        // A shared scrollbar is painted to the right of the minimap below.
                        .scroll_bar_visibility(ScrollBarVisibility::AlwaysHidden)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            let editor_size = egui::vec2(
                                editor_view_width.max(80.0),
                                editor_view_size.y.max(24.0),
                            );
                            let select_all_requested = ui.input(|input| {
                                input.modifiers.command && input.key_pressed(egui::Key::A)
                            });
                            let document = &mut self.conv.editor.documents[index];
                            let revision = document.content_revision;
                            let pixels_per_point_bits = ui.ctx().pixels_per_point().to_bits();
                            let allow_layout_cache = !has_mutating_text_input(ui);
                            let layout_cache = &mut document.layout_cache;
                            let mut layouter =
                                |ui: &Ui, text: &dyn egui::TextBuffer, wrap_width: f32| {
                                    let wrap_width_bits = wrap_width.round().to_bits();
                                    if allow_layout_cache
                                        && layout_cache.revision == revision
                                        && layout_cache.wrap_width_bits == wrap_width_bits
                                        && layout_cache.pixels_per_point_bits
                                            == pixels_per_point_bits
                                        && let Some(galley) = &layout_cache.geometry
                                    {
                                        return Arc::clone(galley);
                                    }

                                    // TextEdit only needs glyph geometry here; cache it before
                                    // egui's whole-LayoutJob hashing so selection-only frames are O(1).
                                    let mut job = egui::text::LayoutJob::simple(
                                        text.as_str().to_owned(),
                                        FontId::monospace(FS_SMALL),
                                        egui::Color32::TRANSPARENT,
                                        wrap_width,
                                    );
                                    job.wrap.max_width = wrap_width;
                                    let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
                                    if allow_layout_cache {
                                        layout_cache.revision = revision;
                                        layout_cache.wrap_width_bits = wrap_width_bits;
                                        layout_cache.pixels_per_point_bits = pixels_per_point_bits;
                                        layout_cache.geometry = Some(Arc::clone(&galley));
                                        // The cached syntax galley was laid out for the previous
                                        // wrap width / dpi. The keys now describe this new
                                        // geometry, so keeping it would repaint stale rows (text
                                        // visibly truncated after the editor is resized).
                                        layout_cache.syntax = None;
                                    }
                                    galley
                                };
                            let mut output = ui
                                .scope(|ui| {
                                    ui.visuals_mut().extreme_bg_color = egui::Color32::TRANSPARENT;
                                    // Paint the complete selection ourselves after TextEdit. Keeping
                                    // egui's pass transparent avoids the moving edge being painted once
                                    // natively and once from our syntax galley (the last-row flicker).
                                    ui.visuals_mut().selection.bg_fill = egui::Color32::TRANSPARENT;
                                    ui.visuals_mut().selection.stroke = egui::Stroke::NONE;
                                    // The native caret is hidden and repainted after syntax text
                                    // below, giving it identical pixel width on empty and text rows.
                                    ui.visuals_mut().text_cursor.stroke.color =
                                        egui::Color32::TRANSPARENT;
                                    ui.visuals_mut().text_cursor.blink = false;
                                    TextEdit::multiline(&mut document.content)
                                        .id_salt(("workspace_text_editor", index))
                                        .font(FontId::monospace(FS_SMALL))
                                        .code_editor()
                                        .frame(egui::Frame::NONE)
                                        .background_color(egui::Color32::TRANSPARENT)
                                        .text_revision(document.content_revision)
                                        .emit_selection_events(false)
                                        .scroll_to_cursor(!select_all_requested)
                                        .desired_width(f32::INFINITY)
                                        .min_size(editor_size)
                                        .margin(Margin::same(8))
                                        .layouter(&mut layouter)
                                        .show(ui)
                                })
                                .inner;
                            if output.response.changed() {
                                document.content_revision =
                                    document.content_revision.wrapping_add(1);
                                document.dirty = document.content != document.saved_content;
                                document.layout_cache = EditorLayoutCache::default();
                                document.minimap_cache = None;
                            }
                            // TextEdit reports `text_clip_rect` as the full text rect — the whole
                            // document laid out inside the ScrollArea — not the visible viewport.
                            // Every "visible only" cull below must use the real viewport, or it
                            // silently degrades to whole-file work per frame (a select-all in a
                            // few-thousand-line file drops to ~12 fps otherwise).
                            let viewport_clip = ui.clip_rect().intersect(output.text_clip_rect);
                            let selection_target = navigation_range.as_ref();
                            let find_caret_target = select_find_match
                                .then(|| &find_ranges[active_find_match.unwrap_or(0)]);
                            if let Some(byte_range) = selection_target.or(find_caret_target) {
                                let start = document.content[..byte_range.start].chars().count();
                                let end =
                                    start + document.content[byte_range.clone()].chars().count();
                                let cursor_range = if selection_target.is_some() {
                                    egui::text::CCursorRange::two(
                                        egui::text::CCursor::new(start),
                                        egui::text::CCursor::new(end),
                                    )
                                } else {
                                    // Find navigation places an insertion caret immediately after
                                    // the match, ready to continue editing the document.
                                    egui::text::CCursorRange::one(egui::text::CCursor::new(end))
                                };
                                output.state.cursor.set_char_range(Some(cursor_range));
                                output.state.store(ui.ctx(), output.response.id);
                                if focus_editor_requested {
                                    output.response.request_focus();
                                }
                            }

                            // Scrolling only needs match geometry. Keep it separate from cursor
                            // mutation so live query updates cannot disturb the focused Find field.
                            let reveal_target = selection_target.or_else(|| {
                                reveal_find_match
                                    .then(|| &find_ranges[active_find_match.unwrap_or(0)])
                            });
                            if let Some(byte_range) = reveal_target {
                                let end = document.content[..byte_range.end].chars().count();
                                let caret = output
                                    .galley
                                    .pos_from_cursor(egui::text::CCursor {
                                        index: egui::text::CharIndex(end),
                                        prefer_next_row: true,
                                    })
                                    .translate(output.galley_pos.to_vec2());
                                ui.scroll_to_rect(caret, Some(egui::Align::Center));
                            }

                            if focus_editor_requested && find_caret_target.is_none() {
                                output.response.request_focus();
                            }

                            // The primary caret's char index is free from the cursor range. The
                            // byte offset (needed only when a definition jump fires) and the logical
                            // line are derived from the layout, not by walking the document. The old
                            // char-by-char scans grew to the whole file whenever the caret sat near
                            // the end, e.g. right after Select All.
                            let caret_char = output.cursor_range.map(|range| range.primary.index.0);
                            let active_line = output
                                .cursor_range
                                .map(|range| caret_logical_line(&output.galley, range.primary));
                            let definition_modifier =
                                ui.input(|input| input.modifiers.command || input.modifiers.ctrl);
                            let hovered_definition = if extension == "rs"
                                && definition_modifier
                                && output.response.hovered()
                            {
                                ui.input(|input| input.pointer.hover_pos())
                                    .filter(|position| viewport_clip.contains(*position))
                                    .and_then(|position| {
                                        let cursor = output
                                            .galley
                                            .cursor_from_pos(position - output.galley_pos);
                                        let byte =
                                            char_index_to_byte(&document.content, cursor.index.0);
                                        crate::rust_goto::identifier_at(&document.content, byte)
                                            .map(|(_, range)| range)
                                            .filter(|range| {
                                                byte_range_rects(
                                                    &output.galley,
                                                    output.galley_pos,
                                                    &document.content,
                                                    range,
                                                )
                                                .iter()
                                                .any(|rect| rect.contains(position))
                                            })
                                    })
                            } else {
                                None
                            };
                            if hovered_definition.is_some() {
                                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                            }
                            let click_byte = (output.response.clicked() && definition_modifier)
                                .then(|| hovered_definition.as_ref().map(|range| range.start))
                                .flatten();
                            let mut context_goto = false;
                            output.response.context_menu(|ui| {
                                if extension == "rs"
                                    && ui.button("Go to definition    F12").clicked()
                                {
                                    context_goto = true;
                                    ui.close();
                                }
                            });
                            let navigation_request = if extension == "rs" {
                                click_byte.or_else(|| {
                                    (goto_definition_requested || context_goto)
                                        .then(|| {
                                            caret_char.map(|index| {
                                                char_index_to_byte(&document.content, index)
                                            })
                                        })
                                        .flatten()
                                })
                            } else {
                                None
                            };
                            let selection = output.cursor_range.filter(|range| !range.is_empty());
                            let wrap_width_bits =
                                output.galley.job.wrap.max_width.round().to_bits();
                            let pixels_per_point_bits = ui.ctx().pixels_per_point().to_bits();
                            let can_reuse_syntax = find_ranges.is_empty()
                                && hovered_definition.is_none()
                                && document.layout_cache.revision == document.content_revision
                                && document.layout_cache.wrap_width_bits == wrap_width_bits
                                && document.layout_cache.pixels_per_point_bits
                                    == pixels_per_point_bits;
                            let mut syntax_job = None;
                            let visible_galley = if can_reuse_syntax {
                                document.layout_cache.syntax.as_ref().map(Arc::clone)
                            } else {
                                None
                            }
                            .unwrap_or_else(|| {
                                let mut job = crate::theme::highlight_editor_code_with_revision(
                                    &mut document.syntax_state,
                                    &document.content,
                                    &extension,
                                    FontId::monospace(FS_SMALL),
                                    Some(document.content_revision),
                                )
                                .unwrap_or_else(|| {
                                    themed_highlight(
                                        ui,
                                        &document.content,
                                        &extension,
                                        FontId::monospace(FS_SMALL),
                                    )
                                });
                                if document.minimap_cache.is_none() {
                                    ensure_minimap_geometry(
                                        &document.content,
                                        &job,
                                        &mut document.minimap_cache,
                                    );
                                }
                                apply_search_highlights(&mut job, &find_ranges, active_find_match);
                                if let Some(range) = hovered_definition.as_ref() {
                                    apply_definition_underline(&mut job, range);
                                }
                                job.wrap.max_width = output.galley.job.wrap.max_width;
                                let galley = ui.fonts_mut(|fonts| fonts.layout_job(job.clone()));
                                syntax_job = Some(job);
                                // Store only when the cache keys already describe this exact
                                // layout (they are written by the geometry layouter). Overwriting
                                // the keys here could relabel a geometry galley from an older
                                // wrap width as current and corrupt both caches.
                                if find_ranges.is_empty()
                                    && hovered_definition.is_none()
                                    && document.layout_cache.revision == document.content_revision
                                    && document.layout_cache.wrap_width_bits == wrap_width_bits
                                    && document.layout_cache.pixels_per_point_bits
                                        == pixels_per_point_bits
                                {
                                    document.layout_cache.syntax = Some(Arc::clone(&galley));
                                }
                                galley
                            });
                            if document.minimap_cache.is_none()
                                && let Some(job) = syntax_job.as_ref()
                            {
                                ensure_minimap_geometry(
                                    &document.content,
                                    job,
                                    &mut document.minimap_cache,
                                );
                            }
                            paint_indent_guides(
                                ui,
                                &output.galley,
                                output.galley_pos,
                                viewport_clip,
                                &document
                                    .minimap_cache
                                    .as_ref()
                                    .expect("editor geometry was just prepared")
                                    .indent_columns,
                            );

                            // Paint selection behind the visible galley. The previous order put a
                            // translucent wash over the glyphs; depending on the backend it looked
                            // opaque and left only our whitespace markers visible.
                            if let Some(selection) = selection {
                                let selection_color = crate::theme::editor_selection_fill();
                                let selection_painter = ui.painter().with_clip_rect(viewport_clip);
                                let selection_rects = editor_selection_rects(
                                    &output.galley,
                                    output.galley_pos,
                                    viewport_clip,
                                    selection,
                                );
                                paint_editor_selection(
                                    &selection_painter,
                                    &selection_rects,
                                    selection_color,
                                );
                            }

                            // TextEdit's geometry is transparent; paint the cached syntax galley.
                            ui.painter().with_clip_rect(viewport_clip).galley(
                                output.galley_pos,
                                visible_galley,
                                c_text(),
                            );

                            if let Some(selection) = selection {
                                paint_selected_whitespace(
                                    ui,
                                    &output.galley,
                                    output.galley_pos,
                                    viewport_clip,
                                    &document.content,
                                    selection,
                                );
                            }
                            if output.response.has_focus()
                                && let Some(cursor_range) = output.cursor_range
                            {
                                paint_editor_caret(
                                    ui,
                                    &output.galley,
                                    output.galley_pos,
                                    viewport_clip,
                                    cursor_range.primary,
                                );
                            }

                            let selected_lines = selection
                                .map(|range| selected_logical_lines(&output.galley, range));

                            // Return the screen-space positions needed to paint the fixed gutter,
                            // plus whether the pointer is extending a text selection. egui normally
                            // suppresses ScrollArea wheel input while a child is being dragged, so
                            // the latter is used below to keep editor scrolling responsive.
                            //
                            // Only the lines inside the viewport are emitted. The gutter lays out a
                            // number glyph per entry, so returning every logical line made a 3000-line
                            // file shape 3000 tiny galleys each frame; culling here keeps that O(visible).
                            let gutter_clip_range = viewport_clip.y_range();
                            let gutter_line_height = FS_SMALL * 1.35;
                            let mut logical_line = 0usize;
                            let mut line_positions: Vec<(usize, f32)> = Vec::new();
                            for (row, placed_row) in output.galley.rows.iter().enumerate() {
                                let starts_line =
                                    row == 0 || output.galley.rows[row - 1].ends_with_newline;
                                if !starts_line {
                                    continue;
                                }
                                let y = placed_row
                                    .rect()
                                    .translate(output.galley_pos.to_vec2())
                                    .center()
                                    .y;
                                if y >= gutter_clip_range.min - gutter_line_height
                                    && y <= gutter_clip_range.max + gutter_line_height
                                {
                                    line_positions.push((logical_line, y));
                                }
                                logical_line += 1;
                            }
                            (
                                line_positions,
                                output.response.dragged(),
                                selected_lines,
                                navigation_request,
                                active_line,
                                caret_char,
                            )
                        })
                })
                .inner;

            let gutter_clip = gutter_rect.intersect(ui.clip_rect());
            for (line, y) in scroll_output.inner.0.iter().copied() {
                let line_height = FS_SMALL * 1.35;
                if scroll_output.inner.4 == Some(line) {
                    ui.painter().with_clip_rect(gutter_clip).rect_filled(
                        egui::Rect::from_center_size(
                            egui::pos2(gutter_rect.center().x, y),
                            egui::vec2(gutter_rect.width(), line_height),
                        ),
                        0.0,
                        c_row_active(),
                    );
                }
                if let Some(change) = git_line_changes.iter().find(|change| change.line == line) {
                    let color = match change.kind {
                        crate::git::GitLineKind::Added => c_diff_add_fg(),
                        crate::git::GitLineKind::Modified => c_warning_fg(),
                    };
                    ui.painter().with_clip_rect(gutter_clip).rect_filled(
                        egui::Rect::from_center_size(
                            egui::pos2(gutter_rect.left() + GIT_MARKER_WIDTH * 0.5, y),
                            egui::vec2(GIT_MARKER_WIDTH, line_height),
                        ),
                        0.0,
                        color,
                    );
                }
                ui.painter().with_clip_rect(gutter_clip).text(
                    egui::pos2(gutter_rect.right() - GUTTER_RIGHT_PADDING, y),
                    egui::Align2::RIGHT_CENTER,
                    line + 1,
                    FontId::monospace(FS_SMALL),
                    if scroll_output.inner.4 == Some(line) {
                        c_text_muted()
                    } else {
                        c_text_faint()
                    },
                );
            }

            // ScrollArea deliberately ignores the wheel while TextEdit owns a selection drag.
            // Restore that expected editor behavior and also auto-scroll when the pointer approaches
            // the top/bottom edge while extending the selection.
            let selection_scroll = if scroll_output.inner.1 {
                let wheel_y = ui.input(|input| input.smooth_scroll_delta.y);
                let edge_y =
                    ui.input(|input| input.pointer.interact_pos())
                        .map_or(0.0, |pointer| {
                            const EDGE_ZONE: f32 = 28.0;
                            if pointer.y < scroll_output.inner_rect.top() + EDGE_ZONE {
                                ((scroll_output.inner_rect.top() + EDGE_ZONE - pointer.y)
                                    / EDGE_ZONE)
                                    .clamp(0.0, 2.5)
                                    * -12.0
                            } else if pointer.y > scroll_output.inner_rect.bottom() - EDGE_ZONE {
                                ((pointer.y - (scroll_output.inner_rect.bottom() - EDGE_ZONE))
                                    / EDGE_ZONE)
                                    .clamp(0.0, 2.5)
                                    * 12.0
                            } else {
                                0.0
                            }
                        });
                -wheel_y + edge_y
            } else {
                0.0
            };
            if selection_scroll != 0.0 {
                let mut state = scroll_output.state;
                let max_y =
                    (scroll_output.content_size.y - scroll_output.inner_rect.height()).max(0.0);
                state.offset.y = (state.offset.y + selection_scroll).clamp(0.0, max_y);
                state.store(ui.ctx(), scroll_output.id);
                ui.ctx().request_repaint();
            }

            goto_definition_byte = scroll_output.inner.3;
            if let Some(caret_char) = scroll_output.inner.5 {
                self.conv.editor.navigation_cursor_char = caret_char;
            }

            // The minimap is outside the editor ScrollArea. Its own narrow scroll strip is painted
            // after it, so the visual order is source → minimap → scrollbar.
            if let Some(fraction) = paint_minimap(
                ui,
                egui::vec2(MINIMAP_WIDTH, editor_view_size.y.max(24.0)),
                &scroll_output,
                scroll_output.inner.2,
                self.conv.editor.documents[index]
                    .minimap_cache
                    .as_ref()
                    .expect("minimap geometry is prepared during editor rendering"),
            ) {
                let mut state = scroll_output.state;
                let max_y =
                    (scroll_output.content_size.y - scroll_output.inner_rect.height()).max(0.0);
                state.offset.y = max_y * fraction;
                state.store(ui.ctx(), scroll_output.id);
                ui.ctx().request_repaint();
            }
        });
        if let Some(byte) = goto_definition_byte {
            self.go_to_rust_definition(byte);
        }
    }

    fn go_to_rust_definition(&mut self, cursor_byte: usize) {
        let Some(document) = self.conv.editor.active_document() else {
            return;
        };
        if language_for_path(&document.path) != "rs" {
            return;
        }
        let root = PathBuf::from(&self.active_workspace().root_path);
        let current_path = document.path.clone();
        let current_source = document.content.clone();
        let open_buffers = self
            .conv
            .editor
            .documents
            .iter()
            .map(|document| (document.path.clone(), document.content.clone()))
            .collect::<Vec<_>>();
        match crate::rust_goto::find_definition(
            &root,
            &current_path,
            &current_source,
            cursor_byte,
            &open_buffers,
        ) {
            Some(location) => {
                self.conv
                    .editor
                    .navigation_back
                    .push((current_path, cursor_byte..cursor_byte));
                self.conv.editor.navigation_forward.clear();
                self.open_editor_file(location.path.clone());
                self.conv.editor.navigation_target = Some((location.path, location.byte_range));
                // Jumping to a definition opens/reuses a tab without focus: hand focus back
                // so the caret is live at the target selection, ready to keep editing.
                self.conv.editor.focus_editor_next_frame = true;
                self.conv.editor.error = None;
            }
            None => {
                self.conv.editor.error = Some("Rust definition not found.".into());
            }
        }
    }

    fn navigate_editor_history(&mut self, forward: bool) {
        let target = if forward {
            self.conv.editor.navigation_forward.pop()
        } else {
            self.conv.editor.navigation_back.pop()
        };
        let Some((path, range)) = target else {
            return;
        };
        if let Some(current) = self.conv.editor.active_document() {
            // The caret is tracked as a char index each frame; resolve it to a byte offset only
            // here, when a jump actually records the current location.
            let byte =
                char_index_to_byte(&current.content, self.conv.editor.navigation_cursor_char);
            let current_location = (current.path.clone(), byte..byte);
            if forward {
                self.conv.editor.navigation_back.push(current_location);
            } else {
                self.conv.editor.navigation_forward.push(current_location);
            }
        }
        self.open_editor_file(path.clone());
        self.conv.editor.navigation_target = Some((path, range));
        // History jumps land on a selection like definition jumps; keep the caret live.
        self.conv.editor.focus_editor_next_frame = true;
    }

    fn close_editor_git_diff(&mut self) {
        self.request(crate::git::GitOp::ClearDiff);
        self.conv.diff_view_open = false;
        self.conv.editor.diff_tab_active = false;
    }

    /// Open the file behind the currently shown git diff in an editable tab.
    pub(crate) fn open_current_diff_file(&mut self) {
        let Some(relative) = self.conv.git.current_diff_path.clone() else {
            return;
        };
        let root = PathBuf::from(&self.active_workspace().root_path);
        self.open_editor_file(root.join(relative));
        self.conv.editor.focus_editor_next_frame = true;
    }

    /// The git diff rendered as an editor tab: files stay open and editable next to it.
    fn render_editor_git_diff(&mut self, ui: &mut Ui) {
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.close_editor_git_diff();
            return;
        }
        let Some((title, diff_text)) = self.conv.git.diff.clone() else {
            return;
        };
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.label(RichText::new("Diff").size(FS_H3).color(c_text()).strong());
            ui.label(
                RichText::new(&title)
                    .size(FS_SMALL)
                    .color(c_text_muted())
                    .monospace(),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if crate::ui::chrome::icon_button_plain(ui, ICON_CLOSE, 24.0, false)
                    .on_hover_text("Close diff (Esc)")
                    .clicked()
                {
                    self.close_editor_git_diff();
                }
                if self.conv.git.current_diff_path.is_some()
                    && crate::ui::chrome::mini_button_icon_enabled(
                        ui,
                        ICON_PROMPTS,
                        "Edit file",
                        true,
                    )
                    .on_hover_text("Open this file in an editable tab")
                    .clicked()
                {
                    self.open_current_diff_file();
                }
            });
        });
        ui.add_space(2.0);
        crate::ui::chrome::hairline(ui);
        ui.add_space(4.0);

        let wrap_width = ui.available_width().max(200.0);
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hash::hash(&title, &mut hasher);
        std::hash::Hash::hash(&diff_text, &mut hasher);
        let key = (std::hash::Hasher::finish(&hasher), wrap_width.to_bits());
        let cached = self
            .conv
            .diff_job_cache
            .as_ref()
            .is_some_and(|(hash, width, _)| (*hash, *width) == key);
        if !cached {
            let job = crate::ui::diff::diff_layout_job(&diff_text, wrap_width);
            self.conv.diff_job_cache = Some((key.0, key.1, job));
        }
        ScrollArea::vertical()
            .id_salt("editor_git_diff_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if let Some((_, _, job)) = &self.conv.diff_job_cache {
                    ui.add(egui::Label::new(job.clone()).selectable(true));
                }
            });
    }

    fn render_editor_diff(&mut self, ui: &mut Ui) {
        let Some(document) = self.conv.editor.active_document() else {
            return;
        };
        let name = document.path.display().to_string();
        let diff = crate::agent::tools::make_unified_diff(
            &name,
            &document.saved_content,
            &document.content,
        );
        ScrollArea::both().show(ui, |ui| {
            if diff.is_empty() {
                ui.label("No unsaved changes.");
            } else {
                let job = crate::ui::diff::diff_layout_job(&diff, f32::INFINITY);
                ui.add(egui::Label::new(job).selectable(true));
            }
        });
    }

    fn reveal_active_file(&mut self) {
        let Some(path) = self
            .conv
            .editor
            .active_document()
            .map(|document| document.path.clone())
        else {
            return;
        };
        let root = PathBuf::from(&self.active_workspace().root_path);
        let mut parent = path.parent();
        while let Some(directory) = parent {
            if directory.starts_with(&root) {
                self.conv.explorer_expanded.insert(directory.to_path_buf());
            }
            if directory == root {
                break;
            }
            parent = directory.parent();
        }
        self.conv.sidebar_mode = super::state::SidebarMode::Explorer;
        self.conv.sidebar_open = true;
    }
}

fn live_git_line_changes(
    disk_changes: &[crate::git::GitLineChange],
    saved: &str,
    current: &str,
) -> Vec<crate::git::GitLineChange> {
    let saved_lines = saved.split('\n').collect::<Vec<_>>();
    let current_lines = current.split('\n').collect::<Vec<_>>();
    if saved_lines.len() == current_lines.len() {
        return disk_changes.to_vec();
    }

    let prefix = saved_lines
        .iter()
        .zip(&current_lines)
        .take_while(|(saved, current)| saved == current)
        .count();
    let suffix = saved_lines[prefix..]
        .iter()
        .rev()
        .zip(current_lines[prefix..].iter().rev())
        .take_while(|(saved, current)| saved == current)
        .count();
    let old_end = saved_lines.len().saturating_sub(suffix);
    let new_end = current_lines.len().saturating_sub(suffix);
    let line_delta = current_lines.len() as isize - saved_lines.len() as isize;

    let mut changes = disk_changes
        .iter()
        .filter_map(|change| {
            let line = if change.line < prefix {
                change.line
            } else if change.line >= old_end {
                change.line.saturating_add_signed(line_delta)
            } else {
                return None;
            };
            Some(crate::git::GitLineChange { line, ..*change })
        })
        .collect::<Vec<_>>();

    let replaced_lines = old_end
        .saturating_sub(prefix)
        .min(new_end.saturating_sub(prefix));
    for line in prefix..new_end {
        let kind = if line < prefix + replaced_lines {
            crate::git::GitLineKind::Modified
        } else {
            crate::git::GitLineKind::Added
        };
        if let Some(change) = changes.iter_mut().find(|change| change.line == line) {
            // An inserted line is more specific than a pre-existing modified marker.
            if kind == crate::git::GitLineKind::Added {
                change.kind = kind;
            }
        } else {
            changes.push(crate::git::GitLineChange { line, kind });
        }
    }
    // A pure deletion has no new line to color; mark the line immediately after it instead.
    if new_end == prefix
        && prefix < current_lines.len()
        && !changes.iter().any(|change| change.line == prefix)
    {
        changes.push(crate::git::GitLineChange {
            line: prefix,
            kind: crate::git::GitLineKind::Modified,
        });
    }
    changes.sort_by_key(|change| change.line);
    changes
}

fn has_mutating_text_input(ui: &Ui) -> bool {
    ui.input(|input| {
        input.events.iter().any(|event| match event {
            egui::Event::Cut | egui::Event::Paste(_) | egui::Event::Text(_) => true,
            egui::Event::Ime(egui::ImeEvent::Preedit { .. } | egui::ImeEvent::Commit(_)) => true,
            egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } => {
                matches!(
                    key,
                    egui::Key::Backspace | egui::Key::Delete | egui::Key::Enter | egui::Key::Tab
                ) || (modifiers.command && matches!(key, egui::Key::Z | egui::Key::Y))
            }
            _ => false,
        })
    })
}

fn char_index_to_byte(content: &str, char_index: usize) -> usize {
    content
        .char_indices()
        .nth(char_index)
        .map(|(byte, _)| byte)
        .unwrap_or(content.len())
}

fn byte_range_rects(
    galley: &egui::Galley,
    galley_pos: egui::Pos2,
    content: &str,
    range: &std::ops::Range<usize>,
) -> Vec<egui::Rect> {
    let start = content[..range.start].chars().count();
    let end = start + content[range.clone()].chars().count();
    selection_rects(
        galley,
        galley_pos,
        egui::text::CCursorRange::two(
            egui::text::CCursor::new(start),
            egui::text::CCursor::new(end),
        ),
    )
}

fn editor_selection_rects(
    galley: &egui::Galley,
    galley_pos: egui::Pos2,
    clip_rect: egui::Rect,
    range: egui::text::CCursorRange,
) -> Vec<egui::Rect> {
    // The font's baseline leaves more visual space below the glyphs than above them. Moving only
    // the painted selection slightly upward keeps the text optically centered without changing
    // cursor positioning, line height, or hit-testing geometry.
    const VERTICAL_OFFSET: f32 = 1.0;
    const CARET_CLEARANCE: f32 = 1.5;
    let [start, end] = range.sorted_cursors();
    let primary_is_start = range.primary.index == start.index;
    let primary_is_end = range.primary.index == end.index;
    // Keep one row outside each clip edge so clipping never exposes the artificial start/end
    // of this reduced contour. A select-all should cost roughly one viewport to paint, not one
    // shape per line in the entire file.
    let mut rects = selection_rects_in_clip(galley, galley_pos, clip_rect, range)
        .into_iter()
        .map(|rect| rect.translate(egui::vec2(0.0, -VERTICAL_OFFSET)))
        .collect::<Vec<_>>();

    // Leave the active edge to egui's native caret instead of repainting a second caret. This keeps
    // the whole caret visible and uses egui's exact blink phase (including its typing pause/reset).
    let start_is_painted = galley
        .pos_from_cursor(start)
        .translate(galley_pos.to_vec2())
        .intersects(clip_rect);
    let end_is_painted = galley
        .pos_from_cursor(end)
        .translate(galley_pos.to_vec2())
        .intersects(clip_rect);
    if primary_is_start && start_is_painted {
        if let Some(first) = rects.first_mut() {
            first.min.x = (first.min.x + CARET_CLEARANCE).min(first.max.x);
        }
    } else if primary_is_end
        && end_is_painted
        && let Some(last) = rects.last_mut()
    {
        last.max.x = (last.max.x - CARET_CLEARANCE).max(last.min.x);
    }
    rects
}

fn selection_rects_in_clip(
    galley: &egui::Galley,
    galley_pos: egui::Pos2,
    clip_rect: egui::Rect,
    range: egui::text::CCursorRange,
) -> Vec<egui::Rect> {
    let [start_cursor, end_cursor] = range.sorted_cursors();
    let start = galley.layout_from_cursor(start_cursor);
    let end = galley.layout_from_cursor(end_cursor);
    let local_clip = clip_rect.translate(-galley_pos.to_vec2());

    let first_visible = galley
        .rows
        .partition_point(|row| row.max_y() < local_clip.top());
    let after_visible = galley
        .rows
        .partition_point(|row| row.min_y() <= local_clip.bottom());
    // Include a neighboring row at either edge. The painter clips it, while the selection outline
    // remains continuous as if the complete (potentially huge) contour had been generated.
    let first_row = start.row.max(first_visible.saturating_sub(1));
    let last_row = end
        .row
        .min(after_visible.min(galley.rows.len().saturating_sub(1)));
    if first_row > last_row {
        return Vec::new();
    }

    let mut rects = Vec::with_capacity(last_row - first_row + 1);
    for row_index in first_row..=last_row {
        let row = &galley.rows[row_index];
        let left = if row_index == start.row {
            row.row.x_offset(start.column)
        } else {
            0.0
        };
        let right = if row_index == end.row {
            row.row.x_offset(end.column)
        } else {
            row.row.size.x
                + if row.ends_with_newline {
                    row.row.height() * 0.5
                } else {
                    0.0
                }
        };
        if right > left {
            rects.push(egui::Rect::from_min_max(
                galley_pos + egui::vec2(row.pos.x + left, row.pos.y),
                galley_pos + egui::vec2(row.pos.x + right, row.pos.y + row.row.height()),
            ));
        }
    }
    rects
}

fn caret_logical_line(galley: &egui::Galley, cursor: egui::text::CCursor) -> usize {
    let row = galley.layout_from_cursor(cursor).row;
    galley.rows[..row]
        .iter()
        .filter(|row| row.ends_with_newline)
        .count()
}

fn selected_logical_lines(
    galley: &egui::Galley,
    range: egui::text::CCursorRange,
) -> (usize, usize) {
    let [start, end] = range.sorted_cursors();
    let start_row = galley.layout_from_cursor(start).row;
    let end_row = galley.layout_from_cursor(end).row;
    let start_line = galley.rows[..start_row]
        .iter()
        .filter(|row| row.ends_with_newline)
        .count();
    let end_line = start_line
        + galley.rows[start_row..end_row]
            .iter()
            .filter(|row| row.ends_with_newline)
            .count();
    (start_line, end_line)
}

fn selection_rects(
    galley: &egui::Galley,
    galley_pos: egui::Pos2,
    range: egui::text::CCursorRange,
) -> Vec<egui::Rect> {
    let [start, end] = range.sorted_cursors();
    let start = galley.layout_from_cursor(start);
    let end = galley.layout_from_cursor(end);
    let mut rects = Vec::new();
    for row_index in start.row..=end.row {
        let row = &galley.rows[row_index];
        let left = if row_index == start.row {
            row.row.x_offset(start.column)
        } else {
            0.0
        };
        let right = if row_index == end.row {
            row.row.x_offset(end.column)
        } else {
            row.row.size.x
                + if row.ends_with_newline {
                    row.row.height() * 0.5
                } else {
                    0.0
                }
        };
        if right > left {
            rects.push(egui::Rect::from_min_max(
                galley_pos + egui::vec2(row.pos.x + left, row.pos.y),
                galley_pos + egui::vec2(row.pos.x + right, row.pos.y + row.row.height()),
            ));
        }
    }
    rects
}

fn paint_selected_whitespace(
    ui: &Ui,
    galley: &egui::Galley,
    galley_pos: egui::Pos2,
    clip_rect: egui::Rect,
    _content: &str,
    range: egui::text::CCursorRange,
) {
    let painter = ui.painter().with_clip_rect(clip_rect);

    // Expose selected whitespace without outlining every selected line. Only walk visible glyphs:
    // iterating every selected character made select-all disproportionately expensive in big files.
    let selected = range.as_sorted_char_range();
    let marker_base =
        crate::theme::blend_color(c_text_muted(), active_palette().selection_stroke, 0.25);
    let marker_color = egui::Color32::from_rgba_unmultiplied(
        marker_base.r(),
        marker_base.g(),
        marker_base.b(),
        105,
    );
    let local_clip = clip_rect.translate(-galley_pos.to_vec2());
    let first_visible = galley
        .rows
        .partition_point(|row| row.max_y() < local_clip.top());
    let Some(first_row) = galley.rows.get(first_visible) else {
        return;
    };
    let mut row_start = galley
        .cursor_from_pos(egui::vec2(
            first_row.rect().left(),
            first_row.rect().center().y,
        ))
        .index
        .0;
    for (row_index, row) in galley.rows.iter().enumerate().skip(first_visible) {
        let row_len = row.row.char_count_excluding_newline().0 + usize::from(row.ends_with_newline);
        if row.min_y() > local_clip.bottom() {
            break;
        }
        for (column, glyph) in row.row.glyphs.iter().enumerate() {
            let char_index = row_start + column;
            if char_index >= selected.end.0 {
                break;
            }
            if char_index < selected.start.0 || (char_index == selected.start.0 && glyph.chr == ' ')
            {
                continue;
            }
            let marker = match glyph.chr {
                ' ' => "·",
                '\t' => "→",
                _ => continue,
            };
            let rect = galley
                .pos_from_layout_cursor(&egui::epaint::text::cursor::LayoutCursor {
                    row: row_index,
                    column: egui::text::CharIndex(column),
                })
                .translate(galley_pos.to_vec2());
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                marker,
                FontId::monospace(FS_TINY),
                marker_color,
            );
        }
        row_start += row_len;
        if row_start >= selected.end.0 {
            break;
        }
    }
}

fn paint_editor_caret(
    ui: &Ui,
    galley: &egui::Galley,
    galley_pos: egui::Pos2,
    clip_rect: egui::Rect,
    cursor: egui::text::CCursor,
) {
    let caret = galley
        .pos_from_cursor(cursor)
        .translate(galley_pos.to_vec2())
        .expand(1.5);
    let pixels_per_point = ui.ctx().pixels_per_point();
    let stroke_width = 3.0 / pixels_per_point;
    // Snap the center to the physical pixel grid. Otherwise the same logical stroke can cover a
    // different number of pixel columns depending on the caret's x coordinate.
    let x = (caret.center().x * pixels_per_point).round() / pixels_per_point;
    let palette = active_palette();
    let color = if palette == Palette::MARIANA {
        // Sublime Text 4 / Mariana's orange, shared with numeric literals.
        palette.syntax.number
    } else {
        c_text()
    };
    ui.painter().with_clip_rect(clip_rect).line_segment(
        [egui::pos2(x, caret.top()), egui::pos2(x, caret.bottom())],
        egui::Stroke::new(stroke_width, color),
    );
}

fn paint_editor_selection(painter: &egui::Painter, rects: &[egui::Rect], fill: egui::Color32) {
    const RADIUS: f32 = 2.0;
    let Some(first) = rects.first() else {
        return;
    };
    let outline = active_palette().selection_stroke;
    let stroke = egui::Stroke::new(
        1.0,
        egui::Color32::from_rgba_unmultiplied(outline.r(), outline.g(), outline.b(), 120),
    );

    // Paint the fill as a union of small rounded row rectangles. Filling the full concave contour
    // directly makes epaint triangulate it, which can produce large diagonal wedges for complex
    // selections. Overlapping connectors preserve one continuous block without those artifacts.
    for rect in rects {
        painter.rect_filled(*rect, egui::CornerRadius::same(RADIUS as u8), fill);
    }
    for rows in rects.windows(2) {
        let upper = rows[0];
        let lower = rows[1];
        let left = upper.left().max(lower.left());
        let right = upper.right().min(lower.right());
        if right > left {
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(left, upper.bottom() - RADIUS),
                    egui::pos2(right, lower.top() + RADIUS),
                ),
                0.0,
                fill,
            );
        }
    }

    // Trace one silhouette around all selected rows for the outline. Width changes become part of
    // that contour, so outer and inner corners are rounded. Equal edges collapse into one straight
    // edge, making complete aligned rows look like one continuous block.
    let mut contour = vec![first.left_top(), first.right_top()];
    for rows in rects.windows(2) {
        let upper = rows[0];
        let lower = rows[1];
        // Change width on a horizontal boundary. Connecting these corners directly would create
        // a diagonal whenever consecutive selected rows have different lengths.
        let boundary_y = (upper.bottom() + lower.top()) * 0.5;
        contour.push(egui::pos2(upper.right(), boundary_y));
        contour.push(egui::pos2(lower.right(), boundary_y));
    }
    let last = *rects.last().unwrap_or(first);
    contour.extend([last.right_bottom(), last.left_bottom()]);
    for rows in rects.windows(2).rev() {
        let upper = rows[0];
        let lower = rows[1];
        let boundary_y = (upper.bottom() + lower.top()) * 0.5;
        contour.push(egui::pos2(lower.left(), boundary_y));
        contour.push(egui::pos2(upper.left(), boundary_y));
    }
    simplify_orthogonal_contour(&mut contour);
    let rounded = rounded_contour(&contour, RADIUS);
    painter.add(egui::Shape::Path(egui::epaint::PathShape {
        points: rounded,
        closed: true,
        fill: egui::Color32::TRANSPARENT,
        stroke: stroke.into(),
    }));
}

fn simplify_orthogonal_contour(points: &mut Vec<egui::Pos2>) {
    let mut changed = true;
    while changed && points.len() > 2 {
        changed = false;
        for index in 0..points.len() {
            let previous = points[(index + points.len() - 1) % points.len()];
            let current = points[index];
            let next = points[(index + 1) % points.len()];
            let duplicate = current.distance_sq(previous) < 0.01;
            let vertical =
                (previous.x - current.x).abs() < 0.01 && (current.x - next.x).abs() < 0.01;
            let horizontal =
                (previous.y - current.y).abs() < 0.01 && (current.y - next.y).abs() < 0.01;
            if duplicate || vertical || horizontal {
                points.remove(index);
                changed = true;
                break;
            }
        }
    }
}

fn rounded_contour(points: &[egui::Pos2], radius: f32) -> Vec<egui::Pos2> {
    const STEPS: usize = 4;
    let mut rounded = Vec::with_capacity(points.len() * (STEPS + 1));
    for index in 0..points.len() {
        let previous = points[(index + points.len() - 1) % points.len()];
        let corner = points[index];
        let next = points[(index + 1) % points.len()];
        let incoming = corner - previous;
        let outgoing = next - corner;
        let corner_radius = radius
            .min(incoming.length() * 0.5)
            .min(outgoing.length() * 0.5);
        let start = corner - incoming.normalized() * corner_radius;
        let end = corner + outgoing.normalized() * corner_radius;
        rounded.push(start);
        // A short quadratic Bézier rounds convex and concave orthogonal corners alike.
        for step in 1..=STEPS {
            let t = step as f32 / STEPS as f32;
            let one_minus_t = 1.0 - t;
            rounded.push(egui::pos2(
                start.x * one_minus_t.powi(2)
                    + corner.x * (2.0 * one_minus_t * t)
                    + end.x * t.powi(2),
                start.y * one_minus_t.powi(2)
                    + corner.y * (2.0 * one_minus_t * t)
                    + end.y * t.powi(2),
            ));
        }
    }
    rounded
}

fn themed_highlight(
    _ui: &Ui,
    content: &str,
    language: &str,
    font_id: FontId,
) -> egui::text::LayoutJob {
    crate::theme::highlight_code(content, language, font_id)
}

/// Paint subtle dotted guides at each complete indentation level. Blank lines inherit the
/// shallower indentation of their nearest non-empty neighbours so guides remain continuous across
/// spacing inside a block.
fn paint_indent_guides(
    ui: &Ui,
    galley: &egui::Galley,
    galley_pos: egui::Pos2,
    clip_rect: egui::Rect,
    indent_columns: &[Option<usize>],
) {
    const TAB_WIDTH: usize = 4;
    const DASH_LENGTH: f32 = 1.5;
    const DASH_GAP: f32 = 2.5;

    let local_clip = clip_rect.translate(-galley_pos.to_vec2());
    let first_visible = galley
        .rows
        .partition_point(|row| row.max_y() < local_clip.top());
    let after_visible = galley
        .rows
        .partition_point(|row| row.min_y() <= local_clip.bottom());
    let logical_line_before = galley.rows[..first_visible]
        .iter()
        .filter(|row| row.ends_with_newline)
        .count();
    let glyph_width = ui.fonts_mut(|fonts| {
        fonts
            .glyph_width(&FontId::monospace(FS_SMALL), ' ')
            .max(FS_SMALL * 0.25)
    });
    let color = crate::theme::blend_color(c_text_faint(), c_bg_main(), 0.38);
    let painter = ui.painter().with_clip_rect(clip_rect);

    let mut logical_line = logical_line_before;
    for row_index in first_visible..after_visible {
        let row = &galley.rows[row_index];
        let starts_line = row_index == 0 || galley.rows[row_index - 1].ends_with_newline;
        if starts_line && let Some(columns) = indent_columns.get(logical_line).copied().flatten() {
            let row_rect = row.rect().translate(galley_pos.to_vec2());
            for column in (TAB_WIDTH..=columns).step_by(TAB_WIDTH) {
                let x = galley_pos.x + column as f32 * glyph_width;
                let mut y = row_rect.top().max(clip_rect.top());
                let bottom = row_rect.bottom().min(clip_rect.bottom());
                while y < bottom {
                    painter.vline(
                        x,
                        y..=(y + DASH_LENGTH).min(bottom),
                        egui::Stroke::new(1.0, color),
                    );
                    y += DASH_LENGTH + DASH_GAP;
                }
            }
        }
        if row.ends_with_newline {
            logical_line += 1;
        }
    }
}

/// One horizontal stroke in the minimap silhouette: a tab-expanded column run on a
/// single line, colored by the syntax section it came from.
struct MinimapSegment {
    line: usize,
    start_col: usize,
    end_col: usize,
    color: egui::Color32,
}

/// Cached minimap silhouette for a document. The strokes and horizontal scale depend
/// only on the buffer and syntax palette, so they are rebuilt on change instead of
/// rescanning the whole file every frame; painting then just culls to the visible strip.
pub struct MinimapGeometry {
    palette: crate::theme::SyntaxPalette,
    line_count: usize,
    max_columns: usize,
    indent_columns: Vec<Option<usize>>,
    segments: Vec<MinimapSegment>,
}

const MINIMAP_TAB_WIDTH: usize = 4;
const MINIMAP_MIN_COLUMNS: usize = 60;

fn minimap_advance_columns(mut column: usize, text: &str) -> usize {
    for character in text.chars() {
        column += match character {
            '\t' => MINIMAP_TAB_WIDTH - column % MINIMAP_TAB_WIDTH,
            _ => 1,
        };
    }
    column
}

fn build_minimap_geometry(
    content: &str,
    highlight_job: &egui::text::LayoutJob,
    palette: crate::theme::SyntaxPalette,
) -> MinimapGeometry {
    // Build line metadata and indentation guides in one pass. Blank lines inherit the shallower
    // indentation of their nearest non-empty neighbours, matching the previous visual behavior.
    let mut indent_columns = Vec::new();
    let mut max_columns = MINIMAP_MIN_COLUMNS;
    for line in content.split('\n') {
        let columns = minimap_advance_columns(0, line);
        max_columns = max_columns.max(columns);
        let indentation = minimap_advance_columns(
            0,
            line.get(..line.len() - line.trim_start_matches([' ', '\t']).len())
                .unwrap_or_default(),
        );
        indent_columns.push((!line.trim().is_empty()).then_some(indentation));
    }
    let line_count = indent_columns.len();
    let mut indentation_before = Vec::with_capacity(line_count);
    let mut nearest = None;
    for indentation in &indent_columns {
        indentation_before.push(nearest);
        if indentation.is_some() {
            nearest = *indentation;
        }
    }
    let mut nearest = None;
    for index in (0..indent_columns.len()).rev() {
        if indent_columns[index].is_some() {
            nearest = indent_columns[index];
        } else if let (Some(before), Some(after)) = (indentation_before[index], nearest) {
            indent_columns[index] = Some(before.min(after));
        }
    }

    // One stroke per visible run of source, keyed to its logical line. Only section byte
    // ranges and colors are read, so the minimap reuses the editor's cached highlight.
    let mut segments = Vec::new();
    let mut line_index = 0usize;
    let mut column = 0usize;
    for section in &highlight_job.sections {
        let start = section.byte_range.start.0.min(content.len());
        let end = section.byte_range.end.0.min(content.len());
        // Never let stale or malformed byte ranges take down the editor.
        let Some(section_text) = content.get(start..end) else {
            continue;
        };
        for fragment in section_text.split_inclusive('\n') {
            let text = fragment.trim_end_matches('\n');
            let leading_text: String = text
                .chars()
                .take_while(|character| character.is_whitespace())
                .collect();
            let visible_start = minimap_advance_columns(column, &leading_text);
            let visible_end = minimap_advance_columns(visible_start, text.trim());
            if visible_end > visible_start {
                segments.push(MinimapSegment {
                    line: line_index,
                    start_col: visible_start,
                    end_col: visible_end,
                    color: section.format.color,
                });
            }
            if fragment.ends_with('\n') {
                line_index += 1;
                column = 0;
            } else {
                column = minimap_advance_columns(column, text);
            }
        }
    }

    MinimapGeometry {
        palette,
        line_count,
        max_columns,
        indent_columns,
        segments,
    }
}

fn ensure_minimap_geometry(
    content: &str,
    highlight_job: &egui::text::LayoutJob,
    cache: &mut Option<MinimapGeometry>,
) {
    let palette = active_palette().syntax;
    if cache
        .as_ref()
        .is_none_or(|geometry| geometry.palette != palette)
    {
        *cache = Some(build_minimap_geometry(content, highlight_job, palette));
    }
}

fn paint_minimap(
    ui: &mut Ui,
    size: egui::Vec2,
    scroll: &EditorScrollOutput,
    selected_lines: Option<(usize, usize)>,
    geometry: &MinimapGeometry,
) -> Option<f32> {
    const SCROLLBAR_WIDTH: f32 = 10.0;
    let (whole_rect, response) = ui.allocate_exact_size(
        egui::vec2(size.x + SCROLLBAR_WIDTH, size.y),
        egui::Sense::click_and_drag(),
    );
    let minimap_rect = egui::Rect::from_min_max(
        whole_rect.min,
        egui::pos2(whole_rect.right() - SCROLLBAR_WIDTH, whole_rect.bottom()),
    );
    let scrollbar_rect = egui::Rect::from_min_max(
        egui::pos2(minimap_rect.right(), whole_rect.top()),
        whole_rect.max,
    );
    ui.painter().rect_filled(minimap_rect, 0.0, c_bg_main());
    ui.painter()
        .rect_filled(scrollbar_rect, 0.0, c_bg_elevated());

    // Fixed-scale rows, VS Code style. Stretching the whole file across the available height made
    // short files look sparse and crushed large files into sub-pixel noise; neither resembled the
    // source. When the file outgrows the strip, the map scrolls in sync with the editor instead.
    const ROW_HEIGHT: f32 = 2.0;
    let row_height = ROW_HEIGHT;

    let natural_height = geometry.line_count as f32 * row_height;

    let max_y = (scroll.content_size.y - scroll.inner_rect.height()).max(0.0);
    let exact_viewport_fraction =
        (scroll.inner_rect.height() / scroll.content_size.y.max(1.0)).clamp(0.0, 1.0);
    let offset_fraction = if max_y > 0.0 {
        (scroll.state.offset.y / max_y).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let map_offset = offset_fraction * (natural_height - minimap_rect.height()).max(0.0);
    let line_top = |line: usize| minimap_rect.top() + line as f32 * row_height - map_offset;

    let map_painter = ui.painter().with_clip_rect(minimap_rect);
    let scale = minimap_rect.width() / geometry.max_columns as f32;
    let first_visible_line = ((map_offset - row_height) / row_height).floor().max(0.0) as usize;
    let after_visible_line =
        ((map_offset + minimap_rect.height()) / row_height).ceil() as usize + 1;
    let first_segment = geometry
        .segments
        .partition_point(|segment| segment.line < first_visible_line);
    let after_segment = geometry
        .segments
        .partition_point(|segment| segment.line < after_visible_line);
    for segment in &geometry.segments[first_segment..after_segment] {
        let y = line_top(segment.line);
        let x = minimap_rect.left() + segment.start_col as f32 * scale;
        let width = ((segment.end_col - segment.start_col) as f32 * scale).max(1.0);
        map_painter.hline(
            x..=(x + width).min(minimap_rect.right()),
            y,
            egui::Stroke::new(
                1.35,
                crate::theme::blend_color(segment.color, c_bg_main(), 0.58),
            ),
        );
    }

    if let Some((start_line, end_line)) = selected_lines {
        let top = line_top(start_line).max(minimap_rect.top());
        let bottom = line_top(end_line + 1).min(minimap_rect.bottom());
        if bottom > minimap_rect.top() && top < minimap_rect.bottom() {
            let selection = active_palette().selection_stroke;
            map_painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(minimap_rect.left(), top),
                    egui::pos2(minimap_rect.right(), bottom.max(top + 1.0)),
                ),
                0.0,
                egui::Color32::from_rgba_unmultiplied(
                    selection.r(),
                    selection.g(),
                    selection.b(),
                    38,
                ),
            );
        }
    }

    // Show which part of the file is currently visible without overpowering the code map.
    let content_height = natural_height.min(minimap_rect.height());
    let viewport_height = (natural_height * exact_viewport_fraction).max(8.0);
    let viewport_top =
        minimap_rect.top() + offset_fraction * (content_height - viewport_height).max(0.0);
    let viewport_rect = egui::Rect::from_min_size(
        egui::pos2(minimap_rect.left() + 1.0, viewport_top),
        egui::vec2((minimap_rect.width() - 2.0).max(0.0), viewport_height),
    );
    let accent = c_accent();
    ui.painter().rect_filled(
        viewport_rect,
        1.0,
        egui::Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 18),
    );
    ui.painter().rect_stroke(
        viewport_rect,
        1.0,
        egui::Stroke::new(
            1.0,
            egui::Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 52),
        ),
        egui::StrokeKind::Inside,
    );

    let scrollbar_viewport_fraction = exact_viewport_fraction.clamp(0.06, 1.0);
    let handle_height = scrollbar_rect.height() * scrollbar_viewport_fraction;
    let handle_top =
        scrollbar_rect.top() + offset_fraction * (scrollbar_rect.height() - handle_height);
    ui.painter().rect_filled(
        egui::Rect::from_min_size(
            egui::pos2(scrollbar_rect.left() + 2.0, handle_top),
            egui::vec2(SCROLLBAR_WIDTH - 4.0, handle_height),
        ),
        3.0,
        crate::theme::blend_color(c_text_faint(), c_bg_elevated(), 0.25),
    );
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if response.clicked() || response.dragged() {
        // Map through the (possibly scrolled) map space so clicking a line jumps to that line.
        response.interact_pointer_pos().map(|position| {
            ((position.y - whole_rect.top() + map_offset) / natural_height.max(1.0)).clamp(0.0, 1.0)
        })
    } else if response.hovered() && max_y > 0.0 {
        // The minimap lives outside the editor ScrollArea, so forward wheel/trackpad movement to
        // the same stored scroll state while the pointer is anywhere over the map or its scrollbar.
        let wheel_y = ui.input(|input| input.smooth_scroll_delta.y);
        (wheel_y != 0.0).then(|| ((scroll.state.offset.y - wheel_y) / max_y).clamp(0.0, 1.0))
    } else {
        None
    }
}

fn explorer_entry_color(color: egui::Color32, git_ignored: bool) -> egui::Color32 {
    if git_ignored {
        crate::theme::blend_color(color, c_bg_sidebar(), 0.48)
    } else {
        color
    }
}

#[cfg(target_os = "macos")]
fn reveal_label() -> &'static str {
    "Reveal in Finder"
}

#[cfg(not(target_os = "macos"))]
fn reveal_label() -> &'static str {
    "Reveal in File Manager"
}

fn reveal_path_in_file_manager(path: &Path) {
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = std::process::Command::new("open");
        command.arg("-R").arg(path);
        command
    };
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = std::process::Command::new("explorer");
        command.arg(format!("/select,{}", path.display()));
        command
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = {
        let mut command = std::process::Command::new("xdg-open");
        command.arg(path.parent().unwrap_or(path));
        command
    };
    let _ = command.spawn();
}

fn paint_explorer_row(ui: &Ui, rect: egui::Rect, hovered: bool, selected: bool) {
    let fill = if selected {
        c_row_active()
    } else if hovered {
        c_row_hover()
    } else {
        egui::Color32::TRANSPARENT
    };
    if fill != egui::Color32::TRANSPARENT {
        ui.painter()
            .rect_filled(rect, egui::CornerRadius::same(RADIUS_ROW), fill);
    }
}

fn git_status_color(status: char) -> egui::Color32 {
    match status {
        '?' | 'A' => c_success(),
        'D' => c_danger(),
        'U' => c_error_fg(),
        _ => c_warning_fg(),
    }
}
