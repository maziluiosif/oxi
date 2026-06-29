//! Embedded interactive terminal: a real PTY-backed shell rendered into egui.
//!
//! A background thread pumps the PTY's output into a [`vt100::Parser`], which maintains the
//! screen grid (text + colors + cursor). The UI thread renders that grid each frame as
//! monospace text and forwards keyboard input back to the PTY.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use eframe::egui::{
    self, Color32, Event, EventFilter, FontId, Key, PointerButton, Pos2, Rect, Sense, Stroke,
    TextFormat, Ui,
};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use vt100::{MouseProtocolEncoding as MEnc, MouseProtocolMode as MMode};

/// Mouse tracking carried between frames so we can report drags / motion.
#[derive(Default)]
struct MouseState {
    /// Base button code (0/1/2) currently held, if any.
    held: Option<u8>,
    /// Last reported cell (col, row), 0-based, to detect motion across cells.
    last_cell: Option<(u16, u16)>,
}

use crate::theme;

/// Monospace font size for the terminal grid.
const TERM_FONT_SIZE: f32 = 13.0;
/// Lines of scrollback kept by the parser.
const SCROLLBACK: usize = 5000;

/// A live PTY session plus its parsed screen state.
pub struct TerminalSession {
    parser: Arc<Mutex<vt100::Parser>>,
    /// Writes here are forwarded to the shell's stdin.
    writer: Box<dyn Write + Send>,
    /// Kept so we can resize the kernel pty window when the panel size changes.
    master: Box<dyn MasterPty + Send>,
    /// Flipped to `false` by the reader thread when the shell exits (EOF).
    alive: Arc<AtomicBool>,
    rows: u16,
    cols: u16,
    mouse: MouseState,
    /// Scrollback view offset (rows up from the bottom) used when no app grabs the mouse.
    scroll_offset: usize,
}

impl TerminalSession {
    /// Spawn the user's default shell in `cwd`, wired to a fresh PTY.
    pub fn spawn(ctx: &egui::Context, cwd: &str, rows: u16, cols: u16) -> Result<Self, String> {
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

        let mut cmd = CommandBuilder::new_default_prog();
        if !cwd.trim().is_empty() {
            cmd.cwd(cwd);
        }
        cmd.env("TERM", "xterm-256color");

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
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| format!("pty writer: {e}"))?;

        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, SCROLLBACK)));
        let alive = Arc::new(AtomicBool::new(true));

        let parser_rd = parser.clone();
        let alive_rd = alive.clone();
        let ctx = ctx.clone();
        std::thread::Builder::new()
            .name("oxi-pty-reader".to_string())
            .spawn(move || {
                let mut buf = [0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if let Ok(mut p) = parser_rd.lock() {
                                p.process(&buf[..n]);
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
            scroll_offset: 0,
        })
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// Send raw bytes to the shell.
    fn send(&mut self, bytes: &[u8]) {
        let _ = self.writer.write_all(bytes);
        let _ = self.writer.flush();
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
            p.set_size(rows, cols);
        }
    }

    /// Render the terminal into `rect` and forward keyboard input when focused.
    pub fn ui(&mut self, ui: &mut Ui, rect: Rect) {
        let font = FontId::monospace(TERM_FONT_SIZE);
        let (cell_w, cell_h) = ui.fonts(|f| {
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
        if resp.clicked() || resp.drag_started() {
            resp.request_focus();
        }

        self.handle_mouse(ui, rect, cell_w, cell_h, resp.hovered());
        // Apply scrollback view (no-op when an app has grabbed the mouse / offset is 0).
        // vt100 0.15's `visible_rows` mixes `offset` scrollback rows with `rows - offset`
        // live rows, so an offset larger than the row count underflows and panics. Clamp
        // to the visible row count before handing it over; `set_scrollback` then clamps
        // again to the buffer length.
        if let Ok(mut p) = self.parser.lock() {
            let max_off = self.rows as usize;
            self.scroll_offset = self.scroll_offset.min(max_off);
            p.set_scrollback(self.scroll_offset);
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
                Event::Text(t) => out.extend_from_slice(t.as_bytes()),
                Event::Paste(t) => out.extend_from_slice(t.as_bytes()),
                Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
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
            // Typing jumps back to the live bottom of the buffer.
            self.scroll_offset = 0;
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
            let scroll_y = ui.input(|i| i.raw_scroll_delta.y);
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
                let mut s = cell.contents();
                if s.is_empty() {
                    s.push(' ');
                }
                let mut fg = vt_color(cell.fgcolor(), true);
                let mut bg = vt_color(cell.bgcolor(), false);
                if cell.inverse() {
                    std::mem::swap(&mut fg, &mut bg);
                }
                let fmt = TextFormat {
                    font_id: font.clone(),
                    color: fg.unwrap_or_else(theme::c_text),
                    background: bg.unwrap_or(Color32::TRANSPARENT),
                    italics: cell.italic(),
                    underline: if cell.underline() {
                        Stroke::new(1.0, fg.unwrap_or_else(theme::c_text))
                    } else {
                        Stroke::NONE
                    },
                    ..Default::default()
                };
                job.append(&s, 0.0, fmt);
                col += 1;
            }
            let galley = ui.fonts(|f| f.layout_job(job));
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
                painter.rect_stroke(cursor_rect, 0.0, Stroke::new(1.0, theme::c_accent()));
            }
        }
    }
}

/// Base button code for a mouse button (None = unsupported / extra buttons).
fn button_base(button: PointerButton) -> Option<u8> {
    match button {
        PointerButton::Primary => Some(0),
        PointerButton::Middle => Some(1),
        PointerButton::Secondary => Some(2),
        _ => None,
    }
}

/// Encode one mouse event for the terminal. `col`/`row` are 0-based; protocols are 1-based.
fn encode_mouse(
    encoding: MEnc,
    base: u8,
    col: u16,
    row: u16,
    pressed: bool,
    motion: bool,
    mods: egui::Modifiers,
) -> Vec<u8> {
    let mut cb = base;
    if mods.shift {
        cb += 4;
    }
    if mods.alt {
        cb += 8;
    }
    if mods.ctrl {
        cb += 16;
    }
    if motion {
        cb += 32;
    }
    let cx = col as u32 + 1;
    let cy = row as u32 + 1;
    match encoding {
        MEnc::Sgr => {
            let final_char = if pressed { 'M' } else { 'm' };
            format!("\x1b[<{cb};{cx};{cy}{final_char}").into_bytes()
        }
        // Default / Utf8 X10-style: button byte uses release code 3 in the low bits.
        _ => {
            let cb_out = if pressed { cb } else { (cb & !0b11) | 0b11 };
            let clamp = |v: u32| (v.min(223) as u8).saturating_add(32);
            vec![
                0x1b,
                b'[',
                b'M',
                cb_out.saturating_add(32),
                clamp(cx),
                clamp(cy),
            ]
        }
    }
}

/// Map a vt100 color to an egui color. `fg` selects the default-color fallback (None = inherit).
fn vt_color(color: vt100::Color, _fg: bool) -> Option<Color32> {
    match color {
        vt100::Color::Default => None,
        vt100::Color::Rgb(r, g, b) => Some(Color32::from_rgb(r, g, b)),
        vt100::Color::Idx(i) => Some(xterm_256_color(i)),
    }
}

/// Translate special keys to terminal byte sequences (no Ctrl modifier).
fn key_sequence(key: Key, app_cursor: bool) -> Option<&'static [u8]> {
    let arrows = |normal: &'static [u8], app: &'static [u8]| {
        if app_cursor {
            app
        } else {
            normal
        }
    };
    let seq: &[u8] = match key {
        Key::Enter => b"\r",
        Key::Backspace => b"\x7f",
        Key::Tab => b"\t",
        Key::Escape => b"\x1b",
        Key::ArrowUp => arrows(b"\x1b[A", b"\x1bOA"),
        Key::ArrowDown => arrows(b"\x1b[B", b"\x1bOB"),
        Key::ArrowRight => arrows(b"\x1b[C", b"\x1bOC"),
        Key::ArrowLeft => arrows(b"\x1b[D", b"\x1bOD"),
        Key::Home => b"\x1b[H",
        Key::End => b"\x1b[F",
        Key::Delete => b"\x1b[3~",
        Key::Insert => b"\x1b[2~",
        Key::PageUp => b"\x1b[5~",
        Key::PageDown => b"\x1b[6~",
        _ => return None,
    };
    Some(seq)
}

/// Control byte for Ctrl + letter / common symbols.
fn ctrl_byte(key: Key) -> Option<u8> {
    let b = match key {
        Key::A => 1,
        Key::B => 2,
        Key::C => 3,
        Key::D => 4,
        Key::E => 5,
        Key::F => 6,
        Key::G => 7,
        Key::H => 8,
        Key::I => 9,
        Key::J => 10,
        Key::K => 11,
        Key::L => 12,
        Key::M => 13,
        Key::N => 14,
        Key::O => 15,
        Key::P => 16,
        Key::Q => 17,
        Key::R => 18,
        Key::S => 19,
        Key::T => 20,
        Key::U => 21,
        Key::V => 22,
        Key::W => 23,
        Key::X => 24,
        Key::Y => 25,
        Key::Z => 26,
        Key::OpenBracket => 27,  // Ctrl+[ == ESC
        Key::Backslash => 28,    // Ctrl+\\
        Key::CloseBracket => 29, // Ctrl+]
        _ => return None,
    };
    Some(b)
}

/// Standard xterm 256-color palette.
fn xterm_256_color(i: u8) -> Color32 {
    match i {
        0 => Color32::from_rgb(0, 0, 0),
        1 => Color32::from_rgb(205, 49, 49),
        2 => Color32::from_rgb(13, 188, 121),
        3 => Color32::from_rgb(229, 229, 16),
        4 => Color32::from_rgb(36, 114, 200),
        5 => Color32::from_rgb(188, 63, 188),
        6 => Color32::from_rgb(17, 168, 205),
        7 => Color32::from_rgb(229, 229, 229),
        8 => Color32::from_rgb(102, 102, 102),
        9 => Color32::from_rgb(241, 76, 76),
        10 => Color32::from_rgb(35, 209, 139),
        11 => Color32::from_rgb(245, 245, 67),
        12 => Color32::from_rgb(59, 142, 234),
        13 => Color32::from_rgb(214, 112, 214),
        14 => Color32::from_rgb(41, 184, 219),
        15 => Color32::from_rgb(255, 255, 255),
        16..=231 => {
            let i = i - 16;
            let r = i / 36;
            let g = (i % 36) / 6;
            let b = i % 6;
            let conv = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            Color32::from_rgb(conv(r), conv(g), conv(b))
        }
        232..=255 => {
            let v = 8 + (i - 232) * 10;
            Color32::from_rgb(v, v, v)
        }
    }
}
