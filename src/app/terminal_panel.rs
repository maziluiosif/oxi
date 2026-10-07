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
        self.conv.terminal_panel.open = !self.conv.terminal_panel.open;
        self.conv.settings.terminal_open = self.conv.terminal_panel.open;
        if self.conv.terminal_panel.open {
            self.conv.composer.focus_next_frame = false;
            self.conv.editor.focus_editor_next_frame = false;
            self.conv.editor.focus_find_next_frame = false;
            self.conv.editor.find_focus_editor_pending = false;
            self.conv.terminal_panel.focus_next_frame = true;
        } else {
            self.conv.terminal_panel.focus_next_frame = false;
            self.focus_active_view_next_frame();
        }
        self.save_settings_quietly();
    }

    /// Hand the terminals over after a workspace switch. Shells with a command still running
    /// are parked (a dev server or build must survive the switch), idle ones are dropped; the
    /// new workspace gets its parked shells back, or a fresh one spawned lazily in its root.
    pub(crate) fn swap_workspace_terminal(&mut self, old_root: &str) {
        let busy: Vec<_> = std::mem::take(&mut self.terminals)
            .into_iter()
            .filter(|term| term.has_foreground_job())
            .collect();
        if !busy.is_empty() {
            self.parked_terminals.insert(old_root.to_string(), busy);
        }
        let new_root = self.active_workspace().root_path.clone();
        self.terminals = self
            .parked_terminals
            .remove(&new_root)
            .unwrap_or_default()
            .into_iter()
            .filter(|term| term.is_alive())
            .collect();
        self.active_terminal = 0;
    }

    fn spawn_terminal(&mut self, ctx: &egui::Context) -> Result<(), String> {
        let cwd = self.active_workspace().root_path.clone();
        let term = crate::terminal::TerminalSession::spawn(
            ctx,
            &cwd,
            24,
            80,
            self.conv.settings.windows_terminal,
        )?;
        self.terminals.push(term);
        self.active_terminal = self.terminals.len() - 1;
        Ok(())
    }

    /// Open a new terminal tab in the workspace and type `command` at its prompt (not run).
    pub(crate) fn open_command_in_terminal(&mut self, ctx: &egui::Context, command: &str) {
        if !self.conv.terminal_panel.open {
            self.toggle_terminal();
        }
        match self.spawn_terminal(ctx) {
            Ok(()) => {
                if let Some(term) = self.terminals.get_mut(self.active_terminal) {
                    term.type_text(&single_line_command(command));
                }
                self.conv.terminal_panel.focus_next_frame = true;
            }
            Err(e) => self.notify_composer(format!("Could not open a terminal: {e}")),
        }
    }

    /// Close one tab. The panel stays open; the body respawns a shell if it was the last one.
    fn close_terminal(&mut self, index: usize) {
        if index >= self.terminals.len() {
            return;
        }
        self.terminals.remove(index);
        if self.active_terminal > index || self.active_terminal >= self.terminals.len() {
            self.active_terminal = self.active_terminal.saturating_sub(1);
        }
        self.conv.terminal_panel.focus_next_frame = true;
    }

    /// Render the bottom terminal panel (call before the `CentralPanel`).
    pub(crate) fn render_terminal_panel(&mut self, ui: &mut egui::Ui) {
        let height = self
            .conv
            .terminal_panel
            .height
            .clamp(TERMINAL_H_MIN, TERMINAL_H_MAX);

        egui::Panel::bottom("terminal_panel")
            .resizable(false)
            .exact_size(height)
            .frame(
                Frame::new()
                    .fill(c_bg_elevated())
                    .stroke(Stroke::new(1.0, c_border())),
            )
            .show(ui, |ui| {
                self.render_terminal_resize_handle(ui);
                self.render_terminal_header(ui);
                if self.conv.terminal_panel.open {
                    self.render_terminal_body(ui);
                }
            });
    }

    fn render_terminal_resize_handle(&mut self, ui: &mut egui::Ui) {
        let full_w = ui.available_width();
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(full_w, RESIZE_H), Sense::drag());
        if resp.hovered() || resp.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
        }
        if resp.dragged()
            && let Some(pos) = ui.input(|i| i.pointer.interact_pos())
        {
            // Position-based like the sidebar sep (see there for why deltas jitter).
            // The panel's bottom edge is pinned, so height = bottom - pointer.
            self.conv.terminal_panel.height =
                (ui.max_rect().bottom() - pos.y).clamp(TERMINAL_H_MIN, TERMINAL_H_MAX);
            self.conv.settings.terminal_height = self.conv.terminal_panel.height;
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
                ui.add_space(4.0);
                let mut select = None;
                let mut close = None;
                let tabs = self.terminals.len().max(1);
                for index in 0..tabs {
                    let term = self.terminals.get(index);
                    let label = match term {
                        Some(t) if t.process.is_some() => format!(
                            "Agent {}{}",
                            index + 1,
                            if t.is_alive() { "" } else { " (exited)" }
                        ),
                        Some(t) if !t.is_alive() => format!("Shell {} (exited)", index + 1),
                        _ => format!("Shell {}", index + 1),
                    };
                    // Windows has no foreground process group, so every live shell would
                    // read as busy there; only show the dot where it means something.
                    let busy = term.is_some_and(|t| {
                        (cfg!(unix) || t.process.is_some()) && t.has_foreground_job()
                    });
                    let (clicked, closed) = terminal_tab(
                        ui,
                        index,
                        &label,
                        index == self.active_terminal,
                        busy,
                        tabs > 1,
                    );
                    if clicked {
                        select = Some(index);
                    }
                    if closed {
                        close = Some(index);
                    }
                }
                if crate::ui::chrome::icon_button_plain(ui, ICON_PLUS, 20.0, false)
                    .on_hover_text("New shell")
                    .clicked()
                    && let Err(e) = self.spawn_terminal(ui.ctx())
                {
                    self.run_state_mut(self.active_session_key()).stream_error =
                        Some(format!("Failed to start terminal: {e}"));
                }
                if let Some(index) = select {
                    self.active_terminal = index;
                    self.conv.terminal_panel.focus_next_frame = true;
                }
                if let Some(index) = close {
                    self.close_terminal(index);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add_space(6.0);
                    if crate::ui::chrome::icon_button_plain(ui, ICON_ANGLE_DOWN, 22.0, false)
                        .on_hover_text("Hide terminal (Cmd/Ctrl+`)")
                        .clicked()
                    {
                        self.toggle_terminal();
                    }
                    if crate::ui::chrome::icon_button_plain(ui, ICON_REFRESH, 22.0, false)
                        .on_hover_text("Restart this shell")
                        .clicked()
                        && self.active_terminal < self.terminals.len()
                    {
                        let cwd = self.active_workspace().root_path.clone();
                        match crate::terminal::TerminalSession::spawn(
                            ui.ctx(),
                            &cwd,
                            24,
                            80,
                            self.conv.settings.windows_terminal,
                        ) {
                            Ok(term) => self.terminals[self.active_terminal] = term,
                            Err(e) => {
                                self.run_state_mut(self.active_session_key()).stream_error =
                                    Some(format!("Failed to start terminal: {e}"));
                            }
                        }
                        self.conv.terminal_panel.focus_next_frame = true;
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
        // `TerminalSession::ui` owns interaction/focus for this rectangle. Only reserve layout
        // space here; a second overlapping widget can win the hit-test and leave the PTY unfocused.
        let (_, rect) = ui.allocate_space(avail);
        let inner = rect.shrink2(egui::vec2(6.0, 2.0));

        // Lazily spawn the first shell, rooted at the active workspace.
        if self.terminals.is_empty()
            && let Err(e) = self.spawn_terminal(ui.ctx())
        {
            ui.painter().text(
                inner.left_top() + egui::vec2(2.0, 2.0),
                egui::Align2::LEFT_TOP,
                format!("Failed to start terminal: {e}"),
                egui::FontId::monospace(12.0),
                c_danger(),
            );
            return;
        }
        self.active_terminal = self.active_terminal.min(self.terminals.len() - 1);
        if let Some(term) = self.terminals.get_mut(self.active_terminal) {
            term.ui(ui, inner, &mut self.conv.terminal_panel.focus_next_frame);
        }
    }

    /// Persist settings, surfacing any error on the active session.
    pub(crate) fn save_settings_quietly(&mut self) {
        self.remember_acp_workspace_preferences();
        if let Err(e) = self.conv.settings.save() {
            self.run_state_mut(self.active_session_key()).stream_error =
                Some(format!("Save settings: {e}"));
        }
    }
}

/// One header tab: label, a dot while a command runs, and a close "×" on hover (only when
/// there is more than one tab). Returns `(clicked, close_clicked)`.
fn terminal_tab(
    ui: &mut egui::Ui,
    index: usize,
    label: &str,
    active: bool,
    busy: bool,
    closable: bool,
) -> (bool, bool) {
    const H: f32 = 20.0;
    const CLOSE_W: f32 = 16.0;
    let font = FontId::proportional(FS_TINY);
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), font, egui::Color32::PLACEHOLDER);
    let dot_w = if busy { 10.0 } else { 0.0 };
    let close_w = if closable { CLOSE_W } else { 0.0 };
    let width = galley.size().x + 16.0 + dot_w + close_w;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, H), Sense::click());
    let hovered = ui.rect_contains_pointer(rect);
    let fill = if active {
        c_row_active()
    } else if hovered {
        c_row_hover()
    } else {
        egui::Color32::TRANSPARENT
    };
    ui.painter()
        .rect_filled(rect, egui::CornerRadius::same(RADIUS_ROW), fill);
    let mut x = rect.left() + 8.0;
    if busy {
        ui.painter()
            .circle_filled(egui::pos2(x + 3.0, rect.center().y), 3.0, c_accent());
        x += dot_w;
    }
    let text_color = if active { c_text() } else { c_text_muted() };
    ui.painter().galley(
        egui::pos2(x, rect.center().y - galley.size().y * 0.5),
        galley,
        text_color,
    );
    let mut closed = false;
    if closable && (hovered || active) {
        let close_rect = egui::Rect::from_center_size(
            egui::pos2(rect.right() - 4.0 - CLOSE_W * 0.5, rect.center().y),
            egui::vec2(CLOSE_W, CLOSE_W),
        );
        let close = ui
            .interact(
                close_rect,
                ui.id().with(("terminal_tab_close", index)),
                Sense::click(),
            )
            .on_hover_text(if busy {
                "Close shell (stops the running command)"
            } else {
                "Close shell"
            });
        ui.painter().text(
            close_rect.center(),
            egui::Align2::CENTER_CENTER,
            ICON_CLOSE,
            FontId::new(FS_TINY - 1.0, icon_font()),
            if close.hovered() {
                c_accent()
            } else {
                c_text_faint()
            },
        );
        closed = close.clicked();
    }
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    (response.clicked() && !closed, closed)
}

/// A newline at the prompt would run the command, so a multi-line one becomes one editable line:
/// `\`-continued lines are joined, separate lines become `;`-separated commands.
fn single_line_command(command: &str) -> String {
    let mut out = String::new();
    let mut continued = true;
    for line in command.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if !out.is_empty() {
            out.push_str(if continued { " " } else { "; " });
        }
        match line.strip_suffix('\\') {
            Some(rest) => {
                out.push_str(rest.trim_end());
                continued = true;
            }
            None => {
                out.push_str(line);
                continued = false;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::single_line_command;

    #[test]
    fn multi_line_commands_become_one_line() {
        assert_eq!(single_line_command("cargo test"), "cargo test");
        assert_eq!(single_line_command("cd src\nls -la\n"), "cd src; ls -la");
        assert_eq!(
            single_line_command("cargo build \\\n  --release\nls"),
            "cargo build --release; ls"
        );
    }
}
