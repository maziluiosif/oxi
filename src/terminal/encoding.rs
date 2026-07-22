//! Terminal selection, keyboard, mouse, and color encoding helpers.

use eframe::egui::{self, Color32, Key, PointerButton};
use vt100::MouseProtocolEncoding as MEnc;

use super::TerminalSelection;

pub(super) fn ordered_selection(selection: TerminalSelection) -> ((u16, u16), (u16, u16)) {
    if selection.anchor <= selection.focus {
        (selection.anchor, selection.focus)
    } else {
        (selection.focus, selection.anchor)
    }
}

pub(super) fn button_base(button: PointerButton) -> Option<u8> {
    match button {
        PointerButton::Primary => Some(0),
        PointerButton::Middle => Some(1),
        PointerButton::Secondary => Some(2),
        _ => None,
    }
}

pub(super) fn encode_mouse(
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
        _ => {
            let cb_out = if pressed { cb } else { (cb & !0b11) | 0b11 };
            let clamp = |value: u32| (value.min(223) as u8).saturating_add(32);
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

pub(super) fn vt_color(color: vt100::Color, _foreground: bool) -> Option<Color32> {
    match color {
        vt100::Color::Default => None,
        vt100::Color::Rgb(r, g, b) => Some(Color32::from_rgb(r, g, b)),
        vt100::Color::Idx(index) => Some(xterm_256_color(index)),
    }
}

pub(super) fn key_sequence(key: Key, app_cursor: bool) -> Option<&'static [u8]> {
    let arrows = |normal: &'static [u8], app: &'static [u8]| {
        if app_cursor { app } else { normal }
    };
    let sequence: &[u8] = match key {
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
    Some(sequence)
}

pub(super) fn ctrl_byte(key: Key) -> Option<u8> {
    let byte = match key {
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
        Key::OpenBracket => 27,
        Key::Backslash => 28,
        Key::CloseBracket => 29,
        _ => return None,
    };
    Some(byte)
}

fn xterm_256_color(index: u8) -> Color32 {
    match index {
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
            let index = index - 16;
            let r = index / 36;
            let g = (index % 36) / 6;
            let b = index % 6;
            let convert = |value: u8| if value == 0 { 0 } else { 55 + value * 40 };
            Color32::from_rgb(convert(r), convert(g), convert(b))
        }
        232..=255 => {
            let value = 8 + (index - 232) * 10;
            Color32::from_rgb(value, value, value)
        }
    }
}
