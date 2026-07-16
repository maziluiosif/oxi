//! Type scale, icon glyphs, font loading, and building egui `Style`/`Visuals` from the
//! active palette.

use std::collections::BTreeMap;
use std::sync::{OnceLock, RwLock};

use eframe::egui::{self, FontData, FontDefinitions, FontFamily, FontId, Stroke, Visuals};

use super::palette::active_palette;

const NOTO_SANS_REGULAR: &[u8] = include_bytes!("../../assets/fonts/NotoSans-Regular.ttf");
const UBUNTU_MONO_REGULAR: &[u8] = include_bytes!("../../assets/fonts/UbuntuMono-R.ttf");
const APPLE_SYMBOLS: &[u8] = include_bytes!("../../assets/fonts/AppleSymbols.ttf");
/// Monochrome Noto Emoji (outline). egui/eframe renders glyphs single-channel, so emoji show
/// as black-and-white outlines rather than in color, but this covers the full emoji range
/// instead of the small subset bundled with egui's defaults.
const NOTO_EMOJI: &[u8] = include_bytes!("../../assets/fonts/NotoEmoji-Regular.ttf");
const SYMBOLS_NERD_FONT_MONO: &[u8] =
    include_bytes!("../../assets/fonts/SymbolsNerdFontMono-Regular.ttf");

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
/// Prompts (settings nav) — nf-fa-pencil.
pub const ICON_PROMPTS: &str = "\u{f040}";
/// About (settings nav) — nf-fa-info_circle.
pub const ICON_INFO: &str = "\u{f05a}";
/// Close / dismiss (diff viewer, settings, image chip) — nf-fa-xmark.
pub const ICON_CLOSE: &str = "\u{f00d}";
/// Affirmative / done (commit button, tool enable check) — nf-fa-check.
pub const ICON_CHECK: &str = "\u{f00c}";
/// Copy to clipboard (code block header) — nf-fa-copy.
pub const ICON_COPY: &str = "\u{f0c5}";
/// Gear / cog — settings entry points and the empty-state settings button (`nf-fa-gear`).
pub const ICON_SETTINGS: &str = "\u{f013}";
/// Plus — "new" / "add" actions (new chat, add profile) (`nf-fa-plus`).
pub const ICON_PLUS: &str = "\u{f067}";
/// Chevron pointed right — "hide" right panel (`nf-fa-chevron-right`).
pub const ICON_CHEVRON_RIGHT: &str = "\u{f054}";
pub const ICON_CHEVRON_LEFT: &str = "\u{f053}";
/// Hamburger menu — toggle sidebar (`nf-fa-bars`).
pub const ICON_MENU: &str = "\u{f02a}";
/// File explorer; uses the verified open-folder glyph from the bundled icon font.
pub const ICON_EXPLORER: &str = "\u{f0770}";
/// Regular file (`nf-fa-file-lines`).
pub const ICON_FILE: &str = "\u{f15c}";
/// Terminal — toggle the embedded terminal panel (`nf-fa-terminal`).
pub const ICON_TERMINAL: &str = "\u{f120}";
/// Git — source-control panel and header (`nf-fa-git`).
pub const ICON_GIT: &str = "\u{f1d3}";
/// Rotate / refresh (`nf-fa-refresh`).
pub const ICON_REFRESH: &str = "\u{f021}";
/// Arrow pointing up — send message button (`nf-fa-arrow-up`).
pub const ICON_SEND: &str = "\u{f062}";
/// Filled stop square — stop streaming (`nf-fa-stop`).
pub const ICON_STOP: &str = "\u{f04d}";
/// Paperclip — attach image composer button (`nf-fa-paperclip`).
pub const ICON_ATTACH: &str = "\u{f0c6}";
/// Microphone — voice dictation composer button (`nf-fa-microphone`).
pub const ICON_MIC: &str = "\u{f130}";
/// Arrow rising out of a box — "external" suggestion / prompt chips (`nf-fa-external-link`).
pub const ICON_EXTERNAL: &str = "\u{f08e}";
/// Trash — delete / discard actions (`nf-fa-trash`).
pub const ICON_TRASH: &str = "\u{f1f8}";
/// Folder plus — "add workspace" (`nf-md-folder-plus`).
pub const ICON_FOLDER_PLUS: &str = "\u{f0257}";
/// Closed folder — folded workspace row in the sidebar (`nf-md-folder`).
pub const ICON_FOLDER: &str = "\u{f024b}";
/// Open folder — unfolded workspace row in the sidebar (`nf-md-folder_open`).
pub const ICON_FOLDER_OPEN: &str = "\u{f0770}";
/// Check inside a circle — affirmative pill ("Signed in", committed) (`nf-fa-check-circle`).
pub const ICON_CHECK_CIRCLE: &str = "\u{f058}";
/// Up angle chevron — "stage" direction / collapse hint (`nf-fa-angle-up`).
pub const ICON_ANGLE_UP: &str = "\u{f077}";
/// Down angle chevron — unfold/expand hint (`nf-fa-angle-down`).
pub const ICON_ANGLE_DOWN: &str = "\u{f078}";
/// Magic wand — "generate" (commit-message generator) (`nf-fa-magic`).
pub const ICON_MAGIC: &str = "\u{f135}";
/// Cloud download — pull/fetch from remote (`nf-fa-cloud-download`).
pub const ICON_DOWNLOAD: &str = "\u{f019}";
/// Cloud upload / arrow up — push to remote (`nf-fa-cloud-upload`).
pub const ICON_UPLOAD: &str = "\u{f0aa}";
/// Magnifier over a globe — web search tool pill (`nf-md-search_web`).
pub const ICON_WEB_SEARCH: &str = "\u{f070f}";
/// Globe — web fetch / URL tool pill (`nf-fa-globe`).
pub const ICON_GLOBE: &str = "\u{f0ac}";
/// Git branch — current-branch line in the source-control panel (`nf-oct-git_branch`).
pub const ICON_BRANCH: &str = "\u{f418}";
/// Play triangle — start a local/remote model (`nf-fa-play`).
pub const ICON_PLAY: &str = "\u{f04b}";
/// Heart — likes / favorites metadata (`nf-fa-heart`).
pub const ICON_HEART: &str = "\u{f004}";
/// Magnifying glass — search fields (`nf-fa-search`).
pub const ICON_SEARCH: &str = "\u{f002}";

/// Small loading indicator (avoids default large `interact_size` spinners).
pub fn small_spinner(ui: &mut egui::Ui) {
    use eframe::egui::Spinner;
    ui.add(
        Spinner::new()
            .size(13.0)
            .color(super::palette::c_text_muted()),
    );
}

// ─── System fonts (user-selectable in Appearance settings) ──────────────────────
//
// Font families are discovered once at runtime instead of being tied to a hard-coded list of
// OS paths. The bundled fonts remain available as "Default" and as glyph fallbacks.

/// One selectable font family. System ids are prefixed so they cannot collide with `default`.
#[derive(Clone, Debug)]
pub struct FontOption {
    pub id: String,
    pub name: String,
}

struct SystemFontCatalog {
    db: fontdb::Database,
    all: Vec<FontOption>,
    monospace: Vec<FontOption>,
}

static SYSTEM_FONTS: OnceLock<SystemFontCatalog> = OnceLock::new();

fn system_fonts() -> &'static SystemFontCatalog {
    SYSTEM_FONTS.get_or_init(|| {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();

        // Key by a case-folded name to collapse localized/duplicate faces while preserving the
        // font's own display spelling. Hidden/internal families are not useful in a picker.
        let mut families: BTreeMap<String, (String, bool)> = BTreeMap::new();
        for face in db.faces() {
            let Some((name, _)) = face.families.first() else {
                continue;
            };
            if name.trim().is_empty() || name.starts_with('.') {
                continue;
            }
            let entry = families
                .entry(name.to_lowercase())
                .or_insert_with(|| (name.clone(), false));
            entry.1 |= face.monospaced;
        }

        let default = FontOption {
            id: "default".to_string(),
            name: "Default".to_string(),
        };
        let mut all = vec![default.clone()];
        let mut monospace = vec![default];
        for (_, (name, is_monospace)) in families {
            let option = FontOption {
                id: format!("system:{name}"),
                name,
            };
            if is_monospace {
                monospace.push(option.clone());
            }
            all.push(option);
        }
        SystemFontCatalog { db, all, monospace }
    })
}

/// All installed font families, plus the bundled Noto Sans default.
pub fn ui_font_options() -> &'static [FontOption] {
    &system_fonts().all
}

/// Installed families reported as fixed-width, plus the bundled Ubuntu Mono default.
pub fn mono_font_options() -> &'static [FontOption] {
    &system_fonts().monospace
}

pub fn ui_font_is_known(id: &str) -> bool {
    ui_font_options().iter().any(|o| o.id == id)
}

pub fn mono_font_is_known(id: &str) -> bool {
    mono_font_options().iter().any(|o| o.id == id)
}

fn selected_family(id: &str) -> Option<&str> {
    id.strip_prefix("system:")
}

/// The user's selected interface + code fonts, resolved by [`install_fonts`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FontSelection {
    pub ui: String,
    pub mono: String,
}

impl Default for FontSelection {
    fn default() -> Self {
        Self {
            ui: "default".to_string(),
            mono: "default".to_string(),
        }
    }
}

/// Globally-active font selection. Read by [`install_fonts`] (which runs inside
/// [`setup_style`]); written on startup and whenever the user changes fonts.
static ACTIVE_FONTS: RwLock<Option<FontSelection>> = RwLock::new(None);

/// Set the active fonts. Call [`setup_style`] afterwards to reinstall and relayout.
pub fn set_active_fonts(sel: FontSelection) {
    if let Ok(mut guard) = ACTIVE_FONTS.write() {
        *guard = Some(sel);
    }
}

fn active_fonts() -> FontSelection {
    ACTIVE_FONTS
        .read()
        .ok()
        .and_then(|g| g.clone())
        .unwrap_or_default()
}

/// Find the best normal face for a discovered family and register it with egui.
fn install_system_font(fonts: &mut FontDefinitions, key: &str, family: &str) -> Option<String> {
    let catalog = system_fonts();
    let family_query = [fontdb::Family::Name(family)];
    let id = catalog.db.query(&fontdb::Query {
        families: &family_query,
        weight: fontdb::Weight::NORMAL,
        stretch: fontdb::Stretch::Normal,
        style: fontdb::Style::Normal,
    })?;
    let (bytes, index) = catalog
        .db
        .with_face_data(id, |data, index| (data.to_vec(), index))?;
    let mut data = FontData::from_owned(bytes);
    data.index = index;
    fonts
        .font_data
        .insert(key.to_string(), std::sync::Arc::new(data));
    Some(key.to_string())
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "noto_sans".to_string(),
        std::sync::Arc::new(FontData::from_static(NOTO_SANS_REGULAR)),
    );
    fonts.font_data.insert(
        "ubuntu_mono".to_string(),
        std::sync::Arc::new(FontData::from_static(UBUNTU_MONO_REGULAR)),
    );
    fonts.font_data.insert(
        "apple_symbols".to_string(),
        std::sync::Arc::new(FontData::from_static(APPLE_SYMBOLS)),
    );
    fonts.font_data.insert(
        "noto_emoji".to_string(),
        std::sync::Arc::new(FontData::from_static(NOTO_EMOJI)),
    );
    fonts.font_data.insert(
        "symbols_nerd_font_mono".to_string(),
        std::sync::Arc::new(FontData::from_static(SYMBOLS_NERD_FONT_MONO)),
    );

    // Optional user-selected system fonts, loaded from disk. A missing/unreadable file
    // yields `None` and the bundled font is used instead.
    let sel = active_fonts();
    let sys_ui = selected_family(&sel.ui)
        .and_then(|family| install_system_font(&mut fonts, "sys_ui", family));
    let sys_mono = selected_family(&sel.mono)
        .and_then(|family| install_system_font(&mut fonts, "sys_mono", family));

    let proportional = fonts.families.entry(FontFamily::Proportional).or_default();
    proportional.insert(0, "noto_sans".to_string());
    // Prefer full Noto Emoji over egui's trimmed default emoji font for glyph coverage.
    proportional.insert(1, "noto_emoji".to_string());
    proportional.push("apple_symbols".to_string());
    // The chosen system font wins; the bundled Noto Sans stays as a glyph fallback.
    if let Some(key) = &sys_ui {
        proportional.insert(0, key.clone());
    }

    let monospace = fonts.families.entry(FontFamily::Monospace).or_default();
    monospace.insert(0, "ubuntu_mono".to_string());
    monospace.push("noto_sans".to_string());
    monospace.push("noto_emoji".to_string());
    monospace.push("apple_symbols".to_string());
    if let Some(key) = &sys_mono {
        monospace.insert(0, key.clone());
    }

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

/// Build egui visuals from the active palette: neutral surfaces, subtle borders, one accent.
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
    visuals.window_corner_radius = egui::CornerRadius::same(crate::theme::RADIUS_CARD);
    visuals.menu_corner_radius = egui::CornerRadius::same(crate::theme::RADIUS_CHIP);
    let widget_radius = egui::CornerRadius::same(crate::theme::RADIUS_ROW);
    visuals.widgets.noninteractive.corner_radius = widget_radius;
    visuals.widgets.inactive.corner_radius = widget_radius;
    visuals.widgets.hovered.corner_radius = widget_radius;
    visuals.widgets.active.corner_radius = widget_radius;
    visuals.widgets.open.corner_radius = widget_radius;
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

    let mut style = (*ctx.style_of(ctx.theme())).clone();
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
    style.spacing.menu_margin = egui::Margin::same(6);
    style.spacing.window_margin = egui::Margin::same(10);
    style.spacing.combo_width = 220.0;
    // Floating scroll bars stay hidden while dormant and fade in on hover/scroll.
    style.spacing.scroll.bar_width = 6.0;
    style.spacing.scroll.floating_width = 3.0;
    style.spacing.scroll.floating_allocated_width = 0.0;
    style.spacing.scroll.handle_min_length = 24.0;
    style.spacing.scroll.floating = true;
    style.spacing.scroll.dormant_background_opacity = 0.0;
    style.spacing.scroll.dormant_handle_opacity = 0.0;
    ctx.all_styles_mut(|s| *s = style.clone());
}
