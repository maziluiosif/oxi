//! Workspace file explorer and multi-tab text editor.

use eframe::egui::scroll_area::ScrollBarVisibility;
use eframe::egui::{self, Align, FontId, Layout, Margin, RichText, ScrollArea, TextEdit, Ui};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::theme::*;

use super::state::FileOperation;
use super::{EditorDocument, OxiApp};

const MAX_TEXT_FILE_BYTES: u64 = 2 * 1024 * 1024;
const ALWAYS_SKIPPED_DIRS: &[&str] = &[".git"];

type EditorScrollOutput = egui::scroll_area::ScrollAreaOutput<(
    Vec<f32>,
    bool,
    Option<(usize, usize)>,
    egui::text::LayoutJob,
    String,
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
        ui.add_space(6.0);

        let label = root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_else(|| root.to_str().unwrap_or("workspace"));
        let root_response = ui
            .label(
                RichText::new(label)
                    .strong()
                    .size(FS_SMALL)
                    .color(c_sidebar_section()),
            )
            .on_hover_text(root.display().to_string());
        root_response.context_menu(|ui| {
            if ui.button("New file").clicked() {
                self.start_file_operation(FileOperation::NewFile(root.clone()));
                ui.close();
            }
            if ui.button("New folder").clicked() {
                self.start_file_operation(FileOperation::NewFolder(root.clone()));
                ui.close();
            }
        });
        ui.add_space(4.0);

        if let Some(operation) = self.conv.editor.file_operation.clone() {
            self.render_file_operation(ui, operation);
            ui.add_space(6.0);
        }

        ScrollArea::vertical()
            .id_salt("workspace_file_explorer")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                self.render_explorer_directory(ui, &root, &root, &ignored, 0)
            });

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
            if should_ignore(root, &path, kind.is_dir(), ignored) {
                continue;
            }
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
                                c_text(),
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
                let selected = self
                    .conv
                    .editor
                    .active_document()
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
                                if selected {
                                    crate::theme::blend_color(color, c_text_strong(), 0.28)
                                } else {
                                    color
                                },
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
                let response = response.on_hover_text(path.display().to_string());
                if response.clicked() {
                    self.open_editor_file(path.clone());
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

    fn render_path_context_menu(&mut self, ui: &mut Ui, path: &Path, directory: bool) {
        if directory {
            if ui.button("New file").clicked() {
                self.start_file_operation(FileOperation::NewFile(path.to_path_buf()));
                ui.close();
            }
            if ui.button("New folder").clicked() {
                self.start_file_operation(FileOperation::NewFolder(path.to_path_buf()));
                ui.close();
            }
        } else if ui.button("Open").clicked() {
            self.open_editor_file(path.to_path_buf());
            ui.close();
        }
        if ui.button("Rename").clicked() {
            self.start_file_operation(FileOperation::Rename(path.to_path_buf()));
            ui.close();
        }
        if ui.button("Delete").clicked() {
            self.start_file_operation(FileOperation::Delete(path.to_path_buf()));
            ui.close();
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
            ui.add(
                TextEdit::singleline(&mut self.conv.editor.file_operation_name)
                    .desired_width(f32::INFINITY),
            );
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
                let result = std::fs::rename(&path, &destination);
                if result.is_ok() {
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

    fn open_editor_file(&mut self, path: PathBuf) {
        let root = PathBuf::from(&self.active_workspace().root_path);
        let safe_root = std::fs::canonicalize(&root).unwrap_or(root);
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
                    path: safe_path,
                    saved_content: content.clone(),
                    content,
                    disk_modified: metadata.modified().ok(),
                    externally_modified: false,
                    syntax_state: None,
                });
                self.conv.editor.active = Some(self.conv.editor.documents.len() - 1);
                self.conv.editor.error = None;
                self.conv.editor.show_diff = false;
                // An open git diff stays reachable as an editor tab; just show the file.
                self.conv.editor.diff_tab_active = false;
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
        self.conv.editor.file_picker_open = true;
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
        // Keep the palette geometry stable as filtering changes the number of matches. An
        // auto-sized, top-anchored Window visibly jumps between frames for short result lists.
        let available = ctx.content_rect().size();
        let picker_size = egui::vec2(
            560.0_f32.min((available.x - 32.0).max(280.0)),
            440.0_f32.min((available.y - 104.0).max(220.0)),
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
                ScrollArea::vertical().show(ui, |ui| {
                    if matches.is_empty() {
                        ui.label(RichText::new("No matching files").color(c_text_muted()));
                    }
                    for (match_index, path) in matches.iter().enumerate() {
                        let relative = path.strip_prefix(&root).unwrap_or(path);
                        let response = ui
                            .selectable_label(
                                match_index == self.conv.editor.file_picker_selected,
                                relative.to_string_lossy(),
                            )
                            .on_hover_text(path.display().to_string());
                        // Scrolling a hover-selected row every frame moves a different row under
                        // the stationary pointer, which changes selection again and causes a
                        // scroll/hover feedback loop. Only keyboard navigation needs auto-scroll.
                        if keyboard_navigation
                            && match_index == self.conv.editor.file_picker_selected
                        {
                            response.scroll_to_me(Some(egui::Align::Center));
                        }
                        if pointer_moved && response.hovered() {
                            self.conv.editor.file_picker_selected = match_index;
                        }
                        if response.clicked() {
                            selected = Some(path.clone());
                        }
                    }
                });
            });
        self.conv.editor.file_picker_open = open && selected.is_none();
        if let Some(path) = selected {
            self.open_editor_file(path);
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
        if self.conv.editor.find_open {
            self.render_find_replace(ui);
        }
        if let Some(error) = self.conv.editor.error.clone() {
            ui.label(RichText::new(error).size(FS_SMALL).color(c_error_fg()));
        }

        if self.conv.editor.show_diff {
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
        let git_diff_tab = self.conv.diff_view_open && self.conv.git.diff.is_some();
        let git_diff_active = git_diff_tab && self.conv.editor.diff_tab_active;
        ScrollArea::horizontal()
            .id_salt("editor_tabs")
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    for (index, document) in self.conv.editor.documents.iter().enumerate() {
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
                        let active = !git_diff_active && self.conv.editor.active == Some(index);
                        let font = FontId::proportional(FS_SMALL);
                        let label_width = ui.fonts_mut(|fonts| {
                            fonts
                                .layout_no_wrap(label.clone(), font.clone(), c_text())
                                .rect
                                .width()
                        });
                        let tab_width = label_width + 42.0;
                        let (rect, response) = ui
                            .allocate_exact_size(egui::vec2(tab_width, 28.0), egui::Sense::click());
                        // The close hit target overlaps the tab response. Test the full rectangle
                        // so the name and close icon still share one hover surface.
                        let hovered = ui.rect_contains_pointer(rect);
                        let fill = if active {
                            c_row_active()
                        } else if hovered {
                            c_row_hover()
                        } else {
                            egui::Color32::TRANSPARENT
                        };
                        if fill != egui::Color32::TRANSPARENT {
                            ui.painter().rect_filled(
                                rect,
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
                        let response = response.on_hover_text(document.path.display().to_string());
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
                            .map(|path| path.rsplit_once('/').map_or(path, |(_, file)| file))
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
                        let (rect, response) = ui
                            .allocate_exact_size(egui::vec2(tab_width, 28.0), egui::Sense::click());
                        let hovered = ui.rect_contains_pointer(rect);
                        let fill = if git_diff_active {
                            c_row_active()
                        } else if hovered {
                            c_row_hover()
                        } else {
                            egui::Color32::TRANSPARENT
                        };
                        if fill != egui::Color32::TRANSPARENT {
                            ui.painter().rect_filled(
                                rect,
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
        if let Some(index) = select {
            self.conv.editor.active = Some(index);
            self.conv.editor.diff_tab_active = false;
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
            }
        }
    }

    fn render_find_replace(&mut self, ui: &mut Ui) {
        let query_changed = self.conv.editor.find_query != self.conv.editor.find_last_query;
        if query_changed {
            self.conv
                .editor
                .find_last_query
                .clone_from(&self.conv.editor.find_query);
            self.conv.editor.find_active_match = 0;
            self.conv.editor.find_select_pending = !self.conv.editor.find_query.is_empty();
        }
        let ranges = self
            .conv
            .editor
            .active_document()
            .map(|document| find_match_ranges(&document.content, &self.conv.editor.find_query))
            .unwrap_or_default();
        if ranges.is_empty() {
            self.conv.editor.find_active_match = 0;
        } else {
            self.conv.editor.find_active_match %= ranges.len();
        }

        let mut previous = false;
        let mut next = false;
        let mut close = false;
        ui.horizontal(|ui| {
            let find_response = ui.add(
                TextEdit::singleline(&mut self.conv.editor.find_query)
                    .id_salt("workspace_editor_find")
                    .hint_text("Find"),
            );
            if query_changed {
                find_response.request_focus();
            }
            previous = ui
                .button("↑")
                .on_hover_text("Previous match (Shift+Enter)")
                .clicked();
            next = ui.button("↓").on_hover_text("Next match (Enter)").clicked();
            let position = if ranges.is_empty() {
                "0/0".to_owned()
            } else {
                format!(
                    "{}/{}",
                    self.conv.editor.find_active_match + 1,
                    ranges.len()
                )
            };
            ui.label(position);
            close = ui
                .button(RichText::new(ICON_CLOSE).family(icon_font()).size(FS_TINY))
                .on_hover_text("Close find and replace")
                .clicked();

            if find_response.has_focus() {
                let (enter, shift_enter) = ui.input_mut(|input| {
                    let shift = input.modifiers.shift;
                    let enter = input.consume_key(input.modifiers, egui::Key::Enter);
                    (enter && !shift, enter && shift)
                });
                next |= enter;
                previous |= shift_enter;
            }
        });
        if !ranges.is_empty() && (next || previous) {
            if previous {
                self.conv.editor.find_active_match = if self.conv.editor.find_active_match == 0 {
                    ranges.len() - 1
                } else {
                    self.conv.editor.find_active_match - 1
                };
            } else {
                self.conv.editor.find_active_match =
                    (self.conv.editor.find_active_match + 1) % ranges.len();
            }
            self.conv.editor.find_select_pending = true;
        }

        ui.horizontal(|ui| {
            ui.add(TextEdit::singleline(&mut self.conv.editor.replace_query).hint_text("Replace"));
            let replace_one = ui.button("Replace").clicked();
            let replace_all = ui.button("Replace all").clicked();
            if (replace_one || replace_all) && !ranges.is_empty() {
                let replacement = self.conv.editor.replace_query.clone();
                let find = self.conv.editor.find_query.clone();
                let active = self.conv.editor.find_active_match;
                if let Some(document) = self.conv.editor.active_document_mut() {
                    if replace_all {
                        document.content = document.content.replace(&find, &replacement);
                    } else {
                        document
                            .content
                            .replace_range(ranges[active].clone(), &replacement);
                    }
                }
                if replace_all {
                    self.conv.editor.find_active_match = 0;
                }
                self.conv.editor.find_select_pending = true;
            }
        });
        if close {
            self.conv.editor.find_open = false;
            self.conv.editor.find_select_pending = false;
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
        let find_ranges = if self.conv.editor.find_open {
            find_match_ranges(
                &self.conv.editor.documents[index].content,
                &self.conv.editor.find_query,
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
        self.conv.editor.find_select_pending = false;
        let logical_line_count = self.conv.editor.documents[index]
            .content
            .split('\n')
            .count()
            .max(1);
        let gutter_digits = logical_line_count.to_string().len().max(2) as f32;
        let digit_width = ui.fonts_mut(|fonts| {
            fonts
                .glyph_width(&FontId::monospace(FS_SMALL), '0')
                .max(FS_SMALL * 0.5)
        });
        const GIT_MARKER_WIDTH: f32 = 4.0;
        let gutter_width = gutter_digits * digit_width + 16.0 + GIT_MARKER_WIDTH;
        let root = PathBuf::from(&self.active_workspace().root_path);
        let relative_path = self.conv.editor.documents[index]
            .path
            .strip_prefix(&root)
            .unwrap_or(&self.conv.editor.documents[index].path)
            .to_string_lossy()
            .replace('\\', "/");
        let git_line_changes = self
            .conv
            .git
            .line_changes
            .get(&relative_path)
            .cloned()
            .unwrap_or_default();
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
                            let mut layouter =
                                |ui: &Ui, text: &dyn egui::TextBuffer, wrap_width: f32| {
                                    // TextEdit only needs glyph geometry here; the syntax-colored
                                    // galley is painted once below. Running Syntect in both passes
                                    // made every keystroke parse and lay out the whole file twice.
                                    let mut job = egui::text::LayoutJob::simple(
                                        text.as_str().to_owned(),
                                        FontId::monospace(FS_SMALL),
                                        egui::Color32::TRANSPARENT,
                                        wrap_width,
                                    );
                                    job.wrap.max_width = wrap_width;
                                    ui.fonts_mut(|fonts| fonts.layout_job(job))
                                };
                            let editor_size = egui::vec2(
                                editor_view_width.max(80.0),
                                editor_view_size.y.max(24.0),
                            );
                            let document = &mut self.conv.editor.documents[index];
                            let mut output = ui
                                .scope(|ui| {
                                    ui.visuals_mut().extreme_bg_color = egui::Color32::TRANSPARENT;
                                    // Paint the complete selection ourselves after TextEdit. Keeping
                                    // egui's pass transparent avoids the moving edge being painted once
                                    // natively and once from our syntax galley (the last-row flicker).
                                    ui.visuals_mut().selection.bg_fill = egui::Color32::TRANSPARENT;
                                    ui.visuals_mut().selection.stroke = egui::Stroke::NONE;
                                    TextEdit::multiline(&mut document.content)
                                        .id_salt(("workspace_text_editor", index))
                                        .font(FontId::monospace(FS_SMALL))
                                        .code_editor()
                                        .frame(egui::Frame::NONE)
                                        .background_color(egui::Color32::TRANSPARENT)
                                        .desired_width(f32::INFINITY)
                                        .min_size(editor_size)
                                        .margin(Margin::same(8))
                                        .layouter(&mut layouter)
                                        .show(ui)
                                })
                                .inner;
                            let selection_target = navigation_range.as_ref().or_else(|| {
                                select_find_match
                                    .then(|| &find_ranges[active_find_match.unwrap_or(0)])
                            });
                            if let Some(byte_range) = selection_target {
                                let start = document.content[..byte_range.start].chars().count();
                                let end =
                                    start + document.content[byte_range.clone()].chars().count();
                                output.state.cursor.set_char_range(Some(
                                    egui::text::CCursorRange::two(
                                        egui::text::CCursor::new(start),
                                        egui::text::CCursor::new(end),
                                    ),
                                ));
                                output.state.store(ui.ctx(), output.response.id);
                                let caret = output
                                    .galley
                                    .pos_from_cursor(egui::text::CCursor {
                                        index: egui::text::CharIndex(start),
                                        prefer_next_row: true,
                                    })
                                    .translate(output.galley_pos.to_vec2());
                                ui.scroll_to_rect(caret, Some(egui::Align::Center));
                            }

                            let caret_byte = output.cursor_range.map(|range| {
                                char_index_to_byte(&document.content, range.primary.index.0)
                            });
                            let definition_modifier =
                                ui.input(|input| input.modifiers.command || input.modifiers.ctrl);
                            let hovered_definition = if extension == "rs"
                                && definition_modifier
                                && output.response.hovered()
                            {
                                ui.input(|input| input.pointer.hover_pos())
                                    .filter(|position| output.text_clip_rect.contains(*position))
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
                                        .then_some(caret_byte)
                                        .flatten()
                                })
                            } else {
                                None
                            };
                            let selection = output.cursor_range.filter(|range| !range.is_empty());
                            // Draw indentation guides behind both selections and source text.
                            paint_indent_guides(
                                ui,
                                &output.galley,
                                output.galley_pos,
                                output.text_clip_rect,
                                &document.content,
                            );

                            // Paint selection behind the visible galley. The previous order put a
                            // translucent wash over the glyphs; depending on the backend it looked
                            // opaque and left only our whitespace markers visible.
                            if let Some(selection) = selection {
                                let selection_color = crate::theme::editor_selection_fill();
                                let selection_painter =
                                    ui.painter().with_clip_rect(output.text_clip_rect);
                                for rect in
                                    selection_rects(&output.galley, output.galley_pos, selection)
                                {
                                    selection_painter.rect_filled(rect, 0.0, selection_color);
                                }
                            }

                            // TextEdit's layout pass is transparent. Tree-sitter edits and reuses
                            // the document's syntax tree, then queries the new tree immediately.
                            let syntax_job = crate::theme::highlight_editor_code(
                                &mut document.syntax_state,
                                &document.content,
                                &extension,
                                FontId::monospace(FS_SMALL),
                            )
                            .unwrap_or_else(|| {
                                themed_highlight(
                                    ui,
                                    &document.content,
                                    &extension,
                                    FontId::monospace(FS_SMALL),
                                )
                            });
                            let mut visible_job = syntax_job.clone();
                            apply_search_highlights(
                                &mut visible_job,
                                &find_ranges,
                                active_find_match,
                            );
                            if let Some(range) = hovered_definition.as_ref() {
                                apply_definition_underline(&mut visible_job, range);
                            }
                            visible_job.wrap.max_width = output.galley.job.wrap.max_width;
                            let minimap_job = syntax_job;
                            let visible_galley =
                                ui.fonts_mut(|fonts| fonts.layout_job(visible_job));
                            ui.painter().with_clip_rect(output.text_clip_rect).galley(
                                output.galley_pos,
                                visible_galley,
                                c_text(),
                            );

                            if let Some(selection) = selection {
                                paint_selected_whitespace(
                                    ui,
                                    &output.galley,
                                    output.galley_pos,
                                    output.text_clip_rect,
                                    &document.content,
                                    selection,
                                );
                            }

                            let selected_lines = selection.map(|range| {
                                let selected = range.as_sorted_char_range();
                                let start = document
                                    .content
                                    .chars()
                                    .take(selected.start.0)
                                    .filter(|character| *character == '\n')
                                    .count();
                                let end = start
                                    + document
                                        .content
                                        .chars()
                                        .skip(selected.start.0)
                                        .take(selected.end.0 - selected.start.0)
                                        .filter(|character| *character == '\n')
                                        .count();
                                (start, end)
                            });

                            // Return the screen-space positions needed to paint the fixed gutter,
                            // plus whether the pointer is extending a text selection. egui normally
                            // suppresses ScrollArea wheel input while a child is being dragged, so
                            // the latter is used below to keep editor scrolling responsive.
                            let line_positions = output
                                .galley
                                .rows
                                .iter()
                                .enumerate()
                                .filter(|(row, _)| {
                                    *row == 0 || output.galley.rows[*row - 1].ends_with_newline
                                })
                                .map(|(_, placed_row)| {
                                    placed_row
                                        .rect()
                                        .translate(output.galley_pos.to_vec2())
                                        .center()
                                        .y
                                })
                                .collect::<Vec<_>>();
                            (
                                line_positions,
                                output.response.dragged(),
                                selected_lines,
                                minimap_job,
                                document.content.clone(),
                                navigation_request,
                            )
                        })
                })
                .inner;

            let gutter_clip = gutter_rect.intersect(ui.clip_rect());
            for (line, y) in scroll_output.inner.0.iter().copied().enumerate() {
                if let Some(change) = git_line_changes.iter().find(|change| change.line == line) {
                    let color = match change.kind {
                        crate::git::GitLineKind::Added => c_diff_add_fg(),
                        crate::git::GitLineKind::Modified => c_warning_fg(),
                    };
                    let line_height = FS_SMALL * 1.35;
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
                    egui::pos2(gutter_rect.right() - 8.0, y),
                    egui::Align2::RIGHT_CENTER,
                    line + 1,
                    FontId::monospace(FS_SMALL),
                    c_text_faint(),
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

            goto_definition_byte = scroll_output.inner.5;

            // The minimap is outside the editor ScrollArea. Its own narrow scroll strip is painted
            // after it, so the visual order is source → minimap → scrollbar.
            if let Some(fraction) = paint_minimap(
                ui,
                &scroll_output.inner.4,
                egui::vec2(MINIMAP_WIDTH, editor_view_size.y.max(24.0)),
                &scroll_output,
                scroll_output.inner.2,
                &scroll_output.inner.3,
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
                self.open_editor_file(location.path.clone());
                self.conv.editor.navigation_target = Some((location.path, location.byte_range));
                self.conv.editor.error = None;
            }
            None => {
                self.conv.editor.error = Some("Rust definition not found.".into());
            }
        }
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
    content: &str,
    range: egui::text::CCursorRange,
) {
    let painter = ui.painter().with_clip_rect(clip_rect);
    let selected_rects = selection_rects(galley, galley_pos, range);

    // Outline only the outside silhouette of the selection. Shared edges between adjacent rows
    // stay empty, avoiding the boxed-per-line appearance while preserving the outer border.
    let outline = active_palette().selection_stroke;
    let stroke = egui::Stroke::new(
        1.0,
        egui::Color32::from_rgba_unmultiplied(outline.r(), outline.g(), outline.b(), 120),
    );
    for (index, rect) in selected_rects.iter().copied().enumerate() {
        let previous = index.checked_sub(1).and_then(|i| selected_rects.get(i));
        let next = selected_rects.get(index + 1);
        paint_exposed_horizontal_edge(
            &painter,
            rect.left()..=rect.right(),
            previous,
            rect.top(),
            stroke,
        );
        paint_exposed_horizontal_edge(
            &painter,
            rect.left()..=rect.right(),
            next,
            rect.bottom(),
            stroke,
        );
        painter.line_segment([rect.left_top(), rect.left_bottom()], stroke);
        painter.line_segment([rect.right_top(), rect.right_bottom()], stroke);
    }

    // Expose selected whitespace without outlining every selected line. Spaces use centered dots;
    // tabs use a small arrow so
    // indentation remains distinguishable without showing invisibles throughout the whole file.
    let selected = range.as_sorted_char_range();
    let marker_base =
        crate::theme::blend_color(c_text_muted(), active_palette().selection_stroke, 0.25);
    let marker_color = egui::Color32::from_rgba_unmultiplied(
        marker_base.r(),
        marker_base.g(),
        marker_base.b(),
        105,
    );
    for (offset, character) in content
        .chars()
        .skip(selected.start.0)
        .take(selected.end.0 - selected.start.0)
        .enumerate()
    {
        // The selection anchor already communicates its starting position; avoid placing a dot
        // directly on top of it when the first selected character is whitespace.
        if offset == 0 && character == ' ' {
            continue;
        }
        let marker = match character {
            ' ' => "·",
            '\t' => "→",
            _ => continue,
        };
        let cursor = egui::text::CCursor {
            index: egui::text::CharIndex(selected.start.0 + offset),
            prefer_next_row: true,
        };
        let rect = galley
            .pos_from_cursor(cursor)
            .translate(galley_pos.to_vec2());
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            marker,
            FontId::monospace(FS_TINY),
            marker_color,
        );
    }
}

fn paint_exposed_horizontal_edge(
    painter: &egui::Painter,
    edge: std::ops::RangeInclusive<f32>,
    adjacent: Option<&egui::Rect>,
    y: f32,
    stroke: egui::Stroke,
) {
    let left = *edge.start();
    let right = *edge.end();
    let Some(adjacent) = adjacent else {
        painter.line_segment([egui::pos2(left, y), egui::pos2(right, y)], stroke);
        return;
    };
    if left < adjacent.left() {
        painter.line_segment(
            [
                egui::pos2(left, y),
                egui::pos2(adjacent.left().min(right), y),
            ],
            stroke,
        );
    }
    if right > adjacent.right() {
        painter.line_segment(
            [
                egui::pos2(adjacent.right().max(left), y),
                egui::pos2(right, y),
            ],
            stroke,
        );
    }
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
    content: &str,
) {
    const TAB_WIDTH: usize = 4;
    const DASH_LENGTH: f32 = 1.5;
    const DASH_GAP: f32 = 2.5;

    let lines = content.split('\n').collect::<Vec<_>>();
    let mut indent_columns = lines
        .iter()
        .map(|line| {
            let mut column = 0usize;
            for character in line.chars() {
                match character {
                    ' ' => column += 1,
                    '\t' => column += TAB_WIDTH - column % TAB_WIDTH,
                    _ => break,
                }
            }
            (!line.trim().is_empty()).then_some(column)
        })
        .collect::<Vec<_>>();

    // Preserve guides across empty separator lines, but do not invent a deeper indentation than
    // either neighbouring block has. Keep this linear: searching both sides for every blank line
    // caused quadratic work in files containing large whitespace regions.
    let mut indentation_before = Vec::with_capacity(indent_columns.len());
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

    let logical_rows = galley
        .rows
        .iter()
        .enumerate()
        .filter(|(row, _)| *row == 0 || galley.rows[*row - 1].ends_with_newline)
        .map(|(_, row)| row.rect().translate(galley_pos.to_vec2()))
        .collect::<Vec<_>>();
    let glyph_width = ui.fonts_mut(|fonts| {
        fonts
            .glyph_width(&FontId::monospace(FS_SMALL), ' ')
            .max(FS_SMALL * 0.25)
    });
    let color = crate::theme::blend_color(c_text_faint(), c_bg_main(), 0.38);
    let painter = ui.painter().with_clip_rect(clip_rect);

    for (line, row_rect) in logical_rows.iter().enumerate() {
        let Some(columns) = indent_columns.get(line).copied().flatten() else {
            continue;
        };
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
}

fn paint_minimap(
    ui: &mut Ui,
    content: &str,
    size: egui::Vec2,
    scroll: &EditorScrollOutput,
    selected_lines: Option<(usize, usize)>,
    highlight_job: &egui::text::LayoutJob,
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
    const TAB_WIDTH: usize = 4;
    const MIN_COLUMNS: usize = 60;

    fn advance_columns(mut column: usize, text: &str) -> usize {
        for character in text.chars() {
            column += match character {
                '\t' => TAB_WIDTH - column % TAB_WIDTH,
                _ => 1,
            };
        }
        column
    }

    // Keep the minimap's line count identical to the editor's, including a final empty line.
    let lines = content.split('\n').collect::<Vec<_>>();
    // Only section ranges and colors are used by the minimap. Reuse the editor's cached or
    // incrementally adjusted highlight so the minimap neither reparses nor loses colors on input.
    let job = highlight_job;
    let row_height = ROW_HEIGHT;
    let natural_height = lines.len() as f32 * row_height;
    // Tab-expanded columns so tab-indented files keep their editor silhouette; the floor keeps
    // short-lined files from being stretched to the full strip width.
    let max_columns = lines
        .iter()
        .map(|line| advance_columns(0, line))
        .max()
        .unwrap_or(1)
        .max(MIN_COLUMNS);

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
    let mut line_index = 0usize;
    let mut column = 0usize;
    for section in &job.sections {
        let start = section.byte_range.start.0.min(content.len());
        let end = section.byte_range.end.0.min(content.len());
        // Never let stale or malformed byte ranges take down the editor. Incremental highlight
        // ranges are sanitized at creation too; this guard also protects cached legacy jobs.
        let Some(section_text) = content.get(start..end) else {
            continue;
        };
        for fragment in section_text.split_inclusive('\n') {
            let text = fragment.trim_end_matches('\n');
            let y = line_top(line_index);
            if y >= minimap_rect.top() - row_height && y < minimap_rect.bottom() {
                let leading_text: String = text
                    .chars()
                    .take_while(|character| character.is_whitespace())
                    .collect();
                let visible_start = advance_columns(column, &leading_text);
                let visible_end = advance_columns(visible_start, text.trim());
                if visible_end > visible_start {
                    let scale = minimap_rect.width() / max_columns as f32;
                    let x = minimap_rect.left() + visible_start as f32 * scale;
                    let width = ((visible_end - visible_start) as f32 * scale).max(1.0);
                    map_painter.hline(
                        x..=(x + width).min(minimap_rect.right()),
                        y,
                        egui::Stroke::new(
                            1.35,
                            crate::theme::blend_color(section.format.color, c_bg_main(), 0.58),
                        ),
                    );
                }
            }
            if fragment.ends_with('\n') {
                line_index += 1;
                column = 0;
            } else {
                column = advance_columns(column, text);
            }
        }
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
    } else {
        None
    }
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

/// Sublime-style fuzzy score. Filename hits, consecutive characters, word/path boundaries and
/// earlier matches rank higher; long gaps and deep paths rank lower.
fn fuzzy_path_score(path: &str, query: &str) -> Option<i64> {
    if query.is_empty() {
        let depth = path.bytes().filter(|byte| *byte == b'/').count() as i64;
        return Some(-depth * 20 - path.len() as i64);
    }
    let path = path.to_ascii_lowercase();
    let query = query.to_ascii_lowercase();
    let filename_start = path.rfind('/').map_or(0, |index| index + 1);
    let mut score = 0i64;
    let mut search_from = 0usize;
    let mut previous = None;
    for wanted in query.chars() {
        let relative = path[search_from..].find(wanted)?;
        let index = search_from + relative;
        let boundary =
            index == 0 || matches!(path.as_bytes()[index - 1], b'/' | b'_' | b'-' | b'.' | b' ');
        score += if index >= filename_start { 90 } else { 35 };
        if boundary {
            score += 85;
        }
        if previous.is_some_and(|previous| previous + 1 == index) {
            score += 120;
        }
        score -= relative as i64 * 4;
        previous = Some(index);
        search_from = index + wanted.len_utf8();
    }
    if let Some(index) = path[filename_start..].find(&query) {
        score += 900 - index as i64 * 10;
    } else if let Some(index) = path.find(&query) {
        score += 350 - index as i64 * 3;
    }
    score -= path.len() as i64;
    score -= path.bytes().filter(|byte| *byte == b'/').count() as i64 * 12;
    Some(score)
}

fn find_match_ranges(content: &str, query: &str) -> Vec<std::ops::Range<usize>> {
    if query.is_empty() {
        return Vec::new();
    }
    content
        .match_indices(query)
        .map(|(start, matched)| start..start + matched.len())
        .collect()
}

fn apply_search_highlights(
    job: &mut egui::text::LayoutJob,
    matches: &[std::ops::Range<usize>],
    active: Option<usize>,
) {
    if matches.is_empty() {
        return;
    }
    let passive = crate::theme::blend_color(c_bg_main(), c_warning_fg(), 0.38);
    let active_color = crate::theme::blend_color(c_bg_main(), c_accent(), 0.72);
    let mut sections = Vec::with_capacity(job.sections.len() + matches.len() * 2);
    for section in &job.sections {
        let section_start = section.byte_range.start.0;
        let section_end = section.byte_range.end.0;
        let mut cursor = section_start;
        for (match_index, range) in matches.iter().enumerate() {
            let start = range.start.max(section_start);
            let end = range.end.min(section_end);
            if start >= end {
                continue;
            }
            if cursor < start {
                let mut untouched = section.clone();
                untouched.byte_range = egui::text::ByteIndex(cursor)..egui::text::ByteIndex(start);
                sections.push(untouched);
            }
            let mut highlighted = section.clone();
            highlighted.byte_range = egui::text::ByteIndex(start)..egui::text::ByteIndex(end);
            highlighted.format.background = if active == Some(match_index) {
                active_color
            } else {
                passive
            };
            sections.push(highlighted);
            cursor = end;
        }
        if cursor < section_end {
            let mut tail = section.clone();
            tail.byte_range = egui::text::ByteIndex(cursor)..egui::text::ByteIndex(section_end);
            sections.push(tail);
        }
    }
    job.sections = sections;
}

fn apply_definition_underline(job: &mut egui::text::LayoutJob, range: &std::ops::Range<usize>) {
    let mut sections = Vec::with_capacity(job.sections.len() + 2);
    for section in &job.sections {
        let section_start = section.byte_range.start.0;
        let section_end = section.byte_range.end.0;
        let start = range.start.max(section_start);
        let end = range.end.min(section_end);
        if start >= end {
            sections.push(section.clone());
            continue;
        }
        if section_start < start {
            let mut before = section.clone();
            before.byte_range = egui::text::ByteIndex(section_start)..egui::text::ByteIndex(start);
            sections.push(before);
        }
        let mut underlined = section.clone();
        underlined.byte_range = egui::text::ByteIndex(start)..egui::text::ByteIndex(end);
        underlined.format.underline = egui::Stroke::new(1.0, c_accent());
        sections.push(underlined);
        if end < section_end {
            let mut after = section.clone();
            after.byte_range = egui::text::ByteIndex(end)..egui::text::ByteIndex(section_end);
            sections.push(after);
        }
    }
    job.sections = sections;
}

fn git_status_color(status: char) -> egui::Color32 {
    match status {
        '?' | 'A' => c_success(),
        'D' => c_danger(),
        'U' => c_error_fg(),
        _ => c_warning_fg(),
    }
}

fn language_for_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "rs" => "rs",
        "py" => "py",
        "js" => "js",
        "jsx" => "jsx",
        "ts" => "ts",
        "tsx" => "tsx",
        "json" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "html" => "html",
        "css" | "scss" => "css",
        "md" => "md",
        "sh" | "bash" | "zsh" => "sh",
        "c" | "h" => "c",
        "cpp" | "cc" | "hpp" => "cpp",
        "go" => "go",
        "java" => "java",
        _ => "txt",
    }
}

fn file_icon(path: &Path) -> (&'static str, egui::Color32) {
    // Keep the glyph itself inside the verified Nerd Font set. File types are distinguished by
    // color; ad-hoc Unicode/ASCII badges rendered through the icon family can become tofu boxes
    // on platforms with stricter font fallback.
    let color = match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "rs" | "js" | "jsx" => crate::theme::blend_color(c_text_muted(), c_warning_fg(), 0.72),
        "ts" | "tsx" | "md" | "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" => {
            crate::theme::blend_color(c_text_muted(), c_accent(), 0.72)
        }
        "json" | "toml" | "yaml" | "yml" => {
            crate::theme::blend_color(c_text_muted(), c_warning_fg(), 0.72)
        }
        "html" | "css" | "scss" => crate::theme::blend_color(c_text_muted(), c_danger(), 0.68),
        _ => c_text_muted(),
    };
    (ICON_FILE, color)
}

fn load_gitignore_patterns(root: &Path) -> Vec<String> {
    std::fs::read_to_string(root.join(".gitignore"))
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#') && !line.starts_with('!'))
        .map(|line| {
            line.trim_start_matches('/')
                .trim_end_matches('/')
                .to_owned()
        })
        .collect()
}

fn should_ignore(root: &Path, path: &Path, directory: bool, patterns: &[String]) -> bool {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if name == ".gitignore" {
        return false;
    }
    if directory && ALWAYS_SKIPPED_DIRS.contains(&name) {
        return true;
    }
    let relative = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    patterns.iter().any(|pattern| {
        let direct = if pattern.contains('*') {
            glob::Pattern::new(pattern)
                .is_ok_and(|glob| glob.matches(&relative) || glob.matches(name))
        } else {
            relative == *pattern || relative.starts_with(&format!("{pattern}/")) || name == pattern
        };
        if direct {
            return true;
        }
        relative.split('/').enumerate().any(|(index, _)| {
            let suffix = relative
                .split('/')
                .skip(index)
                .collect::<Vec<_>>()
                .join("/");
            glob::Pattern::new(pattern).is_ok_and(|glob| glob.matches(&suffix))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_is_selected_from_extension() {
        assert_eq!(language_for_path(Path::new("src/main.rs")), "rs");
        assert_eq!(language_for_path(Path::new("web/app.tsx")), "tsx");
        assert_eq!(language_for_path(Path::new("README")), "txt");
    }

    #[test]
    fn fuzzy_search_ranks_filename_and_consecutive_matches_higher() {
        let direct = fuzzy_path_score("src/file_explorer.rs", "file").unwrap();
        let scattered = fuzzy_path_score("src/features/image_loader.rs", "file").unwrap();
        assert!(direct > scattered);
        assert!(fuzzy_path_score("src/file_explorer.rs", "fexp").is_some());
        assert!(fuzzy_path_score("src/file_explorer.rs", "xyz").is_none());
    }

    #[test]
    fn search_ranges_use_non_overlapping_matches() {
        assert_eq!(find_match_ranges("one two one", "one"), vec![0..3, 8..11]);
        assert!(find_match_ranges("anything", "").is_empty());
    }

    #[test]
    fn gitignore_patterns_hide_matching_paths_but_not_gitignore_itself() {
        let root = Path::new("/workspace");
        let patterns = vec!["target".into(), "*.log".into(), "build/*.js".into()];
        assert!(should_ignore(
            root,
            Path::new("/workspace/target"),
            true,
            &patterns
        ));
        assert!(should_ignore(
            root,
            Path::new("/workspace/debug.log"),
            false,
            &patterns
        ));
        assert!(should_ignore(
            root,
            Path::new("/workspace/build/app.js"),
            false,
            &patterns
        ));
        assert!(!should_ignore(
            root,
            Path::new("/workspace/.gitignore"),
            false,
            &patterns
        ));
    }
}
