//! Bottom terminal panel: a resizable, hideable frame hosting a live PTY shell.

use eframe::egui::{self, Align, FontId, Frame, Layout, RichText, Sense, Stroke};

use crate::settings::{TERMINAL_H_MAX, TERMINAL_H_MIN};
use crate::theme::*;

use super::OxiApp;

/// Height of the panel header row (title + buttons).
const HEADER_H: f32 = 26.0;
/// Thickness of the draggable top edge.
const RESIZE_H: f32 = 6.0;

impl OxiApp {
    /// Show or hide the terminal panel, persisting the choice.
    pub(crate) fn toggle_terminal(&mut self) {
        self.conv.terminal_open = !self.conv.terminal_open;
        self.conv.settings.terminal_open = self.conv.terminal_open;
        self.save_settings_quietly();
    }

    /// Render the bottom terminal panel (call before the `CentralPanel`).
    pub(crate) fn render_terminal_panel(&mut self, ui: &mut egui::Ui) {
        let height = self
            .conv
            .terminal_height
            .clamp(TERMINAL_H_MIN, TERMINAL_H_MAX);

        egui::Panel::bottom("terminal_panel")
            .resizable(false)
            .exact_size(height)
            .frame(
                Frame::new()
                    .fill(c_bg_sidebar())
                    .stroke(Stroke::new(1.0, c_border_subtle())),
            )
            .show(ui, |ui| {
                self.render_terminal_resize_handle(ui);
                self.render_terminal_header(ui);
                self.render_terminal_body(ui);
            });
    }

    fn render_terminal_resize_handle(&mut self, ui: &mut egui::Ui) {
        let full_w = ui.available_width();
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(full_w, RESIZE_H), Sense::drag());
        if resp.hovered() || resp.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
        }
        if resp.dragged() {
            let dy = ui.input(|i| i.pointer.delta().y);
            // Dragging the top edge upward (negative dy) grows the panel.
            self.conv.terminal_height =
                (self.conv.terminal_height - dy).clamp(TERMINAL_H_MIN, TERMINAL_H_MAX);
            self.conv.settings.terminal_height = self.conv.terminal_height;
        }
        if resp.drag_stopped() {
            self.save_settings_quietly();
        }
        let col = if resp.hovered() || resp.dragged() {
            c_accent()
        } else {
            c_border_subtle()
        };
        ui.painter()
            .hline(rect.x_range(), rect.center().y, Stroke::new(1.0, col));
    }

    fn render_terminal_header(&mut self, ui: &mut egui::Ui) {
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), HEADER_H),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.add_space(8.0);
                ui.label(
                    RichText::new(ICON_TERMINAL)
                        .font(FontId::new(FS_TINY, icon_font()))
                        .color(c_sidebar_section())
                        .strong(),
                );
                let alive = self.terminal.as_ref().is_none_or(|t| t.is_alive());
                if !alive {
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new("(exited)")
                            .size(FS_TINY)
                            .color(c_text_muted()),
                    );
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add_space(6.0);
                    if crate::ui::chrome::icon_button_plain(ui, ICON_ANGLE_DOWN, 22.0, false)
                        .on_hover_text("Hide terminal")
                        .clicked()
                    {
                        self.toggle_terminal();
                    }
                    if crate::ui::chrome::icon_button_plain(ui, ICON_REFRESH, 22.0, false)
                        .on_hover_text("Restart shell")
                        .clicked()
                    {
                        self.terminal = None;
                    }
                });
            },
        );
    }

    fn render_terminal_body(&mut self, ui: &mut egui::Ui) {
        let avail = ui.available_size();
        if avail.y < 8.0 {
            return;
        }
        let (rect, _) = ui.allocate_exact_size(avail, Sense::hover());
        let inner = rect.shrink2(egui::vec2(6.0, 2.0));

        // Lazily (re)spawn the shell rooted at the active workspace.
        if self.terminal.is_none() {
            let cwd = self.active_workspace().root_path.clone();
            match crate::terminal::TerminalSession::spawn(ui.ctx(), &cwd, 24, 80) {
                Ok(term) => self.terminal = Some(term),
                Err(e) => {
                    ui.painter().text(
                        inner.left_top() + egui::vec2(2.0, 2.0),
                        egui::Align2::LEFT_TOP,
                        format!("Failed to start terminal: {e}"),
                        egui::FontId::monospace(12.0),
                        c_danger(),
                    );
                    return;
                }
            }
        }

        if let Some(term) = self.terminal.as_mut() {
            term.ui(ui, inner);
        }
    }

    /// Persist settings, surfacing any error on the active session.
    fn save_settings_quietly(&mut self) {
        if let Err(e) = self.conv.settings.save() {
            self.run_state_mut(self.active_session_key()).stream_error =
                Some(format!("Save settings: {e}"));
        }
    }
}
