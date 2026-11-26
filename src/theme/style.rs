//! Type scale, icon glyphs, font loading, and building egui `Style`/`Visuals` from the
//! active palette.

use std::collections::BTreeMap;
use std::sync::{OnceLock, RwLock};

use eframe::egui::{self, FontData, FontDefinitions, FontFamily, FontId, Stroke, Visuals};

use super::palette::active_palette;

const NOTO_SANS_REGULAR: &[u8] = include_bytes!("../../assets/fonts/NotoSans-Regular.ttf");
const UBUNTU_MONO_REGULAR: &[u8] = include_bytes!("../../assets/fonts/UbuntuMono-R.ttf");
const INTER_REGULAR: &[u8] = include_bytes!("../../assets/fonts/Inter-Regular.ttf");
const JETBRAINS_MONO_REGULAR: &[u8] =
    include_bytes!("../../assets/fonts/JetBrainsMono-Regular.ttf");
/// Zed's interface face (Delta, Zed's agent app) and its semibold for strong text.
const IBM_PLEX_SANS_REGULAR: &[u8] = include_bytes!("../../assets/fonts/IBMPlexSans-Regular.ttf");
const IBM_PLEX_SANS_SEMIBOLD: &[u8] = include_bytes!("../../assets/fonts/IBMPlexSans-SemiBold.ttf");
/// Zed's code face.
const LILEX_REGULAR: &[u8] = include_bytes!("../../assets/fonts/Lilex-Regular.ttf");
/// Script-specific fallbacks stay behind the selected UI/code faces.
const SCRIPT_FONTS: &[(&str, &[u8])] = &[
    (
        "noto_arabic",
        include_bytes!("../../assets/fonts/NotoSansArabic-Regular.ttf"),
    ),
    (
        "noto_hebrew",
        include_bytes!("../../assets/fonts/NotoSansHebrew-Regular.ttf"),
    ),
    (
        "noto_devanagari",
        include_bytes!("../../assets/fonts/NotoSansDevanagari-Regular.ttf"),
    ),
    (
        "noto_armenian",
        include_bytes!("../../assets/fonts/NotoSansArmenian-Regular.ttf"),
    ),
    (
        "noto_georgian",
        include_bytes!("../../assets/fonts/NotoSansGeorgian-Regular.ttf"),
    ),
];
/// Math operators, arrows, technical symbols, box drawing and braille, last in every chain.
const SYMBOL_FONTS: &[(&str, &[u8])] = &[
    (
        "noto_math",
        include_bytes!("../../assets/fonts/NotoSansMath-Regular.ttf"),
    ),
    (
        "noto_symbols",
        include_bytes!("../../assets/fonts/NotoSansSymbols-Regular.ttf"),
    ),
    (
        "noto_symbols2",
        include_bytes!("../../assets/fonts/NotoSansSymbols2-Regular.ttf"),
    ),
];
/// Monochrome Noto Emoji (outline). egui/eframe renders glyphs single-channel, so emoji show
/// as black-and-white outlines rather than in color. Color and distinct skin tones are not
/// available in this face.
const NOTO_EMOJI: &[u8] = include_bytes!("../../assets/fonts/NotoEmoji-Regular.ttf");
const SYMBOLS_NERD_FONT_MONO: &[u8] =
    include_bytes!("../../assets/fonts/SymbolsNerdFontMono-Regular.ttf");

/// App-wide type scale — single source of truth for every text size in the UI.
/// Headings step down 19 → 16.5 → 15; body is 14; secondary text 13 / 11.5; code 13.
/// Keep all `.size(...)` / `FontId` text sizes routed through these so the app stays uniform.
pub const FS_H1: f32 = 19.0;
pub const FS_H2: f32 = 16.5;
pub const FS_H3: f32 = 15.0;
pub const FS_BODY: f32 = 14.0;
pub const FS_SMALL: f32 = 13.0;
pub const FS_TINY: f32 = 11.5;
/// Monospace code (blocks). Inline code sits slightly more compact inside rounded pills.
pub const FS_CODE: f32 = 13.0;

/// [`FontFamily`] for Nerd Font icon glyphs used in tool pills.
/// Using a named family keeps PUA codepoints out of the normal text fallback chains.
#[inline]
pub fn icon_font() -> FontFamily {
    FontFamily::Name("icons".into())
}

/// [`FontFamily`] for bold/strong prose: a real semibold face when the interface font ships
/// one (IBM Plex Sans, or a system family's semibold/bold face), otherwise the regular chain.
#[inline]
pub fn strong_font() -> FontFamily {
    FontFamily::Name("strong".into())
}

/// [`egui::RichText`] set in [`strong_font`] so headings, sidebar titles, and emphasized UI
/// labels render in a true semibold weight (egui's `.strong()` alone only alters ink color).
#[inline]
pub fn strong_rich(text: impl Into<String>, size: f32, color: egui::Color32) -> egui::RichText {
    egui::RichText::new(text)
        .font(FontId::new(size, strong_font()))
        .color(color)
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

/// Models & providers (settings nav) — nf-md-cube_outline.
pub const ICON_PROVIDERS: &str = "\u{f01a7}";
/// Tools & safety (settings nav) — nf-md-shield_check_outline.
pub const ICON_AGENT: &str = "\u{f0cc8}";
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
/// Chats sidebar toggle (`nf-cod-comment_discussion`).
pub const ICON_MENU: &str = "\u{eac7}";
/// File explorer; uses the verified open-folder glyph from the bundled icon font.
pub const ICON_EXPLORER: &str = "\u{f0770}";
/// Regular file (`nf-fa-file-lines`).
pub const ICON_FILE: &str = "\u{f15c}";
/// Terminal — toggle the embedded terminal panel (`nf-fa-terminal`).
pub const ICON_TERMINAL: &str = "\u{f120}";
/// Git — source-control panel and header (`nf-cod-source_control`, the VS Code glyph).
pub const ICON_GIT: &str = "\u{ea68}";
/// Rotate / refresh (`nf-fa-refresh`).
pub const ICON_REFRESH: &str = "\u{f021}";
/// Font Awesome `undo`.
pub const ICON_UNDO: &str = "\u{f0e2}";
/// Arrow pointing up — send message button (`nf-fa-arrow-up`).
pub const ICON_SEND: &str = "\u{f062}";
/// Arrow pointing down — scroll-to-bottom jump button (`nf-fa-arrow-down`).
pub const ICON_ARROW_DOWN: &str = "\u{f063}";
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
/// Thumbtack — pinned chats and projects in the sidebar (`nf-fa-thumb_tack`).
pub const ICON_PIN: &str = "\u{f08d}";
/// Folder plus — "add workspace" (`nf-md-folder-plus`).
pub const ICON_FOLDER_PLUS: &str = "\u{f0257}";
/// Closed folder — folded workspace row in the sidebar (`nf-md-folder`).
pub const ICON_FOLDER: &str = "\u{f024b}";
/// Open folder — unfolded workspace row in the sidebar (`nf-md-folder_open`).
pub const ICON_FOLDER_OPEN: &str = "\u{f0770}";
/// Check inside a circle — affirmative pill ("Signed in", committed) (`nf-fa-check-circle`).
pub const ICON_CHECK_CIRCLE: &str = "\u{f058}";
/// Warning triangle (error callouts).
pub const ICON_WARNING: &str = "\u{f071}";
/// Up angle chevron — "stage" direction / collapse hint (`nf-fa-angle-up`).
pub const ICON_ANGLE_UP: &str = "\u{f077}";
/// Down angle chevron — unfold/expand hint (`nf-fa-angle-down`).
pub const ICON_ANGLE_DOWN: &str = "\u{f078}";
/// Sparkles — "generate" (commit-message generator, starter prompts) (`nf-md-creation`).
pub const ICON_MAGIC: &str = "\u{f0674}";
/// Cloud download — pull/fetch from remote (`nf-fa-cloud-download`).
pub const ICON_DOWNLOAD: &str = "\u{f019}";
/// Cloud upload / arrow up — push to remote (`nf-fa-cloud-upload`).
pub const ICON_UPLOAD: &str = "\u{f0aa}";
/// Magnifier over a globe — web search tool pill (`nf-md-search_web`).
pub const ICON_WEB_SEARCH: &str = "\u{f070f}";
/// Globe — web fetch / URL tool pill (`nf-fa-globe`).
pub const ICON_GLOBE: &str = "\u{f0ac}";
pub const ICON_SERVER: &str = "\u{f233}";
/// Git branch — current-branch line in the source-control panel (`nf-oct-git_branch`).
pub const ICON_BRANCH: &str = "\u{f418}";
/// Play triangle — start a local/remote model (`nf-fa-play`).
pub const ICON_PLAY: &str = "\u{f04b}";
/// Heart — likes / favorites metadata (`nf-fa-heart`).
pub const ICON_HEART: &str = "\u{f004}";
/// Magnifying glass — search fields (`nf-fa-search`).
pub const ICON_SEARCH: &str = "\u{f002}";
/// Activity log (status bar) — nf-md-pulse.
pub const ICON_ACTIVITY: &str = "\u{f0430}";
/// Evidence mode toggle, captures — nf-fa-camera.
pub const ICON_CAMERA: &str = "\u{f030}";
/// Plan mode toggle — nf-md-map_outline.
pub const ICON_PLAN: &str = "\u{f0982}";
/// Agent checklist (`todo_write`) — nf-md-format_list_checks.
pub const ICON_TASKS: &str = "\u{f0756}";
/// Checklist item states — nf-fa-circle_o / nf-fa-dot_circle_o.
pub const ICON_CIRCLE: &str = "\u{f10c}";
pub const ICON_DOT_CIRCLE: &str = "\u{f192}";

/// Small loading indicator (avoids default large `interact_size` spinners).
pub fn small_spinner(ui: &mut egui::Ui) {
    spinner(ui, 13.0, super::palette::c_text_muted());
}

/// How long a visible spinner waits before its next frame. Slightly under 1/30 s, so the frame
/// after it (plus the frame's own CPU time) still lands on every second vsync at 60 Hz.
const SPINNER_FRAME: std::time::Duration = std::time::Duration::from_millis(30);

/// Frame interval for progress animations while the window is visible but not focused: the run
/// keeps showing it is alive at ~10 fps instead of redrawing the whole window at ~30.
pub const UNFOCUSED_ANIMATION_FRAME: std::time::Duration = std::time::Duration::from_millis(100);

/// A `size`-square loading spinner, drawn like egui's [`egui::Spinner`].
pub fn spinner(ui: &mut egui::Ui, size: f32, color: egui::Color32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    paint_spinner(ui, rect, color);
    response
}

/// Paint a spinner into `rect`. egui's own spinner requests a repaint every frame, which keeps
/// the whole window redrawing at the display's refresh rate (120 Hz on ProMotion) for as long as
/// a run shows one; this one asks for ~30 frames per second (~10 in an unfocused window).
pub fn paint_spinner(ui: &egui::Ui, rect: egui::Rect, color: egui::Color32) {
    if !ui.is_rect_visible(rect) {
        return;
    }
    // egui starts a delayed frame one predicted frame time early; without adding it back, 30 ms
    // shrinks to ~13 ms at 60 Hz and the spinner still redraws on every vsync.
    let (predicted_dt, focused) = ui.input(|i| (i.predicted_dt, i.focused));
    let frame = if focused {
        SPINNER_FRAME
    } else {
        UNFOCUSED_ANIMATION_FRAME
    };
    ui.ctx()
        .request_repaint_after(frame + std::time::Duration::from_secs_f32(predicted_dt));
    // A fixed 3 pt stroke turned the small (10–13 pt) inline spinners into a blob; thin it with
    // the size and keep the arc inside the rect.
    let size = rect.height().min(rect.width());
    let stroke = (size / 7.0).clamp(1.5, 3.0);
    let radius = size / 2.0 - stroke / 2.0 - 0.5;
    let n_points = (radius.round() as u32).clamp(8, 128);
    let time = ui.input(|i| i.time);
    let start_angle = time * std::f64::consts::TAU;
    let end_angle = start_angle + 240f64.to_radians() * time.sin();
    let points: Vec<egui::Pos2> = (0..n_points)
        .map(|i| {
            let t = i as f64 / n_points as f64;
            let (sin, cos) = (start_angle + (end_angle - start_angle) * t).sin_cos();
            rect.center() + radius * egui::vec2(cos as f32, sin as f32)
        })
        .collect();
    ui.painter()
        .add(egui::Shape::line(points, Stroke::new(stroke, color)));
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

        let options = |entries: &[(&str, &str)]| {
            entries
                .iter()
                .map(|(id, name)| FontOption {
                    id: (*id).to_string(),
                    name: (*name).to_string(),
                })
                .collect::<Vec<_>>()
        };
        let mut all = options(&[
            ("default", "Default — Inter"),
            ("bundled:noto_sans", "Noto Sans (bundled)"),
            ("bundled:ibm_plex_sans", "IBM Plex Sans (bundled)"),
        ]);
        let mut monospace = options(&[
            ("default", "Default — JetBrains Mono"),
            ("bundled:ubuntu_mono", "Ubuntu Mono (bundled)"),
            ("bundled:lilex", "Lilex (bundled)"),
        ]);
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

/// CJK faces borrowed from the system, set once by [`load_system_cjk_fonts`]. Every desktop OS
/// ships one, so Oxi doesn't bundle a ~16 MiB CJK face of its own.
static SYSTEM_CJK_FONTS: OnceLock<Vec<FontData>> = OnceLock::new();

/// Preferred CJK families, best first: macOS, Windows, then common Linux packages. Any other
/// installed face is tried only for scripts these leave uncovered.
const CJK_FAMILIES: &[&str] = &[
    "Hiragino Sans",
    "PingFang SC",
    "Apple SD Gothic Neo",
    "Yu Gothic UI",
    "Yu Gothic",
    "Microsoft YaHei UI",
    "Microsoft YaHei",
    "Malgun Gothic",
    "Noto Sans CJK JP",
    "Noto Sans CJK SC",
    "Noto Sans CJK KR",
    "Source Han Sans JP",
    "Source Han Sans SC",
    "WenQuanYi Micro Hei",
    "Droid Sans Fallback",
];

/// One sample per script the CJK fallbacks must cover: kana, Han, Hangul.
const CJK_SAMPLES: [char; 3] = ['あ', '中', '한'];

/// Find system CJK faces off the UI thread (scanning installed fonts takes a while), then
/// reinstall the fonts with them as fallbacks.
pub fn load_system_cjk_fonts(ctx: &egui::Context) {
    let ctx = ctx.clone();
    let _ = std::thread::Builder::new()
        .name("cjk-fonts".into())
        .spawn(move || {
            if !system_cjk_fonts().is_empty() {
                ctx.set_fonts(font_definitions(&active_fonts()));
                ctx.request_repaint();
            }
        });
}

/// The system CJK fallbacks, searched for on first call (blocking).
pub(crate) fn system_cjk_fonts() -> &'static [FontData] {
    SYSTEM_CJK_FONTS.get_or_init(find_system_cjk_fonts)
}

/// Pick the fewest faces that cover [`CJK_SAMPLES`], preferring [`CJK_FAMILIES`].
fn find_system_cjk_fonts() -> Vec<FontData> {
    use skrifa::MetadataProvider as _;
    let db = &system_fonts().db;
    let preferred = CJK_FAMILIES.iter().filter_map(|name| {
        db.query(&fontdb::Query {
            families: &[fontdb::Family::Name(name)],
            weight: fontdb::Weight::NORMAL,
            stretch: fontdb::Stretch::Normal,
            style: fontdb::Style::Normal,
        })
    });
    let others = db
        .faces()
        .filter(|face| face.weight == fontdb::Weight::NORMAL && face.style == fontdb::Style::Normal)
        .map(|face| face.id);
    let mut missing = CJK_SAMPLES.to_vec();
    let mut fonts = Vec::new();
    for id in preferred.chain(others) {
        if missing.is_empty() {
            break;
        }
        let covered: Vec<char> = db
            .with_face_data(id, |data, index| {
                let font = skrifa::FontRef::from_index(data, index).ok()?;
                let charmap = font.charmap();
                Some(
                    missing
                        .iter()
                        .copied()
                        .filter(|&c| charmap.map(c).is_some())
                        .collect(),
                )
            })
            .flatten()
            .unwrap_or_default();
        if covered.is_empty() {
            continue;
        }
        if let Some(data) = map_system_face(db, id) {
            missing.retain(|c| !covered.contains(c));
            fonts.push(centered_text_font(data));
        }
    }
    fonts
}

/// Memory-map a system face for the process lifetime. CJK faces run 20–60 MiB; mapping keeps
/// only the glyph pages actually rendered resident instead of copying whole files.
fn map_system_face(db: &fontdb::Database, id: fontdb::ID) -> Option<FontData> {
    let (source, index) = db.face_source(id)?;
    let mut data = match source {
        fontdb::Source::File(path) | fontdb::Source::SharedFile(path, _) => {
            let file = std::fs::File::open(path).ok()?;
            // SAFETY: system font files are not rewritten while an app has them open; this is
            // the same assumption fontdb's own memory mapping makes.
            let map = unsafe { memmap2::Mmap::map(&file) }.ok()?;
            FontData::from_static(Box::leak(Box::new(map)))
        }
        fontdb::Source::Binary(bytes) => FontData::from_owned((*bytes).as_ref().to_vec()),
    };
    data.index = index;
    Some(data)
}

/// All installed font families, plus the bundled interface fonts.
pub fn ui_font_options() -> &'static [FontOption] {
    &system_fonts().all
}

/// Installed families reported as fixed-width, plus the bundled code fonts.
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

static FONTS_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Set the active fonts. Call [`setup_style`] afterwards to reinstall and relayout.
pub fn set_active_fonts(sel: FontSelection) {
    if let Ok(mut guard) = ACTIVE_FONTS.write() {
        *guard = Some(sel);
    }
    FONTS_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// Bumped whenever the fonts change. Caches that keep galleys across frames key on it, since
/// glyph widths change with the font.
pub fn fonts_generation() -> u64 {
    FONTS_GENERATION.load(std::sync::atomic::Ordering::Relaxed)
}

static EDITOR_FONT_SIZE_BITS: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(FS_SMALL.to_bits());

/// Set the editor's code font size (Sublime's `font_size`). A change bumps
/// [`fonts_generation`], so cached editor layouts are rebuilt at the new size.
pub fn set_editor_font_size(size: f32) {
    let bits = size.to_bits();
    if EDITOR_FONT_SIZE_BITS.swap(bits, std::sync::atomic::Ordering::Relaxed) != bits {
        FONTS_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

static EDITOR_WRAP_INDENT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Whether soft-wrapped editor rows continue at their line's indentation (Sublime's
/// `indent_subsequent_lines`). Part of the editor's layout, so a change bumps
/// [`fonts_generation`] like a font change does.
pub fn set_editor_wrap_indent(on: bool) {
    if EDITOR_WRAP_INDENT.swap(on, std::sync::atomic::Ordering::Relaxed) != on {
        FONTS_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

pub fn editor_wrap_indent() -> bool {
    EDITOR_WRAP_INDENT.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn editor_font_size() -> f32 {
    f32::from_bits(EDITOR_FONT_SIZE_BITS.load(std::sync::atomic::Ordering::Relaxed))
}

/// The font source text is laid out in by the editor and the diff panes.
pub fn editor_font() -> FontId {
    FontId::monospace(editor_font_size())
}

/// Height of one editor row, before the galley says otherwise.
pub fn editor_line_height() -> f32 {
    editor_font_size() * 1.35
}

fn active_fonts() -> FontSelection {
    ACTIVE_FONTS
        .read()
        .ok()
        .and_then(|g| g.clone())
        .unwrap_or_default()
}

/// Glyph shift (fraction of the font size, negative = up) that centers a text face optically in
/// the rows egui lays out for it.
///
/// egui sizes a row from the face's ascent, descent and line gap and puts the baseline at the
/// ascent, so where the letters land inside the row depends on the face. Noto Sans reserves a tall
/// ascent for accents, which left its lowercase ~1.5 px below the row middle at 14 pt. Selections,
/// highlights and the caret span the whole row and icons are centered on it, so text looked low
/// against all of them. The middle of the band between the x-height and cap-height middles goes
/// on the row middle instead. Faces without Latin letters (icons, emoji) keep their own metrics.
fn optical_center_offset(data: &[u8], index: u32) -> Option<f32> {
    use skrifa::MetadataProvider as _;
    let font = skrifa::FontRef::from_index(data, index).ok()?;
    let size = skrifa::instance::Size::unscaled();
    let location = skrifa::instance::LocationRef::default();
    let metrics = font.metrics(size, location);
    let em = f32::from(metrics.units_per_em);
    let glyph_metrics = font.glyph_metrics(size, location);
    let charmap = font.charmap();
    // Measured outlines first: some faces (Ubuntu Mono) declare an x-height their glyphs miss.
    let top = |c: char, declared: Option<f32>| {
        charmap
            .map(c)
            .and_then(|id| glyph_metrics.bounds(id))
            .map(|bounds| bounds.y_max)
            .or(declared)
            .filter(|height| *height > 0.0)
    };
    let x_height = top('x', metrics.x_height)?;
    let cap_height = top('H', metrics.cap_height)?;
    if em <= 0.0 {
        return None;
    }
    let row_middle = (metrics.ascent - metrics.descent + metrics.leading) * 0.5;
    let ink_middle = metrics.ascent - (x_height + cap_height) * 0.25;
    Some(((row_middle - ink_middle) / em).clamp(-0.2, 0.2))
}

/// `data` with [`optical_center_offset`] applied, for the faces that set text.
fn centered_text_font(data: FontData) -> FontData {
    match optical_center_offset(&data.font, data.index) {
        Some(y_offset_factor) => FontData {
            tweak: egui::FontTweak {
                y_offset_factor,
                ..data.tweak
            },
            ..data
        },
        None => data,
    }
}

/// Find the best face at `weight` for a discovered family and register it with egui.
fn install_system_font_weight(
    fonts: &mut FontDefinitions,
    key: &str,
    family: &str,
    weight: fontdb::Weight,
) -> Option<String> {
    let catalog = system_fonts();
    let family_query = [fontdb::Family::Name(family)];
    let id = catalog.db.query(&fontdb::Query {
        families: &family_query,
        weight,
        stretch: fontdb::Stretch::Normal,
        style: fontdb::Style::Normal,
    })?;
    // When looking for a heavier face, reject a fallback that resolved back to regular weight.
    if weight.0 >= fontdb::Weight::SEMIBOLD.0
        && catalog
            .db
            .face(id)
            .is_some_and(|face| face.weight.0 <= fontdb::Weight::NORMAL.0)
    {
        return None;
    }
    let (bytes, index) = catalog
        .db
        .with_face_data(id, |data, index| (data.to_vec(), index))?;
    let mut data = FontData::from_owned(bytes);
    data.index = index;
    fonts.font_data.insert(
        key.to_string(),
        std::sync::Arc::new(centered_text_font(data)),
    );
    Some(key.to_string())
}

/// Find the best normal face for a discovered family and register it with egui.
fn install_system_font(fonts: &mut FontDefinitions, key: &str, family: &str) -> Option<String> {
    install_system_font_weight(fonts, key, family, fontdb::Weight::NORMAL)
}

/// Every face Oxi renders with. egui's own default fonts are disabled in Cargo.toml: these
/// cover all of their glyphs, so nothing else is needed at runtime.
pub(crate) fn font_definitions(sel: &FontSelection) -> FontDefinitions {
    let mut fonts = FontDefinitions::empty();
    fonts.font_data.insert(
        "inter".to_string(),
        std::sync::Arc::new(centered_text_font(FontData::from_static(INTER_REGULAR))),
    );
    fonts.font_data.insert(
        "jetbrains_mono".to_string(),
        std::sync::Arc::new(centered_text_font(FontData::from_static(
            JETBRAINS_MONO_REGULAR,
        ))),
    );
    fonts.font_data.insert(
        "noto_sans".to_string(),
        std::sync::Arc::new(centered_text_font(FontData::from_static(NOTO_SANS_REGULAR))),
    );
    fonts.font_data.insert(
        "ubuntu_mono".to_string(),
        std::sync::Arc::new(centered_text_font(FontData::from_static(
            UBUNTU_MONO_REGULAR,
        ))),
    );
    fonts.font_data.insert(
        "noto_emoji".to_string(),
        std::sync::Arc::new(FontData::from_static(NOTO_EMOJI)),
    );
    fonts.font_data.insert(
        "symbols_nerd_font_mono".to_string(),
        std::sync::Arc::new(FontData::from_static(SYMBOLS_NERD_FONT_MONO)),
    );

    let plex_ui = sel.ui == "bundled:ibm_plex_sans";
    if plex_ui {
        for (key, bytes) in [
            ("ibm_plex_sans", IBM_PLEX_SANS_REGULAR),
            ("ibm_plex_sans_semibold", IBM_PLEX_SANS_SEMIBOLD),
        ] {
            // Plex's x-height (0.516 em) is below Inter's (0.546 em); scale it up so the
            // shared type scale reads the same size in either face.
            let mut data = centered_text_font(FontData::from_static(bytes));
            data.tweak.scale *= 1.06;
            fonts
                .font_data
                .insert(key.to_string(), std::sync::Arc::new(data));
        }
    }
    if sel.mono == "bundled:lilex" {
        fonts.font_data.insert(
            "lilex".to_string(),
            std::sync::Arc::new(centered_text_font(FontData::from_static(LILEX_REGULAR))),
        );
    }

    for &(key, bytes) in SCRIPT_FONTS {
        fonts.font_data.insert(
            key.into(),
            std::sync::Arc::new(centered_text_font(FontData::from_static(bytes))),
        );
    }
    let cjk = SYSTEM_CJK_FONTS.get().map_or(&[][..], Vec::as_slice);
    let cjk_keys: Vec<String> = (0..cjk.len()).map(|i| format!("system_cjk_{i}")).collect();
    for (key, data) in cjk_keys.iter().zip(cjk) {
        fonts
            .font_data
            .insert(key.clone(), std::sync::Arc::new(data.clone()));
    }
    for &(key, bytes) in SYMBOL_FONTS {
        fonts.font_data.insert(
            key.into(),
            std::sync::Arc::new(FontData::from_static(bytes)),
        );
    }
    // Optional user-selected system fonts, loaded from disk. A missing/unreadable file
    // yields `None` and the bundled font is used instead.
    let sys_ui = selected_family(&sel.ui)
        .and_then(|family| install_system_font(&mut fonts, "sys_ui", family));
    let sys_ui_strong = selected_family(&sel.ui).and_then(|family| {
        install_system_font_weight(
            &mut fonts,
            "sys_ui_strong",
            family,
            fontdb::Weight::SEMIBOLD,
        )
        .or_else(|| {
            install_system_font_weight(&mut fonts, "sys_ui_strong", family, fontdb::Weight::BOLD)
        })
    });
    let sys_mono = selected_family(&sel.mono)
        .and_then(|family| install_system_font(&mut fonts, "sys_mono", family));

    let proportional = fonts.families.entry(FontFamily::Proportional).or_default();
    proportional.extend(["inter", "noto_sans", "noto_emoji"].map(String::from));
    if sel.ui == "bundled:noto_sans" {
        proportional.retain(|key| key != "noto_sans");
        proportional.insert(0, "noto_sans".to_string());
    }
    if plex_ui {
        proportional.insert(0, "ibm_plex_sans".to_string());
    }
    // The chosen system font wins; bundled faces stay as glyph fallbacks.
    if let Some(key) = &sys_ui {
        proportional.insert(0, key.clone());
    }

    let monospace = fonts.families.entry(FontFamily::Monospace).or_default();
    monospace
        .extend(["jetbrains_mono", "ubuntu_mono", "noto_sans", "noto_emoji"].map(String::from));
    if sel.mono == "bundled:ubuntu_mono" {
        monospace.retain(|key| key != "ubuntu_mono");
        monospace.insert(0, "ubuntu_mono".to_string());
    }
    if sel.mono == "bundled:lilex" {
        monospace.insert(0, "lilex".to_string());
    }
    if let Some(key) = &sys_mono {
        monospace.insert(0, key.clone());
    }

    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        fonts.families.get_mut(&family).unwrap().extend(
            SCRIPT_FONTS
                .iter()
                .map(|(key, _)| (*key).to_string())
                .chain(cjk_keys.iter().cloned())
                .chain(SYMBOL_FONTS.iter().map(|(key, _)| (*key).to_string())),
        );
    }

    // Strong prose: real semibold/bold face first (system family's own weight when picked,
    // bundled IBM Plex Sans SemiBold when Plex is active, or system Inter SemiBold if present).
    let mut strong_chain = fonts.families[&FontFamily::Proportional].clone();
    if let Some(key) = &sys_ui_strong {
        strong_chain.insert(0, key.clone());
    } else if plex_ui {
        strong_chain.insert(0, "ibm_plex_sans_semibold".into());
    } else if sel.ui == "default"
        && let Some(key) = install_system_font_weight(
            &mut fonts,
            "sys_inter_strong",
            "Inter",
            fontdb::Weight::SEMIBOLD,
        )
    {
        strong_chain.insert(0, key);
    }
    fonts.families.insert(strong_font(), strong_chain);

    // Sequence emoji must shape with Noto Emoji even when their base (a digit or heart)
    // also exists in the primary text face. Keep fallback coverage for unsupported bases.
    let mut emoji_chain = fonts.families[&FontFamily::Proportional].clone();
    emoji_chain.retain(|key| key != "noto_emoji");
    emoji_chain.insert(0, "noto_emoji".into());
    fonts
        .families
        .insert(FontFamily::Name("emoji".into()), emoji_chain);

    // Dedicated icon family — used only for tool pill glyphs.
    // Keeps Nerd Font PUA codepoints out of the proportional/monospace fallback chains
    // so they never accidentally substitute real text characters.
    fonts
        .families
        .entry(FontFamily::Name("icons".into()))
        .or_default()
        .push("symbols_nerd_font_mono".to_string());

    fonts
}

fn install_fonts(ctx: &egui::Context) {
    ctx.set_fonts(font_definitions(&active_fonts()));
}

/// Build egui visuals from the active palette: neutral surfaces, subtle borders, one accent.
pub fn setup_style(ctx: &egui::Context) {
    // A resize can update viewport geometry, then the composer's wrapped draft, then
    // the transcript's bottom offset. Allow these dependent corrections before presenting;
    // unchanged frames still use a single pass.
    ctx.options_mut(|options| options.max_passes = std::num::NonZeroUsize::new(4).unwrap());
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
    // Sliders: accent fill up to the handle, so the value reads even where the rail color sits
    // close to the card surface.
    visuals.slider_trailing_fill = true;
    visuals.handle_shape = egui::style::HandleShape::Circle;

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
    style.spacing.slider_width = 240.0;
    style.spacing.slider_rail_height = 4.0;
    // Floating scroll bars stay hidden while dormant and fade in on hover/scroll.
    style.spacing.scroll.bar_width = 6.0;
    style.spacing.scroll.floating_width = 3.0;
    style.spacing.scroll.floating_allocated_width = 0.0;
    style.spacing.scroll.handle_min_length = 24.0;
    style.spacing.scroll.floating = true;
    style.spacing.scroll.dormant_background_opacity = 0.0;
    style.spacing.scroll.dormant_handle_opacity = 0.0;
    // Responsiveness over flourish, like a native editor: keeping the caret in view (arrow keys,
    // Enter on the last visible line, find) scrolls at once instead of easing over 100-300 ms,
    // and expand/collapse and fades take ~one frame instead of 200 ms.
    style.scroll_animation = egui::style::ScrollAnimation::none();
    style.animation_time = 0.06;
    ctx.all_styles_mut(|s| *s = style.clone());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What egui will wait before the next frame of a window showing only a spinner, after it
    /// takes off one predicted frame time.
    fn spinner_delay(focused: bool) -> std::time::Duration {
        let ctx = egui::Context::default();
        let input = || egui::RawInput {
            focused,
            ..Default::default()
        };
        // The first passes of a new context repaint right away regardless.
        for _ in 0..2 {
            let _ = ctx.run_ui(input(), |ui| {
                spinner(ui, 13.0, egui::Color32::WHITE);
            });
        }
        let output = ctx.run_ui(input(), |ui| {
            spinner(ui, 13.0, egui::Color32::WHITE);
        });
        output.viewport_output[&egui::ViewportId::ROOT].repaint_delay
    }

    /// Text faces sit low in egui's rows and get pulled up; icon and emoji faces, already
    /// centered on their rows, are left alone.
    #[test]
    fn only_text_faces_get_an_optical_shift() {
        let noto = optical_center_offset(NOTO_SANS_REGULAR, 0).unwrap();
        let mono = optical_center_offset(UBUNTU_MONO_REGULAR, 0).unwrap();
        println!("noto {noto} mono {mono}");
        assert!((-0.1..-0.05).contains(&noto), "{noto}");
        assert!((-0.1..-0.03).contains(&mono), "{mono}");
        assert_eq!(optical_center_offset(SYMBOLS_NERD_FONT_MONO, 0), None);
        assert_eq!(optical_center_offset(NOTO_EMOJI, 0), None);
    }

    /// The bundled defaults must cover Romanian text and code without replacement glyphs.
    #[test]
    fn bundled_defaults_cover_text_and_fixed_width_code() {
        use skrifa::MetadataProvider as _;
        for data in [INTER_REGULAR, JETBRAINS_MONO_REGULAR] {
            let font = skrifa::FontRef::from_index(data, 0).unwrap();
            for c in "ĂăÂâÎîȘșȚț0123456789{}[]()<>!=+-_".chars() {
                assert!(font.charmap().map(c).is_some(), "missing {c}");
            }
            assert!(optical_center_offset(data, 0).is_some());
        }
        let font = skrifa::FontRef::from_index(JETBRAINS_MONO_REGULAR, 0).unwrap();
        let metrics = font.glyph_metrics(
            skrifa::instance::Size::unscaled(),
            skrifa::instance::LocationRef::default(),
        );
        let width = |c| {
            metrics
                .advance_width(font.charmap().map(c).unwrap())
                .unwrap()
        };
        for c in " iWM0{}".chars() {
            assert_eq!(width(c), width('0'), "variable width for {c}");
        }
    }

    #[test]
    fn bundled_font_choices_keep_fallbacks() {
        for (sel, ui, mono) in [
            (FontSelection::default(), "inter", "jetbrains_mono"),
            (
                FontSelection {
                    ui: "bundled:noto_sans".into(),
                    mono: "bundled:ubuntu_mono".into(),
                },
                "noto_sans",
                "ubuntu_mono",
            ),
            (
                FontSelection {
                    ui: "system:Oxi Missing Font 93A6".into(),
                    mono: "system:Oxi Missing Font 93A6".into(),
                },
                "inter",
                "jetbrains_mono",
            ),
        ] {
            let fonts = font_definitions(&sel);
            assert_eq!(fonts.families[&FontFamily::Proportional][0], ui);
            assert_eq!(fonts.families[&FontFamily::Monospace][0], mono);
            for (family, fallback) in [
                (FontFamily::Proportional, "noto_sans"),
                (FontFamily::Monospace, "ubuntu_mono"),
            ] {
                let chain = &fonts.families[&family];
                assert!(chain.iter().any(|key| key == fallback));
                assert!(chain.iter().any(|key| key == "noto_emoji"));
                assert!(chain.iter().all(|key| fonts.font_data.contains_key(key)));
            }
        }
    }

    #[test]
    fn bundled_fallbacks_cover_multilingual_samples_in_both_families() {
        use skrifa::MetadataProvider as _;
        let definitions = font_definitions(&FontSelection::default());
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            for sample in [
                "العربيةمرحبابالعالم",
                "עבריתשלוםעולם",
                "हिन्दीनमस्तेदुनिया",
                "Հայերեն բարեւ",
                "ქართული გამარჯობა",
                // Previously covered only by Apple Symbols or egui's default fonts.
                "☐☑∑∫≠≤≥∞√∀∃⊂⊆⟵⟶⇒⤴⨀⌘⌥⏎⎈⣿⠋⠙▲◆★✓✗♥",
            ] {
                for c in sample.chars() {
                    assert!(
                        definitions.families[&family].iter().any(|key| {
                            let data = &definitions.font_data[key];
                            skrifa::FontRef::from_index(&data.font, data.index)
                                .unwrap()
                                .charmap()
                                .map(c)
                                .is_some()
                        }),
                        "missing {c} in {family:?}"
                    );
                }
            }
        }
    }

    /// CJK comes from the system. Machines without a CJK face (bare CI images) find none, but
    /// whatever is picked must cover what it was picked for and stay off the bundled list.
    #[test]
    fn system_cjk_fallbacks_cover_their_scripts() {
        use skrifa::MetadataProvider as _;
        let fonts = system_cjk_fonts();
        assert!(fonts.len() <= CJK_SAMPLES.len());
        let definitions = font_definitions(&FontSelection::default());
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            let chain = &definitions.families[&family];
            let found = chain
                .iter()
                .filter(|k| k.starts_with("system_cjk_"))
                .count();
            assert_eq!(found, fonts.len(), "CJK fallbacks missing from {family:?}");
        }
        for c in CJK_SAMPLES {
            let covered = fonts.iter().any(|data| {
                skrifa::FontRef::from_index(&data.font, data.index)
                    .unwrap()
                    .charmap()
                    .map(c)
                    .is_some()
            });
            assert!(covered || cfg!(not(target_os = "macos")), "macOS ships {c}");
        }
    }

    /// A spinner on screen must not pin the window to the display's refresh rate.
    #[test]
    fn spinner_repaints_at_its_own_rate() {
        assert_eq!(spinner_delay(true), SPINNER_FRAME);
        assert_eq!(spinner_delay(false), UNFOCUSED_ANIMATION_FRAME);
    }
}
