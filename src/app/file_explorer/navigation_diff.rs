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
        let block_actions = git_diff_kind(title).block_actions();
        let source = if title.starts_with("Commit ") {
            String::new()
        } else if title.starts_with("Staged: ") {
            "Staged".to_owned()
        } else if let Some(base) = super::editor_tabs::compare_base(title) {
            format!("Since {}", crate::git::checkpoint::base_label(base))
        } else {
            "Working Tree".to_owned()
        };
        crate::ui::diff_view::DiffView::sync(&mut self.conv.git_diff_view, diff_text);
        let root = PathBuf::from(&self.active_workspace().root_path);
        let can_open = |path: &str| root.join(path).is_file();
        self.conv
            .git_diff_view
            .as_mut()?
            .show(ui, &source, &can_open, block_actions)
    }

    pub(crate) fn apply_diff_action(&mut self, action: crate::ui::diff_view::DiffAction) {
        let (path, line) = match action {
            crate::ui::diff_view::DiffAction::OpenFile { path, line } => (path, line),
            crate::ui::diff_view::DiffAction::Block { action, block } => {
                self.apply_git_block(action, block);
                return;
            }
        };
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
        let action = view.show(
            ui,
            "Unsaved changes",
            &|_| true,
            &[crate::ui::diff_view::BlockAction::Revert],
        );
        if let Some(crate::ui::diff_view::DiffAction::Block { block, .. }) = action {
            // Back to the saved lines, in the buffer (undoable), not on disk.
            if let Some(index) = self.conv.editor.active {
                let edit = revert_edit(block, crate::git::BlockTarget::WorkTree);
                if let Err(error) = self.edit_document_block(ui.ctx(), index, &edit) {
                    self.conv.editor.error = Some(error);
                }
            }
        } else if let Some(crate::ui::diff_view::DiffAction::OpenFile { line, .. }) = action {
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

impl OxiApp {
    /// A per-block action from the git diff view, run by the git worker.
    fn apply_git_block(
        &mut self,
        action: crate::ui::diff_view::BlockAction,
        block: crate::ui::diff_view::DiffBlock,
    ) {
        use crate::git::{BlockEdit, BlockTarget};
        use crate::ui::diff_view::BlockAction;
        let edit = match action {
            BlockAction::Revert => revert_edit(block, BlockTarget::WorkTree),
            BlockAction::Unstage => revert_edit(block, BlockTarget::Index),
            BlockAction::Stage => BlockEdit {
                path: block.path,
                target: BlockTarget::Index,
                start: block.old_start,
                expected: block.old_lines,
                replacement: block.new_lines,
            },
        };
        if edit.target == BlockTarget::WorkTree {
            let path = PathBuf::from(&self.active_workspace().root_path).join(&edit.path);
            let dirty = self
                .conv
                .editor
                .documents
                .iter()
                .any(|document| document.path == path && document.is_dirty());
            if dirty {
                self.conv.git.error = Some(format!(
                    "{} has unsaved edits in the editor; save it first",
                    edit.path
                ));
                return;
            }
        }
        self.request(crate::git::GitOp::ApplyBlock(edit));
    }

    /// Splice a block edit into an open document as one undoable step.
    pub(crate) fn edit_document_block(
        &mut self,
        ctx: &egui::Context,
        index: usize,
        edit: &crate::git::BlockEdit,
    ) -> Result<(), String> {
        let Some(document) = self.conv.editor.documents.get_mut(index) else {
            return Ok(());
        };
        let updated = crate::git::splice_lines(&document.content, edit)?;
        let caret_byte = line_start_byte(&updated, edit.start.saturating_sub(1));
        let caret = egui::text::CCursor::new(updated[..caret_byte].chars().count());
        let id = self.conv.editor.text_edit_ids.get(&document.path).copied();
        if let Some(id) = id
            && let Some(mut state) = egui::text_edit::TextEditState::load(ctx, id)
        {
            let mut undoer = state.undoer();
            let before = state.cursor.char_range().unwrap_or_default();
            undoer.add_undo(&(before, document.content.clone()));
            let after = egui::text::CCursorRange::one(caret);
            undoer.add_undo(&(after, updated.clone()));
            state.set_undoer(undoer);
            state.cursor.set_char_range(Some(after));
            state.store(ctx, id);
        }
        document.content = updated;
        let _ = super::editor_body::mark_document_edited(document);
        Ok(())
    }
}

/// Which git diff the view shows, from its title.
#[derive(Clone, Copy, PartialEq, Eq)]
enum GitDiffKind {
    Commit,
    Staged,
    WorkTree,
    Compare,
}

fn git_diff_kind(title: &str) -> GitDiffKind {
    if title.starts_with("Commit ") {
        GitDiffKind::Commit
    } else if title.starts_with("Staged: ") {
        GitDiffKind::Staged
    } else if title.starts_with(crate::git::COMPARE_TITLE_PREFIX) {
        GitDiffKind::Compare
    } else {
        GitDiffKind::WorkTree
    }
}

impl GitDiffKind {
    fn block_actions(self) -> &'static [crate::ui::diff_view::BlockAction] {
        use crate::ui::diff_view::BlockAction;
        match self {
            Self::Commit => &[],
            Self::Staged => &[BlockAction::Unstage],
            Self::WorkTree => &[BlockAction::Stage, BlockAction::Revert],
            Self::Compare => &[BlockAction::Revert],
        }
    }
}

/// Put a block's old lines back in place of its new ones, in `target`.
fn revert_edit(
    block: crate::ui::diff_view::DiffBlock,
    target: crate::git::BlockTarget,
) -> crate::git::BlockEdit {
    crate::git::BlockEdit {
        path: block.path,
        target,
        start: block.new_start,
        expected: block.new_lines,
        replacement: block.old_lines,
    }
}

/// Byte offset where 0-based `line` starts (end of text past the last line).
fn line_start_byte(content: &str, line: usize) -> usize {
    content.split_inclusive('\n').take(line).map(str::len).sum()
}
