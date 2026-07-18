use eframe::egui::{self, Align, Layout, RichText, Ui};

use crate::app::LocalRuntimeState;
use crate::app::task_runner::spawn_async_task;
use crate::local_models::{self, LocalModelMsg};
use crate::settings::{ComputeLocation, LlmProviderKind};
use crate::theme::*;
use crate::ui::chrome::{
    alert_banner, card_frame, field_hint, field_label, field_label_first, ghost_button,
    ghost_button_icon, hairline, icon_glyph_rich, primary_button_icon_widget, settings_card_header,
    settings_list_row, settings_text_field, settings_text_field_width,
};

use super::super::OxiApp;
use super::layout::active_pill;

impl OxiApp {
    pub(super) fn render_local_hf_section(&mut self, ui: &mut Ui, kind: LlmProviderKind) {
        let is_remote = kind == LlmProviderKind::RemoteHf;

        if is_remote {
            self.ensure_remote_models_listed(ui.ctx());
        }

        // ── Runtime ────────────────────────────────────────────────────────
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let runtime_ok = is_remote
                || !self.conv.local_models.runtime_path.trim().is_empty()
                || local_models::installed_runtime_path().is_some();
            let has_model = if is_remote {
                !self.conv.local_models.remote_downloaded.is_empty()
            } else {
                !self.conv.local_models.downloaded.is_empty()
            };
            let running = if is_remote {
                self.conv.local_models.remote_running_model_id.is_some()
            } else {
                self.conv.local_models.running_model_id.is_some()
            };
            let steps = [
                ("1 Runtime", runtime_ok),
                ("2 Model", has_model),
                ("3 Running", running),
            ];
            for (i, (label, done)) in steps.iter().enumerate() {
                if i > 0 {
                    ui.label(RichText::new("→").size(FS_TINY).color(c_text_faint()));
                }
                ui.label(
                    RichText::new(*label)
                        .size(FS_TINY)
                        .color(if *done { c_accent() } else { c_text_muted() })
                        .strong(),
                );
            }
        });
        ui.add_space(8.0);
        card_frame().show(ui, |ui| {
            settings_card_header(
                ui,
                if is_remote {
                    "Remote runtime"
                } else {
                    "Local runtime"
                },
                Some(if is_remote {
                    "Install llama-server on the SSH host, then tune port / context / GPU layers."
                } else {
                    "Install llama-server locally, then tune port / context / GPU layers."
                }),
            );

            ui.horizontal(|ui| {
                let installed = local_models::installed_runtime_path();
                let (label, ok) = if is_remote {
                    ("Runtime managed on SSH host", true)
                } else if installed.is_some() {
                    ("Runtime installed", true)
                } else {
                    ("Runtime not installed", false)
                };
                ui.label(RichText::new(label).size(FS_SMALL).color(if ok {
                    c_success()
                } else {
                    c_text_muted()
                }));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let installing = self.conv.local_models.runtime_installing;
                    let button = if is_remote {
                        "Install on SSH"
                    } else {
                        "Install runtime"
                    };
                    if ui
                        .add_enabled(
                            !installing,
                            primary_button_icon_widget(
                                ICON_DOWNLOAD,
                                if installing { "Installing…" } else { button },
                            ),
                        )
                        .clicked()
                    {
                        if is_remote {
                            self.spawn_remote_runtime_install(ui.ctx());
                        } else {
                            self.spawn_runtime_install(ui.ctx());
                        }
                    }
                });
            });

            if self.conv.local_models.runtime_installing {
                ui.add_space(6.0);
                let text = match self.conv.local_models.runtime_install_progress {
                    Some((done, Some(total))) if total > 0 => format!(
                        "Downloading runtime… {:.1}% ({}/{})",
                        done as f64 * 100.0 / total as f64,
                        fmt_bytes(done),
                        fmt_bytes(total)
                    ),
                    Some((done, _)) => format!("Downloading runtime… {}", fmt_bytes(done)),
                    None => "Downloading runtime…".to_string(),
                };
                ui.label(RichText::new(text).size(FS_TINY).color(c_text_muted()));
            }

            if !is_remote {
                field_label(ui, "llama-server path (optional override)");
                settings_text_field(
                    ui,
                    &mut self.conv.local_models.runtime_path,
                    "empty = bundled runtime, then PATH fallback",
                );
            }

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    field_label_first(ui, "Port");
                    let configured_port = if is_remote {
                        self.conv
                            .settings
                            .provider(LlmProviderKind::RemoteHf)
                            .ssh_config()
                            .map(|cfg| cfg.remote_runtime_port)
                            .unwrap_or_else(|| {
                                LlmProviderKind::RemoteHf.default_remote_runtime_port()
                            })
                    } else {
                        self.conv.local_models.runtime_port
                    };
                    let mut port = configured_port.to_string();
                    let port_hint = if is_remote { "18081" } else { "18080" };
                    if settings_text_field_width(ui, &mut port, port_hint, 90.0).changed()
                        && let Ok(p) = port.parse::<u16>()
                    {
                        if is_remote {
                            if let ComputeLocation::RemoteSsh(cfg) = &mut self
                                .conv
                                .settings
                                .provider_mut(LlmProviderKind::RemoteHf)
                                .location
                            {
                                cfg.remote_runtime_port = p;
                                self.conv.local_models.remote_list_for = None;
                            }
                        } else {
                            self.conv.local_models.runtime_port = p;
                            self.conv.settings.local_hf.runtime_port = p;
                        }
                    }
                });
                ui.add_space(12.0);
                ui.vertical(|ui| {
                    field_label_first(ui, "Context");
                    let mut ctx = self.conv.local_models.context_size.to_string();
                    if settings_text_field_width(ui, &mut ctx, "8192", 100.0).changed()
                        && let Ok(n) = ctx.parse::<usize>()
                    {
                        let n = n.max(512);
                        self.conv.local_models.context_size = n;
                        self.conv.settings.local_hf.context_size = n;
                    }
                });
                ui.add_space(12.0);
                ui.vertical(|ui| {
                    field_label_first(ui, "GPU layers");
                    let mut ngl = self.conv.local_models.gpu_layers.to_string();
                    if settings_text_field_width(ui, &mut ngl, "-1", 80.0).changed()
                        && let Ok(n) = ngl.parse::<i32>()
                    {
                        self.conv.local_models.gpu_layers = n;
                        self.conv.settings.local_hf.gpu_layers = n;
                    }
                });
            });
            field_hint(ui, "GPU layers: -1 = all layers on GPU when supported.");
        });

        // ── Search & download ──────────────────────────────────────────────
        ui.add_space(12.0);
        card_frame().show(ui, |ui| {
            // Keep long HuggingFace repo/file names from increasing this card's minimum
            // width (and therefore the width of the entire settings canvas).
            ui.set_min_width(0.0);
            ui.set_max_width(ui.available_width());
            settings_card_header(
                ui,
                "Search & download",
                Some(if is_remote {
                    "Find a GGUF on HuggingFace, then download it onto the SSH host."
                } else {
                    "Find a GGUF on HuggingFace, then download it into oxi."
                }),
            );

            field_label_first(ui, "Search HuggingFace");
            // Whole row laid out right-to-left: button pinned to the right edge, field
            // fills the rest flush-left (same idiom as the provider model row).
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let busy = self.conv.local_models.search_loading;
                    let search_clicked = ui
                        .add_enabled(
                            !busy,
                            primary_button_icon_widget(
                                ICON_SEARCH,
                                if busy { "Searching…" } else { "Search" },
                            ),
                        )
                        .clicked();
                    settings_text_field(
                        ui,
                        &mut self.conv.local_models.search_query,
                        "e.g. qwen coder gguf",
                    );
                    if search_clicked {
                        self.spawn_hf_search(ui.ctx());
                    }
                });
            });

            if let Some(e) = self.conv.local_models.search_error.clone() {
                ui.add_space(6.0);
                alert_banner(ui, &e, true);
            }

            let hits: Vec<_> = self
                .conv
                .local_models
                .search_results
                .iter()
                .take(8)
                .cloned()
                .collect();
            if !hits.is_empty() {
                ui.add_space(8.0);
                let n = hits.len();
                for (i, hit) in hits.into_iter().enumerate() {
                    settings_list_row(ui, i + 1 < n, |ui| {
                        ui.vertical(|ui| {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&hit.model_id)
                                        .size(FS_SMALL)
                                        .color(c_text())
                                        .strong(),
                                )
                                .wrap(),
                            );
                            ui.horizontal(|ui| {
                                ui.label(icon_glyph_rich(ICON_DOWNLOAD, FS_TINY, c_text_faint()));
                                ui.label(
                                    RichText::new(format!("{}", hit.downloads.unwrap_or(0)))
                                        .size(FS_TINY)
                                        .color(c_text_faint()),
                                );
                                ui.add_space(8.0);
                                ui.label(icon_glyph_rich(ICON_HEART, FS_TINY, c_text_faint()));
                                ui.label(
                                    RichText::new(format!("{}", hit.likes.unwrap_or(0)))
                                        .size(FS_TINY)
                                        .color(c_text_faint()),
                                );
                            });
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ghost_button(ui, "Select", false).clicked() {
                                self.conv.local_models.selected_repo = hit.model_id.clone();
                                self.spawn_hf_files(ui.ctx(), hit.model_id);
                            }
                        });
                    });
                }
            }

            ui.add_space(8.0);
            hairline(ui);
            ui.add_space(8.0);

            field_label_first(ui, "Repo / GGUF file");
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let load_clicked = ui
                        .add_enabled(
                            !self.conv.local_models.files_loading,
                            crate::ui::chrome::ghost_button_widget("Load files", false),
                        )
                        .clicked();
                    settings_text_field(
                        ui,
                        &mut self.conv.local_models.selected_repo,
                        "org/model-GGUF",
                    );
                    if load_clicked {
                        let repo = self.conv.local_models.selected_repo.clone();
                        self.spawn_hf_files(ui.ctx(), repo);
                    }
                });
            });

            if let Some(e) = self.conv.local_models.files_error.clone() {
                ui.add_space(6.0);
                alert_banner(ui, &e, true);
            }

            if !self.conv.local_models.gguf_files.is_empty() {
                ui.add_space(6.0);
                let current = if self.conv.local_models.selected_file.is_empty() {
                    "choose .gguf".to_string()
                } else {
                    self.conv.local_models.selected_file.clone()
                };
                egui::ComboBox::from_id_salt("local_hf_file_combo")
                    .selected_text(current)
                    .width(ui.available_width())
                    .show_ui(ui, |ui| {
                        for f in self.conv.local_models.gguf_files.clone() {
                            if ui
                                .selectable_label(self.conv.local_models.selected_file == f, &f)
                                .clicked()
                            {
                                self.conv.local_models.selected_file = f;
                            }
                        }
                    });
                ui.add_space(8.0);
                if ui
                    .add_enabled(
                        !self.conv.local_models.downloading
                            && !self.conv.local_models.selected_file.is_empty(),
                        primary_button_icon_widget(ICON_DOWNLOAD, "Download"),
                    )
                    .clicked()
                {
                    if is_remote {
                        self.spawn_remote_hf_download(ui.ctx());
                    } else {
                        self.spawn_hf_download(ui.ctx());
                    }
                }
            }

            if self.conv.local_models.downloading {
                ui.add_space(6.0);
                let mut text = self.conv.local_models.download_label.clone();
                if let Some((done, total)) = self.conv.local_models.download_progress {
                    text = match total {
                        Some(t) if t > 0 => format!(
                            "Downloading… {:.1}% ({}/{})",
                            done as f64 * 100.0 / t as f64,
                            fmt_bytes(done),
                            fmt_bytes(t)
                        ),
                        _ => format!("Downloading… {}", fmt_bytes(done)),
                    };
                }
                ui.label(RichText::new(text).size(FS_TINY).color(c_text_muted()));
            }
        });

        // ── Downloaded models ──────────────────────────────────────────────
        ui.add_space(12.0);
        card_frame().show(ui, |ui| {
            settings_card_header(
                ui,
                "Downloaded models",
                Some(if is_remote {
                    "Models on the SSH host. Play starts llama-server there. \
                     Make active points chat at that model."
                } else {
                    "Play starts llama-server. Make active points chat at that model."
                }),
            );

            if is_remote {
                ui.horizontal(|ui| {
                    let loading = self.conv.local_models.remote_list_loading;
                    if loading {
                        ui.label(
                            RichText::new("Loading models from SSH host…")
                                .size(FS_TINY)
                                .color(c_text_muted()),
                        );
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add_enabled(
                                !loading,
                                crate::ui::chrome::ghost_button_widget("Refresh", false),
                            )
                            .on_hover_text("Re-list models on the SSH host")
                            .clicked()
                        {
                            self.spawn_remote_list(ui.ctx());
                        }
                    });
                });
                ui.add_space(4.0);
            }

            let models = if is_remote {
                self.conv.local_models.remote_downloaded.clone()
            } else {
                self.conv.local_models.downloaded.clone()
            };
            if models.is_empty() {
                ui.label(
                    RichText::new(if is_remote {
                        "No models on the SSH host yet."
                    } else {
                        "No models downloaded yet."
                    })
                    .size(FS_TINY)
                    .color(c_text_faint()),
                );
            } else {
                let n = models.len();
                for (i, m) in models.into_iter().enumerate() {
                    let running_id = if is_remote {
                        &self.conv.local_models.remote_running_model_id
                    } else {
                        &self.conv.local_models.running_model_id
                    };
                    let running = running_id.as_deref() == Some(&m.id);
                    settings_list_row(ui, i + 1 < n, |ui| {
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(&m.id).size(FS_SMALL).color(c_text()).strong(),
                                );
                                if running {
                                    ui.add_space(6.0);
                                    active_pill(ui, "Running");
                                }
                            });
                            ui.label(
                                RichText::new(fmt_bytes(m.bytes))
                                    .size(FS_TINY)
                                    .color(c_text_faint()),
                            );
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ghost_button_icon(ui, ICON_TRASH, "Delete", true).clicked() {
                                self.request_confirm(if is_remote {
                                    crate::app::state::ConfirmAction::DeleteRemoteModel {
                                        id: m.id.clone(),
                                        path: m.path.clone(),
                                    }
                                } else {
                                    crate::app::state::ConfirmAction::DeleteLocalModel {
                                        id: m.id.clone(),
                                    }
                                });
                            }
                            if ghost_button(ui, "Make active", false).clicked() {
                                self.activate_hf_model(&m, kind);
                            }
                            if running {
                                if ghost_button_icon(ui, ICON_STOP, "Stop", true).clicked() {
                                    if is_remote {
                                        self.spawn_remote_stop(ui.ctx());
                                    } else {
                                        self.stop_local_model();
                                    }
                                }
                            } else if ui
                                .add(primary_button_icon_widget(ICON_PLAY, "Play"))
                                .on_hover_text("Start llama-server with this model")
                                .clicked()
                            {
                                if is_remote {
                                    self.start_remote_model(ui.ctx(), m.clone());
                                } else {
                                    self.start_local_model(ui.ctx(), m.clone());
                                }
                            }
                        });
                    });
                }
            }

            let runtime_status = if is_remote {
                self.conv.local_models.remote_runtime_status.clone()
            } else {
                self.conv.local_models.runtime_status.clone()
            };
            if let Some(s) = runtime_status {
                ui.add_space(8.0);
                alert_banner(
                    ui,
                    &s,
                    s.contains("failed") || s.contains("exited") || s.contains("Could not"),
                );
            }
        });
    }

    fn spawn_runtime_install(&mut self, ctx: &egui::Context) {
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

    fn spawn_remote_runtime_install(&mut self, ctx: &egui::Context) {
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

    fn spawn_hf_search(&mut self, ctx: &egui::Context) {
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

    fn spawn_hf_files(&mut self, ctx: &egui::Context, repo: String) {
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
    fn ensure_remote_models_listed(&mut self, ctx: &egui::Context) {
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

    fn spawn_remote_hf_download(&mut self, ctx: &egui::Context) {
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

    fn spawn_hf_download(&mut self, ctx: &egui::Context) {
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

    fn activate_hf_model(&mut self, m: &local_models::DownloadedModel, kind: LlmProviderKind) {
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

    fn upsert_remote_downloaded(&mut self, m: local_models::DownloadedModel) {
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

    fn start_local_model(&mut self, _ctx: &egui::Context, m: local_models::DownloadedModel) {
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

    fn start_remote_model(&mut self, ctx: &egui::Context, m: local_models::DownloadedModel) {
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

    fn spawn_remote_stop(&mut self, ctx: &egui::Context) {
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

    fn stop_local_model(&mut self) {
        if let Some(mut rt) = self.conv.local_runtime.take() {
            let _ = rt.child.kill();
            let _ = rt.child.wait();
            self.conv.local_models.runtime_status =
                Some(format!("Stopped {} on port {}", rt.model_id, rt.port));
        }
        self.conv.local_models.running_model_id = None;
    }
}
