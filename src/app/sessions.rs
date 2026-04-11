use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::process::Command;

use eframe::egui;

use super::state::Workspace;
use crate::theme::{MAX_IMAGE_ATTACHMENT_BYTES, MAX_PENDING_IMAGES};

use super::PiChatApp;

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

impl PiChatApp {
    /// Paste image from the OS clipboard when present. Returns `true` if an image was added.
    /// Does not set [`RunState::stream_error`] (for silent use with Ctrl/Cmd+V before text paste).
    pub(crate) fn try_paste_image_from_clipboard(&mut self) -> bool {
        if self.conv.pending_images.len() >= MAX_PENDING_IMAGES {
            return false;
        }
        use arboard::Clipboard;
        let Ok(mut clip) = Clipboard::new() else {
            return false;
        };
        let Ok(img) = clip.get_image() else {
            return false;
        };
        let width = img.width as u32;
        let height = img.height as u32;
        let rgba: Vec<u8> = img.bytes.to_vec();
        if width == 0 || height == 0 || rgba.len() < (width * height * 4) as usize {
            return false;
        }
        let Some(img_buf) = image::RgbaImage::from_raw(width, height, rgba) else {
            return false;
        };
        let dyn_img = image::DynamicImage::from(img_buf);
        let mut png_bytes = Vec::new();
        let mut cursor = std::io::Cursor::new(&mut png_bytes);
        if dyn_img
            .write_to(&mut cursor, image::ImageFormat::Png)
            .is_err()
        {
            return false;
        }
        if png_bytes.len() > MAX_IMAGE_ATTACHMENT_BYTES {
            return false;
        }
        self.conv
            .pending_images
            .push(("image/png".to_string(), png_bytes));
        self.flow.stream_error = None;
        true
    }

    /// New tab in [`ConversationState::active_workspace`] (the workspace whose chats are in use).
    pub(crate) fn new_chat(&mut self) {
        let title = {
            let w = self.active_workspace();
            format!("Chat {}", w.sessions.len() + 1)
        };

        let w = self.active_workspace_mut();
        w.sessions.insert(0, Self::blank_session(title));
        w.active = 0;

        if let Some(stream_idx) = self.flow.stream_session_idx {
            self.flow.stream_session_idx = Some(stream_idx + 1);
        }
        if let Some(pending_idx) = self.flow.pending_session_idx {
            let new_idx = pending_idx + 1;
            self.flow.pending_session_idx = Some(new_idx);
            self.flow.pending_load_session_idx = Some(new_idx);
        }

        self.conv.scroll_to_bottom_once = true;
        self.flow.stream_error = None;
    }

    /// Pick a folder as a new workspace or focus an existing one.
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
            self.flow.stream_error =
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
            self.flow.stream_error = Some("Failed to read image file".to_string());
            return;
        };
        if bytes.len() > MAX_IMAGE_ATTACHMENT_BYTES {
            self.flow.stream_error = Some(format!(
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

    /// Remove a staged image by index (composer "×" buttons).
    pub(crate) fn remove_pending_image_at(&mut self, index: usize) {
        if index < self.conv.pending_images.len() {
            self.conv.pending_images.remove(index);
        }
    }

    /// Handle OS file drops (requires `ViewportBuilder::with_drag_and_drop(true)`).
    pub(crate) fn consume_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped: Vec<egui::DroppedFile> = ctx.input(|i| i.raw.dropped_files.clone());
        if dropped.is_empty() {
            return;
        }
        for file in dropped {
            if self.conv.pending_images.len() >= MAX_PENDING_IMAGES {
                self.flow.stream_error =
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
                self.flow.stream_error = Some(format!(
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
            self.flow.stream_error = None;
        }
    }

    pub(crate) fn delete_session(&mut self, idx: usize) {
        if self.active_workspace().sessions.len() <= 1 {
            return;
        }
        if self.flow.stream_session_idx == Some(idx) {
            return;
        }

        let session_file = self.active_workspace().sessions[idx].session_file.clone();
        let deleting_current_backend_session =
            session_file.as_deref() == self.flow.current_backend_session_file.as_deref();

        if let Some(session_file) = session_file.as_deref() {
            if deleting_current_backend_session && self.conn.agent_rx.is_some() {
                self.stop_agent_run();
            } else if deleting_current_backend_session {
                self.flow.current_backend_session_file = None;
            }

            if let Err(err) = delete_session_file_from_disk(Path::new(session_file)) {
                self.flow.stream_error = Some(format!("Failed to delete chat file: {err}"));
                return;
            }
        }

        let w = self.active_workspace_mut();
        w.sessions.remove(idx);
        if idx < w.active {
            w.active -= 1;
        }
        if let Some(stream_idx) = self.flow.stream_session_idx {
            if idx < stream_idx {
                self.flow.stream_session_idx = Some(stream_idx - 1);
            }
        }
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
        let w = self.active_workspace_mut();
        if w.active >= w.sessions.len() {
            w.active = w.sessions.len().saturating_sub(1);
        }
        self.flow.stream_error = None;
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use super::PiChatApp;

    fn unique_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_else(|_| Duration::from_secs(0))
            .as_nanos();
        std::env::temp_dir().join(format!("oxi-{name}-{nanos}.jsonl"))
    }

    #[test]
    fn delete_session_skips_when_streaming_that_session() {
        let mut app = PiChatApp::new();
        app.active_workspace_mut().sessions = vec![PiChatApp::blank_session("New chat")];
        app.active_workspace_mut()
            .sessions
            .push(crate::model::Session {
                title: "s2".into(),
                messages: vec![],
                session_file: None,
                messages_loaded: true,
            });
        app.flow.stream_session_idx = Some(1);
        app.delete_session(1);
        assert_eq!(app.active_workspace().sessions.len(), 2);
    }

    #[test]
    fn delete_session_adjusts_pending_messages_phase_index() {
        let mut app = PiChatApp::new();
        app.active_workspace_mut().sessions = vec![PiChatApp::blank_session("New chat")];
        app.active_workspace_mut()
            .sessions
            .push(crate::model::Session {
                title: "s2".into(),
                messages: vec![],
                session_file: None,
                messages_loaded: true,
            });
        app.flow.pending_session_idx = Some(1);
        app.flow.pending_load_session_idx = Some(1);
        app.delete_session(0);
        assert_eq!(app.flow.pending_session_idx, Some(0));
        assert_eq!(app.flow.pending_load_session_idx, Some(0));
    }

    #[test]
    fn new_chat_inserts_session_at_top_and_selects_it() {
        let mut app = PiChatApp::new();
        app.active_workspace_mut().sessions = vec![
            crate::model::Session {
                title: "First existing".into(),
                messages: vec![],
                session_file: Some("one.jsonl".into()),
                messages_loaded: false,
            },
            crate::model::Session {
                title: "Second existing".into(),
                messages: vec![],
                session_file: Some("two.jsonl".into()),
                messages_loaded: false,
            },
        ];
        app.active_workspace_mut().active = 1;

        app.new_chat();

        let workspace = app.active_workspace();
        assert_eq!(workspace.active, 0);
        assert_eq!(workspace.sessions.len(), 3);
        assert_eq!(workspace.sessions[0].title, "Chat 3");
        assert!(workspace.sessions[0].session_file.is_none());
        assert_eq!(workspace.sessions[1].title, "First existing");
        assert_eq!(workspace.sessions[2].title, "Second existing");
    }

    #[test]
    fn new_chat_shifts_in_flight_indexes_for_insert_at_top() {
        let mut app = PiChatApp::new();
        app.active_workspace_mut().sessions = vec![
            crate::model::Session {
                title: "s1".into(),
                messages: vec![],
                session_file: Some("one.jsonl".into()),
                messages_loaded: true,
            },
            crate::model::Session {
                title: "s2".into(),
                messages: vec![],
                session_file: Some("two.jsonl".into()),
                messages_loaded: true,
            },
        ];
        app.active_workspace_mut().active = 1;
        app.flow.stream_session_idx = Some(1);
        app.flow.pending_session_idx = Some(0);
        app.flow.pending_load_session_idx = Some(0);

        app.new_chat();

        assert_eq!(app.flow.stream_session_idx, Some(2));
        assert_eq!(app.flow.pending_session_idx, Some(1));
        assert_eq!(app.flow.pending_load_session_idx, Some(1));
    }

    #[test]
    fn delete_session_removes_persisted_chat_file() {
        let session_path = unique_path("delete-session");
        fs::write(&session_path, "{\"type\":\"session\",\"id\":\"one\"}\n").unwrap();

        let mut app = PiChatApp::new();
        app.active_workspace_mut().sessions = vec![
            crate::model::Session {
                title: "persisted".into(),
                messages: vec![],
                session_file: Some(session_path.to_string_lossy().to_string()),
                messages_loaded: false,
            },
            crate::model::Session {
                title: "other".into(),
                messages: vec![],
                session_file: None,
                messages_loaded: true,
            },
        ];
        app.active_workspace_mut().active = 0;

        app.delete_session(0);

        assert!(!session_path.exists());
        assert_eq!(app.active_workspace().sessions.len(), 1);
        assert_eq!(app.active_workspace().sessions[0].title, "other");
    }
}
