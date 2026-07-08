//! Settings → Voice: local speech-to-text dictation (see [`crate::voice_engine`],
//! [`crate::voice_models`]). Unlike the local-LLM HF panel there's a small fixed model
//! catalog instead of a HuggingFace search UI, since there's one canonical upstream repo.

use eframe::egui::{self, RichText, TextEdit, Ui};

use crate::app::task_runner::spawn_async_task;
use crate::theme::*;
use crate::ui::chrome::{
    card_frame, field_label, ghost_button, settings_caption, settings_section_title,
};
use crate::voice_models::{self, VoiceModelCatalogEntry, VoiceModelMsg, VOICE_MODEL_CATALOG};

use super::super::OxiApp;

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

        let mut enabled = self.conv.settings.dictation.enabled;
        if ui
            .checkbox(
                &mut enabled,
                RichText::new("Enable dictation").size(FS_SMALL).color(c_text()),
            )
            .on_hover_text(
                "Shows the mic button in the composer. No model is loaded until you actually dictate.",
            )
            .changed()
        {
            self.conv.settings.dictation.enabled = enabled;
        }
        ui.add_space(10.0);

        card_frame().show(ui, |ui| {
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
                        "Off (default): the model is unloaded from memory right after each \
                         transcription. On: it stays warm in memory for faster repeat use.",
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
            ui.add_space(8.0);
            field_label(ui, "Language (\"en\", \"ro\", or \"auto\" to detect)");
            let mut language = self.conv.settings.dictation.language.clone();
            if ui
                .add(TextEdit::singleline(&mut language).desired_width(120.0))
                .changed()
            {
                self.conv.settings.dictation.language = language;
            }
        });

        ui.add_space(14.0);
        settings_caption(ui, "Model");
        ui.label(
            RichText::new(
                "Download a model, then make it active. Larger models are more accurate but \
                 slower to load and transcribe.",
            )
            .size(FS_TINY)
            .color(c_text_muted()),
        );
        ui.add_space(6.0);

        for entry in VOICE_MODEL_CATALOG {
            let downloaded = self
                .conv
                .voice_ui
                .downloaded
                .iter()
                .find(|m| m.id == entry.id)
                .cloned();
            let is_active = self.conv.settings.dictation.model_id.as_deref() == Some(entry.id);
            let downloading = self.conv.voice_ui.downloading_id.as_deref() == Some(entry.id);
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(entry.label).size(FS_SMALL).color(c_text()));
                ui.label(
                    RichText::new(format!("~{} MB", entry.approx_mb))
                        .size(FS_TINY)
                        .color(c_text_faint()),
                );
                if let Some(m) = &downloaded {
                    if is_active {
                        super::layout::active_pill(ui, "Active");
                    } else if ghost_button(ui, "Make active", false).clicked() {
                        self.conv.settings.dictation.model_id = Some(m.id.clone());
                    }
                    if ghost_button(ui, "Delete", true).clicked() {
                        let _ = voice_models::remove_downloaded(&m.id);
                        self.conv.voice_ui.downloaded = voice_models::load_manifest().models;
                        if is_active {
                            self.conv.settings.dictation.model_id = None;
                        }
                    }
                } else if downloading {
                    let text = match self.conv.voice_ui.download_progress {
                        Some((done, Some(total))) if total > 0 => format!(
                            "Downloading… {:.0}% ({}/{})",
                            done as f64 * 100.0 / total as f64,
                            fmt_bytes(done),
                            fmt_bytes(total)
                        ),
                        Some((done, _)) => format!("Downloading… {}", fmt_bytes(done)),
                        None => "Downloading…".to_string(),
                    };
                    ui.label(RichText::new(text).size(FS_TINY).color(c_text_muted()));
                } else if ui
                    .add_enabled(
                        self.conv.voice_ui.downloading_id.is_none(),
                        crate::ui::chrome::primary_button_widget("Download"),
                    )
                    .clicked()
                {
                    self.spawn_voice_download(ui.ctx(), entry);
                }
            });
            ui.add_space(4.0);
        }
        if let Some(e) = &self.conv.voice_ui.download_error {
            ui.label(RichText::new(e).size(FS_TINY).color(c_danger()));
        }
        if let Some(e) = &self.conv.voice_ui.error {
            ui.add_space(8.0);
            ui.label(
                RichText::new(format!("Dictation error: {e}"))
                    .size(FS_TINY)
                    .color(c_danger()),
            );
        }
    }

    fn spawn_voice_download(&mut self, ctx: &egui::Context, entry: &'static VoiceModelCatalogEntry) {
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
