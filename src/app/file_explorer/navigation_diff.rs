//! Editor definition/history navigation and diff views.

use std::path::PathBuf;

use eframe::egui::{self, Ui};

use super::super::OxiApp;
use super::editor_logic::char_index_to_byte;

impl OxiApp {
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
        if self.conv.editor.documents.is_empty() {
            self.conv.sidebar_mode = super::super::state::SidebarMode::Chats;
            self.focus_active_view_next_frame();
        }
    }

    /// The git diff rendered as an editor tab: files stay open and editable next to it.
    pub(super) fn render_editor_git_diff(&mut self, ui: &mut Ui) {
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.close_editor_git_diff();
            return;
        }
        if let Some(action) = self.show_git_diff_view(ui) {
            self.apply_diff_action(action);
        }
    }

    /// Sync and draw the git diff view; shared by the editor tab and the in-chat viewer.
    pub(crate) fn show_git_diff_view(
        &mut self,
        ui: &mut Ui,
    ) -> Option<crate::ui::diff_view::DiffAction> {
        let (title, diff_text) = self.conv.git.diff.as_ref()?;
        let source = if title.starts_with("Commit ") {
            ""
        } else if title.starts_with("Staged: ") {
            "Staged"
        } else {
            "Working Tree"
        };
        crate::ui::diff_view::DiffView::sync(&mut self.conv.git_diff_view, diff_text);
        let root = PathBuf::from(&self.active_workspace().root_path);
        let can_open = |path: &str| root.join(path).is_file();
        self.conv
            .git_diff_view
            .as_mut()?
            .show(ui, source, &can_open)
    }

    pub(crate) fn apply_diff_action(&mut self, action: crate::ui::diff_view::DiffAction) {
        let crate::ui::diff_view::DiffAction::OpenFile { path, line } = action;
        let path = PathBuf::from(&self.active_workspace().root_path).join(path);
        self.conv.editor.show_diff = false;
        self.open_editor_file_only(path);
        let Some(document) = self.conv.editor.active_document() else {
            return;
        };
        let document_path = document.path.clone();
        if let Some(line) = line {
            let byte = line_start_byte(&document.content, line.saturating_sub(1));
            self.conv.editor.navigation_target = Some((document_path.clone(), byte..byte));
        }
        self.conv.editor.git_full_highlight_path = Some(document_path);
        self.conv.editor.focus_editor_next_frame = true;
    }

    pub(super) fn render_editor_diff(&mut self, ui: &mut Ui) {
        let Some(document) = self.conv.editor.active_document() else {
            return;
        };
        let root = PathBuf::from(&self.active_workspace().root_path);
        let name = document
            .path
            .strip_prefix(&root)
            .unwrap_or(&document.path)
            .to_string_lossy()
            .replace('\\', "/");
        let diff = crate::agent::tools::make_unified_diff(
            &name,
            &document.saved_content,
            &document.content,
        );
        if diff.is_empty() {
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.label(
                    egui::RichText::new("No unsaved changes.").color(crate::theme::c_text_muted()),
                );
                if ui.button("Back to editor").clicked() {
                    self.conv.editor.show_diff = false;
                }
            });
            return;
        }
        crate::ui::diff_view::DiffView::sync(&mut self.conv.unsaved_diff_view, &diff);
        let Some(view) = self.conv.unsaved_diff_view.as_mut() else {
            return;
        };
        if let Some(crate::ui::diff_view::DiffAction::OpenFile { line, .. }) =
            view.show(ui, "Unsaved changes", &|_| true)
        {
            // The "file" is the open buffer itself: return to it at the clicked line.
            self.conv.editor.show_diff = false;
            if let (Some(line), Some(document)) = (line, self.conv.editor.active_document()) {
                let byte = line_start_byte(&document.content, line.saturating_sub(1));
                self.conv.editor.navigation_target = Some((document.path.clone(), byte..byte));
            }
            self.conv.editor.focus_editor_next_frame = true;
        }
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

/// Byte offset where 0-based `line` starts (end of text past the last line).
fn line_start_byte(content: &str, line: usize) -> usize {
    content.split_inclusive('\n').take(line).map(str::len).sum()
}
