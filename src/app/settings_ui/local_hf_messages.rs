//! Draining and applying asynchronous local-model task results.

use eframe::egui;

use crate::local_models::{self, LocalModelMsg};
use crate::settings::{ComputeLocation, LlmProviderKind};

use super::super::OxiApp;

impl OxiApp {
    pub(crate) fn drain_local_models(&mut self, ctx: &egui::Context) {
        let receivers = std::mem::take(&mut self.conv.local_model_rxs);
        for rx in receivers {
            if self.drain_local_model_rx(ctx, &rx) {
                self.conv.local_model_rxs.push(rx);
            }
        }
    }

    fn drain_local_model_rx(
        &mut self,
        ctx: &egui::Context,
        rx: &std::sync::mpsc::Receiver<LocalModelMsg>,
    ) -> bool {
        let mut keep = true;
        loop {
            match rx.try_recv() {
                Ok(LocalModelMsg::Search(r)) => {
                    self.conv.local_models.search_loading = false;
                    match r {
                        Ok(v) => {
                            self.conv.local_models.search_results = v;
                            self.conv.local_models.search_error = None;
                        }
                        Err(e) => self.conv.local_models.search_error = Some(e),
                    }
                    ctx.request_repaint();
                }
                Ok(LocalModelMsg::Files { repo, result }) => {
                    self.conv.local_models.files_loading = false;
                    self.conv.local_models.selected_repo = repo;
                    match result {
                        Ok(v) => {
                            self.conv.local_models.selected_file =
                                v.first().cloned().unwrap_or_default();
                            self.conv.local_models.gguf_files = v;
                            self.conv.local_models.files_error = None;
                        }
                        Err(e) => self.conv.local_models.files_error = Some(e),
                    }
                    ctx.request_repaint();
                }
                Ok(LocalModelMsg::DownloadProgress {
                    id,
                    downloaded,
                    total,
                }) => {
                    self.conv.local_models.download_label = id;
                    self.conv.local_models.download_progress = Some((downloaded, total));
                    ctx.request_repaint();
                }
                Ok(LocalModelMsg::DownloadDone(r)) => {
                    self.conv.local_models.downloading = false;
                    match r {
                        Ok(m) => {
                            self.conv.local_models.downloaded =
                                local_models::load_manifest().models;
                            self.start_local_model(ctx, m);
                        }
                        Err(e) => {
                            self.conv.local_models.runtime_status =
                                Some(format!("Download failed: {e}"))
                        }
                    }
                    ctx.request_repaint();
                }
                Ok(LocalModelMsg::RuntimeInstallProgress { downloaded, total }) => {
                    self.conv.local_models.runtime_install_progress = Some((downloaded, total));
                    ctx.request_repaint();
                }
                Ok(LocalModelMsg::RuntimeInstallDone(r)) => {
                    self.conv.local_models.runtime_installing = false;
                    match r {
                        Ok(path) => {
                            self.conv.local_models.runtime_path = path.clone();
                            self.conv.local_models.runtime_status =
                                Some(format!("Runtime installed: {path}"));
                        }
                        Err(e) => {
                            self.conv.local_models.runtime_status =
                                Some(format!("Runtime install failed: {e}"))
                        }
                    }
                    ctx.request_repaint();
                }
                Ok(LocalModelMsg::RemoteRuntimeInstallDone(r)) => {
                    self.conv.local_models.runtime_installing = false;
                    match r {
                        Ok(path) => {
                            self.conv.local_models.remote_runtime_status =
                                Some(format!("Remote runtime installed: {path}"))
                        }
                        Err(e) => {
                            self.conv.local_models.remote_runtime_status =
                                Some(format!("Remote runtime install failed: {e}"))
                        }
                    }
                    ctx.request_repaint();
                }
                Ok(LocalModelMsg::RemoteListDone(r)) => {
                    self.conv.local_models.remote_list_loading = false;
                    match r {
                        Ok(list) => {
                            // A fresh successful listing supersedes a stale listing error.
                            if self
                                .conv
                                .local_models
                                .remote_runtime_status
                                .as_deref()
                                .is_some_and(|s| s.starts_with("Could not list models"))
                            {
                                self.conv.local_models.remote_runtime_status = None;
                            }
                            self.conv.local_models.remote_downloaded = list.models;
                            let fetched = self
                                .conv
                                .fetched_models
                                .entry(LlmProviderKind::RemoteHf)
                                .or_default();
                            fetched.loading = false;
                            fetched.error = None;
                            fetched.models = self
                                .conv
                                .local_models
                                .remote_downloaded
                                .iter()
                                .map(|m| m.id.clone())
                                .collect();
                            let is_remote = matches!(
                                self.conv
                                    .settings
                                    .provider(LlmProviderKind::RemoteHf)
                                    .location,
                                ComputeLocation::RemoteSsh(_)
                            );
                            if is_remote {
                                // Adopt the host's actual runtime state, so a server left
                                // running from an earlier session shows up with a Stop
                                // button instead of being invisible.
                                self.conv.local_models.remote_running_model_id = list
                                    .running_path
                                    .as_ref()
                                    .filter(|p| !p.is_empty())
                                    .and_then(|p| {
                                        self.conv
                                            .local_models
                                            .remote_downloaded
                                            .iter()
                                            .find(|m| &m.path == p)
                                            .map(|m| m.id.clone())
                                    });
                                if matches!(list.running_path.as_deref(), Some("")) {
                                    self.conv.local_models.remote_runtime_status = Some(
                                        "A llama-server is running on the SSH host, but its \
                                         model could not be identified. Press Play on a model \
                                         to replace it."
                                            .into(),
                                    );
                                }
                            }
                            self.refresh_local_hf_model_choices();
                        }
                        Err(e) => {
                            let message = format!("Could not list models on SSH host: {e}");
                            self.conv.local_models.remote_runtime_status = Some(message.clone());
                            let fetched = self
                                .conv
                                .fetched_models
                                .entry(LlmProviderKind::RemoteHf)
                                .or_default();
                            fetched.loading = false;
                            fetched.error = Some(message);
                        }
                    }
                    ctx.request_repaint();
                }
                Ok(LocalModelMsg::RemoteDeleteDone { id, result }) => {
                    match result {
                        Ok(()) => self
                            .conv
                            .local_models
                            .remote_downloaded
                            .retain(|m| m.id != id),
                        Err(e) => {
                            self.conv.local_models.remote_runtime_status =
                                Some(format!("Remote delete failed: {e}"))
                        }
                    }
                    ctx.request_repaint();
                }
                Ok(LocalModelMsg::RemoteDownloadDone(r)) => {
                    self.conv.local_models.downloading = false;
                    match r {
                        Ok(m) => {
                            self.upsert_remote_downloaded(m.clone());
                            self.start_remote_model(ctx, m);
                        }
                        Err(e) => {
                            self.conv.local_models.remote_runtime_status =
                                Some(format!("Remote download failed: {e}"))
                        }
                    }
                    ctx.request_repaint();
                }
                Ok(LocalModelMsg::RemoteStartDone { model, result }) => {
                    match result {
                        Ok(msg) => {
                            self.conv.local_models.remote_running_model_id = Some(model.id.clone());
                            self.conv.local_models.remote_runtime_status = Some(msg);
                            self.activate_hf_model(&model, LlmProviderKind::RemoteHf);
                            self.notify_composer(format!("Remote HF is now running {}.", model.id));
                        }
                        Err(e) => {
                            self.conv.local_models.remote_running_model_id = None;
                            self.conv.local_models.remote_runtime_status =
                                Some(format!("Remote start failed: {e}"));
                            self.notify_composer(format!("Could not switch Remote HF model: {e}"));
                        }
                    }
                    ctx.request_repaint();
                }
                Ok(LocalModelMsg::RemoteStopDone(r)) => {
                    self.conv.local_models.remote_running_model_id = None;
                    self.conv.local_models.remote_runtime_status = Some(match r {
                        Ok(s) => format!("Remote runtime {s}"),
                        Err(e) => format!("Remote stop failed: {e}"),
                    });
                    ctx.request_repaint();
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    keep = false;
                    break;
                }
            }
        }
        keep
    }
}
