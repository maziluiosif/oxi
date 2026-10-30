//! Putting a draft aside: Cmd/Ctrl+Shift+S in the composer stashes what is written (text and
//! attachments) and leaves it empty for something else; with the composer empty, the same keys
//! bring the latest stashed draft back. Stashed drafts are listed above the composer of every
//! chat, so one written for a chat can be restored into another. Their text survives a restart
//! (like the hot-exit drafts, pasted images are not kept on disk).

use eframe::egui::{self, RichText, Ui};

use super::OxiApp;
use super::compaction::QueuedSend;
use crate::theme::*;

fn stash_path() -> std::path::PathBuf {
    crate::app_dirs::data_dir().join("stashed_drafts.json")
}

/// The texts a previous run left stashed.
pub(crate) fn load() -> Vec<QueuedSend> {
    std::fs::read(stash_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Vec<String>>(&bytes).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|text| QueuedSend {
            text,
            images: Vec::new(),
            texts: Vec::new(),
        })
        .collect()
}

impl OxiApp {
    fn save_stash(&self) {
        let texts: Vec<&str> = self
            .conv
            .composer
            .stash
            .iter()
            .map(|d| d.text.as_str())
            .collect();
        let content = serde_json::to_vec_pretty(&texts).unwrap_or_default();
        let _ = std::thread::Builder::new()
            .name("oxi-stash".into())
            .spawn(move || {
                if let Err(e) = crate::fsutil::write_atomic_private(&stash_path(), &content) {
                    log::warn!("stash: could not save: {e}");
                }
            });
    }

    /// Cmd/Ctrl+Shift+S in the composer: stash the draft, or restore the latest one.
    pub(super) fn handle_stash_key(&mut self, ui: &mut Ui) {
        let pressed = ui.input_mut(|i| {
            i.consume_key(
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                egui::Key::S,
            )
        });
        if !pressed {
            return;
        }
        let composer = &self.conv.composer;
        let empty = composer.input.trim().is_empty()
            && composer.pending_images.is_empty()
            && composer.pending_texts.is_empty();
        if empty {
            let last = self.conv.composer.stash.len().saturating_sub(1);
            self.restore_stashed(last);
        } else {
            let draft = self.take_composer_payload();
            self.conv.composer.stash.push(draft);
            self.save_stash();
            self.notify_composer(
                "Draft stashed. Cmd/Ctrl+Shift+S with an empty composer brings it back.",
            );
        }
    }

    /// Put stashed draft `index` back into the composer, ahead of what is written.
    fn restore_stashed(&mut self, index: usize) {
        if index >= self.conv.composer.stash.len() {
            return;
        }
        let draft = self.conv.composer.stash.remove(index);
        let composer = &mut self.conv.composer;
        composer.input = if composer.input.trim().is_empty() {
            draft.text
        } else {
            format!("{}\n\n{}", draft.text, composer.input.trim())
        };
        composer.pending_images.extend(draft.images);
        composer.pending_texts.extend(draft.texts);
        composer.focus_next_frame = true;
        self.save_stash();
    }

    /// Stashed drafts, one row each, with Restore and remove.
    pub(super) fn render_stash(&mut self, ui: &mut Ui) {
        if self.conv.composer.stash.is_empty() {
            return;
        }
        let rows: Vec<String> = self
            .conv
            .composer
            .stash
            .iter()
            .map(|d| d.preview())
            .collect();
        let mut restore = None;
        let mut remove = None;
        for (i, preview) in rows.iter().enumerate().rev() {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(
                    RichText::new("Stashed")
                        .size(FS_SMALL)
                        .color(c_text_muted()),
                )
                .on_hover_text("Put aside with Cmd/Ctrl+Shift+S");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("×").on_hover_text("Discard").clicked() {
                        remove = Some(i);
                    }
                    if ui
                        .small_button("Restore")
                        .on_hover_text("Back into the composer")
                        .clicked()
                    {
                        restore = Some(i);
                    }
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.add(
                            egui::Label::new(RichText::new(preview).size(FS_SMALL).color(c_text()))
                                .truncate()
                                .selectable(false),
                        )
                        .on_hover_text(preview);
                    });
                });
            });
        }
        ui.add_space(4.0);
        if let Some(i) = restore {
            self.restore_stashed(i);
        } else if let Some(i) = remove {
            self.conv.composer.stash.remove(i);
            self.save_stash();
        }
    }
}
