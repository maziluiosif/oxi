//! Settings → Voice: local speech-to-text dictation (see [`crate::voice_engine`],
//! [`crate::voice_models`]). Unlike the local-LLM HF panel there's a small fixed model
//! catalog instead of a HuggingFace search UI, since there's one canonical upstream repo.

use eframe::egui::{self, Align, Layout, RichText, Ui};

use crate::app::task_runner::spawn_async_task;
use crate::theme::*;
use crate::ui::chrome::{
    alert_banner, card_frame, field_label, ghost_button, ghost_button_icon,
    primary_button_icon_widget, settings_card_header, settings_list_row, settings_section_title,
    settings_text_field_width,
};
use crate::voice_models::{self, VOICE_MODEL_CATALOG, VoiceModelCatalogEntry, VoiceModelMsg};

use super::super::OxiApp;
use super::layout::active_pill;

impl OxiApp {
    pub(super) fn render_settings_voice_panel(&mut self, ui: &mut Ui) {
        settings_section_title(
            ui,
            "Voice dictation",
            Some(
                "Transcribe speech locally with a downloaded Whisper model — audio never \
                 leaves this machine.",
            ),
        );

        card_frame().show(ui, |ui| {
            settings_card_header(
                ui,
                "Dictation",
                Some("Shows the mic button in the composer. No model is loaded until you dictate."),
            );
            let mut enabled = self.conv.settings.dictation.enabled;
            if ui
                .checkbox(
                    &mut enabled,
                    RichText::new("Enable dictation")
                        .size(FS_SMALL)
                        .color(c_text()),
                )
                .changed()
            {
                self.conv.settings.dictation.enabled = enabled;
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let mut keep_loaded = self.conv.settings.dictation.keep_loaded;
                if ui
                    .checkbox(
                        &mut keep_loaded,
                        RichText::new("Keep model loaded after use")
                            .size(FS_SMALL)
                            .color(c_text()),
                    )
                    .on_hover_text(
                        "Off (default): unload after each transcription. On: keep warm in memory.",
                    )
                    .changed()
                {
                    self.conv.settings.dictation.keep_loaded = keep_loaded;
                }
                if ghost_button(ui, "Unload model now", false)
                    .on_hover_text("Free the whisper model from memory immediately.")
                    .clicked()
                {
                    self.voice.unload();
                }
            });
            field_label(ui, "Language (\"en\", \"ro\", or \"auto\" to detect)");
            let mut language = self.conv.settings.dictation.language.clone();
            if settings_text_field_width(ui, &mut language, "auto", 140.0).changed() {
                self.conv.settings.dictation.language = language;
            }
        });

        ui.add_space(14.0);
        card_frame().show(ui, |ui| {
            settings_card_header(
                ui,
                "Whisper model",
                Some("Download a model, then make it active. Larger = more accurate, slower."),
            );

            let n = VOICE_MODEL_CATALOG.len();
            for (i, entry) in VOICE_MODEL_CATALOG.iter().enumerate() {
                let downloaded = self
                    .conv
                    .voice_ui
                    .downloaded
                    .iter()
                    .find(|m| m.id == entry.id)
                    .cloned();
                let is_active = self.conv.settings.dictation.model_id.as_deref() == Some(entry.id);
                let downloading = self.conv.voice_ui.downloading_id.as_deref() == Some(entry.id);

                settings_list_row(ui, i + 1 < n, |ui| {
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(entry.label)
                                    .size(FS_SMALL)
                                    .color(c_text())
                                    .strong(),
                            );
                            if is_active {
                                ui.add_space(6.0);
                                active_pill(ui, "Active");
                            }
                        });
                        ui.label(
                            RichText::new(format!("~{} MB", entry.approx_mb))
                                .size(FS_TINY)
                                .color(c_text_faint()),
                        );
                    });

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if let Some(m) = &downloaded {
                            if ghost_button_icon(ui, ICON_TRASH, "Delete", true).clicked() {
                                let _ = voice_models::remove_downloaded(&m.id);
                                self.conv.voice_ui.downloaded =
                                    voice_models::load_manifest().models;
                                if is_active {
                                    self.conv.settings.dictation.model_id = None;
                                }
                            }
                            if !is_active && ghost_button(ui, "Make active", false).clicked() {
                                self.conv.settings.dictation.model_id = Some(m.id.clone());
                            }
                        } else if downloading {
                            let text = match self.conv.voice_ui.download_progress {
                                Some((done, Some(total))) if total > 0 => format!(
                                    "{:.0}% ({}/{})",
                                    done as f64 * 100.0 / total as f64,
                                    fmt_bytes(done),
                                    fmt_bytes(total)
                                ),
                                Some((done, _)) => fmt_bytes(done),
                                None => "…".to_string(),
                            };
                            ui.label(RichText::new(text).size(FS_TINY).color(c_text_muted()));
                        } else if ui
                            .add_enabled(
                                self.conv.voice_ui.downloading_id.is_none(),
                                primary_button_icon_widget(ICON_DOWNLOAD, "Download"),
                            )
                            .clicked()
                        {
                            self.spawn_voice_download(ui.ctx(), entry);
                        }
                    });
                });
            }

            if let Some(e) = self.conv.voice_ui.download_error.clone() {
                ui.add_space(6.0);
                alert_banner(ui, &e, true);
            }
            if let Some(e) = self.conv.voice_ui.error.clone() {
                ui.add_space(8.0);
                alert_banner(ui, &format!("Dictation error: {e}"), true);
            }
        });
    }

    fn spawn_voice_download(
        &mut self,
        ctx: &egui::Context,
        entry: &'static VoiceModelCatalogEntry,
    ) {
        self.conv.voice_ui.downloading_id = Some(entry.id.to_string());
        self.conv.voice_ui.download_progress = None;
        self.conv.voice_ui.download_error = None;
        let (tx, rx) = std::sync::mpsc::channel();
        self.conv.voice_model_rx = Some(rx);
        let err_tx = tx.clone();
        let err_ctx = ctx.clone();
        let work_ctx = ctx.clone();
        spawn_async_task(
            move |err| {
                let _ = err_tx.send(VoiceModelMsg::DownloadDone(Err(err)));
                err_ctx.request_repaint();
            },
            move |rt| {
                let client = reqwest::Client::new();
                let r = rt.block_on(voice_models::download_model(&client, entry, tx.clone()));
                let _ = tx.send(VoiceModelMsg::DownloadDone(r));
                work_ctx.request_repaint();
            },
        );
    }

    /// Drain in-flight voice-model download progress, called once per frame.
    pub(crate) fn drain_voice_models(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.conv.voice_model_rx.take() else {
            return;
        };
        let mut keep = true;
        loop {
            match rx.try_recv() {
                Ok(VoiceModelMsg::DownloadProgress {
                    id,
                    downloaded,
                    total,
                }) => {
                    self.conv.voice_ui.downloading_id = Some(id);
                    self.conv.voice_ui.download_progress = Some((downloaded, total));
                    ctx.request_repaint();
                }
                Ok(VoiceModelMsg::DownloadDone(r)) => {
                    self.conv.voice_ui.downloading_id = None;
                    match r {
                        Ok(m) => {
                            self.conv.voice_ui.downloaded = voice_models::load_manifest().models;
                            if self.conv.settings.dictation.model_id.is_none() {
                                self.conv.settings.dictation.model_id = Some(m.id);
                            }
                        }
                        Err(e) => self.conv.voice_ui.download_error = Some(e),
                    }
                    ctx.request_repaint();
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    keep = false;
                    break;
                }
            }
        }
        if keep {
            self.conv.voice_model_rx = Some(rx);
        }
    }
}

fn fmt_bytes(n: u64) -> String {
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    if n as f64 >= GB {
        format!("{:.2} GB", n as f64 / GB)
    } else if n as f64 >= MB {
        format!("{:.1} MB", n as f64 / MB)
    } else {
        format!("{n} B")
    }
}
