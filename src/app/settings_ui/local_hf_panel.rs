use eframe::egui::{self, Align, Layout, RichText, Ui};

use crate::local_models;
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
}
