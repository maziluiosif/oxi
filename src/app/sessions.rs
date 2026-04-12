use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::process::Command;

use eframe::egui;

use super::state::Workspace;
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

        let active_workspace = self.conv.active_workspace;
        let old_states = std::mem::take(&mut self.flow.sessions);
        self.active_workspace_mut().sessions.insert(0, Self::blank_session(title));
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

        self.conv.scroll_to_bottom_once = true;
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
    }

    pub(crate) fn pick_image_attachment(&mut self) {
        if self.conv.pending_images.len() >= MAX_PENDING_IMAGES {
            self.run_state_mut(self.active_session_key()).stream_error =
                Some(format!("At most {MAX_PENDING_IMAGES} images per message"));
            return;
        }
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Images", &["png", "jpg", "jpeg", "gif", "webp"])
            .pick_file()
        else {
            return;
        };
        let Ok(bytes) = std::fs::read(&path) else {
            self.run_state_mut(self.active_session_key()).stream_error =
                Some("Failed to read image file".to_string());
            return;
        };
        if bytes.len() > MAX_IMAGE_ATTACHMENT_BYTES {
            self.run_state_mut(self.active_session_key()).stream_error = Some(format!(
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
                self.run_state_mut(self.active_session_key()).stream_error =
                    Some(format!("At most {MAX_PENDING_IMAGES} images per message"));
                break;
            }
            let bytes: Vec<u8> = match (&file.bytes, &file.path) {
                (Some(b), _) => b.to_vec(),
                (_, Some(path)) => match std::fs::read(path) {
                    Ok(b) => b,
                    Err(_) => continue,
                },
                _ => continue,
            };
            if bytes.is_empty() {
                continue;
            }
            if bytes.len() > MAX_IMAGE_ATTACHMENT_BYTES {
                self.run_state_mut(self.active_session_key()).stream_error = Some(format!(
                    "Image too large (max {} MB)",
                    MAX_IMAGE_ATTACHMENT_BYTES / (1024 * 1024)
                ));
                continue;
            }
            if image::load_from_memory(&bytes).is_err() {
                continue;
            }
            let mime = file
                .path
                .as_ref()
                .and_then(|p| mime_for_image_path(p))
                .or_else(|| image::guess_format(&bytes).ok().map(mime_from_image_format))
                .unwrap_or("image/png");
            self.conv.pending_images.push((mime.to_string(), bytes));
            self.run_state_mut(self.active_session_key()).stream_error = None;
        }
    }

    pub(crate) fn delete_session(&mut self, idx: usize) {
        if self.active_workspace().sessions.len() <= 1 {
            return;
        }
        let active_key = self.active_session_key();
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
        self.run_state_mut(self.active_session_key()).stream_error = None;
    }
}
