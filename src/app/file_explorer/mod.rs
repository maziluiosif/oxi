//! Workspace file explorer and multi-tab text editor.

use super::OxiApp;
use crate::theme::{FS_SMALL, c_error_fg, c_warning_fg};
use eframe::egui::{self, RichText, Ui};

mod documents;
mod editor_body;
mod editor_logic;
mod editor_paint;
mod editor_tabs;
mod explorer_tree;
mod file_operations;
mod file_picker;
mod find_replace;
mod layout_cache;
mod minimap;
mod navigation_diff;
mod support;

pub(crate) use minimap::MinimapGeometry;
pub(crate) use support::find_match_ranges;

pub(crate) use layout_cache::EditorLayoutCache;

impl OxiApp {
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
}
