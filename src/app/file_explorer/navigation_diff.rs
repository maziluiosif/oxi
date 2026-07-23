//! Editor definition/history navigation and diff views.

use std::path::PathBuf;

use eframe::egui::{self, RichText, ScrollArea, Ui};

use crate::theme::*;

use super::super::OxiApp;
use super::{editor_logic::char_index_to_byte, support::language_for_path};

impl OxiApp {
    pub(super) fn go_to_rust_definition(&mut self, cursor_byte: usize) {
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

    pub(super) fn navigate_editor_history(&mut self, forward: bool) {
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

    pub(crate) fn close_editor_git_diff(&mut self) {
        self.request(crate::git::GitOp::ClearDiff);
        self.conv.diff_view_open = false;
        self.conv.editor.diff_tab_active = false;
    }

    /// The git diff rendered as an editor tab: files stay open and editable next to it.
    pub(super) fn render_editor_git_diff(&mut self, ui: &mut Ui) {
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

    pub(super) fn render_editor_diff(&mut self, ui: &mut Ui) {
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

    pub(super) fn reveal_active_file(&mut self) {
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
        self.conv.sidebar_mode = super::super::state::SidebarMode::Explorer;
        self.conv.sidebar_open = true;
    }
}
