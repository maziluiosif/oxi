//! Local and remote HuggingFace task spawning and runtime control.

use eframe::egui;

use crate::app::LocalRuntimeState;
use crate::app::task_runner::spawn_async_task;
use crate::local_models::{self, LocalModelMsg};
use crate::settings::LlmProviderKind;

use super::super::OxiApp;

impl OxiApp {
    pub(super) fn spawn_runtime_install(&mut self, ctx: &egui::Context) {
        self.conv.local_models.runtime_installing = true;
        self.conv.local_models.runtime_install_progress = None;
        self.conv.local_models.runtime_status = None;
        let (tx, rx) = std::sync::mpsc::channel();
        self.conv.local_model_rxs.push(rx);
        let err_tx = tx.clone();
        let err_ctx = ctx.clone();
        let work_ctx = ctx.clone();
        spawn_async_task(
            move |err| {
                let _ = err_tx.send(LocalModelMsg::RuntimeInstallDone(Err(err)));
                err_ctx.request_repaint();
            },
            move |rt| {
                let client = reqwest::Client::new();
                let r = rt.block_on(local_models::install_llama_server(&client, tx.clone()));
                let _ = tx.send(LocalModelMsg::RuntimeInstallDone(r));
                work_ctx.request_repaint();
            },
        );
    }

    fn remote_ssh_cfg_and_password(&self) -> Option<(crate::settings::SshConfig, String)> {
        let cfg = self
            .conv
            .settings
            .provider(LlmProviderKind::RemoteHf)
            .ssh_config()?
            .clone();
        let pw = self
            .conv
            .ssh_password_drafts
            .get(&LlmProviderKind::RemoteHf)
            .cloned()
            .unwrap_or_else(crate::local_models_remote::password_for_remotehf);
        Some((cfg, pw))
    }

    pub(super) fn spawn_remote_runtime_install(&mut self, ctx: &egui::Context) {
        let Some((cfg, password)) = self.remote_ssh_cfg_and_password() else {
            self.conv.local_models.remote_runtime_status =
                Some("Configure Remote SSH first.".into());
            return;
        };
        self.conv.local_models.runtime_installing = true;
        self.conv.local_models.remote_runtime_status = None;
        let (tx, rx) = std::sync::mpsc::channel();
        self.conv.local_model_rxs.push(rx);
        let err_tx = tx.clone();
        let err_ctx = ctx.clone();
        let work_ctx = ctx.clone();
        spawn_async_task(
            move |err| {
                let _ = err_tx.send(LocalModelMsg::RemoteRuntimeInstallDone(Err(err)));
                err_ctx.request_repaint();
            },
            move |rt| {
                let r = rt.block_on(crate::local_models_remote::install_runtime(&cfg, &password));
                let _ = tx.send(LocalModelMsg::RemoteRuntimeInstallDone(r));
                work_ctx.request_repaint();
            },
        );
    }

    pub(super) fn spawn_hf_search(&mut self, ctx: &egui::Context) {
        let query = self.conv.local_models.search_query.clone();
        self.conv.local_models.search_loading = true;
        self.conv.local_models.search_error = None;
        let (tx, rx) = std::sync::mpsc::channel();
        self.conv.local_model_rxs.push(rx);
        let err_tx = tx.clone();
        let err_ctx = ctx.clone();
        let work_ctx = ctx.clone();
        spawn_async_task(
            move |err| {
                let _ = err_tx.send(LocalModelMsg::Search(Err(err)));
                err_ctx.request_repaint();
            },
            move |rt| {
                let client = reqwest::Client::new();
                let r = rt.block_on(local_models::search_hf_models(&client, &query));
                let _ = tx.send(LocalModelMsg::Search(r));
                work_ctx.request_repaint();
            },
        );
    }

    pub(super) fn spawn_hf_files(&mut self, ctx: &egui::Context, repo: String) {
        if repo.trim().is_empty() {
            return;
        }
        self.conv.local_models.files_loading = true;
        self.conv.local_models.files_error = None;
        self.conv.local_models.selected_repo = repo.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        self.conv.local_model_rxs.push(rx);
        let err_tx = tx.clone();
        let err_ctx = ctx.clone();
        let err_repo = repo.clone();
        let work_ctx = ctx.clone();
        spawn_async_task(
            move |err| {
                let _ = err_tx.send(LocalModelMsg::Files {
                    repo: err_repo.clone(),
                    result: Err(err),
                });
                err_ctx.request_repaint();
            },
            move |rt| {
                let client = reqwest::Client::new();
                let r = rt.block_on(local_models::list_gguf_files(&client, &repo));
                let _ = tx.send(LocalModelMsg::Files { repo, result: r });
                work_ctx.request_repaint();
            },
        );
    }

    /// SSH target key used to detect when the remote model list belongs to a
    /// different host than the one currently configured.
    fn remote_ssh_target(cfg: &crate::settings::SshConfig) -> String {
        format!("{}@{}:{}", cfg.user, cfg.host, cfg.port)
    }

    /// Fetch the SSH host's model list the first time the panel shows in remote mode
    /// (and again whenever the SSH target changes). Deferred while another local-model
    /// task is in flight so its result channel isn't replaced mid-operation.
    pub(super) fn ensure_remote_models_listed(&mut self, ctx: &egui::Context) {
        let lm = &self.conv.local_models;
        if lm.remote_list_loading
            || lm.downloading
            || lm.runtime_installing
            || lm.search_loading
            || lm.files_loading
        {
            return;
        }
        let Some((cfg, _password)) = self.remote_ssh_cfg_and_password() else {
            return;
        };
        // Without a host/user an attempt can only fail. An empty password is still a
        // legitimate SSH credential on servers configured to allow it, so don't suppress
        // the listing solely because the password field is empty.
        if cfg.host.trim().is_empty() || cfg.user.trim().is_empty() {
            return;
        }
        let target = Self::remote_ssh_target(&cfg);
        if self.conv.local_models.remote_list_for.as_deref() == Some(target.as_str()) {
            return;
        }
        self.spawn_remote_list(ctx);
    }

    /// Load Remote HF's catalog directly from the managed model directory over SSH.
    /// This deliberately does not query `/v1/models`: the runtime may be stopped, and the
    /// downloaded-model picker must still be usable so the user can start one.
    pub(super) fn spawn_remote_list(&mut self, ctx: &egui::Context) {
        let Some((cfg, password)) = self.remote_ssh_cfg_and_password() else {
            let message = "Configure Remote SSH first.".to_string();
            self.conv.local_models.remote_runtime_status = Some(message.clone());
            let fetched = self
                .conv
                .fetched_models
                .entry(LlmProviderKind::RemoteHf)
                .or_default();
            fetched.loading = false;
            fetched.error = Some(message);
            return;
        };
        self.conv.local_models.remote_list_loading = true;
        self.conv.local_models.remote_list_for = Some(Self::remote_ssh_target(&cfg));
        let fetched = self
            .conv
            .fetched_models
            .entry(LlmProviderKind::RemoteHf)
            .or_default();
        fetched.loading = true;
        fetched.error = None;
        let (tx, rx) = std::sync::mpsc::channel();
        self.conv.local_model_rxs.push(rx);
        let err_tx = tx.clone();
        let err_ctx = ctx.clone();
        let work_ctx = ctx.clone();
        spawn_async_task(
            move |err| {
                let _ = err_tx.send(LocalModelMsg::RemoteListDone(Err(err)));
                err_ctx.request_repaint();
            },
            move |rt| {
                let r = rt.block_on(crate::local_models_remote::list_models(&cfg, &password));
                let _ = tx.send(LocalModelMsg::RemoteListDone(r));
                work_ctx.request_repaint();
            },
        );
    }

    pub(crate) fn spawn_remote_model_delete(
        &mut self,
        ctx: &egui::Context,
        id: String,
        path: String,
    ) {
        let Some((cfg, password)) = self.remote_ssh_cfg_and_password() else {
            self.conv.local_models.remote_runtime_status =
                Some("Configure Remote SSH first.".into());
            return;
        };
        let (tx, rx) = std::sync::mpsc::channel();
        self.conv.local_model_rxs.push(rx);
        let err_tx = tx.clone();
        let err_ctx = ctx.clone();
        let err_id = id.clone();
        let work_ctx = ctx.clone();
        spawn_async_task(
            move |err| {
                let _ = err_tx.send(LocalModelMsg::RemoteDeleteDone {
                    id: err_id.clone(),
                    result: Err(err),
                });
                err_ctx.request_repaint();
            },
            move |rt| {
                let r = rt.block_on(crate::local_models_remote::delete_model(
                    &cfg, &password, &path,
                ));
                let _ = tx.send(LocalModelMsg::RemoteDeleteDone { id, result: r });
                work_ctx.request_repaint();
            },
        );
    }

    pub(super) fn spawn_remote_hf_download(&mut self, ctx: &egui::Context) {
        let repo = self.conv.local_models.selected_repo.clone();
        let file = self.conv.local_models.selected_file.clone();
        if repo.trim().is_empty() || file.trim().is_empty() {
            return;
        }
        let Some((cfg, password)) = self.remote_ssh_cfg_and_password() else {
            self.conv.local_models.remote_runtime_status =
                Some("Configure Remote SSH first.".into());
            return;
        };
        self.conv.local_models.downloading = true;
        self.conv.local_models.download_label = format!("remote: {repo}/{file}");
        self.conv.local_models.download_progress = None;
        let (tx, rx) = std::sync::mpsc::channel();
        self.conv.local_model_rxs.push(rx);
        let err_tx = tx.clone();
        let err_ctx = ctx.clone();
        let work_ctx = ctx.clone();
        spawn_async_task(
            move |err| {
                let _ = err_tx.send(LocalModelMsg::RemoteDownloadDone(Err(err)));
                err_ctx.request_repaint();
            },
            move |rt| {
                let r = rt.block_on(crate::local_models_remote::download_model(
                    &cfg, &password, &repo, &file,
                ));
                let _ = tx.send(LocalModelMsg::RemoteDownloadDone(r));
                work_ctx.request_repaint();
            },
        );
    }

    pub(super) fn spawn_hf_download(&mut self, ctx: &egui::Context) {
        let repo = self.conv.local_models.selected_repo.clone();
        let file = self.conv.local_models.selected_file.clone();
        if repo.trim().is_empty() || file.trim().is_empty() {
            return;
        }
        self.conv.local_models.downloading = true;
        self.conv.local_models.download_label = format!("{repo}/{file}");
        self.conv.local_models.download_progress = None;
        let (tx, rx) = std::sync::mpsc::channel();
        self.conv.local_model_rxs.push(rx);
        let err_tx = tx.clone();
        let err_ctx = ctx.clone();
        let work_ctx = ctx.clone();
        spawn_async_task(
            move |err| {
                let _ = err_tx.send(LocalModelMsg::DownloadDone(Err(err)));
                err_ctx.request_repaint();
            },
            move |rt| {
                let client = reqwest::Client::new();
                let r = rt.block_on(local_models::download_gguf(
                    &client,
                    &repo,
                    &file,
                    tx.clone(),
                ));
                let _ = tx.send(LocalModelMsg::DownloadDone(r));
                work_ctx.request_repaint();
            },
        );
    }

    pub(super) fn activate_hf_model(
        &mut self,
        m: &local_models::DownloadedModel,
        kind: LlmProviderKind,
    ) {
        let is_remote = kind == LlmProviderKind::RemoteHf;
        let port = self.conv.local_models.runtime_port;
        let cfg = self.conv.settings.provider_mut(kind);
        cfg.model_id = m.id.clone();
        if is_remote {
            // Chat requests go through compute::resolve_base_url, which opens/reuses the SSH tunnel.
            cfg.base_url.clear();
        } else {
            cfg.base_url = format!("http://127.0.0.1:{port}/v1");
        }
        self.conv.settings.active_provider = kind;
        self.refresh_local_hf_model_choices();
        let _ = self.conv.settings.save();
    }

    /// Refresh both HF provider dropdowns without mixing local and SSH-host models.
    pub(crate) fn refresh_local_hf_model_choices(&mut self) {
        for (kind, source) in [
            (LlmProviderKind::LocalHf, &self.conv.local_models.downloaded),
            (
                LlmProviderKind::RemoteHf,
                &self.conv.local_models.remote_downloaded,
            ),
        ] {
            self.conv.fetched_models.entry(kind).or_default().models =
                source.iter().map(|m| m.id.clone()).collect();
        }
    }

    pub(super) fn upsert_remote_downloaded(&mut self, m: local_models::DownloadedModel) {
        let list = &mut self.conv.local_models.remote_downloaded;
        list.retain(|x| x.id != m.id);
        list.push(m);
        list.sort_by(|a, b| a.id.cmp(&b.id));
    }

    /// Start the downloaded Local HF model chosen from the composer. The runtime helper
    /// replaces the currently running model, so changing the dropdown is enough—no trip
    /// through Settings for Stop/Play is required.
    pub(crate) fn start_selected_local_hf_model(&mut self, ctx: &egui::Context, model_id: &str) {
        let kind = self.conv.settings.active_provider;
        let is_remote = kind == LlmProviderKind::RemoteHf;
        let model = if is_remote {
            self.conv
                .local_models
                .remote_downloaded
                .iter()
                .find(|m| m.id == model_id)
                .cloned()
        } else {
            self.conv
                .local_models
                .downloaded
                .iter()
                .find(|m| m.id == model_id)
                .cloned()
        };
        let Some(model) = model else {
            self.notify_composer(format!(
                "Model {model_id} is not present on the selected compute host. Refresh models in Settings."
            ));
            return;
        };

        self.notify_composer(format!(
            "Switching {} runtime to {}…",
            kind.label(),
            model.id
        ));
        if is_remote {
            self.start_remote_model(ctx, model);
        } else {
            self.start_local_model(ctx, model);
        }
    }

    pub(super) fn start_local_model(
        &mut self,
        _ctx: &egui::Context,
        m: local_models::DownloadedModel,
    ) {
        self.stop_local_model();
        let port = self.conv.local_models.runtime_port;
        match local_models::spawn_llama_server(
            &self.conv.local_models.runtime_path,
            &m.path,
            port,
            self.conv.local_models.context_size,
            self.conv.local_models.gpu_layers,
        ) {
            Ok(mut child) => {
                // A process can spawn successfully and then immediately die because a bundled
                // dylib/.so is missing or the model is invalid. Give it a moment and report that
                // as failed instead of showing a false "Running" state.
                std::thread::sleep(std::time::Duration::from_millis(350));
                match child.try_wait() {
                    Ok(Some(status)) => {
                        self.conv.local_models.running_model_id = None;
                        self.conv.local_models.runtime_status = Some(format!(
                            "llama-server exited immediately ({status}). See log: {}",
                            local_models::runtime_log_path().display()
                        ));
                    }
                    Ok(None) => {
                        self.conv.local_models.running_model_id = Some(m.id.clone());
                        self.conv.local_models.runtime_status = Some(format!(
                            "Starting {} on http://127.0.0.1:{port}/v1. If chat fails, wait a few seconds for model load. Log: {}",
                            m.id,
                            local_models::runtime_log_path().display()
                        ));
                        self.conv.local_runtime = Some(LocalRuntimeState {
                            child,
                            model_id: m.id.clone(),
                            port,
                        });
                        self.activate_hf_model(&m, LlmProviderKind::LocalHf);
                    }
                    Err(e) => {
                        self.conv.local_models.running_model_id = None;
                        self.conv.local_models.runtime_status = Some(format!(
                            "Could not check llama-server status: {e}. Log: {}",
                            local_models::runtime_log_path().display()
                        ));
                    }
                }
            }
            Err(e) => self.conv.local_models.runtime_status = Some(e),
        }
    }

    pub(super) fn start_remote_model(
        &mut self,
        ctx: &egui::Context,
        m: local_models::DownloadedModel,
    ) {
        let Some((cfg, password)) = self.remote_ssh_cfg_and_password() else {
            self.conv.local_models.remote_runtime_status =
                Some("Configure Remote SSH first.".into());
            return;
        };
        self.conv.local_models.remote_running_model_id = None;
        self.conv.local_models.remote_runtime_status = Some(format!("Starting remote {}…", m.id));
        let context = self.conv.local_models.context_size;
        let gpu_layers = self.conv.local_models.gpu_layers;
        let (tx, rx) = std::sync::mpsc::channel();
        self.conv.local_model_rxs.push(rx);
        let err_tx = tx.clone();
        let err_ctx = ctx.clone();
        let err_model = m.clone();
        let work_ctx = ctx.clone();
        spawn_async_task(
            move |err| {
                let _ = err_tx.send(LocalModelMsg::RemoteStartDone {
                    model: err_model.clone(),
                    result: Err(err),
                });
                err_ctx.request_repaint();
            },
            move |rt| {
                let r = rt.block_on(crate::local_models_remote::start_model(
                    &cfg,
                    &password,
                    &m.path,
                    &m.repo,
                    &m.filename,
                    context,
                    gpu_layers,
                ));
                let _ = tx.send(LocalModelMsg::RemoteStartDone {
                    model: m,
                    result: r,
                });
                work_ctx.request_repaint();
            },
        );
    }

    pub(super) fn spawn_remote_stop(&mut self, ctx: &egui::Context) {
        let Some((cfg, password)) = self.remote_ssh_cfg_and_password() else {
            return;
        };
        let (tx, rx) = std::sync::mpsc::channel();
        self.conv.local_model_rxs.push(rx);
        let err_tx = tx.clone();
        let err_ctx = ctx.clone();
        let work_ctx = ctx.clone();
        spawn_async_task(
            move |err| {
                let _ = err_tx.send(LocalModelMsg::RemoteStopDone(Err(err)));
                err_ctx.request_repaint();
            },
            move |rt| {
                let r = rt.block_on(crate::local_models_remote::stop_model(&cfg, &password));
                let _ = tx.send(LocalModelMsg::RemoteStopDone(r));
                work_ctx.request_repaint();
            },
        );
    }

    pub(super) fn stop_local_model(&mut self) {
        if let Some(mut rt) = self.conv.local_runtime.take() {
            let _ = rt.child.kill();
            let _ = rt.child.wait();
            self.conv.local_models.runtime_status =
                Some(format!("Stopped {} on port {}", rt.model_id, rt.port));
        }
        self.conv.local_models.running_model_id = None;
    }
}
