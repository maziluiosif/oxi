//! Colors, typography scale, and egui style setup.

use std::time::Duration;

use eframe::egui::text::{LayoutJob, TextFormat};
use eframe::egui::{
    self, Color32, FontData, FontDefinitions, FontFamily, FontId, Label, Stroke, Ui, Visuals,
};

const NOTO_SANS_REGULAR: &[u8] = include_bytes!("../assets/fonts/NotoSans-Regular.ttf");
const UBUNTU_MONO_REGULAR: &[u8] = include_bytes!("../assets/fonts/UbuntuMono-R.ttf");
const APPLE_SYMBOLS: &[u8] = include_bytes!("../assets/fonts/AppleSymbols.ttf");
const SYMBOLS_NERD_FONT_MONO: &[u8] =
    include_bytes!("../assets/fonts/SymbolsNerdFontMono-Regular.ttf");

/// App-wide type scale (slightly larger for readability).
pub const FS_BODY: f32 = 14.5;
pub const FS_SMALL: f32 = 12.75;
pub const FS_TINY: f32 = 11.75;

/// [`FontFamily`] for Nerd Font icon glyphs used in tool pills.
/// Using a named family keeps PUA codepoints out of the normal text fallback chains.
#[inline]
pub fn icon_font() -> FontFamily {
    FontFamily::Name("icons".into())
}

// Codex-style dark palette: neutral near-black surfaces, off-white ink, a single restrained
// blue accent, and clean semantic diff colors (mirrors OpenAI's `codex-theme-v1` defaults).
pub const C_BG_MAIN: Color32 = Color32::from_rgb(0x0f, 0x10, 0x12);
/// Sidebar column — a hair darker than the chat surface for a calm, flat rail.
pub const C_BG_SIDEBAR: Color32 = Color32::from_rgb(0x0b, 0x0c, 0x0d);
pub const C_BG_ELEVATED: Color32 = Color32::from_rgb(0x18, 0x19, 0x1c);
/// Second-level elevated surface (cards inside panels).
pub const C_BG_ELEVATED_2: Color32 = Color32::from_rgb(0x1c, 0x1d, 0x22);
/// Composer field surface.
pub const C_BG_INPUT: Color32 = Color32::from_rgb(0x14, 0x15, 0x17);
pub const C_BORDER: Color32 = Color32::from_rgb(0x26, 0x28, 0x2c);
pub const C_BORDER_SUBTLE: Color32 = Color32::from_rgb(0x1b, 0x1c, 0x1f);
/// Codex interactive accent (`codex-theme-v1` accent), brightened slightly for dark surfaces.
pub const C_ACCENT: Color32 = Color32::from_rgb(0x6c, 0xa2, 0xe0);
/// Primary ink (`codex-theme-v1` ink).
pub const C_TEXT: Color32 = Color32::from_rgb(0xd8, 0xde, 0xe9);
pub const C_TEXT_MUTED: Color32 = Color32::from_rgb(0x8b, 0x8f, 0x99);
pub const C_TEXT_FAINT: Color32 = Color32::from_rgb(0x6b, 0x6d, 0x78);
/// Workspace / folder headers in the sidebar (dim section label).
pub const C_SIDEBAR_SECTION: Color32 = Color32::from_rgb(0x6d, 0x71, 0x7b);
/// User message surface — a subtle, flat lift rather than a heavy chat bubble.
pub const C_USER_BUBBLE: Color32 = Color32::from_rgb(0x1c, 0x1e, 0x22);
/// Selected chat row (sidebar) — quiet neutral pill.
pub const C_ROW_ACTIVE: Color32 = Color32::from_rgb(0x21, 0x24, 0x2a);
/// Unselected row hover (sidebar list).
pub const C_ROW_HOVER: Color32 = Color32::from_rgb(0x18, 0x1a, 0x1e);
/// Subtle success / positive color (connection ok, badges).
pub const C_SUCCESS: Color32 = Color32::from_rgb(0x4a, 0xc8, 0x8c);
/// Danger color for destructive actions.
pub const C_DANGER: Color32 = Color32::from_rgb(0xe0, 0x6c, 0x6c);

// Codex semantic diff colors (`codex-theme-v1.semanticColors`).
pub const C_DIFF_ADD_FG: Color32 = Color32::from_rgb(0x99, 0xc7, 0x94);
pub const C_DIFF_ADD_BG: Color32 = Color32::from_rgb(0x12, 0x24, 0x1b);
pub const C_DIFF_DEL_FG: Color32 = Color32::from_rgb(0xec, 0x5f, 0x66);
pub const C_DIFF_DEL_BG: Color32 = Color32::from_rgb(0x2c, 0x16, 0x18);
/// Max width for message/composer column (left-aligned; extra space stays on the right).
pub const CHAT_COLUMN_MAX: f32 = 720.0;

/// Draggable strip between sidebar and chat (must match `render_main_area`).
pub const SIDEBAR_RESIZE_SEP_W: f32 = 5.0;
pub const CHAT_VIEW_MARGIN_LEFT: f32 = 12.0;
pub const CHAT_VIEW_MARGIN_RIGHT: f32 = 24.0;
/// Inner margin of the chat [`Frame`] (transcript + composer stack).
pub const CHAT_FRAME_TOP: f32 = 10.0;
pub const CHAT_FRAME_BOTTOM: f32 = 10.0;

/// Usable width for the chat column (transcript + bottom composer). Subtracts scrollbar gutter so
/// input and transcript line up.
pub fn chat_column_center_width(available: f32, style: &egui::Style) -> f32 {
    let g = style.spacing.scroll.allocated_width();
    (available - g).max(1.0)
}

/// Wrap width for transcript text (thinking, markdown, tool bodies). `available_width()` alone can
/// exceed the visible column inside [`egui::CollapsingHeader`] and similar; clamp to the current
/// [`Ui::max_rect`] and [`CHAT_COLUMN_MAX`] so [`LayoutJob`] wrap and frames stay inside the chat.
pub fn content_wrap_width(ui: &Ui) -> f32 {
    let avail = ui.available_width();
    if !avail.is_finite() || avail <= 0.0 {
        return CHAT_COLUMN_MAX.max(48.0);
    }
    let rect_w = ui.max_rect().width();
    let bounded = if rect_w.is_finite() && rect_w > 1.0 {
        avail.min(rect_w)
    } else {
        avail
    };
    bounded.clamp(48.0, CHAT_COLUMN_MAX)
}

pub const MAX_IMAGE_ATTACHMENT_BYTES: usize = 6 * 1024 * 1024;
pub const MAX_PENDING_IMAGES: usize = 8;

/// Small loading indicator (avoids default large `interact_size` spinners).
pub fn small_spinner(ui: &mut Ui) {
    use eframe::egui::Spinner;
    ui.add(Spinner::new().size(13.0).color(C_TEXT_MUTED));
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "noto_sans".to_string(),
        FontData::from_static(NOTO_SANS_REGULAR),
    );
    fonts.font_data.insert(
        "ubuntu_mono".to_string(),
        FontData::from_static(UBUNTU_MONO_REGULAR),
    );
    fonts.font_data.insert(
        "apple_symbols".to_string(),
        FontData::from_static(APPLE_SYMBOLS),
    );
    fonts.font_data.insert(
        "symbols_nerd_font_mono".to_string(),
        FontData::from_static(SYMBOLS_NERD_FONT_MONO),
    );

    let proportional = fonts.families.entry(FontFamily::Proportional).or_default();
    proportional.insert(0, "noto_sans".to_string());
    proportional.push("apple_symbols".to_string());

    let monospace = fonts.families.entry(FontFamily::Monospace).or_default();
    monospace.insert(0, "ubuntu_mono".to_string());
    monospace.push("noto_sans".to_string());
    monospace.push("apple_symbols".to_string());

    // Dedicated icon family — used only for tool pill glyphs.
    // Keeps Nerd Font PUA codepoints out of the proportional/monospace fallback chains
    // so they never accidentally substitute real text characters.
    fonts
        .families
        .entry(FontFamily::Name("icons".into()))
        .or_default()
        .push("symbols_nerd_font_mono".to_string());

    ctx.set_fonts(fonts);
}

/// Codex-style dark theme: neutral near-black surfaces, subtle borders, a single blue accent.
pub fn setup_style(ctx: &egui::Context) {
    install_fonts(ctx);
    let mut visuals = Visuals::dark();
    visuals.window_fill = C_BG_MAIN;
    visuals.panel_fill = C_BG_MAIN;
    visuals.extreme_bg_color = C_BG_INPUT;
    visuals.faint_bg_color = Color32::from_rgb(0x16, 0x17, 0x1a);
    visuals.override_text_color = Some(C_TEXT);
    visuals.window_rounding = egui::Rounding::same(10.0);
    visuals.menu_rounding = egui::Rounding::same(8.0);
    visuals.widgets.noninteractive.rounding = egui::Rounding::same(6.0);
    visuals.widgets.inactive.rounding = egui::Rounding::same(6.0);
    visuals.widgets.hovered.rounding = egui::Rounding::same(6.0);
    visuals.widgets.active.rounding = egui::Rounding::same(6.0);
    visuals.widgets.open.rounding = egui::Rounding::same(6.0);
    // Side panel separator, indentation guides — match app chrome.
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, C_BORDER_SUBTLE);
    visuals.widgets.noninteractive.bg_fill = C_BG_ELEVATED;
    visuals.widgets.noninteractive.fg_stroke.color = C_TEXT_MUTED;
    visuals.widgets.inactive.bg_fill = Color32::from_rgb(0x20, 0x22, 0x26);
    visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(0x20, 0x22, 0x26);
    visuals.widgets.inactive.fg_stroke.color = C_TEXT;
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(0x2a, 0x2c, 0x31);
    visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x2a, 0x2c, 0x31);
    visuals.widgets.active.bg_fill = Color32::from_rgb(0x30, 0x33, 0x39);
    visuals.widgets.active.weak_bg_fill = Color32::from_rgb(0x30, 0x33, 0x39);
    visuals.widgets.open.bg_fill = Color32::from_rgb(0x24, 0x26, 0x2b);
    // Text selection uses a desaturated tint of the Codex accent.
    visuals.selection.bg_fill = Color32::from_rgb(0x21, 0x35, 0x4c);
    visuals.selection.stroke = Stroke::new(1.0, Color32::from_rgb(0x3c, 0x66, 0x96));
    visuals.window_stroke = Stroke::new(1.0, C_BORDER_SUBTLE);
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, C_BORDER_SUBTLE);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, C_BORDER);
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0x33, 0x36, 0x3c));

    let mut style = (*ctx.style()).clone();
    style.visuals = visuals;
    style.interaction.selectable_labels = false;
    style.spacing.item_spacing = egui::vec2(6.0, 3.0);
    style.spacing.button_padding = egui::vec2(8.0, 4.0);
    style.spacing.indent = 12.0;
    style.spacing.interact_size.y = 24.0;
    style.spacing.menu_margin = egui::Margin::same(6.0);
    style.spacing.window_margin = egui::Margin::same(10.0);
    style.spacing.combo_width = 220.0;
    style.spacing.scroll.bar_width = 8.0;
    style.spacing.scroll.handle_min_length = 24.0;
    style.spacing.scroll.floating = true;
    ctx.set_style(style);
}

pub fn blend_color(from: Color32, to: Color32, t: f32) -> Color32 {
    let mix = t.clamp(0.0, 1.0);
    let lerp = |a: u8, b: u8| -> u8 {
        let af = a as f32;
        let bf = b as f32;
        (af + (bf - af) * mix).round().clamp(0.0, 255.0) as u8
    };
    Color32::from_rgba_unmultiplied(
        lerp(from.r(), to.r()),
        lerp(from.g(), to.g()),
        lerp(from.b(), to.b()),
        lerp(from.a(), to.a()),
    )
}

pub fn animated_status_job(label: &str, size: f32, time: f64) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = f32::INFINITY;
    let chars: Vec<char> = label.chars().collect();
    let len = chars.len().max(1) as f64;
    let highlight = (time * 7.0) % (len + 3.0);
    for (idx, ch) in chars.iter().enumerate() {
        let dist = (idx as f64 - highlight).abs();
        let mix = if dist < 0.6 {
            1.0
        } else if dist < 1.4 {
            0.55
        } else if dist < 2.2 {
            0.22
        } else {
            0.0
        };
        let color = blend_color(C_TEXT_MUTED, C_ACCENT, mix as f32);
        job.append(
            &ch.to_string(),
            0.0,
            TextFormat::simple(FontId::proportional(size), color),
        );
    }
    job
}

pub fn animated_status_label(ui: &mut Ui, label: &str, size: f32) {
    let time = ui.input(|i| i.time);
    ui.add(Label::new(animated_status_job(label, size, time)).selectable(false));
}

pub fn format_stream_elapsed(d: Duration) -> String {
    let total_ms = d.as_millis() as u64;
    if total_ms < 1000 {
        return format!("{total_ms}ms");
    }
    let s = total_ms / 1000;
    if s < 60 {
        return format!("{s}s");
    }
    let m = s / 60;
    let rs = s % 60;
    format!("{m}m{rs:02}")
}

/// Short label for a workspace root path (last two path segments, e.g. `owner/repo`).
pub fn workspace_sidebar_label(root_path: &str) -> String {
    let path = std::path::Path::new(root_path);
    let parts: Vec<&str> = path
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect();
    match parts.len() {
        0 => root_path.to_string(),
        1 => parts[0].to_string(),
        _ => format!("{}/{}", parts[parts.len() - 2], parts[parts.len() - 1]),
    }
}

/// Full title for sidebar rows; empty/whitespace shows as "New chat". Ellipsis is handled by
/// [`egui::Label::truncate`] with the row’s title width.
pub fn sidebar_session_title_display(title: &str) -> String {
    let t = title.trim();
    if t.is_empty() {
        "New chat".to_string()
    } else {
        t.to_string()
    }
}

pub fn tool_status_label(name: &str) -> String {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        "Running".to_string()
    } else {
        let mut chars = trimmed.chars();
        let first = chars
            .next()
            .map(|ch| ch.to_uppercase().collect::<String>())
            .unwrap_or_default();
        format!("{}{rest}", first, rest = chars.as_str())
    }
}
