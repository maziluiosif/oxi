//! Embedded interactive terminal: a real PTY-backed shell rendered into egui.
//!
//! A background thread pumps the PTY's output into a [`vt100::Parser`], which maintains the
//! screen grid (text + colors + cursor). The UI thread renders that grid each frame as
//! monospace text and forwards keyboard input back to the PTY.

use std::io::{Read, Write};
#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use eframe::egui::{
    self, Color32, Event, EventFilter, FontId, Key, PointerButton, Pos2, Rect, Sense, Stroke,
    TextFormat, Ui,
};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use vt100::MouseProtocolMode as MMode;

#[path = "terminal/encoding.rs"]
mod encoding;
use encoding::{button_base, ctrl_byte, encode_mouse, key_sequence, ordered_selection, vt_color};

/// Mouse tracking carried between frames so we can report drags / motion.
#[derive(Default)]
struct MouseState {
    /// Base button code (0/1/2) currently held, if any.
    held: Option<u8>,
    /// Last reported cell (col, row), 0-based, to detect motion across cells.
    last_cell: Option<(u16, u16)>,
}

#[derive(Clone, Copy, Debug)]
struct TerminalSelection {
    anchor: (u16, u16), // (row, col)
    focus: (u16, u16),
    dragging: bool,
}

fn shell_cwd(cwd: &str) -> String {
    #[cfg(windows)]
    {
        // `std::fs::canonicalize` returns verbatim paths on Windows. CreateProcess accepts them,
        // but cmd.exe can ignore a `\\?\C:\...` current directory and fall back to System32.
        if let Some(unc) = cwd
            .strip_prefix(r"\\?\UNC\")
            .or_else(|| cwd.strip_prefix("//?/UNC/"))
        {
            return format!(r"\\{unc}").replace('/', r"\");
        }
        if let Some(drive_path) = cwd
            .strip_prefix(r"\\?\")
            .or_else(|| cwd.strip_prefix("//?/"))
        {
            return drive_path.replace('/', r"\");
        }
    }
    cwd.to_string()
}

use crate::settings::WindowsTerminal;
use crate::theme;

/// Whether WSL can be launched on this Windows installation.
#[cfg(windows)]
pub fn wsl_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        std::process::Command::new("wsl.exe")
            .arg("--status")
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .status()
            .is_ok_and(|status| status.success())
    })
}

/// Monospace font size for the terminal grid.
const TERM_FONT_SIZE: f32 = 13.0;
/// Lines of scrollback kept by the parser.
const SCROLLBACK: usize = 5000;

/// A live PTY session plus its parsed screen state.
pub struct TerminalSession {
    parser: Arc<Mutex<vt100::Parser>>,
    /// Writes here are forwarded to the shell's stdin. Shared with the reader thread so it can
    /// answer terminal status queries emitted by Windows ConPTY.
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    /// Kept so we can resize the kernel pty window when the panel size changes.
    master: Box<dyn MasterPty + Send>,
    /// Flipped to `false` by the reader thread when the shell exits (EOF).
    alive: Arc<AtomicBool>,
    rows: u16,
    cols: u16,
    mouse: MouseState,
    selection: Option<TerminalSelection>,
    /// Scrollback view offset (rows up from the bottom) used when no app grabs the mouse.
    scroll_offset: usize,
}

impl TerminalSession {
    /// Spawn the user's default shell in `cwd`, wired to a fresh PTY.
    pub fn spawn(
        ctx: &egui::Context,
        cwd: &str,
        rows: u16,
        cols: u16,
        windows_terminal: WindowsTerminal,
    ) -> Result<Self, String> {
        #[cfg(not(windows))]
        let _ = windows_terminal;
        let rows = rows.max(1);
        let cols = cols.max(1);
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("openpty: {e}"))?;

        let cwd = shell_cwd(cwd);
        #[cfg(windows)]
        let mut cmd = match windows_terminal {
            WindowsTerminal::Cmd => {
                // Use the actual Windows command processor and bypass cmd AutoRun entries.
                let shell = std::env::var_os("ComSpec").unwrap_or_else(|| "cmd.exe".into());
                let mut cmd = CommandBuilder::new(shell);
                cmd.arg("/D");
                cmd
            }
            WindowsTerminal::PowerShell => {
                let mut cmd = CommandBuilder::new("powershell.exe");
                cmd.arg("-NoLogo");
                cmd.arg("-NoExit");
                cmd
            }
            WindowsTerminal::Wsl => {
                if !wsl_available() {
                    return Err("WSL is not installed or is unavailable".to_string());
                }
                let mut cmd = CommandBuilder::new("wsl.exe");
                // Let WSL translate the Windows workspace path and enter it before
                // starting the shell.
                if !cwd.trim().is_empty() {
                    cmd.arg("--cd");
                    cmd.arg(&cwd);
                }
                cmd
            }
        };
        #[cfg(not(windows))]
        let mut cmd = CommandBuilder::new_default_prog();
        if !cwd.trim().is_empty() {
            #[cfg(windows)]
            if windows_terminal != WindowsTerminal::Wsl {
                cmd.cwd(&cwd);
            }
            #[cfg(not(windows))]
            cmd.cwd(&cwd);
        }
        cmd.env("TERM", "xterm-256color");
        #[cfg(windows)]
        cmd.env("PROMPT", "$P$G ");

        let _child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| format!("spawn shell: {e}"))?;
        // The slave handle is no longer needed once the child owns it; dropping it lets the
        // pty report EOF cleanly when the shell exits.
        drop(pair.slave);

        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| format!("pty reader: {e}"))?;
        let writer = Arc::new(Mutex::new(
            pair.master
                .take_writer()
                .map_err(|e| format!("pty writer: {e}"))?,
        ));

        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, SCROLLBACK)));
        let alive = Arc::new(AtomicBool::new(true));

        let parser_rd = parser.clone();
        let writer_rd = writer.clone();
        let alive_rd = alive.clone();
        let ctx = ctx.clone();
        std::thread::Builder::new()
            .name("oxi-pty-reader".to_string())
            .spawn(move || {
                let mut buf = [0u8; 8192];
                let mut query_tail = Vec::new();
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let cursor = if let Ok(mut p) = parser_rd.lock() {
                                p.process(&buf[..n]);
                                Some(p.screen().cursor_position())
                            } else {
                                None
                            };

                            // portable-pty 0.9 enables ConPTY's Win32 input mode. ConPTY emits
                            // DSR requests during startup and waits for a reply; vt100 parses the
                            // requests but does not answer them, leaving cmd at a blank cursor.
                            query_tail.extend_from_slice(&buf[..n]);
                            let status_query = query_tail.windows(4).any(|w| w == b"\x1b[5n");
                            let cursor_query = query_tail.windows(4).any(|w| w == b"\x1b[6n");
                            if status_query && let Ok(mut writer) = writer_rd.lock() {
                                let _ = writer.write_all(b"\x1b[0n");
                                let _ = writer.flush();
                            }
                            if cursor_query
                                && let Some((row, col)) = cursor
                                && let Ok(mut writer) = writer_rd.lock()
                            {
                                let reply = format!("\x1b[{};{}R", row + 1, col + 1);
                                let _ = writer.write_all(reply.as_bytes());
                                let _ = writer.flush();
                            }
                            if status_query || cursor_query {
                                query_tail.clear();
                            } else if query_tail.len() > 3 {
                                // A DSR request is four bytes; retain only a possible prefix split
                                // across two pipe reads so old output cannot trigger repeat replies.
                                query_tail.drain(..query_tail.len() - 3);
                            }
                            ctx.request_repaint();
                        }
                    }
                }
                alive_rd.store(false, Ordering::SeqCst);
                ctx.request_repaint();
            })
            .map_err(|e| format!("spawn reader thread: {e}"))?;

        // Keep the child alive without a join handle: dropping `_child` here would not kill it
        // (the pty owns it), and we want it reaped by the OS when the session is dropped. Detach
        // it onto a waiter thread so it doesn't become a zombie.
        let mut child = _child;
        std::thread::Builder::new()
            .name("oxi-pty-waiter".to_string())
            .spawn(move || {
                let _ = child.wait();
            })
            .ok();

        Ok(Self {
            parser,
            writer,
            master: pair.master,
            alive,
            rows,
            cols,
            mouse: MouseState::default(),
            selection: None,
            scroll_offset: 0,
        })
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// Send raw bytes to the shell.
    fn send(&mut self, bytes: &[u8]) {
        if let Ok(mut writer) = self.writer.lock() {
            let _ = writer.write_all(bytes);
            let _ = writer.flush();
        }
    }

    /// Resize the pty and parser if the grid dimensions changed.
    fn resize(&mut self, rows: u16, cols: u16) {
        let rows = rows.max(1);
        let cols = cols.max(1);
        if rows == self.rows && cols == self.cols {
            return;
        }
        self.rows = rows;
        self.cols = cols;
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
        if let Ok(mut p) = self.parser.lock() {
            p.screen_mut().set_size(rows, cols);
        }
    }

    /// Render the terminal into `rect` and forward keyboard input when focused.
    pub fn ui(&mut self, ui: &mut Ui, rect: Rect, focus_next_frame: &mut bool) {
        let font = FontId::monospace(TERM_FONT_SIZE);
        let (cell_w, cell_h) = ui.fonts_mut(|f| {
            (
                f.glyph_width(&font, 'M').max(1.0),
                f.row_height(&font).max(1.0),
            )
        });

        let cols = ((rect.width() / cell_w).floor() as i32).clamp(1, 1000) as u16;
        let rows = ((rect.height() / cell_h).floor() as i32).clamp(1, 1000) as u16;
        self.resize(rows, cols);

        let id = ui.id().with("terminal_surface");
        let resp = ui.interact(rect, id, Sense::click_and_drag());
        if *focus_next_frame {
            resp.request_focus();
            *focus_next_frame = false;
        }
        if resp.clicked() || resp.drag_started() {
            resp.request_focus();
        }

        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
        }
        self.handle_mouse(ui, rect, cell_w, cell_h, resp.hovered());
        // Apply scrollback view (no-op when an app has grabbed the mouse / offset is 0).
        // `set_scrollback` clamps to the buffer length; read the offset back so further
        // wheel deltas accumulate from the effective position.
        if let Ok(mut p) = self.parser.lock() {
            p.screen_mut().set_scrollback(self.scroll_offset);
            self.scroll_offset = p.screen().scrollback();
        }

        let focused = resp.has_focus();
        if focused {
            // Keep tab / arrows / escape flowing into the shell instead of moving egui focus.
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    id,
                    EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: true,
                    },
                )
            });
            self.handle_input(ui);
        }

        self.paint_grid(ui, rect, &font, cell_w, cell_h, focused);
    }

    fn handle_input(&mut self, ui: &mut Ui) {
        let app_cursor = self
            .parser
            .lock()
            .map(|p| p.screen().application_cursor())
            .unwrap_or(false);
        let events = ui.input(|i| i.events.clone());
        let mut out: Vec<u8> = Vec::new();
        for ev in events {
            match ev {
                Event::Copy => {
                    if let Some(text) = self.selected_text() {
                        ui.ctx().copy_text(text);
                        self.selection = None;
                    } else {
                        // egui-winit translates Ctrl+C into `Event::Copy`; without a selection it
                        // must retain terminal semantics and send SIGINT/ETX to the shell.
                        out.push(3);
                    }
                }
                Event::Text(t) => out.extend_from_slice(t.as_bytes()),
                Event::Paste(t) => out.extend_from_slice(t.as_bytes()),
                Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    if modifiers.command && key == Key::C && self.selection.is_some() {
                        if let Some(text) = self.selected_text() {
                            ui.ctx().copy_text(text);
                        }
                        self.selection = None;
                        continue;
                    }
                    if modifiers.ctrl {
                        // Ctrl+A..Z -> control bytes 0x01..0x1A; a few common extras.
                        if let Some(b) = ctrl_byte(key) {
                            out.push(b);
                            continue;
                        }
                    }
                    if let Some(seq) = key_sequence(key, app_cursor) {
                        out.extend_from_slice(seq);
                    }
                }
                _ => {}
            }
        }
        if !out.is_empty() {
            // Typing jumps back to the live bottom and dismisses any stale selection.
            self.scroll_offset = 0;
            self.selection = None;
            self.send(&out);
        }
    }

    /// Forward mouse interactions: wheel + (when an app enables mouse mode) press/release/motion.
    /// When no app has grabbed the mouse, the wheel scrolls the local scrollback buffer.
    fn handle_mouse(&mut self, ui: &Ui, rect: Rect, cell_w: f32, cell_h: f32, hovered: bool) {
        let (mode, encoding) = {
            match self.parser.lock() {
                Ok(p) => (
                    p.screen().mouse_protocol_mode(),
                    p.screen().mouse_protocol_encoding(),
                ),
                Err(_) => return,
            }
        };

        let cols = self.cols;
        let rows = self.rows;
        let cell_at = |pos: Pos2| -> (u16, u16) {
            let col =
                (((pos.x - rect.left()) / cell_w).floor() as i32).clamp(0, cols as i32 - 1) as u16;
            let row =
                (((pos.y - rect.top()) / cell_h).floor() as i32).clamp(0, rows as i32 - 1) as u16;
            (col, row)
        };

        // Wheel.
        if hovered {
            let scroll_y = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll_y.abs() > 0.5 {
                let up = scroll_y > 0.0;
                if mode == MMode::None {
                    // Local scrollback: positive delta scrolls toward older output.
                    let lines = ((scroll_y.abs() / cell_h).ceil() as usize).clamp(1, 6);
                    if up {
                        self.scroll_offset = self.scroll_offset.saturating_add(lines);
                    } else {
                        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
                    }
                } else if let Some(pos) = ui.input(|i| i.pointer.hover_pos()) {
                    let (col, row) = cell_at(pos);
                    let base = if up { 64 } else { 65 };
                    let notches = ((scroll_y.abs() / 40.0).round() as usize).clamp(1, 5);
                    let mut out = Vec::new();
                    for _ in 0..notches {
                        out.extend_from_slice(&encode_mouse(
                            encoding,
                            base,
                            col,
                            row,
                            true,
                            false,
                            egui::Modifiers::default(),
                        ));
                    }
                    self.send(&out);
                }
            }
        }

        if mode == MMode::None {
            self.mouse.held = None;
            self.mouse.last_cell = None;
            let events = ui.input(|i| i.events.clone());
            for event in events {
                match event {
                    Event::PointerButton {
                        pos,
                        button: PointerButton::Primary,
                        pressed,
                        ..
                    } if rect.contains(pos) || self.selection.is_some_and(|s| s.dragging) => {
                        let (col, row) = cell_at(pos);
                        if pressed {
                            self.selection = Some(TerminalSelection {
                                anchor: (row, col),
                                focus: (row, col),
                                dragging: true,
                            });
                        } else if let Some(selection) = self.selection.as_mut() {
                            selection.focus = (row, col);
                            selection.dragging = false;
                        }
                    }
                    Event::PointerMoved(pos) => {
                        if let Some(selection) = self.selection.as_mut()
                            && selection.dragging
                        {
                            let (col, row) = cell_at(pos);
                            selection.focus = (row, col);
                        }
                    }
                    _ => {}
                }
            }
            return;
        }

        // Buttons + motion.
        let events = ui.input(|i| i.events.clone());
        let mut out: Vec<u8> = Vec::new();
        for ev in events {
            match ev {
                Event::PointerButton {
                    pos,
                    button,
                    pressed,
                    modifiers,
                } => {
                    if !rect.contains(pos) && self.mouse.held.is_none() {
                        continue;
                    }
                    let Some(base) = button_base(button) else {
                        continue;
                    };
                    let (col, row) = cell_at(pos);
                    if pressed {
                        out.extend_from_slice(&encode_mouse(
                            encoding, base, col, row, true, false, modifiers,
                        ));
                        self.mouse.held = Some(base);
                        self.mouse.last_cell = Some((col, row));
                    } else {
                        if matches!(
                            mode,
                            MMode::PressRelease | MMode::ButtonMotion | MMode::AnyMotion
                        ) {
                            out.extend_from_slice(&encode_mouse(
                                encoding, base, col, row, false, false, modifiers,
                            ));
                        }
                        self.mouse.held = None;
                    }
                }
                Event::PointerMoved(pos) => {
                    let report = match mode {
                        MMode::ButtonMotion => self.mouse.held.is_some(),
                        MMode::AnyMotion => true,
                        _ => false,
                    };
                    if !report || !rect.contains(pos) {
                        continue;
                    }
                    let cell = cell_at(pos);
                    if self.mouse.last_cell == Some(cell) {
                        continue;
                    }
                    self.mouse.last_cell = Some(cell);
                    let base = self.mouse.held.unwrap_or(3);
                    out.extend_from_slice(&encode_mouse(
                        encoding,
                        base,
                        cell.0,
                        cell.1,
                        true,
                        true,
                        egui::Modifiers::default(),
                    ));
                }
                _ => {}
            }
        }
        if !out.is_empty() {
            self.send(&out);
        }
    }

    fn selected_text(&self) -> Option<String> {
        let selection = self.selection?;
        if selection.anchor == selection.focus {
            return None;
        }
        let (start, end) = ordered_selection(selection);
        let parser = self.parser.lock().ok()?;
        let screen = parser.screen();
        let (_, cols) = screen.size();
        let mut output = String::new();
        for row in start.0..=end.0 {
            let first_col = if row == start.0 { start.1 } else { 0 };
            let last_col = if row == end.0 {
                end.1
            } else {
                cols.saturating_sub(1)
            };
            let mut line = String::new();
            for col in first_col..=last_col {
                let Some(cell) = screen.cell(row, col) else {
                    continue;
                };
                if !cell.is_wide_continuation() {
                    line.push_str(cell.contents());
                }
            }
            output.push_str(line.trim_end());
            if row != end.0 {
                output.push('\n');
            }
        }
        (!output.is_empty()).then_some(output)
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_grid(
        &self,
        ui: &Ui,
        rect: Rect,
        font: &FontId,
        cell_w: f32,
        cell_h: f32,
        focused: bool,
    ) {
        let painter = ui.painter_at(rect);
        let Ok(parser) = self.parser.lock() else {
            return;
        };
        let screen = parser.screen();
        let (rows, cols) = screen.size();

        for row in 0..rows {
            let y = rect.top() + row as f32 * cell_h;
            // Build the row as runs of identical styling for fewer galleys.
            let mut job = egui::text::LayoutJob::default();
            job.wrap.max_width = f32::INFINITY;
            let mut col = 0u16;
            while col < cols {
                let Some(cell) = screen.cell(row, col) else {
                    break;
                };
                if cell.is_wide_continuation() {
                    col += 1;
                    continue;
                }
                let s = cell.contents();
                let s = if s.is_empty() { " " } else { s };
                let mut fg = vt_color(cell.fgcolor(), true);
                let mut bg = vt_color(cell.bgcolor(), false);
                if cell.inverse() {
                    std::mem::swap(&mut fg, &mut bg);
                }
                let selected = self.selection.is_some_and(|selection| {
                    if selection.anchor == selection.focus {
                        return false;
                    }
                    let (start, end) = ordered_selection(selection);
                    (row, col) >= start && (row, col) <= end
                });
                let fmt = TextFormat {
                    font_id: font.clone(),
                    color: fg.unwrap_or_else(theme::c_text),
                    background: if selected {
                        theme::c_accent().linear_multiply(0.35)
                    } else {
                        bg.unwrap_or(Color32::TRANSPARENT)
                    },
                    italics: cell.italic(),
                    underline: if cell.underline() {
                        Stroke::new(1.0, fg.unwrap_or_else(theme::c_text))
                    } else {
                        Stroke::NONE
                    },
                    ..Default::default()
                };
                job.append(s, 0.0, fmt);
                col += 1;
            }
            let galley = ui.fonts_mut(|f| f.layout_job(job));
            painter.galley(egui::pos2(rect.left(), y), galley, theme::c_text());
        }

        // Cursor block.
        if !screen.hide_cursor() {
            let (crow, ccol) = screen.cursor_position();
            let cx = rect.left() + ccol as f32 * cell_w;
            let cy = rect.top() + crow as f32 * cell_h;
            let cursor_rect = Rect::from_min_size(egui::pos2(cx, cy), egui::vec2(cell_w, cell_h));
            if focused {
                painter.rect_filled(cursor_rect, 0.0, theme::c_accent().linear_multiply(0.55));
            } else {
                painter.rect_stroke(
                    cursor_rect,
                    0.0,
                    Stroke::new(1.0, theme::c_accent()),
                    egui::StrokeKind::Middle,
                );
            }
        }
    }
}
