use eframe::egui::{self, Margin, RichText, TextEdit, Ui};

use crate::app::task_runner::spawn_async_task;
use crate::app::LocalRuntimeState;
use crate::local_models::{self, LocalModelMsg};
use crate::settings::{ComputeLocation, LlmProviderKind};
use crate::theme::*;
use crate::ui::chrome::{field_label, ghost_button, nested_card_frame, settings_caption};

use super::super::OxiApp;

impl OxiApp {
    pub(super) fn render_local_hf_section(&mut self, ui: &mut Ui) {
        ui.add_space(8.0);
        nested_card_frame().show(ui, |ui| {
            let is_remote = matches!(self.conv.settings.provider(LlmProviderKind::LocalHf).location, ComputeLocation::RemoteSsh(_));
            settings_caption(ui, if is_remote { "HuggingFace models on SSH" } else { "HuggingFace local models" });
            ui.label(RichText::new(if is_remote { "Search GGUF models on HuggingFace, download them onto the SSH host, then press Play. oxi starts llama-server remotely and tunnels it back for chat." } else { "Search GGUF models on HuggingFace, download them into oxi, then press Play. oxi starts llama-server in the background and makes the model available for chat." }).size(FS_TINY).color(c_text_muted()));
            ui.add_space(10.0);

            field_label(ui, if is_remote { "Remote runtime" } else { "Local runtime" });
            ui.horizontal(|ui| {
                let installed = local_models::installed_runtime_path();
                let label = if is_remote { "Runtime managed on SSH host" } else if installed.is_some() { "Runtime installed" } else { "Runtime not installed" };
                ui.label(RichText::new(label).size(FS_TINY).color(if is_remote || installed.is_some() { c_success() } else { c_text_muted() }));
                let installing = self.conv.local_models.runtime_installing;
                let button = if is_remote { "Install runtime on SSH" } else { "Install runtime" };
                if ui.add_enabled(!installing, crate::ui::chrome::primary_button_widget(if installing { "Installing…" } else { button })).clicked() {
                    if is_remote { self.spawn_remote_runtime_install(ui.ctx()); } else { self.spawn_runtime_install(ui.ctx()); }
                }
            });
            if self.conv.local_models.runtime_installing {
                let text = match self.conv.local_models.runtime_install_progress {
                    Some((done, Some(total))) if total > 0 => format!("Downloading runtime… {:.1}% ({}/{})", done as f64 * 100.0 / total as f64, fmt_bytes(done), fmt_bytes(total)),
                    Some((done, _)) => format!("Downloading runtime… {}", fmt_bytes(done)),
                    None => "Downloading runtime…".to_string(),
                };
                ui.label(RichText::new(text).size(FS_TINY).color(c_text_muted()));
            }
            if !is_remote {
                field_label(ui, "llama-server path (optional override)");
                ui.add(TextEdit::singleline(&mut self.conv.local_models.runtime_path)
                    .desired_width(f32::INFINITY)
                    .hint_text("empty = bundled runtime, then PATH fallback")
                    .margin(Margin::symmetric(8, 5)));
            }
            ui.horizontal(|ui| {
                field_label(ui, "Port");
                let mut port = self.conv.local_models.runtime_port.to_string();
                if ui.add(TextEdit::singleline(&mut port).desired_width(80.0)).changed()
                    && let Ok(p) = port.parse::<u16>()
                { self.conv.local_models.runtime_port = p; }
                field_label(ui, "Context");
                let mut ctx = self.conv.local_models.context_size.to_string();
                if ui.add(TextEdit::singleline(&mut ctx).desired_width(90.0)).changed()
                    && let Ok(n) = ctx.parse::<usize>()
                { self.conv.local_models.context_size = n.max(512); }
                field_label(ui, "GPU layers");
                let mut ngl = self.conv.local_models.gpu_layers.to_string();
                if ui.add(TextEdit::singleline(&mut ngl).desired_width(70.0)).changed()
                    && let Ok(n) = ngl.parse::<i32>()
                { self.conv.local_models.gpu_layers = n; }
            });

            ui.add_space(12.0);
            field_label(ui, "Search HuggingFace");
            ui.horizontal(|ui| {
                ui.add(TextEdit::singleline(&mut self.conv.local_models.search_query)
                    .desired_width(ui.available_width() - 90.0)
                    .hint_text("e.g. qwen coder gguf")
                    .margin(Margin::symmetric(8, 5)));
                let busy = self.conv.local_models.search_loading;
                if ui.add_enabled(!busy, crate::ui::chrome::primary_button_widget(if busy { "Searching…" } else { "Search" })).clicked() {
                    self.spawn_hf_search(ui.ctx());
                }
            });
            if let Some(e) = &self.conv.local_models.search_error {
                ui.label(RichText::new(e).size(FS_TINY).color(c_danger()));
            }
            for hit in self.conv.local_models.search_results.clone().into_iter().take(8) {
                ui.horizontal(|ui| {
                    if ghost_button(ui, &hit.model_id, false).clicked() {
                        self.conv.local_models.selected_repo = hit.model_id.clone();
                        self.spawn_hf_files(ui.ctx(), hit.model_id);
                    }
                    ui.label(RichText::new(format!("↓ {}  ♥ {}", hit.downloads.unwrap_or(0), hit.likes.unwrap_or(0))).size(FS_TINY).color(c_text_faint()));
                });
            }

            ui.add_space(10.0);
            field_label(ui, "Selected repo / GGUF file");
            ui.horizontal(|ui| {
                ui.add(TextEdit::singleline(&mut self.conv.local_models.selected_repo)
                    .desired_width(ui.available_width() - 120.0)
                    .hint_text("org/model-GGUF")
                    .margin(Margin::symmetric(8, 5)));
                if ui.add_enabled(!self.conv.local_models.files_loading, crate::ui::chrome::ghost_button_widget("Load files", false)).clicked() {
                    let repo = self.conv.local_models.selected_repo.clone();
                    self.spawn_hf_files(ui.ctx(), repo);
                }
            });
            if let Some(e) = &self.conv.local_models.files_error {
                ui.label(RichText::new(e).size(FS_TINY).color(c_danger()));
            }
            if !self.conv.local_models.gguf_files.is_empty() {
                let current = if self.conv.local_models.selected_file.is_empty() { "choose .gguf".to_string() } else { self.conv.local_models.selected_file.clone() };
                egui::ComboBox::from_id_salt("local_hf_file_combo")
                    .selected_text(current)
                    .width(f32::INFINITY)
                    .show_ui(ui, |ui| {
                        for f in self.conv.local_models.gguf_files.clone() {
                            if ui.selectable_label(self.conv.local_models.selected_file == f, &f).clicked() {
                                self.conv.local_models.selected_file = f;
                            }
                        }
                    });
                if ui.add_enabled(!self.conv.local_models.downloading && !self.conv.local_models.selected_file.is_empty(), crate::ui::chrome::primary_button_widget("Download")).clicked() {
                    if is_remote { self.spawn_remote_hf_download(ui.ctx()); } else { self.spawn_hf_download(ui.ctx()); }
                }
            }
            if self.conv.local_models.downloading {
                let mut text = self.conv.local_models.download_label.clone();
                if let Some((done, total)) = self.conv.local_models.download_progress {
                    text = match total {
                        Some(t) if t > 0 => format!("Downloading… {:.1}% ({}/{})", done as f64 * 100.0 / t as f64, fmt_bytes(done), fmt_bytes(t)),
                        _ => format!("Downloading… {}", fmt_bytes(done)),
                    };
                }
                ui.label(RichText::new(text).size(FS_TINY).color(c_text_muted()));
            }

            ui.add_space(14.0);
            settings_caption(ui, "Downloaded models");
            if self.conv.local_models.downloaded.is_empty() {
                ui.label(RichText::new("No local HF models downloaded yet.").size(FS_TINY).color(c_text_faint()));
            }
            for m in self.conv.local_models.downloaded.clone() {
                ui.horizontal_wrapped(|ui| {
                    let running = self.conv.local_models.running_model_id.as_deref() == Some(&m.id);
                    if ui.add(crate::ui::chrome::primary_button_widget(if running { "Running" } else { "▶ Play" })).clicked() {
                        if is_remote { self.start_remote_model(ui.ctx(), m.clone()); } else { self.start_local_model(ui.ctx(), m.clone()); }
                    }
                    if running && ghost_button(ui, "Stop", true).clicked() {
                        if is_remote { self.spawn_remote_stop(ui.ctx()); } else { self.stop_local_model(); }
                    }
                    if ghost_button(ui, "Make active", false).clicked() {
                        self.activate_local_model(&m);
                    }
                    if ghost_button(ui, "Delete", true).clicked() {
                        let _ = local_models::remove_downloaded(&m.id);
                        self.conv.local_models.downloaded = local_models::load_manifest().models;
                    }
                    ui.label(RichText::new(format!("{} ({})", m.id, fmt_bytes(m.bytes))).size(FS_TINY).color(c_text_muted()));
                });
            }
            if let Some(s) = &self.conv.local_models.runtime_status {
                ui.add_space(6.0);
                ui.label(RichText::new(s).size(FS_TINY).color(c_text_muted()));
            }
        });
    }

    fn spawn_runtime_install(&mut self, ctx: &egui::Context) {
        self.conv.local_models.runtime_installing = true;
        self.conv.local_models.runtime_install_progress = None;
        self.conv.local_models.runtime_status = None;
        let (tx, rx) = std::sync::mpsc::channel();
        self.conv.local_model_rx = Some(rx);
        let err_tx = tx.clone();
        let err_ctx = ctx.clone();
        let work_ctx = ctx.clone();
        spawn_async_task(move |err| { let _ = err_tx.send(LocalModelMsg::RuntimeInstallDone(Err(err))); err_ctx.request_repaint(); }, move |rt| {
            let client = reqwest::Client::new();
            let r = rt.block_on(local_models::install_llama_server(&client, tx.clone()));
            let _ = tx.send(LocalModelMsg::RuntimeInstallDone(r));
            work_ctx.request_repaint();
        });
    }

    fn remote_ssh_cfg_and_password(&self) -> Option<(crate::settings::SshConfig, String)> {
        let cfg = self.conv.settings.provider(LlmProviderKind::LocalHf).ssh_config()?.clone();
        let pw = self
            .conv
            .ssh_password_drafts
            .get(&LlmProviderKind::LocalHf)
            .cloned()
            .unwrap_or_else(crate::local_models_remote::password_for_localhf);
        Some((cfg, pw))
    }

    fn spawn_remote_runtime_install(&mut self, ctx: &egui::Context) {
        let Some((cfg, password)) = self.remote_ssh_cfg_and_password() else {
            self.conv.local_models.runtime_status = Some("Configure Remote SSH first.".into());
            return;
        };
        self.conv.local_models.runtime_installing = true;
        self.conv.local_models.runtime_status = None;
        let (tx, rx) = std::sync::mpsc::channel();
        self.conv.local_model_rx = Some(rx);
        let err_tx = tx.clone();
        let err_ctx = ctx.clone();
        let work_ctx = ctx.clone();
        spawn_async_task(move |err| { let _ = err_tx.send(LocalModelMsg::RemoteRuntimeInstallDone(Err(err))); err_ctx.request_repaint(); }, move |rt| {
            let r = rt.block_on(crate::local_models_remote::install_runtime(&cfg, &password));
            let _ = tx.send(LocalModelMsg::RemoteRuntimeInstallDone(r));
            work_ctx.request_repaint();
        });
    }

    fn spawn_hf_search(&mut self, ctx: &egui::Context) {
        let query = self.conv.local_models.search_query.clone();
        self.conv.local_models.search_loading = true;
        self.conv.local_models.search_error = None;
        let (tx, rx) = std::sync::mpsc::channel();
        self.conv.local_model_rx = Some(rx);
        let err_tx = tx.clone();
        let err_ctx = ctx.clone();
        let work_ctx = ctx.clone();
        spawn_async_task(move |err| { let _ = err_tx.send(LocalModelMsg::Search(Err(err))); err_ctx.request_repaint(); }, move |rt| {
            let client = reqwest::Client::new();
            let r = rt.block_on(local_models::search_hf_models(&client, &query));
            let _ = tx.send(LocalModelMsg::Search(r));
            work_ctx.request_repaint();
        });
    }

    fn spawn_hf_files(&mut self, ctx: &egui::Context, repo: String) {
        if repo.trim().is_empty() { return; }
        self.conv.local_models.files_loading = true;
        self.conv.local_models.files_error = None;
        self.conv.local_models.selected_repo = repo.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        self.conv.local_model_rx = Some(rx);
        let err_tx = tx.clone();
        let err_ctx = ctx.clone();
        let err_repo = repo.clone();
        let work_ctx = ctx.clone();
        spawn_async_task(move |err| { let _ = err_tx.send(LocalModelMsg::Files { repo: err_repo.clone(), result: Err(err) }); err_ctx.request_repaint(); }, move |rt| {
            let client = reqwest::Client::new();
            let r = rt.block_on(local_models::list_gguf_files(&client, &repo));
            let _ = tx.send(LocalModelMsg::Files { repo, result: r });
            work_ctx.request_repaint();
        });
    }

    fn spawn_remote_hf_download(&mut self, ctx: &egui::Context) {
        let repo = self.conv.local_models.selected_repo.clone();
        let file = self.conv.local_models.selected_file.clone();
        if repo.trim().is_empty() || file.trim().is_empty() { return; }
        let Some((cfg, password)) = self.remote_ssh_cfg_and_password() else {
            self.conv.local_models.runtime_status = Some("Configure Remote SSH first.".into());
            return;
        };
        self.conv.local_models.downloading = true;
        self.conv.local_models.download_label = format!("remote: {repo}/{file}");
        self.conv.local_models.download_progress = None;
        let (tx, rx) = std::sync::mpsc::channel();
        self.conv.local_model_rx = Some(rx);
        let err_tx = tx.clone();
        let err_ctx = ctx.clone();
        let work_ctx = ctx.clone();
        spawn_async_task(move |err| { let _ = err_tx.send(LocalModelMsg::RemoteDownloadDone(Err(err))); err_ctx.request_repaint(); }, move |rt| {
            let r = rt.block_on(crate::local_models_remote::download_model(&cfg, &password, &repo, &file));
            let _ = tx.send(LocalModelMsg::RemoteDownloadDone(r));
            work_ctx.request_repaint();
        });
    }

    fn spawn_hf_download(&mut self, ctx: &egui::Context) {
        let repo = self.conv.local_models.selected_repo.clone();
        let file = self.conv.local_models.selected_file.clone();
        if repo.trim().is_empty() || file.trim().is_empty() { return; }
        self.conv.local_models.downloading = true;
        self.conv.local_models.download_label = format!("{repo}/{file}");
        self.conv.local_models.download_progress = None;
        let (tx, rx) = std::sync::mpsc::channel();
        self.conv.local_model_rx = Some(rx);
        let err_tx = tx.clone();
        let err_ctx = ctx.clone();
        let work_ctx = ctx.clone();
        spawn_async_task(move |err| { let _ = err_tx.send(LocalModelMsg::DownloadDone(Err(err))); err_ctx.request_repaint(); }, move |rt| {
            let client = reqwest::Client::new();
            let r = rt.block_on(local_models::download_gguf(&client, &repo, &file, tx.clone()));
            let _ = tx.send(LocalModelMsg::DownloadDone(r));
            work_ctx.request_repaint();
        });
    }

    pub(crate) fn drain_local_models(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.conv.local_model_rx.take() else { return; };
        let mut keep = true;
        loop {
            match rx.try_recv() {
                Ok(LocalModelMsg::Search(r)) => { self.conv.local_models.search_loading = false; match r { Ok(v) => { self.conv.local_models.search_results = v; self.conv.local_models.search_error = None; }, Err(e) => self.conv.local_models.search_error = Some(e) } ctx.request_repaint(); }
                Ok(LocalModelMsg::Files { repo, result }) => { self.conv.local_models.files_loading = false; self.conv.local_models.selected_repo = repo; match result { Ok(v) => { self.conv.local_models.selected_file = v.first().cloned().unwrap_or_default(); self.conv.local_models.gguf_files = v; self.conv.local_models.files_error = None; }, Err(e) => self.conv.local_models.files_error = Some(e) } ctx.request_repaint(); }
                Ok(LocalModelMsg::DownloadProgress { id, downloaded, total }) => { self.conv.local_models.download_label = id; self.conv.local_models.download_progress = Some((downloaded, total)); ctx.request_repaint(); }
                Ok(LocalModelMsg::DownloadDone(r)) => { self.conv.local_models.downloading = false; match r { Ok(m) => { self.conv.local_models.downloaded = local_models::load_manifest().models; self.start_local_model(ctx, m); }, Err(e) => self.conv.local_models.runtime_status = Some(format!("Download failed: {e}")) } ctx.request_repaint(); }
                Ok(LocalModelMsg::RuntimeInstallProgress { downloaded, total }) => { self.conv.local_models.runtime_install_progress = Some((downloaded, total)); ctx.request_repaint(); }
                Ok(LocalModelMsg::RuntimeInstallDone(r)) => { self.conv.local_models.runtime_installing = false; match r { Ok(path) => { self.conv.local_models.runtime_path = path.clone(); self.conv.local_models.runtime_status = Some(format!("Runtime installed: {path}")); }, Err(e) => self.conv.local_models.runtime_status = Some(format!("Runtime install failed: {e}")) } ctx.request_repaint(); }
                Ok(LocalModelMsg::RemoteRuntimeInstallDone(r)) => { self.conv.local_models.runtime_installing = false; match r { Ok(path) => self.conv.local_models.runtime_status = Some(format!("Remote runtime installed: {path}")), Err(e) => self.conv.local_models.runtime_status = Some(format!("Remote runtime install failed: {e}")) } ctx.request_repaint(); }
                Ok(LocalModelMsg::RemoteDownloadDone(r)) => { self.conv.local_models.downloading = false; match r { Ok(m) => { self.upsert_remote_downloaded(m.clone()); self.start_remote_model(ctx, m); }, Err(e) => self.conv.local_models.runtime_status = Some(format!("Remote download failed: {e}")) } ctx.request_repaint(); }
                Ok(LocalModelMsg::RemoteStartDone { model, result }) => { match result { Ok(msg) => { self.conv.local_models.running_model_id = Some(model.id.clone()); self.conv.local_models.runtime_status = Some(msg); self.activate_local_model(&model); }, Err(e) => { self.conv.local_models.running_model_id = None; self.conv.local_models.runtime_status = Some(format!("Remote start failed: {e}")); } } ctx.request_repaint(); }
                Ok(LocalModelMsg::RemoteStopDone(r)) => { self.conv.local_models.running_model_id = None; self.conv.local_models.runtime_status = Some(match r { Ok(s) => format!("Remote runtime {s}"), Err(e) => format!("Remote stop failed: {e}") }); ctx.request_repaint(); }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => { keep = false; break; }
            }
        }
        if keep { self.conv.local_model_rx = Some(rx); }
    }

    fn activate_local_model(&mut self, m: &local_models::DownloadedModel) {
        let is_remote = matches!(self.conv.settings.provider(LlmProviderKind::LocalHf).location, ComputeLocation::RemoteSsh(_));
        let port = self.conv.local_models.runtime_port;
        let cfg = self.conv.settings.provider_mut(LlmProviderKind::LocalHf);
        cfg.model_id = m.id.clone();
        if is_remote {
            // Chat requests go through compute::resolve_base_url, which opens/reuses the SSH tunnel.
            cfg.base_url.clear();
        } else {
            cfg.base_url = format!("http://127.0.0.1:{port}/v1");
        }
        self.conv.settings.active_provider = LlmProviderKind::LocalHf;
        self.conv.fetched_models.entry(LlmProviderKind::LocalHf).or_default().models = self.conv.local_models.downloaded.iter().map(|m| m.id.clone()).collect();
        let _ = self.conv.settings.save();
    }

    fn upsert_remote_downloaded(&mut self, m: local_models::DownloadedModel) {
        self.conv.local_models.downloaded.retain(|x| x.id != m.id);
        self.conv.local_models.downloaded.push(m);
        self.conv.local_models.downloaded.sort_by(|a, b| a.id.cmp(&b.id));
    }

    fn start_local_model(&mut self, _ctx: &egui::Context, m: local_models::DownloadedModel) {
        self.stop_local_model();
        let port = self.conv.local_models.runtime_port;
        match local_models::spawn_llama_server(&self.conv.local_models.runtime_path, &m.path, port, self.conv.local_models.context_size, self.conv.local_models.gpu_layers) {
            Ok(mut child) => {
                // A process can spawn successfully and then immediately die because a bundled
                // dylib/.so is missing or the model is invalid. Give it a moment and report that
                // as failed instead of showing a false "Running" state.
                std::thread::sleep(std::time::Duration::from_millis(350));
                match child.try_wait() {
                    Ok(Some(status)) => {
                        self.conv.local_models.running_model_id = None;
                        self.conv.local_models.runtime_status = Some(format!("llama-server exited immediately ({status}). See log: {}", local_models::runtime_log_path().display()));
                    }
                    Ok(None) => {
                        self.conv.local_models.running_model_id = Some(m.id.clone());
                        self.conv.local_models.runtime_status = Some(format!("Starting {} on http://127.0.0.1:{port}/v1. If chat fails, wait a few seconds for model load. Log: {}", m.id, local_models::runtime_log_path().display()));
                        self.conv.local_runtime = Some(LocalRuntimeState { child, model_id: m.id.clone(), port });
                        self.activate_local_model(&m);
                    }
                    Err(e) => {
                        self.conv.local_models.running_model_id = None;
                        self.conv.local_models.runtime_status = Some(format!("Could not check llama-server status: {e}. Log: {}", local_models::runtime_log_path().display()));
                    }
                }
            }
            Err(e) => self.conv.local_models.runtime_status = Some(e),
        }
    }

    fn start_remote_model(&mut self, ctx: &egui::Context, m: local_models::DownloadedModel) {
        let Some((cfg, password)) = self.remote_ssh_cfg_and_password() else {
            self.conv.local_models.runtime_status = Some("Configure Remote SSH first.".into());
            return;
        };
        self.conv.local_models.running_model_id = None;
        self.conv.local_models.runtime_status = Some(format!("Starting remote {}…", m.id));
        let context = self.conv.local_models.context_size;
        let gpu_layers = self.conv.local_models.gpu_layers;
        let (tx, rx) = std::sync::mpsc::channel();
        self.conv.local_model_rx = Some(rx);
        let err_tx = tx.clone();
        let err_ctx = ctx.clone();
        let err_model = m.clone();
        let work_ctx = ctx.clone();
        spawn_async_task(move |err| { let _ = err_tx.send(LocalModelMsg::RemoteStartDone { model: err_model.clone(), result: Err(err) }); err_ctx.request_repaint(); }, move |rt| {
            let r = rt.block_on(crate::local_models_remote::start_model(&cfg, &password, &m.path, context, gpu_layers));
            let _ = tx.send(LocalModelMsg::RemoteStartDone { model: m, result: r });
            work_ctx.request_repaint();
        });
    }

    fn spawn_remote_stop(&mut self, ctx: &egui::Context) {
        let Some((cfg, password)) = self.remote_ssh_cfg_and_password() else { return; };
        let (tx, rx) = std::sync::mpsc::channel();
        self.conv.local_model_rx = Some(rx);
        let err_tx = tx.clone();
        let err_ctx = ctx.clone();
        let work_ctx = ctx.clone();
        spawn_async_task(move |err| { let _ = err_tx.send(LocalModelMsg::RemoteStopDone(Err(err))); err_ctx.request_repaint(); }, move |rt| {
            let r = rt.block_on(crate::local_models_remote::stop_model(&cfg, &password));
            let _ = tx.send(LocalModelMsg::RemoteStopDone(r));
            work_ctx.request_repaint();
        });
    }

    fn stop_local_model(&mut self) {
        if let Some(mut rt) = self.conv.local_runtime.take() {
            let _ = rt.child.kill();
            let _ = rt.child.wait();
            self.conv.local_models.runtime_status = Some(format!("Stopped {} on port {}", rt.model_id, rt.port));
        }
        self.conv.local_models.running_model_id = None;
    }
}

fn fmt_bytes(n: u64) -> String {
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    if n as f64 >= GB { format!("{:.2} GB", n as f64 / GB) }
    else if n as f64 >= MB { format!("{:.1} MB", n as f64 / MB) }
    else { format!("{} B", n) }
}
