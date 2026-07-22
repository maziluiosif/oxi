//! Workspace file discovery, fuzzy ranking, and temporary editor previews.

use std::path::{Path, PathBuf};

use eframe::egui::{self, RichText, ScrollArea, TextEdit};
use walkdir::WalkDir;

use crate::theme::c_text_muted;

use super::super::OxiApp;
use super::support::{fuzzy_path_score, load_gitignore_patterns, should_ignore};

impl OxiApp {
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
        self.conv.editor.git_full_highlight_path = None;
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
                            // that same frame's hover state override the arrow-selected row.
                            if pointer_moved && !keyboard_navigation && response.hovered() {
                                self.conv.editor.file_picker_selected = match_index;
                            }
                            if response.clicked() {
                                selected = Some(path.clone());
                            }
                        }
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
}
