//! Diff mode for image tabs: the image as Git has it next to the current one, or both under
//! a draggable divider ("swipe"). The text diff's counterpart for files with no lines.

use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::mpsc::{Receiver, TryRecvError};

use eframe::egui::{
    self, Align, Align2, CornerRadius, CursorIcon, FontId, Image, Layout, Rect, RichText, Sense,
    Stroke, Ui, UiBuilder, Vec2, pos2, vec2,
};

use super::super::OxiApp;
use super::DiffSource;
use super::media_view::{file_size_label, human_size, image_uri, paint_checkerboard};
use crate::theme::*;
use crate::ui::diff_view::{segment, toolbar_icon};

const TOOLBAR_H: f32 = 32.0;
const CAPTION_H: f32 = 26.0;
/// Narrower than this, side by side leaves two thumbnails: fall back to swipe.
const MIN_SPLIT_WIDTH: f32 = 420.0;

type Blobs = Result<(Option<Vec<u8>>, Option<Vec<u8>>), String>;

/// One side read from Git, handed to egui's image loaders as bytes under its own URI.
struct Side {
    uri: String,
    len: usize,
    hash: u64,
    ctx: egui::Context,
}

impl Drop for Side {
    fn drop(&mut self) {
        // Frees the bytes and the decoded texture.
        self.ctx.forget_image(&self.uri);
    }
}

/// The Git sides of an image tab's diff (see `DocumentDiff::images`).
#[derive(Default)]
pub(crate) struct ImageDiffState {
    loading: Option<Receiver<Blobs>>,
    /// `git_ui.epoch` the sides were last requested at: a newer Git snapshot re-reads them.
    loaded_epoch: Option<u64>,
    loaded: bool,
    /// The base image; `None` when the file is not in the base (a new file).
    old: Option<Side>,
    /// The index image of a staged diff (its right side).
    staged: Option<Side>,
    error: Option<String>,
    version: u64,
}

impl ImageDiffState {
    fn set_side(
        slot: &mut Option<Side>,
        bytes: Option<Vec<u8>>,
        ctx: &egui::Context,
        uri: impl FnOnce() -> String,
    ) {
        let Some(bytes) = bytes.filter(|bytes| !bytes.is_empty()) else {
            *slot = None;
            return;
        };
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        bytes.hash(&mut hasher);
        let hash = hasher.finish();
        if slot.as_ref().is_some_and(|side| side.hash == hash) {
            return;
        }
        let uri = uri();
        let len = bytes.len();
        // Drop the previous side first: its URI is forgotten before the new one is included.
        *slot = None;
        ctx.include_bytes(uri.clone(), bytes);
        *slot = Some(Side {
            uri,
            len,
            hash,
            ctx: ctx.clone(),
        });
    }
}

/// Names of the two sides, for the captions.
fn side_labels(source: &DiffSource) -> (String, &'static str) {
    match source {
        DiffSource::WorkTree => ("Index".to_owned(), "Working Tree"),
        DiffSource::Staged => ("HEAD".to_owned(), "Index"),
        DiffSource::Compare { base, .. } => (
            crate::git::checkpoint::base_label(base).to_owned(),
            "Working Tree",
        ),
        DiffSource::Unsaved => ("Saved".to_owned(), "Unsaved"),
    }
}

/// What one pane shows.
struct Shown<'a> {
    label: &'a str,
    /// `None`: the file does not exist on this side.
    uri: Option<String>,
    size_label: String,
}

impl OxiApp {
    /// Read (or re-read after a Git change) the Git sides of image document `index`'s diff.
    fn poll_image_diff(&mut self, index: usize, ctx: &egui::Context) {
        let epoch = self.conv.git_ui.epoch;
        let root = self.active_workspace().root_path.clone();
        let Some(document) = self.conv.editor.documents.get_mut(index) else {
            return;
        };
        let extension = document
            .path
            .extension()
            .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let Some(diff) = document.diff.as_mut() else {
            return;
        };
        let Some(git_base) = diff.source.git_base() else {
            diff.images.error = Some("An image has no unsaved changes to compare.".to_owned());
            return;
        };
        let relative = diff.relative.clone();
        let state = &mut diff.images;
        if let Some(rx) = &state.loading {
            match rx.try_recv() {
                Ok(Ok((old, staged))) => {
                    state.version += 1;
                    let version = state.version;
                    let mut hasher = std::collections::hash_map::DefaultHasher::new();
                    (&root, &relative).hash(&mut hasher);
                    let key = hasher.finish();
                    let uri = |side: &str| {
                        format!("bytes://oxi-image-diff/{key:x}/{version}/{side}.{extension}")
                    };
                    ImageDiffState::set_side(&mut state.old, old, ctx, || uri("old"));
                    ImageDiffState::set_side(&mut state.staged, staged, ctx, || uri("new"));
                    state.loaded = true;
                    state.error = None;
                }
                Ok(Err(error)) => state.error = Some(error),
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    state.error = Some("the Git reader stopped".to_owned());
                }
            }
            state.loading = None;
        }
        if state.loaded_epoch == Some(epoch) {
            return;
        }
        state.loaded_epoch = Some(epoch);
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(crate::git::diff_blobs(&root, &relative, &git_base));
            ctx.request_repaint();
        });
        state.loading = Some(rx);
    }

    /// The active image document in diff mode: toolbar, then both versions.
    pub(super) fn render_image_diff(&mut self, ui: &mut Ui) {
        let Some(index) = self.conv.editor.active else {
            return;
        };
        let ctx = ui.ctx().clone();
        self.poll_image_diff(index, &ctx);
        let document = &mut self.conv.editor.documents[index];
        super::media_view::reload_if_changed(&ctx, document);
        let path = document.path.clone();
        let Some(diff) = document.diff.as_ref() else {
            return;
        };
        let state = &diff.images;
        let (old_label, new_label) = side_labels(&diff.source);
        let staged = diff.source == DiffSource::Staged;
        let relative = diff.relative.clone();

        let full = ui.available_rect_before_wrap();
        let toolbar_rect = Rect::from_min_size(full.min, vec2(full.width(), TOOLBAR_H));
        let body = Rect::from_min_max(pos2(full.left(), toolbar_rect.bottom()), full.max);
        ui.allocate_rect(full, Sense::hover());
        ui.painter()
            .rect_filled(body, CornerRadius::ZERO, c_bg_sidebar());

        let split_possible = body.width() >= MIN_SPLIT_WIDTH;
        let swipe_id = egui::Id::new("oxi_image_diff_swipe");
        let mut swipe = ctx.data(|d| d.get_temp::<bool>(swipe_id).unwrap_or(false));

        let mut close = false;
        let mut toolbar = ui.new_child(
            UiBuilder::new()
                .id_salt("image_diff_toolbar")
                .max_rect(toolbar_rect.shrink2(vec2(10.0, 0.0)))
                .layout(Layout::left_to_right(Align::Center)),
        );
        toolbar.set_clip_rect(toolbar_rect.intersect(ui.clip_rect()));
        toolbar.spacing_mut().item_spacing.x = 6.0;
        let (dir, name) = relative.rsplit_once('/').unwrap_or(("", relative.as_str()));
        toolbar.label(RichText::new(name).size(FS_SMALL).color(c_text_strong()));
        if !dir.is_empty() {
            toolbar.add(
                egui::Label::new(RichText::new(dir).size(FS_TINY).color(c_text_faint())).truncate(),
            );
        }
        toolbar.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            if toolbar_icon(ui, ICON_CLOSE, "Close diff, keep the image open", true).clicked() {
                close = true;
            }
            ui.add_space(6.0);
            let hint = "Both versions under a divider you drag";
            if segment(ui, "Swipe", swipe || !split_possible, true, hint).clicked() {
                swipe = true;
            }
            let hint = if split_possible {
                "Show old and new side by side"
            } else {
                "Too narrow for side by side"
            };
            if segment(
                ui,
                "Side by side",
                !swipe && split_possible,
                split_possible,
                hint,
            )
            .clicked()
            {
                swipe = false;
            }
            ui.add_space(6.0);
            ui.label(
                RichText::new(format!("{old_label} vs {new_label}"))
                    .size(FS_TINY)
                    .color(c_text_muted()),
            );
        });
        ctx.data_mut(|d| d.insert_temp(swipe_id, swipe));
        ui.painter().hline(
            full.x_range(),
            toolbar_rect.bottom() - 0.5,
            Stroke::new(1.0, c_border_subtle()),
        );
        if close {
            self.close_document_diff(index);
            return;
        }

        if let Some(error) = &state.error {
            centered_note(ui, body, error, c_error_fg());
            return;
        }
        if !state.loaded {
            crate::theme::paint_spinner(
                ui,
                Rect::from_center_size(body.center(), Vec2::splat(20.0)),
                ui.visuals().strong_text_color(),
            );
            return;
        }

        let old = Shown {
            label: &old_label,
            uri: state.old.as_ref().map(|side| side.uri.clone()),
            size_label: state
                .old
                .as_ref()
                .map_or_else(String::new, |side| human_size(side.len as u64)),
        };
        let new = if staged {
            Shown {
                label: new_label,
                uri: state.staged.as_ref().map(|side| side.uri.clone()),
                size_label: state
                    .staged
                    .as_ref()
                    .map_or_else(String::new, |side| human_size(side.len as u64)),
            }
        } else {
            Shown {
                label: new_label,
                uri: Some(image_uri(&ctx, &path)),
                size_label: file_size_label(&path),
            }
        };

        if swipe || !split_possible {
            swipe_view(ui, body, &old, &new, &path);
        } else {
            let middle = body.center().x.round();
            let left = Rect::from_min_max(body.min, pos2(middle, body.bottom()));
            let right = Rect::from_min_max(pos2(middle, body.top()), body.max);
            pane(ui, left, &old);
            pane(ui, right, &new);
            ui.painter()
                .vline(middle, body.y_range(), Stroke::new(1.0, c_border_subtle()));
        }
    }
}

fn centered_note(ui: &Ui, rect: Rect, text: &str, color: egui::Color32) {
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        text,
        FontId::proportional(FS_SMALL),
        color,
    );
}

/// The image at `uri`, once decoded: its painter and pixel size.
fn loaded(ui: &Ui, uri: &str, max: Vec2) -> Result<Option<(Image<'static>, Vec2)>, String> {
    let image = Image::from_uri(uri.to_owned());
    match image.load_for_size(ui.ctx(), max) {
        Ok(poll) => Ok(poll.size().map(|size| (image, size))),
        Err(error) => Err(format!("Could not display this image: {error}")),
    }
}

/// `source` scaled down (never up) to fit `canvas`, centered.
fn fit_rect(canvas: Rect, source: Vec2) -> Rect {
    let scale = (canvas.width() / source.x)
        .min(canvas.height() / source.y)
        .min(1.0);
    Rect::from_center_size(canvas.center(), source * scale)
}

fn caption(ui: &Ui, rect: Rect, label: &str, details: &str) {
    let painter = ui.painter().with_clip_rect(rect);
    painter.text(
        pos2(rect.left() + 10.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(FS_TINY),
        c_text(),
    );
    painter.text(
        pos2(rect.right() - 10.0, rect.center().y),
        Align2::RIGHT_CENTER,
        details,
        FontId::proportional(FS_TINY),
        c_text_muted(),
    );
}

fn details(size: Option<Vec2>, size_label: &str) -> String {
    match size {
        Some(size) => format!("{} × {}   {size_label}", size.x as u32, size.y as u32),
        None => size_label.to_owned(),
    }
}

/// One side of the side-by-side layout: caption, then the image fitted to the pane.
fn pane(ui: &Ui, rect: Rect, shown: &Shown<'_>) {
    let caption_rect = Rect::from_min_size(rect.min, vec2(rect.width(), CAPTION_H));
    let canvas =
        Rect::from_min_max(pos2(rect.left(), caption_rect.bottom()), rect.max).shrink(12.0);
    let Some(uri) = &shown.uri else {
        caption(ui, caption_rect, shown.label, "");
        centered_note(
            ui,
            canvas,
            &format!("Not in {}", shown.label),
            c_text_faint(),
        );
        return;
    };
    match loaded(ui, uri, canvas.size()) {
        Err(error) => {
            caption(ui, caption_rect, shown.label, &shown.size_label);
            centered_note(ui, canvas, &error, c_error_fg());
        }
        Ok(None) => {
            caption(ui, caption_rect, shown.label, &shown.size_label);
            crate::theme::paint_spinner(
                ui,
                Rect::from_center_size(canvas.center(), Vec2::splat(20.0)),
                ui.visuals().strong_text_color(),
            );
        }
        Ok(Some((image, size))) => {
            caption(
                ui,
                caption_rect,
                shown.label,
                &details(Some(size), &shown.size_label),
            );
            let target = fit_rect(canvas, size);
            paint_checkerboard(ui, target);
            image.paint_at(ui, target);
        }
    }
}

/// Both versions in one canvas: the old one left of a draggable divider, the new one right.
fn swipe_view(ui: &mut Ui, body: Rect, old: &Shown<'_>, new: &Shown<'_>, path: &Path) {
    let caption_rect = Rect::from_min_size(body.min, vec2(body.width(), CAPTION_H));
    let canvas =
        Rect::from_min_max(pos2(body.left(), caption_rect.bottom()), body.max).shrink(12.0);
    let load = |shown: &Shown<'_>| match &shown.uri {
        Some(uri) => loaded(ui, uri, canvas.size()),
        None => Ok(None),
    };
    let (old_image, new_image) = match (load(old), load(new)) {
        (Err(error), _) | (_, Err(error)) => {
            centered_note(ui, canvas, &error, c_error_fg());
            return;
        }
        (Ok(old_image), Ok(new_image)) => (old_image, new_image),
    };
    let side_details = |shown: &Shown<'_>, image: &Option<(Image<'static>, Vec2)>| {
        if shown.uri.is_none() {
            format!("{} · not there", shown.label)
        } else {
            format!(
                "{} · {}",
                shown.label,
                details(image.as_ref().map(|(_, size)| *size), &shown.size_label)
            )
        }
    };
    let painter = ui.painter().with_clip_rect(caption_rect);
    painter.text(
        pos2(caption_rect.left() + 10.0, caption_rect.center().y),
        Align2::LEFT_CENTER,
        side_details(old, &old_image),
        FontId::proportional(FS_TINY),
        c_text_muted(),
    );
    painter.text(
        pos2(caption_rect.right() - 10.0, caption_rect.center().y),
        Align2::RIGHT_CENTER,
        side_details(new, &new_image),
        FontId::proportional(FS_TINY),
        c_text_muted(),
    );

    // The area both images cover; the divider travels across it.
    let rects = [&old_image, &new_image]
        .map(|image| image.as_ref().map(|(_, size)| fit_rect(canvas, *size)));
    let Some(span) = rects.iter().flatten().copied().reduce(|a, b| a.union(b)) else {
        // Still decoding.
        crate::theme::paint_spinner(
            ui,
            Rect::from_center_size(canvas.center(), Vec2::splat(20.0)),
            ui.visuals().strong_text_color(),
        );
        return;
    };

    let split_id = egui::Id::new(("oxi_image_diff_split", path));
    let mut fraction = ui
        .ctx()
        .data(|d| d.get_temp::<f32>(split_id))
        .unwrap_or(0.5);
    let response = ui.interact(canvas, split_id, Sense::click_and_drag());
    if (response.dragged() || response.clicked())
        && let Some(pointer) = response.interact_pointer_pos()
    {
        fraction = ((pointer.x - span.left()) / span.width().max(1.0)).clamp(0.0, 1.0);
        ui.ctx().data_mut(|d| d.insert_temp(split_id, fraction));
    }
    if response.hovered() || response.dragged() {
        ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
    }
    let divider = (span.left() + span.width() * fraction).round();

    paint_checkerboard(ui, span);
    let sides = [
        (old_image, rects[0], canvas.left(), divider),
        (new_image, rects[1], divider, canvas.right()),
    ];
    for (image, rect, from, to) in sides {
        let (Some((image, _)), Some(rect)) = (image, rect) else {
            continue;
        };
        let clip = Rect::from_min_max(pos2(from, canvas.top()), pos2(to, canvas.bottom()));
        let mut clipped = ui.new_child(UiBuilder::new().max_rect(canvas));
        clipped.set_clip_rect(clip.intersect(ui.clip_rect()));
        image.paint_at(&clipped, rect);
    }

    let painter = ui.painter();
    painter.vline(
        divider,
        span.y_range(),
        Stroke::new(3.0, egui::Color32::from_black_alpha(90)),
    );
    painter.vline(divider, span.y_range(), Stroke::new(1.0, c_accent()));
    let handle = Rect::from_center_size(pos2(divider, span.center().y), vec2(6.0, 28.0));
    painter.rect_filled(handle, CornerRadius::same(3), c_accent());
}
