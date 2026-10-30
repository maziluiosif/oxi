//! Opening and saving files of a remote workspace off the UI thread: each takes one or two
//! round trips to the host, which on a slow link would freeze the window for seconds.

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::SystemTime;

use eframe::egui;

use super::super::OxiApp;
use super::documents::{LoadedDocument, load_document};

enum Done {
    Opened {
        path: PathBuf,
        reveal: bool,
        result: Result<LoadedDocument, String>,
    },
    Saved {
        path: PathBuf,
        written: String,
        outcome: SaveOutcome,
    },
}

enum SaveOutcome {
    Written(std::io::Result<Option<SystemTime>>),
    /// The disk no longer held what the tab last saved, so nothing was written.
    DiskChanged(std::io::Result<Vec<u8>>),
}

/// Remote opens and saves in flight.
pub(crate) struct RemoteIo {
    tx: mpsc::Sender<Done>,
    rx: mpsc::Receiver<Done>,
    opening: Vec<PathBuf>,
    /// Paths being saved, and whether Save was pressed again meanwhile.
    saving: Vec<(PathBuf, bool)>,
    ctx: Option<egui::Context>,
}

impl Default for RemoteIo {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            tx,
            rx,
            opening: Vec::new(),
            saving: Vec::new(),
            ctx: None,
        }
    }
}

impl RemoteIo {
    pub(crate) fn is_saving(&self, path: &std::path::Path) -> bool {
        self.saving.iter().any(|(p, _)| p == path)
    }

    pub(super) fn open(&mut self, path: PathBuf, reveal: bool) {
        if self.opening.contains(&path) {
            return;
        }
        self.opening.push(path.clone());
        let job_path = path.clone();
        if !self.spawn(move || {
            let path = job_path;
            let result = load_document(&path);
            Done::Opened {
                path,
                reveal,
                result,
            }
        }) {
            self.opening.retain(|p| *p != path);
        }
    }

    /// Write `content` unless the disk no longer holds `expected` (`None`: overwrite).
    pub(super) fn save(&mut self, path: PathBuf, expected: Option<String>, content: String) {
        if let Some((_, again)) = self.saving.iter_mut().find(|(p, _)| *p == path) {
            *again = true;
            return;
        }
        self.saving.push((path.clone(), false));
        let job_path = path.clone();
        if !self.spawn(move || {
            let path = job_path;
            let disk = expected.map(|expected| (crate::vfs::read(&path), expected));
            let outcome = match disk {
                Some((Ok(bytes), expected)) if bytes == expected.as_bytes() => None,
                Some((disk, _)) => Some(SaveOutcome::DiskChanged(disk)),
                None => None,
            }
            .unwrap_or_else(|| {
                SaveOutcome::Written(
                    crate::vfs::write(&path, content.as_bytes())
                        .map(|()| crate::vfs::modified(&path)),
                )
            });
            Done::Saved {
                path,
                written: content,
                outcome,
            }
        }) {
            self.saving.retain(|(p, _)| *p != path);
        }
    }

    /// Whether the worker started.
    fn spawn(&self, job: impl FnOnce() -> Done + Send + 'static) -> bool {
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        std::thread::Builder::new()
            .name("oxi-remote-io".into())
            .spawn(move || {
                let _ = tx.send(job());
                if let Some(ctx) = ctx {
                    ctx.request_repaint();
                }
            })
            .is_ok()
    }
}

impl OxiApp {
    /// Apply remote opens and saves that finished since the last frame.
    pub(crate) fn poll_remote_io(&mut self, ctx: &egui::Context) {
        let io = &mut self.conv.editor.remote_io;
        if io.ctx.is_none() {
            io.ctx = Some(ctx.clone());
        }
        while let Ok(done) = self.conv.editor.remote_io.rx.try_recv() {
            match done {
                Done::Opened {
                    path,
                    reveal,
                    result,
                } => {
                    self.conv.editor.remote_io.opening.retain(|p| *p != path);
                    self.finish_remote_open(path, reveal, result);
                }
                Done::Saved {
                    path,
                    written,
                    outcome,
                } => {
                    let io = &mut self.conv.editor.remote_io;
                    let again = io
                        .saving
                        .iter()
                        .position(|(p, _)| *p == path)
                        .is_some_and(|at| io.saving.remove(at).1);
                    self.finish_remote_save(path, written, outcome, again);
                }
            }
        }
    }

    fn finish_remote_open(
        &mut self,
        path: PathBuf,
        reveal: bool,
        result: Result<super::documents::LoadedDocument, String>,
    ) {
        if let Some(index) = self
            .conv
            .editor
            .documents
            .iter()
            .position(|d| d.path == path)
        {
            self.conv.editor.active = Some(index);
            return;
        }
        match result {
            Ok(loaded) => {
                self.conv.editor.hidden_active = None;
                self.push_loaded_document(path, loaded, reveal);
            }
            Err(error) => self.conv.editor.error = Some(error),
        }
    }

    fn finish_remote_save(
        &mut self,
        path: PathBuf,
        written: String,
        outcome: SaveOutcome,
        again: bool,
    ) {
        // A tab closed meanwhile: what was written stays written.
        let Some(index) = self
            .conv
            .editor
            .documents
            .iter()
            .position(|d| d.path == path && d.media.is_none())
        else {
            return;
        };
        let document = &mut self.conv.editor.documents[index];
        let error = match outcome {
            SaveOutcome::Written(Ok(modified)) => {
                document.mark_saved(written, modified);
                self.conv.editor.error = None;
                if let Some(tx) = &self.conv.git_ui.tx {
                    let _ = tx.send(crate::git::GitOp::Refresh);
                }
                if !(again && document.is_dirty()) {
                    return;
                }
                None
            }
            SaveOutcome::Written(Err(error)) => Some(format!("Could not save file: {error}")),
            SaveOutcome::DiskChanged(disk) => document.take_in_disk_before_save(disk).err(),
        };
        if let Some(error) = error {
            if document.externally_modified {
                self.conv.editor.prompt = Some(super::super::state::EditorPrompt::Overwrite {
                    path,
                    close_after: false,
                });
            }
            self.conv.editor.error = Some(error);
            return;
        }
        // Merged with the disk's new text, or edited again while saving: save what it is now.
        let expected = Some(document.saved_content.clone());
        let content = document.content.clone();
        self.conv.editor.remote_io.save(path, expected, content);
    }
}
