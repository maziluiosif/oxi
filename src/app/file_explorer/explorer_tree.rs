//! Workspace explorer tree rendering, Git decorations, and context menus.

use std::path::{Path, PathBuf};

use eframe::egui::{self, Align, Layout, RichText, ScrollArea, Ui};

use crate::theme::*;
use crate::ui::chrome::icon_glyph_rich;

use super::super::{OxiApp, state::FileOperation};
use super::support::{file_icon, is_gitignored, load_gitignore_patterns};

const ALWAYS_SKIPPED_DIRS: &[&str] = &[".git"];

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
