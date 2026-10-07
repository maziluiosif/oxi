//! Editor document loading, persistence, and external-change tracking.

use std::path::{Path, PathBuf};

use super::super::{EditorDocument, OxiApp};
use super::{EditorLayoutCache, MediaKind};

pub(super) const MAX_TEXT_FILE_BYTES: u64 = 2 * 1024 * 1024;

impl OxiApp {
    pub(crate) fn open_scratchpad(&mut self) {
        let path = crate::scratchpad::path();
        let _guard = crate::scratchpad::lock();
        if let Some(index) = self
            .conv
            .editor
            .documents
            .iter()
            .position(|document| document.is_scratchpad)
        {
            self.conv.editor.active = Some(index);
            self.conv.editor.hidden_active = None;
            self.conv.editor.diff_tab_active = false;
            self.conv.editor.markdown_preview_active = false;
            self.conv.editor.focus_editor_next_frame = true;
            return;
        }

        let content = std::fs::read_to_string(&path).unwrap_or_default();
        let disk_modified = std::fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .ok();
        self.conv.editor.documents.push(EditorDocument {
            markdown_preview_open: false,
            path,
            is_scratchpad: true,
            saved_content: content.clone(),
            content,
            disk_modified,
            externally_modified: false,
            syntax_state: None,
            content_revision: 0,
            dirty: false,
            layout_cache: EditorLayoutCache::default(),
            minimap_cache: None,
            viewport_width_bits: None,
            viewport_anchor_line: 0,
            media: None,
        });
        self.conv.editor.active = Some(self.conv.editor.documents.len() - 1);
        self.conv.editor.hidden_active = None;
        self.conv.editor.diff_tab_active = false;
        self.conv.editor.markdown_preview_active = false;
        self.conv.editor.show_diff = false;
        self.conv.editor.error = None;
        self.conv.editor.focus_editor_next_frame = true;
    }

    pub(super) fn autosave_scratchpad(&mut self, index: usize) {
        let Some(document) = self.conv.editor.documents.get_mut(index) else {
            return;
        };
        if !document.is_scratchpad || !document.is_dirty() {
            return;
        }
        let result =
            crate::scratchpad::save(&document.path, &document.saved_content, &document.content);
        match result {
            Ok(()) => {
                document.saved_content.clone_from(&document.content);
                document.dirty = false;
                document.disk_modified = std::fs::metadata(&document.path)
                    .and_then(|metadata| metadata.modified())
                    .ok();
                document.externally_modified = false;
                self.conv.editor.error = None;
            }
            Err(error) => {
                self.conv.editor.error = Some(format!("Could not autosave scratchpad: {error}"));
            }
        }
    }
    pub(super) fn reveal_editor_file_in_explorer(&mut self, path: &Path) {
        let root = PathBuf::from(&self.active_workspace().root_path);
        let safe_root = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
        let safe_path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if !safe_path.starts_with(&safe_root) {
            return;
        }

        let relative = safe_path.strip_prefix(&safe_root).unwrap_or(&safe_path);
        let explorer_path = root.join(relative);
        self.conv.explorer.collapsed_roots.remove(&root);
        let mut parent = explorer_path.parent();
        while let Some(directory) = parent {
            if directory == root {
                break;
            }
            self.conv.explorer.expanded.insert(directory.to_path_buf());
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
            self.conv.sidebar.mode = super::super::state::SidebarMode::Explorer;
            self.conv.sidebar.open = true;
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
            self.conv.editor.markdown_preview_active = false;
            if reveal_in_explorer {
                self.reveal_editor_file_in_explorer(&safe_path);
            }
            return;
        }
        let metadata = match std::fs::metadata(&safe_path) {
            Ok(metadata) => metadata,
            Err(error) => {
                self.conv.editor.error = Some(format!("Could not inspect file: {error}"));
                return;
            }
        };
        // Media and oversized/binary files open as a read-only preview tab.
        let media = MediaKind::from_path(&safe_path)
            .or((metadata.len() > MAX_TEXT_FILE_BYTES).then_some(MediaKind::Binary));
        let content = match media {
            Some(_) => Ok(String::new()),
            None => std::fs::read_to_string(&safe_path),
        };
        let (content, media) = match content {
            Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
                (Ok(String::new()), Some(MediaKind::Binary))
            }
            other => (other, media),
        };
        match content {
            Ok(content) => {
                self.conv.editor.documents.push(EditorDocument {
                    markdown_preview_open: false,
                    path: safe_path.clone(),
                    is_scratchpad: false,
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
                    media,
                });
                self.conv.editor.active = Some(self.conv.editor.documents.len() - 1);
                self.conv.editor.error = None;
                self.conv.editor.show_diff = false;
                // An open git diff stays reachable as an editor tab; just show the file.
                self.conv.editor.diff_tab_active = false;
                self.conv.editor.markdown_preview_active = false;
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
        let Some(index) = self.conv.editor.active else {
            return;
        };
        if self.conv.editor.documents[index].is_scratchpad {
            self.autosave_scratchpad(index);
            return;
        }
        if self.conv.editor.documents[index].media.is_some() {
            return;
        }
        let path = self.conv.editor.documents[index].path.clone();
        if let Err(error) = self.save_editor_document(index, false) {
            if self.conv.editor.documents[index].externally_modified {
                self.conv.editor.prompt = Some(super::super::state::EditorPrompt::Overwrite {
                    path,
                    close_after: false,
                });
            }
            self.conv.editor.error = Some(error);
        }
    }

    pub(super) fn save_editor_document(
        &mut self,
        index: usize,
        overwrite: bool,
    ) -> Result<(), String> {
        self.conv.editor.documents[index].save_to_disk(overwrite)?;
        self.conv.editor.error = None;
        if let Some(tx) = &self.conv.git_ui.tx {
            let _ = tx.send(crate::git::GitOp::Refresh);
        }
        Ok(())
    }

    pub(crate) fn refresh_scratchpad(&mut self) {
        for document in &mut self.conv.editor.documents {
            if document.is_scratchpad {
                document.sync_from_disk();
            }
        }
    }

    pub(crate) fn check_external_file_changes(&mut self) {
        if self
            .conv
            .editor
            .last_external_check
            .is_some_and(|at| at.elapsed() < std::time::Duration::from_millis(500))
        {
            return;
        }
        self.conv.editor.last_external_check = Some(std::time::Instant::now());
        for document in &mut self.conv.editor.documents {
            document.sync_from_disk();
        }
    }

    pub(super) fn reload_active_editor_file(&mut self) {
        let Some(document) = self.conv.editor.active_document_mut() else {
            return;
        };
        match document.reload_from_disk() {
            Ok(()) => self.conv.editor.error = None,
            Err(error) => self.conv.editor.error = Some(format!("Could not reload file: {error}")),
        }
    }
}

impl EditorDocument {
    pub(super) fn sync_from_disk(&mut self) {
        if self.is_scratchpad {
            let _guard = crate::scratchpad::lock();
            match crate::scratchpad::read(&self.path) {
                Ok(content) if content == self.saved_content => {}
                Ok(_) if self.is_dirty() => self.externally_modified = true,
                Ok(_) => self.externally_modified = self.reload_from_disk().is_err(),
                Err(_) => self.externally_modified = true,
            }
            return;
        }
        let metadata = match std::fs::metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(_) => {
                // A scratchpad may not have been saved to disk yet.
                if !self.is_scratchpad || self.disk_modified.is_some() {
                    self.externally_modified = true;
                }
                return;
            }
        };
        let changed = metadata.modified().ok() != self.disk_modified
            || (self.media.is_none() && metadata.len() != self.saved_content.len() as u64);
        if !changed && !self.externally_modified {
            return;
        }
        if self.media.is_some() {
            // The preview renderer also invalidates egui's image cache.
            self.externally_modified = true;
        } else if self.is_dirty() {
            self.externally_modified = true;
        } else {
            self.externally_modified = self.reload_from_disk().is_err();
        }
    }

    pub(super) fn reload_from_disk(&mut self) -> Result<(), String> {
        let metadata = std::fs::metadata(&self.path).map_err(|e| e.to_string())?;
        if metadata.len() > MAX_TEXT_FILE_BYTES {
            return Err("The file is too large for the text editor.".into());
        }
        let content = std::fs::read_to_string(&self.path).map_err(|e| e.to_string())?;
        if self.content != content {
            self.content_revision = self.content_revision.wrapping_add(1);
            self.layout_cache = EditorLayoutCache::default();
            self.minimap_cache = None;
            self.syntax_state = None;
        }
        self.content = content.clone();
        self.saved_content = content;
        self.dirty = false;
        self.disk_modified = metadata.modified().ok();
        self.externally_modified = false;
        Ok(())
    }

    /// Compare actual bytes, not only mtimes: another tool can preserve the timestamp.
    pub(super) fn save_to_disk(&mut self, overwrite: bool) -> Result<(), String> {
        if !overwrite && !self.is_scratchpad {
            match std::fs::read(&self.path) {
                Ok(bytes) if bytes == self.saved_content.as_bytes() => {}
                Ok(_) => {
                    self.externally_modified = true;
                    return Err("The file changed on disk. Review it before overwriting.".into());
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    self.externally_modified = true;
                    return Err(
                        "The file was removed from disk. Overwrite will recreate it.".into(),
                    );
                }
                Err(error) => return Err(format!("Could not check file before saving: {error}")),
            }
        }
        std::fs::write(&self.path, self.content.as_bytes())
            .map_err(|e| format!("Could not save file: {e}"))?;
        self.saved_content.clone_from(&self.content);
        self.dirty = false;
        self.disk_modified = std::fs::metadata(&self.path)
            .and_then(|m| m.modified())
            .ok();
        self.externally_modified = false;
        Ok(())
    }
}
