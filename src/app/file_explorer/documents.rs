//! Editor document loading, persistence, and external-change tracking.

use std::path::{Path, PathBuf};

use super::super::{EditorDocument, OxiApp};
use super::EditorLayoutCache;

const MAX_TEXT_FILE_BYTES: u64 = 2 * 1024 * 1024;

impl OxiApp {
    pub(super) fn reveal_editor_file_in_explorer(&mut self, path: &Path) {
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

    pub(crate) fn open_editor_file(&mut self, path: PathBuf) {
        self.conv.editor.git_full_highlight_path = None;
        self.open_editor_file_impl(path, true);
    }

    /// Open a document without changing or revealing the Explorer sidebar.
    pub(crate) fn open_editor_file_only(&mut self, path: PathBuf) {
        self.open_editor_file_impl(path, false);
    }

    pub(super) fn open_editor_file_impl(&mut self, path: PathBuf, reveal_in_explorer: bool) {
        let root = PathBuf::from(&self.active_workspace().root_path);
        let safe_root = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
        let safe_path = match std::fs::canonicalize(&path) {
            Ok(path) if path.starts_with(&safe_root) => path,
            _ => {
                self.conv.editor.error = Some("The file is outside the active workspace.".into());
                return;
            }
        };
        if reveal_in_explorer {
            self.conv.sidebar_mode = super::super::state::SidebarMode::Explorer;
            self.conv.sidebar_open = true;
        }
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
                    viewport_width_bits: None,
                    viewport_anchor_line: 0,
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

    pub(super) fn check_external_file_changes(&mut self) {
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

    pub(super) fn reload_active_editor_file(&mut self) {
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
}
