//! Editor document loading, persistence, and external-change tracking.

use std::path::{Path, PathBuf};

use eframe::egui;

use super::super::{EditorDocument, OxiApp};
use super::{EditorLayoutCache, MediaKind};

pub(super) const MAX_TEXT_FILE_BYTES: u64 = 2 * 1024 * 1024;

impl OxiApp {
    pub(crate) fn open_scratchpad(&mut self) {
        let path = crate::scratchpad::path();
        let _guard = crate::scratchpad::lock(&path);
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
            view: Default::default(),
            viewport_width_bits: None,
            viewport_anchor_line: 0,
            media: None,
            diff: None,
        });
        self.conv.editor.active = Some(self.conv.editor.documents.len() - 1);
        self.conv.editor.hidden_active = None;
        self.conv.editor.diff_tab_active = false;
        self.conv.editor.markdown_preview_active = false;
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
            Ok(saved) => {
                // An agent's concurrent write was merged into what got saved.
                if saved != document.content {
                    document.replace_content(saved.clone());
                }
                document.saved_content = saved;
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
        let safe_root = crate::vfs::canonicalize(&root).unwrap_or_else(|_| root.clone());
        let safe_path = crate::vfs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
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
        let safe_root = crate::vfs::canonicalize(&root).unwrap_or_else(|_| root.clone());
        let safe_path = match crate::vfs::canonicalize(&path) {
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
        if crate::vfs::is_remote(&safe_path) {
            // Two round trips to the host: wait for them off the UI thread.
            self.conv
                .editor
                .remote_io
                .open(safe_path, reveal_in_explorer);
            self.conv.editor.error = None;
            return;
        }
        match load_document(&safe_path) {
            Ok(loaded) => self.push_loaded_document(safe_path, loaded, reveal_in_explorer),
            Err(error) => self.conv.editor.error = Some(error),
        }
    }

    /// Show a file read by [`load_document`] in a new tab and select it.
    pub(super) fn push_loaded_document(
        &mut self,
        safe_path: PathBuf,
        (content, media, modified): LoadedDocument,
        reveal_in_explorer: bool,
    ) {
        self.conv.editor.documents.push(EditorDocument {
            markdown_preview_open: false,
            path: safe_path.clone(),
            is_scratchpad: false,
            saved_content: content.clone(),
            content,
            disk_modified: modified,
            externally_modified: false,
            syntax_state: None,
            content_revision: 0,
            dirty: false,
            layout_cache: EditorLayoutCache::default(),
            minimap_cache: None,
            view: Default::default(),
            viewport_width_bits: None,
            viewport_anchor_line: 0,
            media,
            diff: None,
        });
        self.conv.editor.active = Some(self.conv.editor.documents.len() - 1);
        self.conv.editor.error = None;
        // An open git diff stays reachable as an editor tab; just show the file.
        self.conv.editor.diff_tab_active = false;
        self.conv.editor.markdown_preview_active = false;
        if reveal_in_explorer {
            self.reveal_editor_file_in_explorer(&safe_path);
        }
    }

    pub(crate) fn save_editor_file(&mut self, ctx: &egui::Context) {
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
        self.tidy_before_save(Some(ctx), index);
        let path = self.conv.editor.documents[index].path.clone();
        if crate::vfs::is_remote(&path) {
            let document = &self.conv.editor.documents[index];
            self.conv.editor.remote_io.save(
                path,
                Some(document.saved_content.clone()),
                document.content.clone(),
            );
            return;
        }
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
        // Saves without an editor frame at hand (close prompts, the diff tab): no undo step.
        self.tidy_before_save(None, index);
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

    pub(crate) fn check_external_file_changes(&mut self, ctx: &egui::Context) {
        let waker = ctx.clone();
        crate::open_buffers::set_waker(move || waker.request_repaint());
        self.poll_remote_io(ctx);
        self.apply_agent_writes(ctx);
        self.publish_open_buffers();
        if self
            .conv
            .editor
            .last_external_check
            .is_some_and(|at| at.elapsed() < std::time::Duration::from_millis(500))
        {
            return;
        }
        self.conv.editor.last_external_check = Some(std::time::Instant::now());
        let remote: Vec<std::path::PathBuf> = self
            .conv
            .editor
            .documents
            .iter()
            .filter(|d| crate::vfs::is_remote(&d.path))
            .map(|d| d.path.clone())
            .collect();
        let waker = ctx.clone();
        crate::vfs::refresh_stats(remote, move || waker.request_repaint());
        for index in 0..self.conv.editor.documents.len() {
            let document = &mut self.conv.editor.documents[index];
            // Its new text arrives through `apply_agent_writes`, as an undoable edit.
            if crate::open_buffers::has_agent_write(&document.path) {
                continue;
            }
            // The save in flight changes the disk under it; its result updates the tab.
            if self.conv.editor.remote_io.is_saving(&document.path) {
                continue;
            }
            // A tab without unsaved changes follows the disk; the old text stays one undo away,
            // so an agent's edit to an open file can be taken back from the editor.
            if let Some(old) = document.sync_from_disk_keeping_old() {
                let new = document.content.clone();
                self.record_replacement_undo(ctx, index, old, new);
            }
        }
    }

    /// Tell ACP agents what the editor shows (see [`crate::open_buffers`]).
    fn publish_open_buffers(&self) {
        crate::open_buffers::publish(
            self.conv
                .editor
                .documents
                .iter()
                .filter(|d| d.media.is_none() && !d.is_scratchpad)
                .map(|d| crate::open_buffers::OpenTab {
                    path: &d.path,
                    revision: d.content_revision,
                    unsaved: d.is_dirty().then_some(d.content.as_str()),
                }),
        );
    }

    /// Put what agents wrote to open files into their tabs as one undoable edit each. The
    /// agent wrote the tab's text plus its change, so the tab matches the disk afterwards.
    fn apply_agent_writes(&mut self, ctx: &egui::Context) {
        for (path, content) in crate::open_buffers::take_agent_writes() {
            let Some(index) = self
                .conv
                .editor
                .documents
                .iter()
                .position(|d| d.path == path && d.media.is_none())
            else {
                continue;
            };
            let document = &mut self.conv.editor.documents[index];
            document.saved_content.clone_from(&content);
            document.disk_modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
            document.externally_modified = false;
            if document.content == content {
                document.dirty = false;
                continue;
            }
            let old = std::mem::replace(&mut document.content, content.clone());
            document.syntax_state = None;
            let _ = super::editor_body::mark_document_edited(document);
            self.record_replacement_undo(ctx, index, old, content);
            ctx.request_repaint();
        }
    }

    /// Make a whole-text replacement of tab `index` one undo step in its editor.
    pub(super) fn record_replacement_undo(
        &self,
        ctx: &egui::Context,
        index: usize,
        old: String,
        new: String,
    ) {
        let path = &self.conv.editor.documents[index].path;
        let Some(id) = self.conv.editor.text_edit_ids.get(path).copied() else {
            return;
        };
        let Some(mut state) = egui::text_edit::TextEditState::load(ctx, id) else {
            return;
        };
        let mut undoer = state.undoer();
        let before = state.cursor.char_range().unwrap_or_default();
        let caret = caret_after_replacement(&old, &new, before.primary.index.0);
        undoer.add_undo(&(before, old));
        let after = egui::text::CCursorRange::one(egui::text::CCursor::new(caret));
        undoer.add_undo(&(after, new));
        state.set_undoer(undoer);
        state.cursor.set_char_range(Some(after));
        state.store(ctx, id);
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

/// Where a caret at char `caret` of `old` belongs once the text became `new`: it stays before
/// the changed region, keeps its distance from the end after it, and lands at the end of it
/// when it was inside.
pub(super) fn caret_after_replacement(old: &str, new: &str, caret: usize) -> usize {
    let prefix = crate::text_diff::common_prefix_len(old.as_bytes(), new.as_bytes());
    let prefix = floor_char_boundary(old, prefix.min(new.len()));
    let suffix = crate::text_diff::common_suffix_len(
        old.as_bytes(),
        new.as_bytes(),
        old.len().min(new.len()) - prefix,
    );
    let caret_byte = old
        .char_indices()
        .nth(caret)
        .map_or(old.len(), |(byte, _)| byte);
    if caret_byte <= prefix {
        return caret.min(new.chars().count());
    }
    let new_chars = new.chars().count();
    if caret_byte >= old.len() - suffix {
        return new_chars.saturating_sub(old[caret_byte..].chars().count());
    }
    let changed_end = floor_char_boundary(new, new.len() - suffix);
    new[..changed_end].chars().count()
}

fn floor_char_boundary(text: &str, mut byte: usize) -> usize {
    while !text.is_char_boundary(byte) {
        byte -= 1;
    }
    byte
}

impl EditorDocument {
    pub(super) fn sync_from_disk(&mut self) {
        let _ = self.sync_from_disk_keeping_old();
    }

    /// Like [`Self::sync_from_disk`]; returns the text a reload replaced, if any.
    pub(super) fn sync_from_disk_keeping_old(&mut self) -> Option<String> {
        if self.is_scratchpad {
            let disk = match crate::scratchpad::read_locked(&self.path) {
                Ok(disk) => disk,
                Err(_) => {
                    self.externally_modified = true;
                    return None;
                }
            };
            if disk == self.saved_content {
                return None;
            }
            self.disk_modified = std::fs::metadata(&self.path)
                .and_then(|m| m.modified())
                .ok();
            return self.take_in_disk_text(disk, true);
        }
        // A remote file's metadata comes from the background refresh, never from a round
        // trip on this (the UI) thread.
        let metadata = if crate::vfs::is_remote(&self.path) {
            match crate::vfs::cached_metadata(&self.path) {
                Some(Some(metadata)) => Ok(metadata),
                Some(None) => Err(()),
                None => return None,
            }
        } else {
            crate::vfs::metadata(&self.path).map_err(|_| ())
        };
        let metadata = match metadata {
            Ok(metadata) => metadata,
            Err(()) => {
                self.externally_modified = true;
                return None;
            }
        };
        let changed = metadata.modified != self.disk_modified
            || (self.media.is_none() && metadata.len != self.saved_content.len() as u64);
        if !changed && !self.externally_modified {
            return None;
        }
        if self.media.is_some() {
            // The preview renderer also invalidates egui's image cache.
            self.externally_modified = true;
            return None;
        }
        match self.read_disk_text() {
            Ok((disk, modified)) => {
                self.disk_modified = modified;
                self.take_in_disk_text(disk, false)
            }
            Err(_) => {
                self.externally_modified = true;
                None
            }
        }
    }

    /// Bring `disk`, written by another program, into the tab: a clean tab adopts it, one with
    /// unsaved edits gets it merged in. Only edits to the same lines (`union` aside) leave the
    /// tab flagged as changed on disk. Returns the text it replaced, if it changed.
    fn take_in_disk_text(&mut self, disk: String, union: bool) -> Option<String> {
        if disk == self.saved_content {
            self.externally_modified = false;
            return None;
        }
        let next = if self.is_dirty() {
            match crate::merge3::merge(&self.saved_content, &self.content, &disk, union) {
                Some(merged) => merged,
                None => {
                    self.externally_modified = true;
                    return None;
                }
            }
        } else {
            disk.clone()
        };
        self.saved_content = disk;
        self.externally_modified = false;
        let old = (next != self.content).then(|| self.replace_content(next));
        self.dirty = self.content != self.saved_content;
        old
    }

    /// Swap in new text, dropping what was derived from the old; returns the old text.
    pub(super) fn replace_content(&mut self, content: String) -> String {
        if self.content != content {
            self.content_revision = self.content_revision.wrapping_add(1);
            self.layout_cache = EditorLayoutCache::default();
            self.minimap_cache = None;
            self.syntax_state = None;
        }
        std::mem::replace(&mut self.content, content)
    }

    fn read_disk_text(&self) -> Result<(String, Option<std::time::SystemTime>), String> {
        let metadata = crate::vfs::metadata(&self.path).map_err(|e| e.to_string())?;
        if metadata.len > MAX_TEXT_FILE_BYTES {
            return Err("The file is too large for the text editor.".into());
        }
        let content = crate::vfs::read_to_string(&self.path).map_err(|e| e.to_string())?;
        // Reading a remote file refreshes its metadata too.
        let modified = crate::vfs::modified(&self.path).or(metadata.modified);
        Ok((content, modified))
    }

    pub(super) fn reload_from_disk(&mut self) -> Result<(), String> {
        let (content, modified) = self.read_disk_text()?;
        self.replace_content(content.clone());
        self.saved_content = content;
        self.dirty = false;
        self.disk_modified = modified;
        self.externally_modified = false;
        Ok(())
    }

    /// Compare actual bytes, not only mtimes: another tool can preserve the timestamp.
    pub(super) fn save_to_disk(&mut self, overwrite: bool) -> Result<(), String> {
        if !overwrite && !self.is_scratchpad {
            match crate::vfs::read(&self.path) {
                Ok(bytes) if bytes == self.saved_content.as_bytes() => {}
                disk => self.take_in_disk_before_save(disk)?,
            }
        }
        crate::vfs::write(&self.path, self.content.as_bytes())
            .map_err(|e| format!("Could not save file: {e}"))?;
        let written = self.content.clone();
        self.mark_saved(written, crate::vfs::modified(&self.path));
        Ok(())
    }

    /// The disk no longer holds what the tab last saved: merge it in, as a sync would have,
    /// so the save writes both. Fails, flagging the tab, when the edits collide or the file
    /// is gone.
    pub(super) fn take_in_disk_before_save(
        &mut self,
        disk: std::io::Result<Vec<u8>>,
    ) -> Result<(), String> {
        match disk {
            Ok(bytes) => {
                let merged = String::from_utf8(bytes).ok().and_then(|disk| {
                    let merged =
                        crate::merge3::merge(&self.saved_content, &self.content, &disk, false)?;
                    Some((disk, merged))
                });
                let Some((disk, merged)) = merged else {
                    self.externally_modified = true;
                    return Err(
                        "The file changed on disk where you also edited it. Review it before overwriting.".into(),
                    );
                };
                self.saved_content = disk;
                self.replace_content(merged);
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.externally_modified = true;
                Err("The file was removed from disk. Overwrite will recreate it.".into())
            }
            Err(error) => Err(format!("Could not check file before saving: {error}")),
        }
    }

    /// `written` is on disk now. The tab may have been edited since it was sent off.
    pub(super) fn mark_saved(&mut self, written: String, modified: Option<std::time::SystemTime>) {
        self.saved_content = written;
        self.dirty = self.content != self.saved_content;
        self.disk_modified = modified;
        self.externally_modified = false;
    }
}

/// A file's text (empty for a preview tab), its preview kind, and its modification time.
pub(super) type LoadedDocument = (String, Option<MediaKind>, Option<std::time::SystemTime>);

/// Read a file for a new tab. Media and oversized/binary files open as a read-only preview.
pub(super) fn load_document(path: &Path) -> Result<LoadedDocument, String> {
    let metadata =
        crate::vfs::metadata(path).map_err(|error| format!("Could not inspect file: {error}"))?;
    let media = MediaKind::from_path(path)
        .or((metadata.len > MAX_TEXT_FILE_BYTES).then_some(MediaKind::Binary));
    let content = match media {
        Some(_) => Ok(String::new()),
        None => crate::vfs::read_to_string(path),
    };
    let (content, media) = match content {
        Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
            (Ok(String::new()), Some(MediaKind::Binary))
        }
        other => (other, media),
    };
    let content = content.map_err(|error| format!("Could not open text file: {error}"))?;
    // Reading a remote file refreshes its metadata too.
    let modified = crate::vfs::modified(path).or(metadata.modified);
    Ok((content, media, modified))
}
