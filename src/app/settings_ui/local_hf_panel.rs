//! Guided setup for the oxi-managed HuggingFace providers (Local HF / Remote HF).
//!
//! The page reads top to bottom as numbered steps — connect (remote only), install the
//! runtime, pick a model — so every precondition is visible and has exactly one action.
//! Tuning knobs live in a collapsed "Advanced" section.

use eframe::egui::{self, Align, Layout, RichText, Ui};

use crate::local_models;
use crate::settings::{ComputeLocation, LlmProviderKind, SshConfig};
use crate::theme::*;
use crate::ui::chrome::{
    alert_banner, card_frame, field_hint, field_label, field_label_first, ghost_button_icon,
    hairline, icon_glyph_rich, primary_button_icon_widget, settings_password_field,
    settings_text_field, settings_text_field_width,
};

use super::super::OxiApp;
use super::layout::active_pill;

/// GGUF quantization most people should start with: small, fast, good quality.
const RECOMMENDED_QUANT: &str = "Q4_K_M";

impl OxiApp {
    pub(super) fn render_managed_hf_setup(&mut self, ui: &mut Ui, kind: LlmProviderKind) {
        let is_remote = kind == LlmProviderKind::RemoteHf;
        let mut step = 1;

        if is_remote {
            if !matches!(
                self.conv.settings.provider(kind).location,
                ComputeLocation::RemoteSsh(_)
            ) {
                self.conv.settings.provider_mut(kind).location =
                    ComputeLocation::RemoteSsh(SshConfig {
                        remote_runtime_port: kind.default_remote_runtime_port(),
                        ..SshConfig::default()
                    });
            }
            let connected = self
                .conv
                .ssh_test
                .get(&kind)
                .is_some_and(|s| matches!(s.result, Some(Ok(_))));
            step_header(
                ui,
                step,
                "Connect to the host",
                connected.then_some("Connected"),
            );
            self.render_compute_target_section(ui, kind);
            step += 1;
            ui.add_space(16.0);
            self.ensure_remote_models_listed(ui.ctx());
        }

        self.render_hf_runtime_step(ui, kind, step);
        step += 1;
        ui.add_space(16.0);
        self.render_hf_models_step(ui, kind, step);
        ui.add_space(16.0);
        self.render_hf_advanced(ui, kind);
    }

    fn render_hf_runtime_step(&mut self, ui: &mut Ui, kind: LlmProviderKind, step: usize) {
        let is_remote = kind == LlmProviderKind::RemoteHf;
        let installed = !is_remote && runtime_installed(&self.conv.local_models.runtime_path);
        step_header(
            ui,
            step,
            "Install the runtime",
            installed.then_some("llama.cpp runtime installed"),
        );
        card_frame().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(if is_remote {
                        "oxi installs llama.cpp's llama-server on the SSH host. Run this once per host."
                    } else if installed {
                        "llama-server is ready. Reinstall only to update it."
                    } else {
                        "oxi downloads llama.cpp's llama-server, which serves the models below."
                    })
                    .size(FS_SMALL)
                    .color(c_text_muted()),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let installing = self.conv.local_models.runtime_installing;
                    let label = match (installing, installed, is_remote) {
                        (true, _, _) => "Installing…",
                        (false, true, _) => "Reinstall",
                        (false, false, true) => "Install on host",
                        (false, false, false) => "Install runtime",
                    };
                    let button = if installed {
                        crate::ui::chrome::ghost_button_widget(label, false)
                    } else {
                        primary_button_icon_widget(ICON_DOWNLOAD, label)
                    };
                    if ui.add_enabled(!installing, button).clicked() {
                        if is_remote {
                            self.spawn_remote_runtime_install(ui.ctx());
                        } else {
                            self.spawn_runtime_install(ui.ctx());
                        }
                    }
                });
            });
            if self.conv.local_models.runtime_installing {
                ui.add_space(8.0);
                progress(ui, self.conv.local_models.runtime_install_progress, "Downloading runtime");
            }
        });
    }

    fn render_hf_models_step(&mut self, ui: &mut Ui, kind: LlmProviderKind, step: usize) {
        let is_remote = kind == LlmProviderKind::RemoteHf;
        // Remote hosts get the runtime installed on demand; locally it must exist first.
        let runtime_ready = is_remote || runtime_installed(&self.conv.local_models.runtime_path);
        let running_id = if is_remote {
            self.conv.local_models.remote_running_model_id.clone()
        } else {
            self.conv.local_models.running_model_id.clone()
        };
        let done = running_id.as_ref().map(|id| format!("Running {id}"));
        step_header(ui, step, "Choose a model", done.as_deref());

        card_frame().show(ui, |ui| {
            // Long HuggingFace names must not widen the whole settings canvas.
            ui.set_min_width(0.0);
            ui.set_max_width(ui.available_width());

            let models = if is_remote {
                self.conv.local_models.remote_downloaded.clone()
            } else {
                self.conv.local_models.downloaded.clone()
            };
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(if is_remote {
                        "Models on the host"
                    } else {
                        "Your models"
                    })
                    .size(FS_SMALL)
                    .color(c_text())
                    .strong(),
                );
                if is_remote {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let loading = self.conv.local_models.remote_list_loading;
                        if ui
                            .add_enabled(
                                !loading,
                                crate::ui::chrome::ghost_button_widget(
                                    if loading { "Loading…" } else { "Refresh" },
                                    false,
                                ),
                            )
                            .on_hover_text("List the models on the SSH host again")
                            .clicked()
                        {
                            self.spawn_remote_list(ui.ctx());
                        }
                    });
                }
            });
            ui.add_space(6.0);

            if models.is_empty() {
                ui.label(
                    RichText::new("No models yet. Search HuggingFace below and download one.")
                        .size(FS_TINY)
                        .color(c_text_faint()),
                );
            }
            let n = models.len();
            for (i, m) in models.into_iter().enumerate() {
                let running = running_id.as_deref() == Some(m.id.as_str());
                model_row(ui, |ui| {
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&m.id).size(FS_SMALL).color(c_text()).strong(),
                                )
                                .truncate(),
                            );
                            if running {
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
                        if crate::ui::chrome::icon_button(ui, ICON_TRASH, 26.0, false)
                            .on_hover_text("Delete this model")
                            .clicked()
                        {
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
                        if running {
                            if ghost_button_icon(ui, ICON_STOP, "Stop", true).clicked() {
                                if is_remote {
                                    self.spawn_remote_stop(ui.ctx());
                                } else {
                                    self.stop_local_model();
                                }
                            }
                        } else if ui
                            .add_enabled(
                                runtime_ready,
                                primary_button_icon_widget(ICON_PLAY, "Run"),
                            )
                            .on_hover_text("Start this model and use it for new chats")
                            .on_disabled_hover_text("Install the runtime first")
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
                if i + 1 < n {
                    hairline(ui);
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

            ui.add_space(12.0);
            hairline(ui);
            ui.add_space(12.0);
            self.render_hf_search(ui, is_remote);
        });
    }

    /// Search HuggingFace; picking a result lists its GGUF files right under it.
    fn render_hf_search(&mut self, ui: &mut Ui, is_remote: bool) {
        ui.label(
            RichText::new("Add a model from HuggingFace")
                .size(FS_SMALL)
                .color(c_text())
                .strong(),
        );
        ui.add_space(6.0);
        let mut search = false;
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let busy = self.conv.local_models.search_loading;
                search |= ui
                    .add_enabled(
                        !busy,
                        primary_button_icon_widget(
                            ICON_SEARCH,
                            if busy { "Searching…" } else { "Search" },
                        ),
                    )
                    .clicked();
                let field = settings_text_field(
                    ui,
                    &mut self.conv.local_models.search_query,
                    "Model name, e.g. qwen coder, or an org/repo",
                );
                search |= field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            });
        });
        if search {
            let query = self.conv.local_models.search_query.trim().to_string();
            // An exact "org/repo" skips the search and lists its files directly.
            if query.contains('/') && !query.contains(' ') {
                self.conv.local_models.search_results.clear();
                self.spawn_hf_files(ui.ctx(), query);
            } else {
                self.spawn_hf_search(ui.ctx());
            }
        }

        for e in [
            self.conv.local_models.search_error.clone(),
            self.conv.local_models.files_error.clone(),
        ]
        .into_iter()
        .flatten()
        {
            ui.add_space(6.0);
            alert_banner(ui, &e, true);
        }

        let selected_repo = self.conv.local_models.selected_repo.clone();
        let mut hits: Vec<(String, Option<u64>, Option<u64>)> = self
            .conv
            .local_models
            .search_results
            .iter()
            .take(8)
            .map(|h| (h.model_id.clone(), h.downloads, h.likes))
            .collect();
        // A repo typed directly still gets a row so its files have somewhere to appear.
        if !selected_repo.is_empty() && hits.iter().all(|(id, ..)| *id != selected_repo) {
            hits.insert(0, (selected_repo.clone(), None, None));
        }
        if hits.is_empty() {
            return;
        }
        ui.add_space(8.0);
        for (repo, downloads, likes) in hits {
            let expanded = repo == selected_repo;
            let row = model_row(ui, |ui| {
                ui.vertical(|ui| {
                    ui.add(
                        egui::Label::new(
                            RichText::new(&repo).size(FS_SMALL).color(c_text()).strong(),
                        )
                        .truncate(),
                    );
                    if downloads.is_some() || likes.is_some() {
                        ui.horizontal(|ui| {
                            ui.label(icon_glyph_rich(ICON_DOWNLOAD, FS_TINY, c_text_faint()));
                            ui.label(
                                RichText::new(format_count(downloads.unwrap_or(0)))
                                    .size(FS_TINY)
                                    .color(c_text_faint()),
                            );
                            ui.add_space(8.0);
                            ui.label(icon_glyph_rich(ICON_HEART, FS_TINY, c_text_faint()));
                            ui.label(
                                RichText::new(format_count(likes.unwrap_or(0)))
                                    .size(FS_TINY)
                                    .color(c_text_faint()),
                            );
                        });
                    }
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(icon_glyph_rich(
                        if expanded {
                            ICON_ANGLE_UP
                        } else {
                            ICON_ANGLE_DOWN
                        },
                        FS_TINY,
                        c_text_muted(),
                    ));
                });
            });
            let row = row.interact(egui::Sense::click());
            if row.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if row.clicked() {
                if expanded {
                    self.conv.local_models.selected_repo.clear();
                    self.conv.local_models.gguf_files.clear();
                } else {
                    self.spawn_hf_files(ui.ctx(), repo.clone());
                }
            }
            if expanded {
                self.render_hf_files(ui, is_remote);
            }
            hairline(ui);
        }
    }

    fn render_hf_files(&mut self, ui: &mut Ui, is_remote: bool) {
        egui::Frame::new()
            .fill(c_bg_elevated_2())
            .corner_radius(egui::CornerRadius::same(RADIUS_CHIP))
            .inner_margin(egui::Margin::symmetric(10, 8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                if self.conv.local_models.files_loading {
                    ui.label(
                        RichText::new("Loading files…")
                            .size(FS_TINY)
                            .color(c_text_muted()),
                    );
                    return;
                }
                let files = self.conv.local_models.gguf_files.clone();
                if files.is_empty() {
                    ui.label(
                        RichText::new("This repository has no GGUF files.")
                            .size(FS_TINY)
                            .color(c_text_faint()),
                    );
                    return;
                }
                let downloading = self.conv.local_models.downloading;
                for f in files {
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::Label::new(
                                RichText::new(&f).size(FS_TINY).color(c_text()).monospace(),
                            )
                            .truncate(),
                        );
                        if f.contains(RECOMMENDED_QUANT) {
                            active_pill(ui, "Recommended");
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui
                                .add_enabled(
                                    !downloading,
                                    crate::ui::chrome::ghost_button_widget("Download", false),
                                )
                                .clicked()
                            {
                                self.conv.local_models.selected_file = f.clone();
                                if is_remote {
                                    self.spawn_remote_hf_download(ui.ctx());
                                } else {
                                    self.spawn_hf_download(ui.ctx());
                                }
                            }
                        });
                    });
                }
                if downloading {
                    ui.add_space(6.0);
                    let label = self.conv.local_models.download_label.clone();
                    progress(
                        ui,
                        self.conv.local_models.download_progress,
                        if label.is_empty() {
                            "Downloading"
                        } else {
                            &label
                        },
                    );
                }
            });
    }

    fn render_hf_advanced(&mut self, ui: &mut Ui, kind: LlmProviderKind) {
        let is_remote = kind == LlmProviderKind::RemoteHf;
        egui::CollapsingHeader::new(
            RichText::new("Advanced")
                .size(FS_SMALL)
                .color(c_text_muted())
                .strong(),
        )
        .id_salt(("hf_advanced", kind.slug()))
        .default_open(false)
        .show(ui, |ui| {
            card_frame().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 16.0;
                    ui.vertical(|ui| {
                        field_label_first(ui, "Port");
                        let configured = if is_remote {
                            self.conv
                                .settings
                                .provider(kind)
                                .ssh_config()
                                .map(|cfg| cfg.remote_runtime_port)
                                .unwrap_or_else(|| kind.default_remote_runtime_port())
                        } else {
                            self.conv.local_models.runtime_port
                        };
                        let mut port = configured.to_string();
                        let hint = if is_remote { "18081" } else { "18080" };
                        if settings_text_field_width(ui, &mut port, hint, 90.0).changed()
                            && let Ok(p) = port.parse::<u16>()
                        {
                            if is_remote {
                                if let ComputeLocation::RemoteSsh(cfg) =
                                    &mut self.conv.settings.provider_mut(kind).location
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
                    ui.vertical(|ui| {
                        field_label_first(ui, "Context (tokens)");
                        let mut ctx = self.conv.local_models.context_size.to_string();
                        if settings_text_field_width(ui, &mut ctx, "8192", 100.0).changed()
                            && let Ok(n) = ctx.parse::<usize>()
                        {
                            let n = n.max(512);
                            self.conv.local_models.context_size = n;
                            self.conv.settings.local_hf.context_size = n;
                            // One number for both sides: the server's context and what oxi
                            // budgets the conversation against.
                            for hf in [LlmProviderKind::LocalHf, LlmProviderKind::RemoteHf] {
                                self.conv.settings.provider_mut(hf).context_window = Some(n);
                            }
                        }
                    });
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
                field_hint(
                    ui,
                    "Changes apply the next time a model starts. GPU layers -1 offloads every layer when supported.",
                );
                if !is_remote {
                    field_label(ui, "llama-server path");
                    settings_text_field(
                        ui,
                        &mut self.conv.local_models.runtime_path,
                        "Empty uses the installed runtime, then PATH",
                    );
                }
                field_label(ui, "API key");
                settings_password_field(
                    ui,
                    &mut self.conv.settings.provider_mut(kind).api_key,
                    "Only if your llama-server was started with --api-key",
                );
            });
        });
    }
}

fn runtime_installed(path_override: &str) -> bool {
    !path_override.trim().is_empty() || local_models::installed_runtime_path().is_some()
}

/// Numbered step title with a check and short status once the step is complete.
fn step_header(ui: &mut Ui, step: usize, title: &str, done: Option<&str>) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let d = 20.0;
        let (rect, _) = ui.allocate_exact_size(egui::vec2(d, d), egui::Sense::hover());
        let complete = done.is_some();
        ui.painter().circle(
            rect.center(),
            d * 0.5,
            if complete {
                c_success()
            } else {
                c_bg_elevated_2()
            },
            egui::Stroke::new(1.0, if complete { c_success() } else { c_border() }),
        );
        if complete {
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                ICON_CHECK,
                egui::FontId::new(FS_TINY, icon_font()),
                c_bg_main(),
            );
        } else {
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                step.to_string(),
                egui::FontId::proportional(FS_TINY),
                c_text_muted(),
            );
        }
        ui.label(
            RichText::new(title)
                .size(FS_BODY)
                .color(c_text_strong())
                .strong(),
        );
        if let Some(done) = done {
            ui.label(RichText::new(done).size(FS_TINY).color(c_success()));
        }
    });
    ui.add_space(8.0);
}

/// One list row with a stable height; returns the row's response so callers can make it clickable.
fn model_row(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui)) -> egui::Response {
    let available = ui.available_width();
    ui.horizontal(|ui| {
        ui.set_min_width(available);
        ui.set_min_height(40.0);
        add_contents(ui);
    })
    .response
}

fn progress(ui: &mut Ui, progress: Option<(u64, Option<u64>)>, label: &str) {
    let (fraction, text) = match progress {
        Some((done, Some(total))) if total > 0 => (
            Some(done as f32 / total as f32),
            format!("{label}… {} / {}", fmt_bytes(done), fmt_bytes(total)),
        ),
        Some((done, _)) => (None, format!("{label}… {}", fmt_bytes(done))),
        None => (None, format!("{label}…")),
    };
    let bar = match fraction {
        Some(f) => egui::ProgressBar::new(f).show_percentage(),
        None => egui::ProgressBar::new(0.0).animate(true),
    };
    ui.add(bar.desired_height(6.0).corner_radius(3.0));
    ui.label(RichText::new(text).size(FS_TINY).color(c_text_muted()));
}

fn format_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}
