//! Workspace file explorer and multi-tab text editor.

use super::OxiApp;
use crate::theme::c_warning_fg;
use eframe::egui::{self, RichText, Ui};

mod code_navigation;
mod documents;
mod editor_body;
mod editor_commands;
mod editor_logic;
mod editor_paint;
mod editor_tabs;
mod editor_text;
mod explorer_tree;
mod file_operations;
mod file_picker;
mod find_replace;
mod layout_cache;
mod line_layout;
mod media_view;
mod minimap;
mod navigation_diff;
mod quick_diff;
mod safety;
mod support;
mod syntax_window;

#[cfg(test)]
mod tests;

pub(crate) use code_navigation::CodeNavState;
pub(crate) use file_picker::{PickerCache, PickerOutline};
pub(crate) use find_replace::{FIND_FIELD_ID, FindCache};
pub(crate) use minimap::MinimapGeometry;
pub(crate) use support::{FindOptions, FindResults, file_icon, find_matches};

pub(crate) use explorer_tree::{ExplorerCache, git_status_color};
pub(crate) use layout_cache::EditorLayoutCache;
pub(crate) use media_view::MediaKind;
pub(crate) use quick_diff::{PendingQuickDiff, QuickDiff};

impl OxiApp {
    pub(crate) fn render_text_editor(&mut self, ui: &mut Ui) {
        self.poll_code_navigation();
        self.conv.editor.editor_area = Some((ui.ctx().cumulative_frame_nr(), ui.max_rect()));
        self.render_editor_tabs(ui);
        if self.conv.editor.diff_tab_active
            && self.conv.diff_view.open
            && self.conv.git.diff.is_some()
        {
            self.render_editor_git_diff(ui);
            return;
        }
        let Some(document) = self.conv.editor.active_document() else {
            return;
        };
        if let Some(kind) = document.media {
            self.render_media_view(ui, kind);
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
                    self.request_reload_editor_file();
                }
            });
        }
        if let Some(error) = self.conv.editor.error.clone()
            && crate::ui::chrome::dismissible_notice(ui, "editor_error", &error)
        {
            self.conv.editor.error = None;
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
            let panel_height = self.find_panel_height();
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
}
