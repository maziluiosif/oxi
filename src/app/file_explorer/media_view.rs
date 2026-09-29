//! Preview tabs for files the text editor cannot edit.
//!
//! Images open in a zoomable, pannable viewer backed by egui's file/image loaders (so decoding
//! happens off the UI thread and animated GIFs play). Video, audio and other binaries get a card
//! that hands playback off to the system's default app; videos show a poster frame generated in
//! the background by Quick Look (macOS) or `ffmpeg`, when either is available.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use eframe::egui::{
    self, Align, Color32, CornerRadius, CursorIcon, Image, Layout, Rect, RichText, Sense,
    TextureOptions, Ui, UiBuilder, Vec2,
};

use super::super::OxiApp;
use crate::theme::*;
use crate::ui::chrome::mini_button_icon_enabled;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MediaKind {
    Image,
    Video,
    Audio,
    /// Not valid UTF-8 or over the editor's size limit; shown as a card only.
    Binary,
}

impl MediaKind {
    pub(crate) fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        Some(match ext.as_str() {
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "tif" | "tiff" | "svg" => {
                Self::Image
            }
            "mp4" | "m4v" | "mov" | "webm" | "mkv" | "avi" | "mpg" | "mpeg" => Self::Video,
            "mp3" | "wav" | "flac" | "ogg" | "oga" | "m4a" | "aac" | "opus" | "aiff" => Self::Audio,
            "pdf" | "zip" | "gz" | "tar" | "7z" | "dmg" | "exe" | "dll" | "so" | "dylib"
            | "wasm" | "ttf" | "otf" | "woff" | "woff2" => Self::Binary,
            _ => return None,
        })
    }

    fn label(self) -> &'static str {
        match self {
            Self::Image => "Image",
            Self::Video => "Video",
            Self::Audio => "Audio",
            Self::Binary => "Binary file",
        }
    }
}

const MIN_ZOOM: f32 = 0.02;
const MAX_ZOOM: f32 = 64.0;
/// At or above this zoom, pixels are drawn crisp (nearest) so they can be inspected.
const NEAREST_ZOOM: f32 = 2.0;

/// Per-file view transform; `zoom: None` means "fit to the viewport".
#[derive(Clone, Copy, Default)]
struct ImageViewState {
    zoom: Option<f32>,
    pan: Vec2,
}

fn file_uri(path: &Path) -> String {
    format!("file://{}", path.display())
}

impl OxiApp {
    pub(super) fn render_media_view(&mut self, ui: &mut Ui, kind: MediaKind) {
        let Some(document) = self.conv.editor.active_document_mut() else {
            return;
        };
        // Nothing to lose in a read-only preview, so external changes reload silently.
        if document.externally_modified {
            ui.ctx().forget_image(&file_uri(&document.path));
            document.disk_modified = std::fs::metadata(&document.path)
                .and_then(|m| m.modified())
                .ok();
            document.externally_modified = false;
        }
        let path = document.path.clone();
        let modified = document.disk_modified;
        match kind {
            MediaKind::Image => image_view(ui, &path),
            _ => media_card(ui, &path, kind, modified),
        }
    }
}

fn file_size_label(path: &Path) -> String {
    std::fs::metadata(path).map_or_else(|_| String::new(), |m| human_size(m.len()))
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

fn external_actions(ui: &mut Ui, path: &Path) {
    if mini_button_icon_enabled(ui, ICON_FOLDER, super::explorer_tree::reveal_label(), true)
        .clicked()
    {
        super::explorer_tree::reveal_path_in_file_manager(path);
    }
    ui.add_space(4.0);
    if mini_button_icon_enabled(ui, ICON_EXTERNAL, "Open in default app", true).clicked() {
        open_with_default_app(path);
    }
}

fn image_view(ui: &mut Ui, path: &Path) {
    let ctx = ui.ctx().clone();
    let state_id = egui::Id::new(("oxi_media_view", path));
    let mut state: ImageViewState = ctx.data(|d| d.get_temp(state_id)).unwrap_or_default();
    let uri = file_uri(path);

    let toolbar_height = 32.0;
    let full = ui.available_rect_before_wrap();
    let toolbar_rect = Rect::from_min_size(full.min, egui::vec2(full.width(), toolbar_height));
    let canvas = Rect::from_min_max(egui::pos2(full.left(), toolbar_rect.bottom()), full.max);

    let options = if state.zoom.is_some_and(|z| z >= NEAREST_ZOOM) {
        TextureOptions::NEAREST
    } else {
        TextureOptions::LINEAR
    };
    let image = Image::from_uri(uri.clone()).texture_options(options);
    let load = image.load_for_size(&ctx, canvas.size());
    let source_size = load.as_ref().ok().and_then(|poll| poll.size());

    let fit = source_size.map_or(1.0, |size| {
        (canvas.width() / size.x)
            .min(canvas.height() / size.y)
            .min(1.0)
    });
    let mut zoom = state.zoom.unwrap_or(fit);

    let mut toolbar = ui.new_child(
        UiBuilder::new()
            .max_rect(toolbar_rect.shrink2(egui::vec2(8.0, 4.0)))
            .layout(Layout::left_to_right(Align::Center)),
    );
    let mut info = file_size_label(path);
    if let Some(size) = source_size {
        info = format!("{} × {}   {info}", size.x as u32, size.y as u32);
    }
    toolbar.label(RichText::new(info).size(FS_SMALL).color(c_text_muted()));
    toolbar.with_layout(Layout::right_to_left(Align::Center), |ui| {
        external_actions(ui, path);
        ui.add_space(8.0);
        if mini_button_icon_enabled(ui, "", "+", true).clicked() {
            zoom = (zoom * 1.25).min(MAX_ZOOM);
            state.zoom = Some(zoom);
        }
        ui.label(
            RichText::new(format!("{:.0}%", zoom * 100.0))
                .size(FS_SMALL)
                .color(c_text()),
        );
        if mini_button_icon_enabled(ui, "", "−", true).clicked() {
            zoom = (zoom / 1.25).max(MIN_ZOOM);
            state.zoom = Some(zoom);
        }
        if mini_button_icon_enabled(ui, "", "1:1", state.zoom != Some(1.0)).clicked() {
            zoom = 1.0;
            state.zoom = Some(1.0);
            state.pan = Vec2::ZERO;
        }
        if mini_button_icon_enabled(ui, "", "Fit", state.zoom.is_some()).clicked() {
            zoom = fit;
            state = ImageViewState::default();
        }
    });

    let response = ui.allocate_rect(canvas, Sense::click_and_drag());
    ui.painter()
        .rect_filled(canvas, CornerRadius::ZERO, c_bg_sidebar());

    match (&load, source_size) {
        (Err(error), _) => {
            ui.painter().text(
                canvas.center(),
                egui::Align2::CENTER_CENTER,
                format!("Could not display this image: {error}"),
                egui::FontId::proportional(FS_SMALL),
                c_error_fg(),
            );
        }
        (Ok(_), None) => {
            egui::Spinner::new().paint_at(
                ui,
                Rect::from_center_size(canvas.center(), Vec2::splat(20.0)),
            );
        }
        (Ok(_), Some(source)) => {
            if response.hovered() {
                let (zoom_delta, scroll, pointer) =
                    ctx.input(|i| (i.zoom_delta(), i.smooth_scroll_delta, i.pointer.hover_pos()));
                // Cmd/Ctrl+scroll and trackpad pinch zoom around the pointer; plain scroll pans.
                if zoom_delta != 1.0 {
                    let new_zoom = (zoom * zoom_delta).clamp(MIN_ZOOM, MAX_ZOOM);
                    let anchor = pointer.unwrap_or(canvas.center()) - canvas.center();
                    state.pan = anchor - (anchor - state.pan) * (new_zoom / zoom);
                    zoom = new_zoom;
                    state.zoom = Some(zoom);
                } else if scroll != Vec2::ZERO {
                    state.pan += scroll;
                }
            }
            if response.dragged() {
                state.pan += response.drag_delta();
            }
            if response.double_clicked() {
                if state.zoom.is_some() {
                    state = ImageViewState::default();
                    zoom = fit;
                } else {
                    let anchor = response.interact_pointer_pos().unwrap_or(canvas.center())
                        - canvas.center();
                    state.pan = anchor - anchor * (1.0 / zoom);
                    zoom = 1.0;
                    state.zoom = Some(1.0);
                }
            }

            let size = source * zoom;
            // Keep the image in view: it can only be panned as far as it overflows the canvas.
            let slack = ((size - canvas.size()) * 0.5).max(Vec2::ZERO);
            state.pan = state.pan.clamp(-slack, slack);
            let rect = Rect::from_center_size(canvas.center() + state.pan, size);

            let pannable = slack != Vec2::ZERO;
            if response.dragged() && pannable {
                ctx.set_cursor_icon(CursorIcon::Grabbing);
            } else if response.hovered() && pannable {
                ctx.set_cursor_icon(CursorIcon::Grab);
            }

            let mut clipped = ui.new_child(UiBuilder::new().max_rect(canvas));
            clipped.set_clip_rect(canvas.intersect(ui.clip_rect()));
            paint_checkerboard(&clipped, rect.intersect(canvas));
            image.paint_at(&clipped, rect);
        }
    }
    ctx.data_mut(|d| d.insert_temp(state_id, state));
}

/// Transparency backdrop, painted only over the visible part of the image.
fn paint_checkerboard(ui: &Ui, visible: Rect) {
    if !visible.is_positive() {
        return;
    }
    let painter = ui.painter();
    painter.rect_filled(visible, CornerRadius::ZERO, Color32::from_gray(58));
    const CELL: f32 = 10.0;
    let dark = Color32::from_gray(46);
    let x0 = (visible.left() / CELL).floor() as i64;
    let y0 = (visible.top() / CELL).floor() as i64;
    let x1 = (visible.right() / CELL).ceil() as i64;
    let y1 = (visible.bottom() / CELL).ceil() as i64;
    for y in y0..y1 {
        for x in x0..x1 {
            if (x + y) % 2 == 0 {
                continue;
            }
            let cell = Rect::from_min_size(
                egui::pos2(x as f32 * CELL, y as f32 * CELL),
                Vec2::splat(CELL),
            )
            .intersect(visible);
            painter.rect_filled(cell, CornerRadius::ZERO, dark);
        }
    }
}

fn media_card(ui: &mut Ui, path: &Path, kind: MediaKind, modified: Option<std::time::SystemTime>) {
    let name = path
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let size = file_size_label(path);
    let too_large = kind == MediaKind::Binary
        && std::fs::metadata(path).is_ok_and(|m| m.len() > super::documents::MAX_TEXT_FILE_BYTES)
        && MediaKind::from_path(path).is_none();
    let poster = (kind == MediaKind::Video)
        .then(|| poster_for(ui.ctx(), path, modified))
        .flatten();

    ui.painter().rect_filled(
        ui.available_rect_before_wrap(),
        CornerRadius::ZERO,
        c_bg_sidebar(),
    );
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(40.0);
                match &poster {
                    Some(PosterState::Ready(poster)) => {
                        let max = egui::vec2((ui.available_width() - 48.0).min(720.0), 405.0);
                        let response = ui
                            .add(
                                Image::from_uri(file_uri(poster))
                                    .fit_to_exact_size(max)
                                    .corner_radius(CornerRadius::same(RADIUS_CARD))
                                    .sense(Sense::click()),
                            )
                            .on_hover_cursor(CursorIcon::PointingHand)
                            .on_hover_text("Play in the default app");
                        paint_play_badge(ui, response.rect.center(), response.hovered());
                        if response.clicked() {
                            open_with_default_app(path);
                        }
                    }
                    Some(PosterState::Pending) => {
                        ui.add_space(60.0);
                        ui.spinner();
                        ui.add_space(60.0);
                    }
                    _ => {
                        let icon = match kind {
                            MediaKind::Video => ICON_PLAY,
                            MediaKind::Audio => ICON_MIC,
                            _ => ICON_FILE,
                        };
                        ui.label(icon_glyph(icon, 56.0, c_text_faint()));
                    }
                }
                ui.add_space(16.0);
                ui.label(RichText::new(&name).size(FS_H3).color(c_text()));
                ui.add_space(4.0);
                let label = if too_large {
                    "Too large for the editor"
                } else {
                    kind.label()
                };
                ui.label(
                    RichText::new(format!("{label} · {size}"))
                        .size(FS_SMALL)
                        .color(c_text_muted()),
                );
                ui.add_space(4.0);
                let hint = match kind {
                    MediaKind::Video | MediaKind::Audio => "Plays in your default media app.",
                    _ => "oxi can't preview this file.",
                };
                ui.label(RichText::new(hint).size(FS_SMALL).color(c_text_faint()));
                ui.add_space(16.0);
                // Two centered buttons: lay them out in a fixed-width row.
                ui.allocate_ui_with_layout(
                    egui::vec2(330.0, 28.0),
                    Layout::right_to_left(Align::Center),
                    |ui| external_actions(ui, path),
                );
            });
        });
}

fn icon_glyph(icon: &str, size: f32, color: Color32) -> RichText {
    crate::ui::chrome::icon_glyph_rich(icon, size, color)
}

fn paint_play_badge(ui: &Ui, center: egui::Pos2, hovered: bool) {
    let painter = ui.painter();
    let radius = 30.0;
    painter.circle_filled(
        center,
        radius,
        Color32::from_black_alpha(if hovered { 190 } else { 140 }),
    );
    let r = radius * 0.42;
    let tip = center + egui::vec2(r * 1.1, 0.0);
    let top = center + egui::vec2(-r * 0.65, -r);
    let bottom = center + egui::vec2(-r * 0.65, r);
    painter.add(egui::Shape::convex_polygon(
        vec![top, tip, bottom],
        Color32::WHITE,
        egui::Stroke::NONE,
    ));
}

#[derive(Clone)]
enum PosterState {
    Pending,
    Ready(PathBuf),
    Failed,
}

fn posters() -> &'static Mutex<HashMap<PathBuf, PosterState>> {
    static POSTERS: OnceLock<Mutex<HashMap<PathBuf, PosterState>>> = OnceLock::new();
    POSTERS.get_or_init(Default::default)
}

/// Poster frame for `video`, cached on disk per path + mtime; generated on a worker thread.
fn poster_for(
    ctx: &egui::Context,
    video: &Path,
    modified: Option<std::time::SystemTime>,
) -> Option<PosterState> {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    video.hash(&mut hasher);
    modified.hash(&mut hasher);
    let out = std::env::temp_dir()
        .join("oxi-posters")
        .join(format!("{:016x}.png", hasher.finish()));

    let mut cache = posters().lock().ok()?;
    if let Some(state) = cache.get(&out) {
        return Some(state.clone());
    }
    if out.is_file() {
        cache.insert(out.clone(), PosterState::Ready(out.clone()));
        return Some(PosterState::Ready(out));
    }
    cache.insert(out.clone(), PosterState::Pending);
    drop(cache);
    let (ctx, video) = (ctx.clone(), video.to_path_buf());
    std::thread::spawn(move || {
        let state = if generate_poster(&video, &out) {
            PosterState::Ready(out.clone())
        } else {
            PosterState::Failed
        };
        if let Ok(mut posters) = posters().lock() {
            posters.insert(out, state);
        }
        ctx.request_repaint();
    });
    Some(PosterState::Pending)
}

fn quiet(command: &mut std::process::Command) -> bool {
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn generate_poster(video: &Path, out: &Path) -> bool {
    let Some(dir) = out.parent() else {
        return false;
    };
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    // Quick Look ships with macOS and thumbnails every format AVFoundation can play.
    #[cfg(target_os = "macos")]
    {
        let scratch = dir.join(format!(
            "{}.ql",
            out.file_stem().and_then(|s| s.to_str()).unwrap_or("poster")
        ));
        let _ = std::fs::create_dir_all(&scratch);
        let ok = quiet(
            std::process::Command::new("qlmanage")
                .args(["-t", "-s", "1280", "-o"])
                .arg(&scratch)
                .arg(video),
        );
        let produced = video
            .file_name()
            .map(|name| scratch.join(format!("{}.png", name.to_string_lossy())));
        let moved = ok && produced.is_some_and(|p| std::fs::rename(p, out).is_ok());
        let _ = std::fs::remove_dir_all(&scratch);
        if moved {
            return true;
        }
    }
    // Seek a second in to skip black lead-in frames; very short clips fall back to frame 0.
    ["1", "0"].iter().any(|seek| {
        quiet(
            std::process::Command::new("ffmpeg")
                .args(["-y", "-loglevel", "error", "-ss", seek, "-i"])
                .arg(video)
                .args(["-frames:v", "1", "-vf", "scale='min(1280,iw)':-2"])
                .arg(out),
        ) && out.is_file()
    })
}

pub(super) fn open_with_default_app(path: &Path) {
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = std::process::Command::new("open");
        command.arg(path);
        command
    };
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = std::process::Command::new("cmd");
        command.args(["/C", "start", ""]).arg(path);
        command
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = {
        let mut command = std::process::Command::new("xdg-open");
        command.arg(path);
        command
    };
    let _ = command.spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_media_by_extension() {
        assert_eq!(
            MediaKind::from_path(Path::new("a/B.PNG")),
            Some(MediaKind::Image)
        );
        assert_eq!(
            MediaKind::from_path(Path::new("clip.mov")),
            Some(MediaKind::Video)
        );
        assert_eq!(
            MediaKind::from_path(Path::new("song.flac")),
            Some(MediaKind::Audio)
        );
        assert_eq!(
            MediaKind::from_path(Path::new("doc.pdf")),
            Some(MediaKind::Binary)
        );
        assert_eq!(MediaKind::from_path(Path::new("main.rs")), None);
        assert_eq!(MediaKind::from_path(Path::new("Makefile")), None);
    }

    #[test]
    fn formats_sizes() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
    }
}
