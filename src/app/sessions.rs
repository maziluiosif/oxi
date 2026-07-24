use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::process::Command;

use eframe::egui;

use super::state::Workspace;
use crate::session_store;
use crate::theme::{MAX_IMAGE_ATTACHMENT_BYTES, MAX_PENDING_IMAGES};

use super::OxiApp;

fn mime_for_image_path(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => return None,
    })
}

fn mime_from_image_format(f: image::ImageFormat) -> &'static str {
    match f {
        image::ImageFormat::Png => "image/png",
        image::ImageFormat::Jpeg => "image/jpeg",
        image::ImageFormat::Gif => "image/gif",
        image::ImageFormat::WebP => "image/webp",
        _ => "image/png",
    }
}

fn delete_session_file_from_disk(path: &Path) -> Result<(), String> {
    let mut trash_error: Option<String> = None;
    let file_name_starts_with_dash = path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with('-'));

    let mut trash = Command::new("trash");
    if file_name_starts_with_dash {
        trash.arg("--");
    }
    trash.arg(path);

    match trash.output() {
        Ok(output) => {
            if output.status.success() || !path.exists() {
                return Ok(());
            }
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            if !stderr.is_empty() {
                trash_error = Some(stderr);
            }
        }
        Err(err) => {
            trash_error = Some(err.to_string());
        }
    }

    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => {
            if let Some(trash_error) = trash_error {
                Err(format!("{err} (trash: {trash_error})"))
            } else {
                Err(err.to_string())
            }
        }
    }
}

impl OxiApp {
    pub(crate) fn new_chat(&mut self) {
        let title = {
            let w = self.active_workspace();
            format!("Chat {}", w.sessions.len() + 1)
        };

        // The composer is app-level UI state while drafts belong to sessions. Stash the
        // current draft before changing the active index; otherwise the new blank chat shows
        // the previous chat's text/images until the user switches away and back.
        self.cancel_edit_last_prompt();
        let active_workspace = self.conv.active_workspace;
        let old_session = self.active_workspace().active;
        self.conv.workspaces[active_workspace].sessions[old_session].input_text =
            std::mem::take(&mut self.conv.input);
        self.conv.workspaces[active_workspace].sessions[old_session].pending_images =
            std::mem::take(&mut self.conv.pending_images);
        self.conv.input_history_index = None;
        self.conv.input_history_draft.clear();
        self.conv.composer_notice = None;

        let old_states = std::mem::take(&mut self.flow.sessions);
        self.active_workspace_mut()
            .sessions
            .insert(0, Self::blank_session(title));
        self.active_workspace_mut().active = 0;

        self.flow.sessions = old_states
            .into_iter()
            .map(|(mut key, state)| {
                if key.workspace_idx == active_workspace {
                    key.session_idx += 1;
                }
                (key, state)
            })
            .collect();

        if let Some(pending_idx) = self.flow.pending_session_idx {
            let new_idx = pending_idx + 1;
            self.flow.pending_session_idx = Some(new_idx);
            self.flow.pending_load_session_idx = Some(new_idx);
        }

        // Keep an in-flight compaction pointed at the same session after the re-key.
        if let Some(c) = self.conv.compaction.as_mut()
            && c.key.workspace_idx == active_workspace
        {
            c.key.session_idx += 1;
        }

        // Surface the new chat no matter what was showing: hide any open editor/history diff so
        // the transcript is in front, and if the sidebar is browsing files, flip it back to the
        // chat list so the new chat is visible there too.
        self.reveal_chat_view();
        if self.conv.sidebar_open
            && self.conv.sidebar_mode == super::state::SidebarMode::Explorer
        {
            self.conv.sidebar_mode = super::state::SidebarMode::Chats;
        }

        self.conv.scroll_to_bottom_once = true;
        self.conv.focus_chat_input_next_frame = true;
        self.persist_active_session_selection();
        if let Some(state) = self.flow.sessions.get_mut(&self.active_session_key()) {
            state.stream_error = None;
        }
    }

    pub(crate) fn open_workspace_folder(&mut self) {
        let Some(folder) = rfd::FileDialog::new().pick_folder() else {
            return;
        };
        let path = std::fs::canonicalize(&folder)
            .unwrap_or(folder)
            .to_string_lossy()
            .to_string();
        if let Some(i) = self
            .conv
            .workspaces
            .iter()
            .position(|w| w.root_path == path)
        {
            self.select_workspace(i);
            return;
        }
        let sessions = Self::initial_workspace_sessions(&path, self.conn.no_session);
        self.conv.workspaces.push(Workspace {
            root_path: path,
            sessions,
            active: 0,
            sidebar_folded: false,
        });
        self.select_workspace(self.conv.workspaces.len() - 1);
        self.sync_workspaces_to_settings();
    }

    /// Mirrors the runtime workspace list (paths + fold state) into settings and saves,
    /// so sidebar projects survive a restart.
    pub(crate) fn sync_workspaces_to_settings(&mut self) {
        self.conv.settings.workspaces = self
            .conv
            .workspaces
            .iter()
            .map(|w| crate::settings::WorkspaceEntry {
                root_path: w.root_path.clone(),
                folded: w.sidebar_folded,
            })
            .collect();
        self.sync_active_session_to_settings();
        if let Err(e) = self.conv.settings.save() {
            self.run_state_mut(self.active_session_key()).stream_error =
                Some(format!("Save settings: {e}"));
        }
    }

    pub(crate) fn pick_image_attachment(&mut self) {
        if self.conv.pending_images.len() >= MAX_PENDING_IMAGES {
            self.notify_composer(format!("At most {MAX_PENDING_IMAGES} images per message"));
            return;
        }
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Images", &["png", "jpg", "jpeg", "gif", "webp"])
            .pick_file()
        else {
            return;
        };
        let Ok(bytes) = std::fs::read(&path) else {
            self.notify_composer("Failed to read image file");
            return;
        };
        if bytes.len() > MAX_IMAGE_ATTACHMENT_BYTES {
            self.notify_composer(format!(
                "Image too large (max {} MB)",
                MAX_IMAGE_ATTACHMENT_BYTES / (1024 * 1024)
            ));
            return;
        }
        let mime = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| match e.to_lowercase().as_str() {
                "png" => "image/png",
                "jpg" | "jpeg" => "image/jpeg",
                "gif" => "image/gif",
                "webp" => "image/webp",
                _ => "image/png",
            })
            .unwrap_or("image/png");
        self.conv.pending_images.push((mime.to_string(), bytes));
    }

    /// Attach an image currently stored in the OS clipboard. egui's paste event carries text
    /// only, so bitmap clipboard formats (notably Windows screenshots / Snipping Tool) must be
    /// read explicitly and encoded into the same PNG byte format used by normal attachments.
    pub(crate) fn paste_clipboard_image(&mut self) -> bool {
        if self.conv.pending_images.len() >= MAX_PENDING_IMAGES {
            self.notify_composer(format!("At most {MAX_PENDING_IMAGES} images per message"));
            return true;
        }

        // Keep the Clipboard value scoped to this operation. On Windows the clipboard is a
        // process-global lock and retaining it can interfere with egui's normal text paste.
        let Some(image) = (|| {
            let mut clipboard = arboard::Clipboard::new().ok()?;
            clipboard.get_image().ok()
        })() else {
            return false; // A normal text paste; let TextEdit handle it.
        };
        if image.width == 0 || image.height == 0 {
            return false;
        }
        let Some(expected_len) = image
            .width
            .checked_mul(image.height)
            .and_then(|pixels| pixels.checked_mul(4))
        else {
            self.notify_composer("Clipboard image dimensions are too large");
            return true;
        };
        if image.bytes.len() != expected_len {
            self.notify_composer("Clipboard returned invalid image data");
            return true;
        }
        let (Ok(width), Ok(height)) = (u32::try_from(image.width), u32::try_from(image.height))
        else {
            self.notify_composer("Clipboard image dimensions are too large");
            return true;
        };
        let Some(rgba) = image::RgbaImage::from_raw(width, height, image.bytes.into_owned()) else {
            self.notify_composer("Could not decode clipboard image");
            return true;
        };
        let mut png = std::io::Cursor::new(Vec::new());
        if image::DynamicImage::ImageRgba8(rgba)
            .write_to(&mut png, image::ImageFormat::Png)
            .is_err()
        {
            self.notify_composer("Could not encode clipboard image");
            return true;
        }
        let bytes = png.into_inner();
        if bytes.len() > MAX_IMAGE_ATTACHMENT_BYTES {
            self.notify_composer(format!(
                "Clipboard image too large (max {} MB)",
                MAX_IMAGE_ATTACHMENT_BYTES / (1024 * 1024)
            ));
            return true;
        }
        self.conv
            .pending_images
            .push(("image/png".to_string(), bytes));
        self.conv.composer_notice = None;
        true
    }

    pub(crate) fn remove_pending_image_at(&mut self, index: usize) {
        if index < self.conv.pending_images.len() {
            self.conv.pending_images.remove(index);
        }
    }

    pub(crate) fn consume_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped: Vec<egui::DroppedFile> = ctx.input(|i| i.raw.dropped_files.clone());
        if dropped.is_empty() {
            return;
        }
        for file in dropped {
            if self.conv.pending_images.len() >= MAX_PENDING_IMAGES {
                self.notify_composer(format!("At most {MAX_PENDING_IMAGES} images per message"));
                break;
            }
            let bytes: Vec<u8> = match (&file.bytes, &file.path) {
                (Some(b), _) => b.to_vec(),
                (_, Some(path)) => match std::fs::read(path) {
                    Ok(b) => b,
                    Err(e) => {
                        self.notify_composer(format!("Could not read dropped file: {e}"));
                        continue;
                    }
                },
                _ => continue,
            };
            if bytes.is_empty() {
                continue;
            }
            if bytes.len() > MAX_IMAGE_ATTACHMENT_BYTES {
                self.notify_composer(format!(
                    "Image too large (max {} MB)",
                    MAX_IMAGE_ATTACHMENT_BYTES / (1024 * 1024)
                ));
                continue;
            }
            if image::load_from_memory(&bytes).is_err() {
                self.notify_composer("Only PNG, JPEG, GIF, or WebP images can be attached.");
                continue;
            }
            let mime = file
                .path
                .as_ref()
                .and_then(|p| mime_for_image_path(p))
                .or_else(|| image::guess_format(&bytes).ok().map(mime_from_image_format))
                .unwrap_or("image/png");
            self.conv.pending_images.push((mime.to_string(), bytes));
            self.conv.composer_notice = None;
        }
    }

    pub(crate) fn rename_session(&mut self, idx: usize, new_title: String) {
        let title = new_title.trim().to_string();
        if title.is_empty() || idx >= self.active_workspace().sessions.len() {
            return;
        }
        let key = self.session_key(self.conv.active_workspace, idx);
        if self
            .run_state(key)
            .is_some_and(|state| state.waiting_response)
        {
            return;
        }
        self.active_workspace_mut().sessions[idx].title = title;
        let root_path = self.active_workspace().root_path.clone();
        if let Err(e) = session_store::save_session_messages(
            &root_path,
            &mut self.active_workspace_mut().sessions[idx],
        ) {
            self.run_state_mut(self.active_session_key()).stream_error =
                Some(format!("Rename chat: {e}"));
        }
    }

    /// Export the active session as Markdown to a user-chosen path.
    pub(crate) fn export_active_session_markdown(&mut self) {
        let session = self.active_session();
        let title = session.title.clone();
        let mut md = format!("# {}\n\n", title);
        for msg in &session.messages {
            match msg.role {
                crate::model::MsgRole::User => {
                    md.push_str("## User\n\n");
                    if !msg.text.is_empty() {
                        md.push_str(&msg.text);
                        md.push_str("\n\n");
                    }
                }
                crate::model::MsgRole::Assistant => {
                    md.push_str("## Assistant\n\n");
                    for block in &msg.blocks {
                        match block {
                            crate::model::AssistantBlock::Thinking(t) if !t.trim().is_empty() => {
                                md.push_str("<details><summary>Thinking</summary>\n\n");
                                md.push_str(t);
                                md.push_str("\n\n</details>\n\n");
                            }
                            crate::model::AssistantBlock::Answer(t) if !t.trim().is_empty() => {
                                md.push_str(t);
                                md.push_str("\n\n");
                            }
                            crate::model::AssistantBlock::Tool { name, output, .. } => {
                                md.push_str(&format!("### Tool: {name}\n\n```\n{output}\n```\n\n"));
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        let suggested = format!("{}.md", title.replace(['/', '\\', ':'], "-"));
        if let Some(path) = rfd::FileDialog::new()
            .set_file_name(&suggested)
            .add_filter("Markdown", &["md"])
            .save_file()
        {
            match std::fs::write(&path, md) {
                Ok(()) => {}
                Err(e) => {
                    self.run_state_mut(self.active_session_key()).stream_error =
                        Some(format!("Export failed: {e}"));
                }
            }
        }
    }

    pub(crate) fn delete_session(&mut self, idx: usize) {
        let active_key = self.active_session_key();
        if idx >= self.active_workspace().sessions.len() {
            return;
        }
        let deleting_last_session = self.active_workspace().sessions.len() == 1;
        let delete_key = self.session_key(active_key.workspace_idx, idx);
        if self
            .run_state(delete_key)
            .is_some_and(|state| state.waiting_response)
        {
            return;
        }

        let session_file = self.active_workspace().sessions[idx].session_file.clone();
        let deleting_current_backend_session =
            session_file.as_deref() == self.flow.current_backend_session_file.as_deref();

        // Tear down any Claude Code (ACP) subprocess bound to this session so it doesn't leak.
        // Keyed the same way as `send_prompt_payload` in streaming.rs.
        let acp_key = session_file.clone().unwrap_or_else(|| {
            format!(
                "mem:{}:{}",
                delete_key.workspace_idx, delete_key.session_idx
            )
        });
        self.acp.close(&acp_key);

        if let Some(session_file) = session_file.as_deref() {
            if deleting_current_backend_session {
                self.flow.current_backend_session_file = None;
            }

            if let Err(err) = delete_session_file_from_disk(Path::new(session_file)) {
                self.run_state_mut(active_key).stream_error =
                    Some(format!("Failed to delete chat file: {err}"));
                return;
            }
        }

        let old_states = std::mem::take(&mut self.flow.sessions);
        let workspace_idx = active_key.workspace_idx;
        self.active_workspace_mut().sessions.remove(idx);

        if deleting_last_session {
            self.active_workspace_mut()
                .sessions
                .push(Self::blank_session("Chat 1"));
            self.active_workspace_mut().active = 0;
            self.flow.sessions = old_states
                .into_iter()
                .filter_map(|(key, state)| {
                    if key.workspace_idx == workspace_idx {
                        None
                    } else {
                        Some((key, state))
                    }
                })
                .collect();
            self.flow.pending_session_idx = None;
            self.flow.pending_load_session_idx = None;
            if let Some(c) = self.conv.compaction.as_ref()
                && c.key.workspace_idx == workspace_idx
            {
                self.conv.compaction = None;
            }
            self.conv.input.clear();
            self.conv.pending_images.clear();
            self.conv.input_history_index = None;
            self.conv.input_history_draft.clear();
            self.conv.scroll_to_bottom_once = true;
        } else {
            if idx < self.active_workspace().active {
                self.active_workspace_mut().active -= 1;
            }

            self.flow.sessions = old_states
                .into_iter()
                .filter_map(|(mut key, state)| {
                    if key.workspace_idx != workspace_idx {
                        return Some((key, state));
                    }
                    if key.session_idx == idx {
                        return None;
                    }
                    if key.session_idx > idx {
                        key.session_idx -= 1;
                    }
                    Some((key, state))
                })
                .collect();

            if let Some(pending_idx) = self.flow.pending_session_idx {
                if idx == pending_idx {
                    self.flow.pending_session_idx = None;
                    self.flow.pending_load_session_idx = None;
                } else if idx < pending_idx {
                    let new_idx = pending_idx - 1;
                    self.flow.pending_session_idx = Some(new_idx);
                    self.flow.pending_load_session_idx = Some(new_idx);
                }
            }
            if self.active_workspace().active >= self.active_workspace().sessions.len() {
                let new_active = self.active_workspace().sessions.len().saturating_sub(1);
                self.active_workspace_mut().active = new_active;
            }
        }
        self.persist_active_session_selection();
        self.run_state_mut(self.active_session_key()).stream_error = None;
    }

    /// Removes a workspace from the sidebar. Doesn't touch its files on disk (chats
    /// live under the project's own root, so they'll reappear if it's re-added).
    /// Index 0 is the cwd workspace and is always kept.
    pub(crate) fn delete_workspace(&mut self, wi: usize) {
        if wi == 0 || wi >= self.conv.workspaces.len() {
            return;
        }
        let running = (0..self.conv.workspaces[wi].sessions.len()).any(|si| {
            self.run_state(self.session_key(wi, si))
                .is_some_and(|state| state.waiting_response)
        });
        if running {
            return;
        }

        let deleting_active = wi == self.conv.active_workspace;
        if deleting_active {
            let active = self.active_workspace().active;
            let session_file = self.active_workspace().sessions[active]
                .session_file
                .clone();
            if session_file.as_deref() == self.flow.current_backend_session_file.as_deref() {
                self.flow.current_backend_session_file = None;
            }
        }

        self.conv.workspaces.remove(wi);

        self.flow.sessions = std::mem::take(&mut self.flow.sessions)
            .into_iter()
            .filter_map(|(mut key, state)| {
                if key.workspace_idx == wi {
                    return None;
                }
                if key.workspace_idx > wi {
                    key.workspace_idx -= 1;
                }
                Some((key, state))
            })
            .collect();

        if self.conv.active_workspace > wi {
            self.conv.active_workspace -= 1;
        } else if deleting_active {
            self.conv.active_workspace = 0;
            self.flow.pending_session_idx = None;
            self.flow.pending_load_session_idx = None;
            let new_si = self.active_workspace().active;
            self.conv.input =
                std::mem::take(&mut self.active_workspace_mut().sessions[new_si].input_text);
            self.conv.pending_images =
                std::mem::take(&mut self.active_workspace_mut().sessions[new_si].pending_images);
            self.conv.input_history_index = None;
            self.conv.input_history_draft.clear();
            self.conv.scroll_to_bottom_once = true;
            self.ensure_active_session_loaded();
            self.refresh_git_cwd();
        }

        self.sync_workspaces_to_settings();
    }
}
