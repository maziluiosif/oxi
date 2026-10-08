//! The product video's stage: the app window on a backdrop, with a chapter headline above it,
//! a chapter rail below, a pointer, and title cards. Frames are composed here and piped to
//! ffmpeg as raw RGBA.

use std::io::Write as _;
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};

use eframe::egui::{self, Align2, Color32, FontFamily, FontId, Pos2, Rect, Shape, pos2, vec2};
use egui_kittest::Harness;
use image::RgbaImage;

use super::SCALE;

/// The whole frame, in points.
pub(super) const STAGE: egui::Vec2 = egui::vec2(1400.0, 960.0);
/// Where the app window sits on the stage, in points.
pub(super) const WINDOW_AT: egui::Vec2 = egui::vec2(80.0, 132.0);
const WINDOW_RADIUS: f32 = 12.0;

const BG: Color32 = Color32::from_rgb(11, 13, 16);
const RUST: Color32 = Color32::from_rgb(226, 106, 44);
const RUST_2: Color32 = Color32::from_rgb(244, 146, 63);
const TEXT: Color32 = Color32::from_rgb(236, 239, 244);
const MUTED: Color32 = Color32::from_rgb(154, 164, 178);
const DIM: Color32 = Color32::from_rgb(88, 96, 108);

pub(super) const INSTALL_COMMAND: &str =
    "curl -fsSL https://maziluiosif.github.io/oxi/install.sh | sh";

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Card {
    Intro,
    Outro,
}

#[derive(Default)]
pub(super) struct StageState {
    pub chapters: Vec<&'static str>,
    pub chapter: Option<usize>,
    pub headline: Option<(String, String)>,
    pub previous: Option<(String, String)>,
    /// 0 → `previous` shown, 1 → `headline` shown.
    pub headline_t: f32,
    pub card: Option<Card>,
    /// How much the card covers the window: 0 → window, 1 → card.
    pub card_t: f32,
    pub card_target: f32,
    /// Fonts are set during the first frame and usable from the next.
    fonts_ready: bool,
}

pub(super) struct Stage {
    pub harness: Harness<'static, StageState>,
    /// Where `finish` writes the chapters' start times (JSON), for the website's player.
    chapters_path: std::path::PathBuf,
    /// Frames composed so far, and the frame each chapter started at.
    frames: usize,
    chapter_starts: Vec<(usize, usize)>,
    ffmpeg: Child,
    stdin: Option<ChildStdin>,
    /// The last composed app image, the start of a crossfade.
    fade_from: Option<RgbaImage>,
    fade: (usize, usize),
    last_app: Option<RgbaImage>,
    pub pointer: Option<Pos2>,
    pub pointer_down: bool,
    /// Save the next composed frame here, too.
    pub save_next: Option<std::path::PathBuf>,
    fps: f32,
}

impl Stage {
    pub fn new(video: &Path, fps: u64, chapters: Vec<&'static str>) -> Self {
        let harness = Harness::builder()
            .with_size(STAGE)
            .with_pixels_per_point(SCALE)
            .with_step_dt(1.0 / fps as f32)
            .wgpu()
            .build_ui_state(
                |ui, state: &mut StageState| paint_stage(ui, state),
                StageState {
                    chapters,
                    ..Default::default()
                },
            );
        egui_extras::install_image_loaders(&harness.ctx);
        let (w, h) = ((STAGE.x * SCALE) as u32, (STAGE.y * SCALE) as u32);
        // A near-lossless intermediate; scripts/render-demo.sh makes the web video and GIF.
        let mut ffmpeg = Command::new("ffmpeg")
            .args([
                "-v", "error", "-y", "-f", "rawvideo", "-pix_fmt", "rgba", "-s",
            ])
            .arg(format!("{w}x{h}"))
            .args(["-r", &fps.to_string(), "-i", "-"])
            .args([
                "-c:v", "libx264", "-preset", "veryfast", "-crf", "6", "-pix_fmt", "yuv444p",
            ])
            .arg(video)
            .stdin(Stdio::piped())
            .spawn()
            .expect("ffmpeg is required to record the demo video");
        let stdin = ffmpeg.stdin.take();
        Self {
            harness,
            chapters_path: video.with_extension("chapters.json"),
            frames: 0,
            chapter_starts: Vec::new(),
            ffmpeg,
            stdin,
            fade_from: None,
            fade: (0, 0),
            last_app: None,
            pointer: None,
            pointer_down: false,
            save_next: None,
            fps: fps as f32,
        }
    }

    pub fn state(&mut self) -> &mut StageState {
        self.harness.state_mut()
    }

    /// Blend from the current window contents to whatever comes next over `frames` frames.
    pub fn crossfade(&mut self, frames: usize) {
        self.fade_from = self.last_app.clone();
        self.fade = (0, frames);
    }

    /// Compose one frame around `app` (the window, at `SCALE`) and send it to ffmpeg.
    pub fn frame(&mut self, mut app: RgbaImage) {
        let fps = self.fps;
        {
            let state = self.harness.state_mut();
            state.headline_t = (state.headline_t + 1.0 / (0.55 * fps)).min(1.0);
            let step = 1.0 / (0.6 * fps);
            state.card_t = if state.card_t < state.card_target {
                (state.card_t + step).min(state.card_target)
            } else {
                (state.card_t - step).max(state.card_target)
            };
        }
        self.harness.step();
        let mut stage = self.harness.render().expect("render stage");
        let shown = app.clone();
        if let Some(from) = &self.fade_from {
            let (done, total) = self.fade;
            let t = ease((done + 1) as f32 / total.max(1) as f32);
            blend_into(&mut app, from, 1.0 - t);
            self.fade.0 += 1;
            if self.fade.0 >= total {
                self.fade_from = None;
            }
        }
        self.last_app = Some(shown);
        let card_t = self.harness.state().card_t;
        let at = ((WINDOW_AT.x * SCALE) as u32, (WINDOW_AT.y * SCALE) as u32);
        composite_window(&mut stage, &app, at, WINDOW_RADIUS * SCALE, 1.0 - card_t);
        if let Some(pointer) = self.pointer.filter(|_| card_t < 0.5) {
            let tip = ((WINDOW_AT + pointer.to_vec2()) * SCALE).to_pos2();
            paint_pointer(&mut stage, tip, SCALE, self.pointer_down);
        }
        if let Some(path) = self.save_next.take() {
            stage.save(path).expect("save stage still");
        }
        if let Some(stdin) = &mut self.stdin {
            stdin
                .write_all(stage.as_raw())
                .expect("write frame to ffmpeg");
        }
        if let Some(chapter) = self.harness.state().chapter
            && self
                .chapter_starts
                .last()
                .is_none_or(|(c, _)| *c != chapter)
        {
            self.chapter_starts.push((chapter, self.frames));
        }
        self.frames += 1;
    }

    pub fn finish(mut self) {
        let chapters = self
            .chapter_starts
            .iter()
            .map(|&(chapter, frame)| {
                serde_json::json!({
                    "title": self.harness.state().chapters[chapter],
                    "start": (frame as f64 / f64::from(self.fps) * 10.0).round() / 10.0,
                })
            })
            .collect::<Vec<_>>();
        std::fs::write(
            &self.chapters_path,
            serde_json::to_string_pretty(&chapters).unwrap(),
        )
        .expect("write chapters");
        drop(self.stdin.take());
        let status = self.ffmpeg.wait().expect("ffmpeg");
        assert!(status.success(), "ffmpeg failed: {status}");
    }
}

pub(super) fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    let mut load = |key: &str, weight: fontdb::Weight| {
        for family in ["SF Pro Display", "Helvetica Neue", "Inter", "Arial"] {
            let query = fontdb::Query {
                families: &[fontdb::Family::Name(family)],
                weight,
                ..Default::default()
            };
            let Some(id) = db.query(&query) else { continue };
            let Some((bytes, index)) = db.with_face_data(id, |data, index| (data.to_vec(), index))
            else {
                continue;
            };
            let mut data = egui::FontData::from_owned(bytes);
            data.index = index;
            fonts
                .font_data
                .insert(key.to_owned(), std::sync::Arc::new(data));
            return Some(key.to_owned());
        }
        None
    };
    let bold = load("stage_bold", fontdb::Weight::BOLD);
    let text = load("stage_text", fontdb::Weight::NORMAL);
    let fallback = fonts.families[&FontFamily::Proportional].clone();
    for (name, key) in [("bold", bold), ("text", text)] {
        let mut family = key.into_iter().collect::<Vec<_>>();
        family.extend(fallback.iter().cloned());
        fonts.families.insert(FontFamily::Name(name.into()), family);
    }
    ctx.set_fonts(fonts);
}

fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("bold".into()))
}

fn text(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("text".into()))
}

fn paint_stage(ui: &mut egui::Ui, state: &mut StageState) {
    if !state.fonts_ready {
        install_fonts(ui.ctx());
        state.fonts_ready = true;
        return;
    }
    let painter = ui.painter();
    let full = Rect::from_min_size(Pos2::ZERO, STAGE);
    painter.rect_filled(full, 0.0, BG);
    glow(
        painter,
        pos2(STAGE.x * 0.5, -140.0),
        820.0,
        RUST.gamma_multiply(0.30),
    );
    glow(
        painter,
        pos2(STAGE.x * 0.92, STAGE.y + 80.0),
        620.0,
        Color32::from_rgb(88, 196, 220).gamma_multiply(0.10),
    );
    glow(
        painter,
        pos2(STAGE.x * 0.05, STAGE.y * 0.65),
        520.0,
        RUST_2.gamma_multiply(0.06),
    );

    let window = Rect::from_min_size(WINDOW_AT.to_pos2(), super::SIZE);
    let shadow = egui::epaint::Shadow {
        offset: [0, 28],
        blur: 70,
        spread: 0,
        color: Color32::from_black_alpha(190),
    };
    painter.add(shadow.as_shape(window, WINDOW_RADIUS));
    painter.rect_stroke(
        window.expand(1.0),
        WINDOW_RADIUS + 1.0,
        egui::Stroke::new(1.0, Color32::from_white_alpha(28)),
        egui::StrokeKind::Inside,
    );

    // Headline: the previous one fades up and out while the new one settles in.
    let headline_alpha = 1.0 - state.card_t;
    let headline = |(title, subtitle): &(String, String), alpha: f32, dy: f32| {
        if alpha <= 0.0 {
            return;
        }
        painter.text(
            pos2(STAGE.x * 0.5, 50.0 + dy),
            Align2::CENTER_CENTER,
            title,
            bold(31.0),
            TEXT.gamma_multiply(alpha),
        );
        painter.text(
            pos2(STAGE.x * 0.5, 94.0 + dy),
            Align2::CENTER_CENTER,
            subtitle,
            text(17.5),
            MUTED.gamma_multiply(alpha),
        );
    };
    let t = ease(state.headline_t);
    if let Some(previous) = &state.previous {
        headline(previous, (1.0 - t) * headline_alpha, -10.0 * t);
    }
    if let Some(current) = &state.headline {
        headline(current, t * headline_alpha, 10.0 * (1.0 - t));
    }

    // Chapter rail under the window.
    if !state.chapters.is_empty() {
        let y = window.bottom() + 26.0;
        let gap = 34.0;
        let galleys = state
            .chapters
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let color = match state.chapter {
                    Some(c) if c == i => RUST_2,
                    Some(c) if c > i => MUTED,
                    _ => DIM,
                };
                painter.layout_no_wrap((*name).to_owned(), text(13.5), color)
            })
            .collect::<Vec<_>>();
        let width =
            galleys.iter().map(|g| g.size().x).sum::<f32>() + gap * (galleys.len() - 1) as f32;
        let mut x = (STAGE.x - width) * 0.5;
        for (i, galley) in galleys.into_iter().enumerate() {
            let size = galley.size();
            if state.chapter == Some(i) {
                painter.circle_filled(pos2(x - 10.0, y), 3.0, RUST_2);
            }
            painter.galley(pos2(x, y - size.y * 0.5), galley, TEXT);
            x += size.x + gap;
        }
    }

    if let Some(card) = state.card.filter(|_| state.card_t > 0.0) {
        paint_card(ui, card, window, ease(state.card_t));
    }
}

fn paint_card(ui: &mut egui::Ui, card: Card, window: Rect, alpha: f32) {
    let painter = ui.painter();
    painter.rect_filled(
        window,
        WINDOW_RADIUS,
        Color32::from_rgb(20, 23, 28).gamma_multiply(alpha),
    );
    glow(
        painter,
        window.center() - vec2(0.0, 120.0),
        520.0,
        RUST.gamma_multiply(0.18 * alpha),
    );
    let center = window.center();
    let icon = Rect::from_center_size(center - vec2(0.0, 150.0), vec2(112.0, 112.0));
    egui::Image::new(egui::include_image!("../../../assets/app-icon.png"))
        .tint(Color32::WHITE.gamma_multiply(alpha))
        .corner_radius(24)
        .paint_at(ui, icon);
    let painter = ui.painter();
    painter.text(
        center - vec2(0.0, 40.0),
        Align2::CENTER_CENTER,
        "oxi",
        bold(64.0),
        TEXT.gamma_multiply(alpha),
    );
    match card {
        Card::Intro => {
            painter.text(
                center + vec2(0.0, 30.0),
                Align2::CENTER_CENTER,
                "The native coding agent for any model",
                text(26.0),
                TEXT.gamma_multiply(alpha * 0.92),
            );
            painter.text(
                center + vec2(0.0, 72.0),
                Align2::CENTER_CENTER,
                "Local GGUF models · Claude Code · Codex · Cursor · any API",
                text(18.0),
                MUTED.gamma_multiply(alpha),
            );
        }
        Card::Outro => {
            painter.text(
                center + vec2(0.0, 26.0),
                Align2::CENTER_CENTER,
                "Free and open source. One native binary.",
                text(24.0),
                TEXT.gamma_multiply(alpha * 0.92),
            );
            let galley = painter.layout_no_wrap(
                INSTALL_COMMAND.to_owned(),
                FontId::monospace(19.0),
                TEXT.gamma_multiply(alpha),
            );
            let pill =
                Rect::from_center_size(center + vec2(0.0, 96.0), galley.size() + vec2(48.0, 26.0));
            painter.rect(
                pill,
                10.0,
                Color32::from_rgb(10, 12, 15).gamma_multiply(alpha),
                egui::Stroke::new(1.0, RUST.gamma_multiply(0.55 * alpha)),
                egui::StrokeKind::Inside,
            );
            painter.galley(pill.center() - galley.size() * 0.5, galley, TEXT);
            painter.text(
                center + vec2(0.0, 162.0),
                Align2::CENTER_CENTER,
                "macOS · Linux · Windows        maziluiosif.github.io/oxi",
                text(17.0),
                MUTED.gamma_multiply(alpha),
            );
        }
    }
}

/// A soft radial light: a triangle fan from `color` at the center to transparent at `radius`.
fn glow(painter: &egui::Painter, center: Pos2, radius: f32, color: Color32) {
    const RINGS: usize = 24;
    const SEGMENTS: usize = 96;
    let mut mesh = egui::Mesh::default();
    for ring in 0..=RINGS {
        let r = ring as f32 / RINGS as f32;
        // Gaussian-ish falloff, so the edge never shows.
        let a = (-(r * r) * 4.0).exp() * (1.0 - r);
        for s in 0..SEGMENTS {
            let angle = s as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
            mesh.colored_vertex(
                center + vec2(angle.cos(), angle.sin()) * radius * r,
                color.gamma_multiply(a),
            );
        }
    }
    for ring in 0..RINGS {
        for s in 0..SEGMENTS {
            let a = (ring * SEGMENTS + s) as u32;
            let b = (ring * SEGMENTS + (s + 1) % SEGMENTS) as u32;
            let c = a + SEGMENTS as u32;
            let d = b + SEGMENTS as u32;
            mesh.add_triangle(a, b, c);
            mesh.add_triangle(b, d, c);
        }
    }
    painter.add(Shape::mesh(mesh));
}

/// Mix `from` over `into` with weight `amount`.
fn blend_into(into: &mut RgbaImage, from: &RgbaImage, amount: f32) {
    if into.dimensions() != from.dimensions() {
        return;
    }
    let k = (amount.clamp(0.0, 1.0) * 256.0) as u32;
    for (dst, src) in into.as_mut().iter_mut().zip(from.as_raw()) {
        *dst = ((*dst as u32 * (256 - k) + *src as u32 * k) >> 8) as u8;
    }
}

/// Paste the window at `at` with rounded corners, at `opacity`.
fn composite_window(
    stage: &mut RgbaImage,
    app: &RgbaImage,
    at: (u32, u32),
    radius: f32,
    opacity: f32,
) {
    if opacity <= 0.0 {
        return;
    }
    let (w, h) = app.dimensions();
    let corner = radius.ceil() as u32 + 1;
    for y in 0..h {
        let in_corner_rows = y < corner || y + corner >= h;
        if opacity >= 1.0 && !in_corner_rows {
            // Fully inside: copy the row.
            let start = ((at.1 + y) * stage.width() + at.0) as usize * 4;
            let src = &app.as_raw()[(y * w) as usize * 4..((y + 1) * w) as usize * 4];
            stage.as_mut()[start..start + src.len()].copy_from_slice(src);
            continue;
        }
        for x in 0..w {
            // Coverage of the rounded rectangle at this pixel's center.
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let cx = px.clamp(radius, w as f32 - radius);
            let cy = py.clamp(radius, h as f32 - radius);
            let d = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
            let coverage = (radius - d + 0.5).clamp(0.0, 1.0) * opacity;
            if coverage <= 0.0 {
                continue;
            }
            let Some(dst) = stage.get_pixel_mut_checked(at.0 + x, at.1 + y) else {
                continue;
            };
            let src = app.get_pixel(x, y);
            for c in 0..3 {
                dst[c] = (dst[c] as f32 * (1.0 - coverage) + src[c] as f32 * coverage) as u8;
            }
        }
    }
}

/// A macOS-style arrow pointer with its tip at `tip` (pixels).
fn paint_pointer(image: &mut RgbaImage, tip: Pos2, scale: f32, pressed: bool) {
    const ARROW: [(f32, f32); 7] = [
        (0.0, 0.0),
        (0.0, 17.0),
        (4.2, 13.2),
        (7.0, 19.4),
        (9.6, 18.2),
        (6.9, 12.2),
        (12.2, 12.2),
    ];
    let size = if pressed { 0.9 } else { 1.0 } * scale * 1.15;
    let outer: Vec<Pos2> = ARROW
        .iter()
        .map(|&(x, y)| tip + vec2(x, y) * size)
        .collect();
    // The white body: the outline shrunk toward a point inside it.
    let anchor = tip + vec2(3.6, 11.0) * size;
    let inner: Vec<Pos2> = outer
        .iter()
        .map(|p| anchor + (*p - anchor) * 0.78)
        .collect();
    let shadow: Vec<Pos2> = outer.iter().map(|p| *p + vec2(0.0, 1.5 * scale)).collect();
    fill_polygon(image, &shadow, [0, 0, 0], 0.28);
    fill_polygon(image, &outer, [0, 0, 0], 1.0);
    fill_polygon(image, &inner, [255, 255, 255], 1.0);
}

fn fill_polygon(image: &mut RgbaImage, points: &[Pos2], color: [u8; 3], opacity: f32) {
    let min = points
        .iter()
        .fold(pos2(f32::MAX, f32::MAX), |a, p| a.min(*p));
    let max = points
        .iter()
        .fold(pos2(f32::MIN, f32::MIN), |a, p| a.max(*p));
    const SS: usize = 4;
    for y in min.y.floor().max(0.0) as u32..=max.y.ceil() as u32 {
        for x in min.x.floor().max(0.0) as u32..=max.x.ceil() as u32 {
            let mut hits = 0;
            for sy in 0..SS {
                for sx in 0..SS {
                    let p = pos2(
                        x as f32 + (sx as f32 + 0.5) / SS as f32,
                        y as f32 + (sy as f32 + 0.5) / SS as f32,
                    );
                    if inside(points, p) {
                        hits += 1;
                    }
                }
            }
            if hits == 0 {
                continue;
            }
            let coverage = hits as f32 / (SS * SS) as f32 * opacity;
            let Some(dst) = image.get_pixel_mut_checked(x, y) else {
                continue;
            };
            for c in 0..3 {
                dst[c] = (dst[c] as f32 * (1.0 - coverage) + color[c] as f32 * coverage) as u8;
            }
        }
    }
}

fn inside(points: &[Pos2], p: Pos2) -> bool {
    let mut odd = false;
    let mut j = points.len() - 1;
    for i in 0..points.len() {
        let (a, b) = (points[i], points[j]);
        if (a.y > p.y) != (b.y > p.y) && p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x {
            odd = !odd;
        }
        j = i;
    }
    odd
}
