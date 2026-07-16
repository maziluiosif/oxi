//! Composer input history (↑/↓ navigation like a shell).

use eframe::egui::{self, Response, Ui};

use super::OxiApp;

impl OxiApp {
    /// ArrowUp / ArrowDown while the composer is focused: walk `input_history`.
    /// Only for single-line drafts (or with Cmd/Ctrl held), so multi-line editing
    /// still moves the caret normally.
    pub(crate) fn handle_composer_history_keys(&mut self, ui: &mut Ui, response: &Response) {
        if !response.has_focus() || self.conv.input_history.is_empty() {
            return;
        }

        let (up, down, force) = ui.input(|i| {
            (
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::ArrowDown),
                i.modifiers.command || i.modifiers.ctrl,
            )
        });
        if !up && !down {
            return;
        }

        let single_line = !self.conv.input.contains('\n');
        if !single_line && !force {
            return;
        }

        if up {
            match self.conv.input_history_index {
                None => {
                    self.conv.input_history_draft = self.conv.input.clone();
                    self.conv.input_history_index = Some(0);
                    if let Some(entry) = self.conv.input_history.first() {
                        self.conv.input = entry.clone();
                    }
                }
                Some(i) => {
                    let next = (i + 1).min(self.conv.input_history.len().saturating_sub(1));
                    self.conv.input_history_index = Some(next);
                    if let Some(entry) = self.conv.input_history.get(next) {
                        self.conv.input = entry.clone();
                    }
                }
            }
        } else if down {
            match self.conv.input_history_index {
                None => {}
                Some(0) => {
                    self.conv.input_history_index = None;
                    self.conv.input = std::mem::take(&mut self.conv.input_history_draft);
                }
                Some(i) => {
                    let next = i.saturating_sub(1);
                    self.conv.input_history_index = Some(next);
                    if let Some(entry) = self.conv.input_history.get(next) {
                        self.conv.input = entry.clone();
                    }
                }
            }
        }
    }
}
