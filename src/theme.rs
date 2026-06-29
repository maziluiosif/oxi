//! Colors, typography scale, and egui style setup.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::RwLock;
use std::time::Duration;

use eframe::egui::text::{LayoutJob, TextFormat};
use eframe::egui::{
    self, Color32, FontData, FontDefinitions, FontFamily, FontId, Label, Stroke, Ui, Visuals,
};
use serde::{Deserialize, Serialize};

const NOTO_SANS_REGULAR: &[u8] = include_bytes!("../assets/fonts/NotoSans-Regular.ttf");
const UBUNTU_MONO_REGULAR: &[u8] = include_bytes!("../assets/fonts/UbuntuMono-R.ttf");
const APPLE_SYMBOLS: &[u8] = include_bytes!("../assets/fonts/AppleSymbols.ttf");
/// Monochrome Noto Emoji (outline). egui/eframe renders glyphs single-channel, so emoji show
/// as black-and-white outlines rather than in color, but this covers the full emoji range
/// instead of the small subset bundled with egui's defaults.
const NOTO_EMOJI: &[u8] = include_bytes!("../assets/fonts/NotoEmoji-Regular.ttf");
const SYMBOLS_NERD_FONT_MONO: &[u8] =
    include_bytes!("../assets/fonts/SymbolsNerdFontMono-Regular.ttf");

/// App-wide type scale — single source of truth for every text size in the UI.
/// Headings step down 20 → 17 → 15; body is 14; secondary text 12.5 / 11.5; code 13.
/// Keep all `.size(...)` / `FontId` text sizes routed through these so the app stays uniform.
pub const FS_H1: f32 = 20.0;
pub const FS_H2: f32 = 17.0;
pub const FS_H3: f32 = 15.0;
pub const FS_BODY: f32 = 14.0;
pub const FS_SMALL: f32 = 12.5;
pub const FS_TINY: f32 = 11.5;
/// Monospace code (blocks). Inline code matches the size of the surrounding prose/heading.
pub const FS_CODE: f32 = 13.0;

/// [`FontFamily`] for Nerd Font icon glyphs used in tool pills.
/// Using a named family keeps PUA codepoints out of the normal text fallback chains.
#[inline]
pub fn icon_font() -> FontFamily {
    FontFamily::Name("icons".into())
}

// ─── Icon glyphs (Nerd Font, rendered with [`icon_font`]) ───────────────────────
//
// These are PUA codepoints from `SymbolsNerdFontMono`, bundled with the app and
// installed under the dedicated `icons` family. We use them for small inline UI
// glyphs instead of bare Unicode symbols (✦ ✎ ◐ ✕ ✓ …), whose codepoints are
// frequently absent from the bundled text/emoji/symbol fonts and render as empty
// boxes at small sizes. Each constant below is verified present in the font.
//
// Always pair these with `​.font(FontId::new(<size>, icon_font()))` so they go
// through the icon family rather than the proportional fallback chain.

/// Models & providers (settings nav) — nf-md-cube.
pub const ICON_PROVIDERS: &str = "\u{f0148}";
/// Agent (settings nav) — nf-fa-robot.
pub const ICON_AGENT: &str = "\u{f2bb}";
/// Appearance (settings nav) — nf-fa-adjust (half-filled circle, matches ◐).
pub const ICON_APPEARANCE: &str = "\u{f042}";
/// Close / dismiss (e.g. diff viewer) — nf-fa-xmark.
pub const ICON_CLOSE: &str = "\u{f00d}";
/// Affirmative / done (commit button, tool enable check) — nf-fa-check.
pub const ICON_CHECK: &str = "\u{f00c}";

// ─── Palette ─────────────────────────────────────────────────────────────────
//
// Every surface / ink / accent color the UI draws with lives in [`Palette`], so the
// whole app can be re-skinned at runtime. These were historically `pub const C_*`
// values; they are now resolved through the globally-active palette (see
// [`active_palette`] and the `c_*` accessor fns) so switching theme is a single swap.

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

/// A complete color scheme. `Copy` and cheap, so accessors return it by value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    /// `true` for a dark scheme (selects egui's `Visuals::dark()` as the base).
    pub dark_base: bool,
    pub bg_main: Color32,
    pub bg_sidebar: Color32,
    pub bg_elevated: Color32,
    pub bg_elevated_2: Color32,
    pub bg_input: Color32,
    pub faint_bg: Color32,
    pub border: Color32,
    pub border_subtle: Color32,
    pub accent: Color32,
    pub text: Color32,
    pub text_strong: Color32,
    pub text_muted: Color32,
    pub text_faint: Color32,
    pub sidebar_section: Color32,
    pub user_bubble: Color32,
    pub row_active: Color32,
    pub row_hover: Color32,
    pub success: Color32,
    pub danger: Color32,
    pub diff_add_fg: Color32,
    pub diff_add_bg: Color32,
    pub diff_del_fg: Color32,
    pub diff_del_bg: Color32,
    // Interactive widget states (consumed by `setup_style`).
    pub widget_inactive_bg: Color32,
    pub widget_hovered_bg: Color32,
    pub widget_active_bg: Color32,
    pub widget_open_bg: Color32,
    pub widget_active_border: Color32,
    pub selection_bg: Color32,
    pub selection_stroke: Color32,
    // Markdown rendering.
    pub md_code_bg: Color32,
    pub md_code_block_bg: Color32,
    pub md_code_block_header_bg: Color32,
    pub md_code_block_border: Color32,
    pub md_quote_accent: Color32,
    /// Inline / block code text color.
    pub md_code_fg: Color32,
}

impl Palette {
    /// Codex-style dark theme: neutral near-black surfaces, off-white ink, a single
    /// restrained blue accent (the original hardcoded palette, `codex-theme-v1`).
    pub const DARK: Palette = Palette {
        dark_base: true,
        bg_main: rgb(0x0f, 0x10, 0x12),
        bg_sidebar: rgb(0x0b, 0x0c, 0x0d),
        bg_elevated: rgb(0x18, 0x19, 0x1c),
        bg_elevated_2: rgb(0x1c, 0x1d, 0x22),
        bg_input: rgb(0x14, 0x15, 0x17),
        faint_bg: rgb(0x16, 0x17, 0x1a),
        border: rgb(0x26, 0x28, 0x2c),
        border_subtle: rgb(0x1b, 0x1c, 0x1f),
        accent: rgb(0x6c, 0xa2, 0xe0),
        text: rgb(0xd8, 0xde, 0xe9),
        text_strong: rgb(0xee, 0xee, 0xf4),
        text_muted: rgb(0x8b, 0x8f, 0x99),
        text_faint: rgb(0x6b, 0x6d, 0x78),
        sidebar_section: rgb(0x6d, 0x71, 0x7b),
        user_bubble: rgb(0x1c, 0x1e, 0x22),
        row_active: rgb(0x21, 0x24, 0x2a),
        row_hover: rgb(0x18, 0x1a, 0x1e),
        success: rgb(0x4a, 0xc8, 0x8c),
        danger: rgb(0xe0, 0x6c, 0x6c),
        diff_add_fg: rgb(0x99, 0xc7, 0x94),
        diff_add_bg: rgb(0x12, 0x24, 0x1b),
        diff_del_fg: rgb(0xec, 0x5f, 0x66),
        diff_del_bg: rgb(0x2c, 0x16, 0x18),
        widget_inactive_bg: rgb(0x20, 0x22, 0x26),
        widget_hovered_bg: rgb(0x2a, 0x2c, 0x31),
        widget_active_bg: rgb(0x30, 0x33, 0x39),
        widget_open_bg: rgb(0x24, 0x26, 0x2b),
        widget_active_border: rgb(0x33, 0x36, 0x3c),
        selection_bg: rgb(0x21, 0x35, 0x4c),
        selection_stroke: rgb(0x3c, 0x66, 0x96),
        md_code_bg: rgb(0x24, 0x26, 0x2d),
        md_code_block_bg: rgb(0x13, 0x14, 0x18),
        md_code_block_header_bg: rgb(0x19, 0x1b, 0x21),
        md_code_block_border: rgb(0x2a, 0x2d, 0x34),
        md_quote_accent: rgb(0x4f, 0x83, 0xc4),
        md_code_fg: rgb(0xe1, 0xe4, 0xea),
    };

    /// Light theme: warm near-white surfaces with dark ink, derived from [`Palette::DARK`]
    /// (accent and semantic colors darkened so they read on light backgrounds).
    pub const LIGHT: Palette = Palette {
        dark_base: false,
        bg_main: rgb(0xfb, 0xfb, 0xfa),
        bg_sidebar: rgb(0xf1, 0xf1, 0xef),
        bg_elevated: rgb(0xff, 0xff, 0xff),
        bg_elevated_2: rgb(0xf6, 0xf6, 0xf4),
        bg_input: rgb(0xff, 0xff, 0xff),
        faint_bg: rgb(0xf0, 0xf0, 0xee),
        border: rgb(0xd6, 0xd6, 0xd0),
        border_subtle: rgb(0xe6, 0xe6, 0xe1),
        accent: rgb(0x25, 0x63, 0xa8),
        text: rgb(0x1c, 0x1d, 0x20),
        text_strong: rgb(0x05, 0x06, 0x09),
        text_muted: rgb(0x5f, 0x63, 0x6b),
        text_faint: rgb(0x8b, 0x8f, 0x99),
        sidebar_section: rgb(0x80, 0x84, 0x8e),
        user_bubble: rgb(0xee, 0xf0, 0xf3),
        row_active: rgb(0xe3, 0xe6, 0xec),
        row_hover: rgb(0xec, 0xec, 0xf0),
        success: rgb(0x2f, 0x9e, 0x6f),
        danger: rgb(0xc0, 0x39, 0x2b),
        diff_add_fg: rgb(0x1a, 0x7f, 0x37),
        diff_add_bg: rgb(0xe6, 0xff, 0xec),
        diff_del_fg: rgb(0xcf, 0x22, 0x2e),
        diff_del_bg: rgb(0xff, 0xeb, 0xe9),
        widget_inactive_bg: rgb(0xed, 0xed, 0xea),
        widget_hovered_bg: rgb(0xe3, 0xe3, 0xdf),
        widget_active_bg: rgb(0xd7, 0xd7, 0xd2),
        widget_open_bg: rgb(0xe8, 0xe8, 0xe4),
        widget_active_border: rgb(0xc4, 0xc4, 0xbe),
        selection_bg: rgb(0xcf, 0xe0, 0xf5),
        selection_stroke: rgb(0x7a, 0xa8, 0xde),
        md_code_bg: rgb(0xec, 0xec, 0xea),
        md_code_block_bg: rgb(0xf4, 0xf4, 0xf2),
        md_code_block_header_bg: rgb(0xeb, 0xeb, 0xe8),
        md_code_block_border: rgb(0xde, 0xde, 0xda),
        md_quote_accent: rgb(0x25, 0x63, 0xa8),
        md_code_fg: rgb(0x1f, 0x21, 0x26),
    };

    /// Midnight: a pure-black OLED-friendly dark variant of [`Palette::DARK`].
    pub const MIDNIGHT: Palette = Palette {
        dark_base: true,
        bg_main: rgb(0x00, 0x00, 0x00),
        bg_sidebar: rgb(0x00, 0x00, 0x00),
        bg_elevated: rgb(0x0b, 0x0b, 0x0d),
        bg_elevated_2: rgb(0x10, 0x10, 0x13),
        bg_input: rgb(0x06, 0x06, 0x08),
        faint_bg: rgb(0x0a, 0x0a, 0x0c),
        border: rgb(0x1f, 0x20, 0x24),
        border_subtle: rgb(0x14, 0x15, 0x19),
        accent: rgb(0x6c, 0xa2, 0xe0),
        text: rgb(0xd8, 0xde, 0xe9),
        text_strong: rgb(0xee, 0xee, 0xf4),
        text_muted: rgb(0x8b, 0x8f, 0x99),
        text_faint: rgb(0x6b, 0x6d, 0x78),
        sidebar_section: rgb(0x6d, 0x71, 0x7b),
        user_bubble: rgb(0x0e, 0x0e, 0x11),
        row_active: rgb(0x17, 0x18, 0x1d),
        row_hover: rgb(0x10, 0x10, 0x13),
        success: rgb(0x4a, 0xc8, 0x8c),
        danger: rgb(0xe0, 0x6c, 0x6c),
        diff_add_fg: rgb(0x99, 0xc7, 0x94),
        diff_add_bg: rgb(0x0d, 0x1a, 0x13),
        diff_del_fg: rgb(0xec, 0x5f, 0x66),
        diff_del_bg: rgb(0x1f, 0x0f, 0x11),
        widget_inactive_bg: rgb(0x16, 0x16, 0x19),
        widget_hovered_bg: rgb(0x1f, 0x1f, 0x24),
        widget_active_bg: rgb(0x26, 0x26, 0x2c),
        widget_open_bg: rgb(0x1a, 0x1a, 0x1f),
        widget_active_border: rgb(0x2a, 0x2a, 0x30),
        selection_bg: rgb(0x1a, 0x2c, 0x40),
        selection_stroke: rgb(0x3c, 0x66, 0x96),
        md_code_bg: rgb(0x1a, 0x1c, 0x22),
        md_code_block_bg: rgb(0x0a, 0x0b, 0x0e),
        md_code_block_header_bg: rgb(0x10, 0x12, 0x18),
        md_code_block_border: rgb(0x20, 0x23, 0x29),
        md_quote_accent: rgb(0x4f, 0x83, 0xc4),
        md_code_fg: rgb(0xe1, 0xe4, 0xea),
    };
}

/// The globally-active palette. Read on every frame by the `c_*` accessors; written
/// only when the theme changes. egui renders on a single thread, so contention is nil.
static ACTIVE_PALETTE: RwLock<Palette> = RwLock::new(Palette::DARK);

/// Snapshot of the active palette.
pub fn active_palette() -> Palette {
    ACTIVE_PALETTE.read().map(|g| *g).unwrap_or(Palette::DARK)
}

/// Swap the active palette. Call [`setup_style`] afterwards to rebuild egui visuals.
pub fn set_active_palette(p: Palette) {
    if let Ok(mut guard) = ACTIVE_PALETTE.write() {
        *guard = p;
    }
}

macro_rules! palette_accessors {
    ($($name:ident => $field:ident),* $(,)?) => {
        $(
            #[inline]
            pub fn $name() -> Color32 {
                active_palette().$field
            }
        )*
    };
}

// Accessors mirroring the former `C_*` constants. They resolve against the active palette.
palette_accessors! {
    c_bg_main => bg_main,
    c_bg_sidebar => bg_sidebar,
    c_bg_elevated => bg_elevated,
    c_bg_elevated_2 => bg_elevated_2,
    c_bg_input => bg_input,
    c_border => border,
    c_border_subtle => border_subtle,
    c_accent => accent,
    c_text => text,
    c_text_strong => text_strong,
    c_text_muted => text_muted,
    c_text_faint => text_faint,
    c_sidebar_section => sidebar_section,
    c_user_bubble => user_bubble,
    c_row_active => row_active,
    c_row_hover => row_hover,
    c_success => success,
    c_danger => danger,
    c_diff_add_fg => diff_add_fg,
    c_diff_add_bg => diff_add_bg,
    c_diff_del_fg => diff_del_fg,
    c_diff_del_bg => diff_del_bg,
    c_md_code_bg => md_code_bg,
    c_md_code_block_bg => md_code_block_bg,
    c_md_code_block_header_bg => md_code_block_header_bg,
    c_md_code_block_border => md_code_block_border,
    c_md_quote_accent => md_quote_accent,
    c_md_code_fg => md_code_fg,
}

// ─── Theme registry ──────────────────────────────────────────────────────────

/// A selectable theme: stable `id`, display `name`, and its resolved `palette`.
#[derive(Debug, Clone, PartialEq)]
pub struct ThemeChoice {
    pub id: String,
    pub name: String,
    pub palette: Palette,
}

/// The default theme id, used when settings carry no (or an unknown) theme.
pub const DEFAULT_THEME_ID: &str = "dark";

/// Built-in themes, in display order.
pub fn builtin_themes() -> Vec<ThemeChoice> {
    vec![
        ThemeChoice {
            id: "dark".to_string(),
            name: "Dark".to_string(),
            palette: Palette::DARK,
        },
        ThemeChoice {
            id: "light".to_string(),
            name: "Light".to_string(),
            palette: Palette::LIGHT,
        },
        ThemeChoice {
            id: "midnight".to_string(),
            name: "Midnight".to_string(),
            palette: Palette::MIDNIGHT,
        },
    ]
}

/// Base scheme a custom theme builds on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ThemeBase {
    #[default]
    Dark,
    Light,
}

/// On-disk custom theme. Drop a `<id>.json` file in [`custom_themes_dir`] and it shows
/// up in the theme picker. `colors` maps palette field names (e.g. `"bg_main"`) to
/// `#rrggbb` strings; any field left out inherits from `base`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeSpec {
    pub name: String,
    #[serde(default)]
    pub base: ThemeBase,
    #[serde(default)]
    pub colors: BTreeMap<String, String>,
}

/// Parse a `#rrggbb` (or `rrggbb`) hex string into a [`Color32`].
pub fn parse_hex_color(s: &str) -> Option<Color32> {
    let h = s.trim().trim_start_matches('#');
    if h.len() != 6 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let r = u8::from_str_radix(&h[0..2], 16).ok()?;
    let g = u8::from_str_radix(&h[2..4], 16).ok()?;
    let b = u8::from_str_radix(&h[4..6], 16).ok()?;
    Some(rgb(r, g, b))
}

impl ThemeSpec {
    /// Resolve to a concrete palette, applying recognized color overrides over the base.
    pub fn into_palette(self) -> Palette {
        let mut p = match self.base {
            ThemeBase::Light => Palette::LIGHT,
            ThemeBase::Dark => Palette::DARK,
        };
        for (key, value) in &self.colors {
            let Some(c) = parse_hex_color(value) else {
                continue;
            };
            match key.as_str() {
                "bg_main" => p.bg_main = c,
                "bg_sidebar" => p.bg_sidebar = c,
                "bg_elevated" => p.bg_elevated = c,
                "bg_elevated_2" => p.bg_elevated_2 = c,
                "bg_input" => p.bg_input = c,
                "faint_bg" => p.faint_bg = c,
                "border" => p.border = c,
                "border_subtle" => p.border_subtle = c,
                "accent" => p.accent = c,
                "text" => p.text = c,
                "text_strong" => p.text_strong = c,
                "text_muted" => p.text_muted = c,
                "text_faint" => p.text_faint = c,
                "sidebar_section" => p.sidebar_section = c,
                "user_bubble" => p.user_bubble = c,
                "row_active" => p.row_active = c,
                "row_hover" => p.row_hover = c,
                "success" => p.success = c,
                "danger" => p.danger = c,
                "diff_add_fg" => p.diff_add_fg = c,
                "diff_add_bg" => p.diff_add_bg = c,
                "diff_del_fg" => p.diff_del_fg = c,
                "diff_del_bg" => p.diff_del_bg = c,
                "widget_inactive_bg" => p.widget_inactive_bg = c,
                "widget_hovered_bg" => p.widget_hovered_bg = c,
                "widget_active_bg" => p.widget_active_bg = c,
                "widget_open_bg" => p.widget_open_bg = c,
                "widget_active_border" => p.widget_active_border = c,
                "selection_bg" => p.selection_bg = c,
                "selection_stroke" => p.selection_stroke = c,
                "md_code_bg" => p.md_code_bg = c,
                "md_code_block_bg" => p.md_code_block_bg = c,
                "md_code_block_header_bg" => p.md_code_block_header_bg = c,
                "md_code_block_border" => p.md_code_block_border = c,
                "md_quote_accent" => p.md_quote_accent = c,
                "md_code_fg" => p.md_code_fg = c,
                _ => {}
            }
        }
        p
    }
}

/// Directory custom theme files are read from: `<config>/oxi/themes`.
pub fn custom_themes_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("oxi")
        .join("themes")
}

/// Load custom themes from disk. Malformed files are skipped. Custom ids are namespaced
/// `custom:<file-stem>` so they never collide with built-ins.
pub fn load_custom_themes() -> Vec<ThemeChoice> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(custom_themes_dir()) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Ok(spec) = serde_json::from_slice::<ThemeSpec>(&bytes) else {
            continue;
        };
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("custom")
            .to_string();
        let name = if spec.name.trim().is_empty() {
            stem.clone()
        } else {
            spec.name.clone()
        };
        out.push(ThemeChoice {
            id: format!("custom:{stem}"),
            name,
            palette: spec.into_palette(),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// All selectable themes: built-ins followed by any custom themes on disk.
pub fn available_themes() -> Vec<ThemeChoice> {
    let mut v = builtin_themes();
    v.extend(load_custom_themes());
    v
}

/// Resolve a theme id to its palette, falling back to [`Palette::DARK`] if unknown.
pub fn resolve_palette(id: &str) -> Palette {
    available_themes()
        .into_iter()
        .find(|t| t.id == id)
        .map(|t| t.palette)
        .unwrap_or(Palette::DARK)
}

/// Set the active theme by id and rebuild egui visuals on `ctx`.
pub fn apply_theme(ctx: &egui::Context, id: &str) {
    set_active_palette(resolve_palette(id));
    setup_style(ctx);
}
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
    ui.add(Spinner::new().size(13.0).color(c_text_muted()));
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
    fonts
        .font_data
        .insert("noto_emoji".to_string(), FontData::from_static(NOTO_EMOJI));
    fonts.font_data.insert(
        "symbols_nerd_font_mono".to_string(),
        FontData::from_static(SYMBOLS_NERD_FONT_MONO),
    );

    let proportional = fonts.families.entry(FontFamily::Proportional).or_default();
    proportional.insert(0, "noto_sans".to_string());
    // Prefer full Noto Emoji over egui's trimmed default emoji font for glyph coverage.
    proportional.insert(1, "noto_emoji".to_string());
    proportional.push("apple_symbols".to_string());

    let monospace = fonts.families.entry(FontFamily::Monospace).or_default();
    monospace.insert(0, "ubuntu_mono".to_string());
    monospace.push("noto_sans".to_string());
    monospace.push("noto_emoji".to_string());
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
    let p = active_palette();
    let mut visuals = if p.dark_base {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    visuals.window_fill = p.bg_main;
    visuals.panel_fill = p.bg_main;
    visuals.extreme_bg_color = p.bg_input;
    visuals.faint_bg_color = p.faint_bg;
    visuals.override_text_color = Some(p.text);
    visuals.window_rounding = egui::Rounding::same(10.0);
    visuals.menu_rounding = egui::Rounding::same(8.0);
    visuals.widgets.noninteractive.rounding = egui::Rounding::same(6.0);
    visuals.widgets.inactive.rounding = egui::Rounding::same(6.0);
    visuals.widgets.hovered.rounding = egui::Rounding::same(6.0);
    visuals.widgets.active.rounding = egui::Rounding::same(6.0);
    visuals.widgets.open.rounding = egui::Rounding::same(6.0);
    // Side panel separator, indentation guides — match app chrome.
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.border_subtle);
    visuals.widgets.noninteractive.bg_fill = p.bg_elevated;
    visuals.widgets.noninteractive.fg_stroke.color = p.text_muted;
    visuals.widgets.inactive.bg_fill = p.widget_inactive_bg;
    visuals.widgets.inactive.weak_bg_fill = p.widget_inactive_bg;
    visuals.widgets.inactive.fg_stroke.color = p.text;
    visuals.widgets.hovered.bg_fill = p.widget_hovered_bg;
    visuals.widgets.hovered.weak_bg_fill = p.widget_hovered_bg;
    visuals.widgets.active.bg_fill = p.widget_active_bg;
    visuals.widgets.active.weak_bg_fill = p.widget_active_bg;
    visuals.widgets.open.bg_fill = p.widget_open_bg;
    // Text selection uses a desaturated tint of the accent.
    visuals.selection.bg_fill = p.selection_bg;
    visuals.selection.stroke = Stroke::new(1.0, p.selection_stroke);
    visuals.window_stroke = Stroke::new(1.0, p.border_subtle);
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, p.border_subtle);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, p.border);
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, p.widget_active_border);

    let mut style = (*ctx.style()).clone();
    style.visuals = visuals;
    // Route egui's default text styles through the shared scale so widgets without an explicit
    // size (composer input, combo boxes, default buttons) stay uniform with the rest of the UI.
    style.text_styles = [
        (
            egui::TextStyle::Heading,
            FontId::new(FS_H2, FontFamily::Proportional),
        ),
        (
            egui::TextStyle::Body,
            FontId::new(FS_BODY, FontFamily::Proportional),
        ),
        (
            egui::TextStyle::Monospace,
            FontId::new(FS_CODE, FontFamily::Monospace),
        ),
        (
            egui::TextStyle::Button,
            FontId::new(FS_BODY, FontFamily::Proportional),
        ),
        (
            egui::TextStyle::Small,
            FontId::new(FS_SMALL, FontFamily::Proportional),
        ),
    ]
    .into();
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
        let color = blend_color(c_text_muted(), c_accent(), mix as f32);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_themes_present_and_unique() {
        let themes = builtin_themes();
        assert!(themes.iter().any(|t| t.id == "dark"));
        assert!(themes.iter().any(|t| t.id == "light"));
        assert!(themes.iter().any(|t| t.id == "midnight"));
        let mut ids: Vec<&str> = themes.iter().map(|t| t.id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), themes.len(), "theme ids must be unique");
    }

    #[test]
    fn dark_and_light_differ_in_base() {
        let dark = resolve_palette("dark");
        let light = resolve_palette("light");
        assert!(dark.dark_base);
        assert!(!light.dark_base);
        assert_ne!(dark.bg_main, light.bg_main);
        assert_ne!(dark.text, light.text);
    }

    #[test]
    fn resolve_palette_known_and_unknown() {
        assert_eq!(resolve_palette("light"), Palette::LIGHT);
        assert_eq!(resolve_palette("midnight"), Palette::MIDNIGHT);
        // Unknown ids fall back to dark.
        assert_eq!(resolve_palette("does-not-exist"), Palette::DARK);
    }

    #[test]
    fn set_and_read_active_palette() {
        set_active_palette(Palette::LIGHT);
        assert_eq!(active_palette(), Palette::LIGHT);
        assert_eq!(c_bg_main(), Palette::LIGHT.bg_main);
        // Restore default so other tests are not affected by global state.
        set_active_palette(Palette::DARK);
        assert_eq!(c_bg_main(), Palette::DARK.bg_main);
    }

    #[test]
    fn parse_hex_color_valid_and_invalid() {
        assert_eq!(parse_hex_color("#0f1012"), Some(rgb(0x0f, 0x10, 0x12)));
        assert_eq!(parse_hex_color("0f1012"), Some(rgb(0x0f, 0x10, 0x12)));
        assert_eq!(parse_hex_color("  #ffffff "), Some(rgb(0xff, 0xff, 0xff)));
        assert_eq!(parse_hex_color("#fff"), None);
        assert_eq!(parse_hex_color("#gggggg"), None);
        assert_eq!(parse_hex_color(""), None);
    }

    #[test]
    fn theme_spec_overrides_base() {
        let mut colors = BTreeMap::new();
        colors.insert("bg_main".to_string(), "#123456".to_string());
        colors.insert("accent".to_string(), "#abcdef".to_string());
        // Unknown key is ignored rather than erroring.
        colors.insert("not_a_field".to_string(), "#000000".to_string());
        let spec = ThemeSpec {
            name: "Custom".to_string(),
            base: ThemeBase::Light,
            colors,
        };
        let p = spec.into_palette();
        assert_eq!(p.bg_main, rgb(0x12, 0x34, 0x56));
        assert_eq!(p.accent, rgb(0xab, 0xcd, 0xef));
        // Unspecified fields inherit from the light base.
        assert_eq!(p.text, Palette::LIGHT.text);
        assert!(!p.dark_base);
    }

    #[test]
    fn theme_spec_parses_from_json() {
        let json = r##"{ "name": "Mine", "base": "dark", "colors": { "accent": "#ff0000" } }"##;
        let spec: ThemeSpec = serde_json::from_str(json).unwrap();
        assert_eq!(spec.name, "Mine");
        let p = spec.into_palette();
        assert_eq!(p.accent, rgb(0xff, 0x00, 0x00));
        assert!(p.dark_base);
    }
}
