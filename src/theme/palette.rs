//! Color palette: the [`Palette`] struct, built-in `DARK`/`LIGHT`/`MIDNIGHT` palettes, the
//! globally-active palette, and every `c_*`/`badge_*` accessor derived from it.

use std::sync::RwLock;

use eframe::egui::Color32;

pub(crate) const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

// ─── Palette ─────────────────────────────────────────────────────────────────
//
// Every surface / ink / accent color the UI draws with lives in [`Palette`], so the
// whole app can be re-skinned at runtime. These were historically `pub const C_*`
// values; they are now resolved through the globally-active palette (see
// [`active_palette`] and the `c_*` accessor fns) so switching theme is a single swap.

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
    /// Default dark theme: neutral near-black surfaces, off-white ink, and a single warm
    /// rust/copper accent (oxi ⇒ oxide — the brand color).
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
        accent: rgb(0xe2, 0x8f, 0x5b),
        text: rgb(0xd8, 0xde, 0xe9),
        text_strong: rgb(0xee, 0xee, 0xf4),
        text_muted: rgb(0x8b, 0x8f, 0x99),
        text_faint: rgb(0x6b, 0x6d, 0x78),
        sidebar_section: rgb(0x6d, 0x71, 0x7b),
        user_bubble: rgb(0x22, 0x24, 0x2a),
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
        selection_bg: rgb(0x44, 0x2d, 0x1d),
        selection_stroke: rgb(0x96, 0x5f, 0x35),
        md_code_bg: rgb(0x24, 0x26, 0x2d),
        md_code_block_bg: rgb(0x13, 0x14, 0x18),
        md_code_block_header_bg: rgb(0x19, 0x1b, 0x21),
        md_code_block_border: rgb(0x2a, 0x2d, 0x34),
        md_quote_accent: rgb(0xc4, 0x82, 0x4f),
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
        accent: rgb(0xb4, 0x52, 0x19),
        text: rgb(0x1c, 0x1d, 0x20),
        text_strong: rgb(0x05, 0x06, 0x09),
        text_muted: rgb(0x5f, 0x63, 0x6b),
        text_faint: rgb(0x8b, 0x8f, 0x99),
        sidebar_section: rgb(0x80, 0x84, 0x8e),
        user_bubble: rgb(0xe8, 0xea, 0xef),
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
        selection_bg: rgb(0xf5, 0xe0, 0xcf),
        selection_stroke: rgb(0xd8, 0x9a, 0x62),
        md_code_bg: rgb(0xec, 0xec, 0xea),
        md_code_block_bg: rgb(0xf4, 0xf4, 0xf2),
        md_code_block_header_bg: rgb(0xeb, 0xeb, 0xe8),
        md_code_block_border: rgb(0xde, 0xde, 0xda),
        md_quote_accent: rgb(0xb4, 0x52, 0x19),
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
        accent: rgb(0xe2, 0x8f, 0x5b),
        text: rgb(0xd8, 0xde, 0xe9),
        text_strong: rgb(0xee, 0xee, 0xf4),
        text_muted: rgb(0x8b, 0x8f, 0x99),
        text_faint: rgb(0x6b, 0x6d, 0x78),
        sidebar_section: rgb(0x6d, 0x71, 0x7b),
        user_bubble: rgb(0x14, 0x14, 0x18),
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
        selection_bg: rgb(0x38, 0x25, 0x18),
        selection_stroke: rgb(0x96, 0x5f, 0x35),
        md_code_bg: rgb(0x1a, 0x1c, 0x22),
        md_code_block_bg: rgb(0x0a, 0x0b, 0x0e),
        md_code_block_header_bg: rgb(0x10, 0x12, 0x18),
        md_code_block_border: rgb(0x20, 0x23, 0x29),
        md_quote_accent: rgb(0xc4, 0x82, 0x4f),
        md_code_fg: rgb(0xe1, 0xe4, 0xea),
    };

    /// Sublime: the classic Sublime Text "Monokai" scheme — warm charcoal surfaces
    /// (`#272822`), off-white ink, a vivid orange accent, and the signature pink/green
    /// signal colors.
    pub const SUBLIME: Palette = Palette {
        dark_base: true,
        bg_main: rgb(0x27, 0x28, 0x22),
        bg_sidebar: rgb(0x1e, 0x1f, 0x1c),
        bg_elevated: rgb(0x2f, 0x30, 0x2a),
        bg_elevated_2: rgb(0x38, 0x39, 0x30),
        bg_input: rgb(0x21, 0x22, 0x1c),
        faint_bg: rgb(0x2c, 0x2d, 0x26),
        border: rgb(0x3e, 0x3d, 0x32),
        border_subtle: rgb(0x33, 0x34, 0x2b),
        accent: rgb(0xfd, 0x97, 0x1f),
        text: rgb(0xf8, 0xf8, 0xf2),
        text_strong: rgb(0xfd, 0xff, 0xf1),
        text_muted: rgb(0xa5, 0x9f, 0x85),
        text_faint: rgb(0x75, 0x71, 0x5e),
        sidebar_section: rgb(0x8f, 0x8a, 0x75),
        user_bubble: rgb(0x38, 0x39, 0x30),
        success: rgb(0xa6, 0xe2, 0x2e),
        danger: rgb(0xf9, 0x26, 0x72),
        diff_add_fg: rgb(0xa6, 0xe2, 0x2e),
        diff_add_bg: rgb(0x26, 0x30, 0x1c),
        diff_del_fg: rgb(0xf9, 0x26, 0x72),
        diff_del_bg: rgb(0x35, 0x1a, 0x24),
        widget_inactive_bg: rgb(0x34, 0x35, 0x2c),
        widget_hovered_bg: rgb(0x3f, 0x40, 0x34),
        widget_active_bg: rgb(0x49, 0x4a, 0x3c),
        widget_open_bg: rgb(0x3a, 0x3b, 0x30),
        widget_active_border: rgb(0x57, 0x57, 0x45),
        selection_bg: rgb(0x49, 0x48, 0x3e),
        selection_stroke: rgb(0x75, 0x71, 0x5e),
        md_code_bg: rgb(0x38, 0x39, 0x30),
        md_code_block_bg: rgb(0x1e, 0x1f, 0x1c),
        md_code_block_header_bg: rgb(0x2a, 0x2b, 0x24),
        md_code_block_border: rgb(0x3e, 0x3d, 0x32),
        md_quote_accent: rgb(0xfd, 0x97, 0x1f),
        md_code_fg: rgb(0xe6, 0xdb, 0x74),
    };

    /// Sublime 4: the Sublime Text 4 default "Mariana" scheme — desaturated blue-grey
    /// surfaces (`#2b303b`), a soft blue accent, and Mariana's green/red/teal signals.
    pub const MARIANA: Palette = Palette {
        dark_base: true,
        bg_main: rgb(0x2b, 0x30, 0x3b),
        bg_sidebar: rgb(0x21, 0x25, 0x2b),
        bg_elevated: rgb(0x33, 0x3a, 0x45),
        bg_elevated_2: rgb(0x3b, 0x43, 0x51),
        bg_input: rgb(0x23, 0x27, 0x2e),
        faint_bg: rgb(0x2f, 0x35, 0x40),
        border: rgb(0x3f, 0x46, 0x50),
        border_subtle: rgb(0x2c, 0x31, 0x3b),
        accent: rgb(0x66, 0x99, 0xcc),
        text: rgb(0xd8, 0xde, 0xe9),
        text_strong: rgb(0xf2, 0xf4, 0xf8),
        text_muted: rgb(0x8b, 0x95, 0xa3),
        text_faint: rgb(0x65, 0x70, 0x7d),
        sidebar_section: rgb(0x7e, 0x8a, 0x99),
        user_bubble: rgb(0x33, 0x3a, 0x45),
        success: rgb(0x99, 0xc7, 0x94),
        danger: rgb(0xec, 0x5f, 0x67),
        diff_add_fg: rgb(0x99, 0xc7, 0x94),
        diff_add_bg: rgb(0x26, 0x31, 0x2a),
        diff_del_fg: rgb(0xec, 0x5f, 0x67),
        diff_del_bg: rgb(0x32, 0x23, 0x2a),
        widget_inactive_bg: rgb(0x33, 0x3a, 0x45),
        widget_hovered_bg: rgb(0x3d, 0x45, 0x52),
        widget_active_bg: rgb(0x47, 0x50, 0x5e),
        widget_open_bg: rgb(0x38, 0x40, 0x49),
        widget_active_border: rgb(0x56, 0x60, 0x6e),
        selection_bg: rgb(0x41, 0x50, 0x5f),
        selection_stroke: rgb(0x66, 0x99, 0xcc),
        md_code_bg: rgb(0x33, 0x3a, 0x45),
        md_code_block_bg: rgb(0x21, 0x25, 0x2b),
        md_code_block_header_bg: rgb(0x2c, 0x32, 0x3d),
        md_code_block_border: rgb(0x3f, 0x46, 0x50),
        md_quote_accent: rgb(0x66, 0x99, 0xcc),
        md_code_fg: rgb(0x5f, 0xb3, 0xb3),
    };
}

/// The globally-active palette. Read on every frame by the `c_*` accessors; written
/// only when the theme changes. egui renders on a single thread, so contention is nil.
static ACTIVE_PALETTE: RwLock<Palette> = RwLock::new(Palette::DARK);

/// Snapshot of the active palette.
pub fn active_palette() -> Palette {
    ACTIVE_PALETTE.read().map(|g| *g).unwrap_or(Palette::DARK)
}

/// Swap the active palette. Call [`super::setup_style`] afterwards to rebuild egui visuals.
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

// ─── Derived semantic colors ─────────────────────────────────────────────────
//
// Signals (error/warning/info/tool-state) used in the transcript, git panel, composer, etc.
// Historically dozens of `Color32::from_rgb(...)` literals were scattered through the UI and
// shadowed the active palette — they read fine on the dark theme but stayed identical on light,
// breaking consistency. These helpers build each tint from the *active* accent/danger/success so a
// theme swap re-skins them automatically. All take a base hue (an accent/danger/success color).

/// Composite `base` over the panel background at `alpha/255` opacity (gives a translucent panel
/// tint that works on both dark and light surfaces without a hard-coded RGB). Note the blend
/// starts from the panel background: alpha 30 means a faint wash of `base`, not 88% of it —
/// the earlier inverted blend is what made accent/danger "tints" render as near-solid color.
fn tint_on_panels(base: Color32, alpha: u8) -> Color32 {
    let bg = c_bg_main();
    Color32::from_rgba_unmultiplied(
        u8_blend(bg.r(), base.r(), alpha),
        u8_blend(bg.g(), base.g(), alpha),
        u8_blend(bg.b(), base.b(), alpha),
        255,
    )
}

/// Blend a surface channel toward a `tint` color by `alpha/255` — used to give elevated surfaces
/// (pills, borders) a faint accent/danger hue without darkening toward bg_main.
fn surface_tint(surface: Color32, tint: Color32, alpha: u8) -> Color32 {
    Color32::from_rgb(
        u8_blend(surface.r(), tint.r(), alpha),
        u8_blend(surface.g(), tint.g(), alpha),
        u8_blend(surface.b(), tint.b(), alpha),
    )
}

/// Linear blend of two u8 channels toward `b` by `t` (0..=255 → 0..=1).
fn u8_blend(a: u8, b: u8, t: u8) -> u8 {
    let f = t as f32 / 255.0;
    let af = a as f32;
    let bf = b as f32;
    (af + (bf - af) * f).round().clamp(0.0, 255.0) as u8
}

/// Solid stem (text/foreground) color for an error — danger, but nudged so it stays readable on
/// elevated backgrounds; on light themes `c_danger` is already dark enough.
pub fn c_error_fg() -> Color32 {
    let p = active_palette();
    if p.dark_base {
        // brightened danger, red-ish
        Color32::from_rgb(0xff, 0xb0, 0xb0)
    } else {
        p.danger
    }
}

/// Solid stem color for a warning (agent stream errors).
pub fn c_warning_fg() -> Color32 {
    let p = active_palette();
    if p.dark_base {
        Color32::from_rgb(0xff, 0xd0, 0xa0)
    } else {
        Color32::from_rgb(0x9a, 0x6a, 0x10)
    }
}

/// Error banner background (danger tint on the panel).
pub fn c_error_bg() -> Color32 {
    tint_on_panels(c_danger(), 40)
}
/// Error banner stroke.
pub fn c_error_stroke() -> Color32 {
    tint_on_panels(c_danger(), 110)
}
/// Warning banner background.
pub fn c_warning_bg() -> Color32 {
    tint_on_panels(Color32::from_rgb(0xb9, 0x8a, 0x2a), 36)
}
/// Warning banner stroke.
pub fn c_warning_stroke() -> Color32 {
    tint_on_panels(Color32::from_rgb(0xb9, 0x8a, 0x2a), 110)
}
/// Info / approval card background — a faint accent tint.
pub fn c_info_bg() -> Color32 {
    tint_on_panels(c_accent(), 30)
}

/// Dimmed backdrop behind modal dialogs. Lighter on light themes so the page still
/// reads through instead of going near-black.
pub fn c_modal_backdrop() -> Color32 {
    if active_palette().dark_base {
        Color32::from_black_alpha(140)
    } else {
        Color32::from_black_alpha(80)
    }
}

/// Left rail on thinking blocks — accent sunk toward the panel so it reads as a
/// quiet hairline, not a highlight.
pub fn c_thinking_rail() -> Color32 {
    tint_on_panels(c_accent(), 90)
}

/// Selected-row background, app-wide (sidebar rows, settings nav, git panel, …) —
/// an accent tint rather than a neutral grey, so the active item reads as "accent"
/// consistently everywhere a row/button has a selected or pressed state.
pub fn c_row_active() -> Color32 {
    tint_on_panels(c_accent(), 40)
}
/// Hover background, app-wide — the same accent tint dialed down, so hover reads
/// as a preview of the active-row treatment rather than a mismatched grey.
pub fn c_row_hover() -> Color32 {
    tint_on_panels(c_accent(), 16)
}

/// Foreground for text/icons drawn on an accent-filled surface (primary buttons, send).
/// The dark themes use a light copper accent, so ink on it must be near-black; on light
/// themes the accent is dark enough for white.
pub fn c_on_accent() -> Color32 {
    if active_palette().dark_base {
        Color32::from_rgb(0x17, 0x0e, 0x07)
    } else {
        Color32::WHITE
    }
}

/// Background for a selected pill tab (provider row, compute target, sub-tabs) — a faint
/// accent tint over the elevated surface so the active choice reads as "on", not just lighter.
pub fn c_pill_selected_bg() -> Color32 {
    let p = active_palette();
    surface_tint(p.bg_elevated_2, p.accent, 46)
}

/// Border for a selected pill tab.
pub fn c_pill_selected_border() -> Color32 {
    let p = active_palette();
    surface_tint(p.border, p.accent, 120)
}

/// Composer card border while the input has keyboard focus — border nudged toward
/// the accent so the field reads as "live" without shouting.
pub fn c_composer_focus_border() -> Color32 {
    let p = active_palette();
    surface_tint(p.border, p.accent, 115)
}

/// Soft accent border for user chat bubbles so they separate from the transcript.
pub fn c_user_bubble_border() -> Color32 {
    let p = active_palette();
    surface_tint(p.border, p.accent, 70)
}

// ── Tool pill palettes (single source of truth for the transcript tool pills + edit blocks) ──

/// Background for a plain (done) tool pill. Uses the theme's input surface on dark themes so
/// the pill remains distinct without introducing a foreign near-black block (especially visible
/// on blue-grey themes such as Sublime Text 4 / Mariana).
pub fn c_tool_pill_bg() -> Color32 {
    let p = active_palette();
    if p.dark_base {
        p.bg_input
    } else {
        p.bg_elevated_2
    }
}
/// Background for a running tool pill. Deliberately identical to the plain pill — the
/// running state is carried by the spinner and the "running" badge, so the pill surface
/// itself stays quiet instead of tinting the whole block with the accent.
pub fn c_tool_running_bg() -> Color32 {
    c_tool_pill_bg()
}
/// Background for an errored tool pill. Keep the normal tool surface and add only a quiet danger
/// wash, so failures remain recognizable without turning into high-contrast red/black blocks.
pub fn c_tool_error_bg() -> Color32 {
    let p = active_palette();
    if p.dark_base {
        surface_tint(p.bg_input, p.danger, 14)
    } else {
        surface_tint(p.bg_elevated_2, p.danger, 18)
    }
}
/// Border for a plain tool pill.
pub fn c_tool_pill_border() -> Color32 {
    c_border_subtle()
}
/// Border for a running tool pill — quiet like [`c_tool_pill_border`], see [`c_tool_running_bg`].
pub fn c_tool_running_border() -> Color32 {
    c_tool_pill_border()
}
/// Border for an errored tool pill.
pub fn c_tool_error_border() -> Color32 {
    let p = active_palette();
    if p.dark_base {
        surface_tint(p.border_subtle, p.danger, 55)
    } else {
        surface_tint(p.border, p.danger, 90)
    }
}
/// Foreground for an errored tool label. On dark themes, blend the danger hue into normal text
/// rather than using a bright fixed pink that can overpower muted palettes such as Mariana.
pub fn c_tool_error_fg() -> Color32 {
    let p = active_palette();
    if p.dark_base {
        surface_tint(p.text, p.danger, 100)
    } else {
        p.danger
    }
}
/// Background for expanded tool output and the diff body of edit tools. Follow the theme's
/// input surface on dark themes so edit views share the same quiet surface as tool-call pills.
pub fn c_tool_diff_bg() -> Color32 {
    let p = active_palette();
    if p.dark_base { p.bg_input } else { p.bg_main }
}

// ── Status-badge tints ("running" / "done" / "failed" chips under tool pills) ──

// These chips sit on *every* tool pill, so they were the loudest repeated element in the
// transcript. The tints are kept deliberately quiet — a faint wash over the pill surface with
// a slightly dimmed colored label — so state reads at a glance without shouting. `done` (the
// resting state of every finished call) is the quietest; `running`/`failed` carry a bit more
// so the two states worth noticing still stand out. All derived from the active palette via
// `surface_tint` so every theme (dark and light) stays consistent.

/// Running badge (accent): (fg, bg, stroke).
pub fn badge_running_parts() -> (Color32, Color32, Color32) {
    let p = active_palette();
    if p.dark_base {
        (
            surface_tint(p.accent, p.bg_elevated_2, 40),
            surface_tint(p.bg_elevated_2, p.accent, 24),
            surface_tint(p.border, p.accent, 55),
        )
    } else {
        (
            p.accent,
            surface_tint(p.bg_elevated_2, p.accent, 22),
            surface_tint(p.border, p.accent, 70),
        )
    }
}
/// Done badge (success): (fg, bg, stroke). The quietest of the three.
pub fn badge_done_parts() -> (Color32, Color32, Color32) {
    let p = active_palette();
    if p.dark_base {
        (
            surface_tint(p.diff_add_fg, p.bg_elevated_2, 55),
            surface_tint(p.bg_elevated_2, p.success, 16),
            surface_tint(p.border, p.success, 42),
        )
    } else {
        (
            surface_tint(p.diff_add_fg, p.bg_main, 30),
            surface_tint(p.bg_elevated_2, p.success, 18),
            surface_tint(p.border, p.success, 60),
        )
    }
}
/// Failed badge (danger): (fg, bg, stroke).
pub fn badge_failed_parts() -> (Color32, Color32, Color32) {
    let p = active_palette();
    if p.dark_base {
        (
            surface_tint(p.diff_del_fg, p.bg_elevated_2, 40),
            surface_tint(p.bg_elevated_2, p.danger, 22),
            surface_tint(p.border, p.danger, 55),
        )
    } else {
        (
            p.danger,
            surface_tint(p.bg_elevated_2, p.danger, 22),
            surface_tint(p.border, p.danger, 70),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_read_active_palette() {
        set_active_palette(Palette::LIGHT);
        assert_eq!(active_palette(), Palette::LIGHT);
        assert_eq!(c_bg_main(), Palette::LIGHT.bg_main);
        // Restore default so other tests are not affected by global state.
        set_active_palette(Palette::DARK);
        assert_eq!(c_bg_main(), Palette::DARK.bg_main);
    }
}
