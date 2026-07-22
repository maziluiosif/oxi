//! Voice dictation and context-usage controls.

use super::*;

impl OxiApp {
    /// Round mic button: idle → click starts recording (lazy-loads the whisper model on
    /// first use if needed); recording → click stops and transcribes into `conv.input`.
    ///
    /// While `transcribing` (mic just turned off, waiting on model load + inference — can
    /// take a few seconds on first use) the button shows three pulsing dots instead of the
    /// mic glyph so it's clear the app is still working, not stuck.
    pub(super) fn render_mic_button(&mut self, ui: &mut Ui) {
        let recording = self.conv.voice_ui.recording;
        let transcribing = self.conv.voice_ui.transcribing;
        let rounding = CornerRadius::same((MIC_DIAM * 0.5) as u8);

        if transcribing {
            let (rect, response) =
                ui.allocate_exact_size(egui::vec2(MIC_DIAM, MIC_DIAM), Sense::hover());
            ui.painter().rect_filled(rect, rounding, c_bg_elevated_2());
            ui.painter().rect_stroke(
                rect,
                rounding,
                Stroke::new(1.0, c_border_subtle()),
                egui::StrokeKind::Middle,
            );
            let time = ui.input(|i| i.time);
            crate::theme::paint_three_dots(ui.painter(), rect.center(), time, c_text_faint(), 1.6);
            response.on_hover_text("Transcribing…");
            return;
        }

        let (fill, stroke, glyph, hover) = if recording {
            (c_danger(), c_danger(), c_on_accent(), "Stop recording")
        } else {
            (c_bg_input(), c_border_subtle(), c_text_muted(), "Dictate")
        };
        let mic = crate::ui::chrome::icon_button_core_with_hover(
            ui,
            ICON_MIC,
            egui::vec2(MIC_DIAM, MIC_DIAM),
            14.0,
            false,
            !recording,
            &crate::ui::chrome::IconButtonLook {
                fill,
                hover_fill: c_row_hover(),
                stroke,
                hover_stroke: c_border(),
                rounding,
                glyph,
            },
        )
        .on_hover_text(hover);
        if mic.clicked() {
            self.toggle_dictation();
        }
        if let Some(err) = self.conv.voice_ui.error.as_ref() {
            ui.label(
                RichText::new(format!("Dictation: {err}"))
                    .size(FS_TINY)
                    .color(c_danger()),
            )
            .on_hover_text(err);
        }
    }

    /// Path of the downloaded model selected in Settings → Voice, if any.
    fn active_voice_model_path(&self) -> Option<std::path::PathBuf> {
        let id = self.conv.settings.dictation.model_id.as_ref()?;
        self.conv
            .voice_ui
            .downloaded
            .iter()
            .find(|m| &m.id == id)
            .map(|m| std::path::PathBuf::from(&m.path))
    }

    fn toggle_dictation(&mut self) {
        let Some(model_path) = self.active_voice_model_path() else {
            self.open_settings_page();
            self.conv.settings_tab = crate::app::state::SettingsTab::Voice;
            return;
        };
        if self.conv.voice_ui.recording {
            self.conv.voice_ui.recording = false;
            self.conv.voice_ui.transcribing = true;
            self.conv.voice_ui.error = None;
            let keep_loaded = self.conv.settings.dictation.keep_loaded;
            let language = self.conv.settings.dictation.language.clone();
            self.voice
                .stop_and_transcribe(model_path, keep_loaded, language);
        } else {
            self.conv.voice_ui.error = None;
            self.conv.voice_ui.recording = true;
            self.voice.start_recording();
        }
    }

    /// Drain results from the background voice engine (see [`crate::voice_engine`]),
    /// called once per frame from the main update loop.
    pub(crate) fn drain_voice(&mut self, ctx: &egui::Context) {
        use crate::voice_engine::VoiceMsg;
        loop {
            match self.conv.voice_rx.try_recv() {
                Ok(VoiceMsg::RecordingStarted(Ok(()))) => {
                    ctx.request_repaint();
                }
                Ok(VoiceMsg::RecordingStarted(Err(e))) => {
                    self.conv.voice_ui.recording = false;
                    self.conv.voice_ui.error = Some(e);
                    ctx.request_repaint();
                }
                Ok(VoiceMsg::ModelLoading) => {
                    ctx.request_repaint();
                }
                Ok(VoiceMsg::TranscriptionDone(result)) => {
                    self.conv.voice_ui.transcribing = false;
                    match result {
                        Ok(text) if !text.trim().is_empty() => {
                            let text = text.trim();
                            if !self.conv.input.is_empty()
                                && !self.conv.input.ends_with(' ')
                                && !self.conv.input.ends_with('\n')
                            {
                                self.conv.input.push(' ');
                            }
                            self.conv.input.push_str(text);
                            self.conv.focus_chat_input_next_frame = true;
                        }
                        Ok(_) => {}
                        Err(e) => self.conv.voice_ui.error = Some(e),
                    }
                    ctx.request_repaint();
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            }
        }
    }

    pub(super) fn render_context_indicator(&self, ui: &mut Ui) {
        let cfg = self.conv.settings.active_config();
        let max_tokens = cfg.effective_context_window(self.conv.settings.context_window_default);
        let key = self.active_session_key();
        let used_chars = self.estimated_active_context_chars();
        let cpt = self.calibrated_chars_per_token(key) as f64;
        let used_tokens = ((used_chars as f64) / cpt).ceil().max(0.0) as usize;
        let pct = if max_tokens == 0 {
            0.0
        } else {
            used_tokens as f32 / max_tokens as f32
        }
        .clamp(0.0, 1.0);

        let size = egui::vec2(26.0, 26.0);
        let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
        let center = rect.center();
        let radius = 8.0;
        ui.painter()
            .circle_stroke(center, radius, Stroke::new(3.0, c_border_subtle()));
        paint_arc(
            ui,
            center,
            radius,
            -std::f32::consts::FRAC_PI_2,
            std::f32::consts::TAU * pct,
            Stroke::new(3.0, context_indicator_color(pct)),
        );
        ui.painter().circle_filled(center, 3.0, c_bg_elevated_2());

        let hover = format!(
            "Context {} / {} ({:.0}%)",
            format_context_tokens(used_tokens as u64),
            format_context_tokens(max_tokens as u64),
            pct * 100.0
        );
        // Show the context tooltip immediately; egui's default hover tooltip delay feels
        // sluggish for this tiny status indicator in the composer.
        if resp.hovered() {
            resp.show_tooltip_text(hover);
        }
    }

    fn estimated_active_context_chars(&self) -> usize {
        let key = self.active_session_key();
        let current_input = self.conv.input.len()
            + self
                .conv
                .pending_images
                .iter()
                .map(|(_, data)| data.len() * 4 / 3)
                .sum::<usize>();
        let budget_chars = context_char_budget_from_tokens(
            self.conv
                .settings
                .active_config()
                .effective_context_window(self.conv.settings.context_window_default),
            self.calibrated_chars_per_token(key),
        );
        (self.estimated_session_context_chars(key) + current_input).min(budget_chars)
    }

    /// The calibrated chars-per-token ratio for a session (measured from the last provider
    /// `Usage` event), or the conservative default before any turn has reported usage.
    pub(crate) fn calibrated_chars_per_token(&self, key: SessionKey) -> f32 {
        self.session_by_key(key)
            .chars_per_token
            .unwrap_or(crate::agent::DEFAULT_CHARS_PER_TOKEN)
    }

    /// Estimated size (chars) of what a run for `key` sends: system prompt + tool definitions
    /// + all persisted messages. Excludes unsent composer input.
    pub(crate) fn estimated_session_context_chars(&self, key: SessionKey) -> usize {
        let root = std::path::Path::new(self.conv.workspaces[key.workspace_idx].root_path.as_str());
        let system_chars =
            crate::agent::prompt::build_system_prompt_for_workspace(&self.conv.settings, root)
                .len();
        let messages_chars = self
            .session_by_key(key)
            .messages
            .iter()
            .map(estimate_message_chars)
            .sum::<usize>();
        let tools_chars = crate::agent::tools::tool_definitions_json(
            &self.conv.settings.tools_enabled,
            self.conv.settings.bash_timeout_cap_secs,
        )
        .iter()
        .map(|v| v.to_string().len())
        .sum::<usize>();
        system_chars + messages_chars + tools_chars
    }

    /// Estimated tokens currently in a session's context, using the calibrated ratio.
    pub(crate) fn estimated_session_context_tokens(&self, key: SessionKey) -> usize {
        let cpt = self.calibrated_chars_per_token(key).max(0.1);
        ((self.estimated_session_context_chars(key) as f32) / cpt).ceil() as usize
    }
}
