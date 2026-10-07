//! Fire-and-forget filesystem work (trashing a chat file, deleting a work tree folder) that
//! must not stall a frame. Each chore runs on its own thread; a failure surfaces as a composer
//! notice once it is drained on the UI thread.

use std::sync::mpsc::{Receiver, TryRecvError};

use eframe::egui;

use super::OxiApp;

pub(crate) struct Chore {
    /// Prefix of the notice shown when the chore fails, e.g. "Could not delete the chat file".
    failure: &'static str,
    rx: Receiver<Result<(), String>>,
}

impl OxiApp {
    pub(crate) fn spawn_chore(
        &mut self,
        failure: &'static str,
        work: impl FnOnce() -> Result<(), String> + Send + 'static,
    ) {
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = self.conv.git_ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(work());
            ctx.request_repaint();
        });
        self.conv.chores.push(Chore { failure, rx });
    }

    pub(crate) fn drain_chores(&mut self, ctx: &egui::Context) {
        if self.conv.chores.is_empty() {
            return;
        }
        let mut failures = Vec::new();
        self.conv.chores.retain(|chore| match chore.rx.try_recv() {
            Ok(Ok(())) | Err(TryRecvError::Disconnected) => false,
            Ok(Err(e)) => {
                failures.push(format!("{}: {e}", chore.failure));
                false
            }
            Err(TryRecvError::Empty) => true,
        });
        for failure in failures {
            log::warn!("{failure}");
            self.notify_composer(failure);
            ctx.request_repaint();
        }
    }
}
