//! Background check for a newer GitHub release, following the app's spawn/drain
//! pattern (see `drain_models` / `drain_ssh_test`): a worker thread with its own tokio
//! runtime sends one message over an mpsc channel, drained each frame.

use eframe::egui;

use crate::update::{fetch_latest_release, is_newer, ReleaseInfo, APP_VERSION};

use super::state::UpdateMsg;
use super::task_runner::spawn_async_task;
use super::OxiApp;

impl OxiApp {
    /// Run the update check once per app start (plus explicit re-runs from the About
    /// panel's button with `force`). Failures are stored and shown only in About.
    pub(crate) fn ensure_update_checked(&mut self, ctx: &egui::Context, force: bool) {
        if self.conv.update_check_started && !force {
            return;
        }
        if self.conv.update_checking {
            return;
        }
        self.conv.update_check_started = true;
        self.conv.update_checking = true;

        let (tx, rx) = std::sync::mpsc::channel::<UpdateMsg>();
        self.conv.update_rx = Some(rx);
        let ctx = ctx.clone();
        let err_tx = tx.clone();
        let err_ctx = ctx.clone();
        spawn_async_task(
            move |err| {
                let _ = err_tx.send(UpdateMsg(Err(err)));
                err_ctx.request_repaint();
            },
            move |rt| {
                let r = rt.block_on(async {
                    let client = reqwest::Client::new();
                    fetch_latest_release(&client).await
                });
                let _ = tx.send(UpdateMsg(r));
                ctx.request_repaint();
            },
        );
    }

    /// Drain the update-check result. Mirrors [`Self::drain_models`].
    pub(crate) fn drain_update_check(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.conv.update_rx.take() else {
            return;
        };
        let mut repainted = false;
        loop {
            match rx.try_recv() {
                Ok(UpdateMsg(result)) => {
                    self.conv.update_checking = false;
                    self.conv.update_result = Some(result);
                    repainted = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    self.conv.update_rx = Some(rx);
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            }
        }
        if repainted {
            ctx.request_repaint();
        }
    }

    /// The latest release, when it is strictly newer than the running binary.
    pub(crate) fn update_available(&self) -> Option<&ReleaseInfo> {
        match &self.conv.update_result {
            Some(Ok(info)) if is_newer(&info.version, APP_VERSION) => Some(info),
            _ => None,
        }
    }
}
