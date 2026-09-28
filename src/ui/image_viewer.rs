//! Full-size viewer for image attachments in the transcript.
//!
//! Clicking a thumbnail calls [`open`], which decodes the original bytes into a texture and
//! parks it in egui temp data; [`show`] (called once per frame from the app's top-level `ui`)
//! draws it in a modal, fitted to the window. Escape, a backdrop click, or a click on the image
//! closes it. Living outside the transcript keeps it open while the message scrolls away.

use eframe::egui::{self, Color32, CornerRadius, Frame, Id, Image, Sense, TextureHandle};

/// Longest side of the decoded full-size texture; keeps GPU memory bounded for huge photos.
const MAX_TEXTURE_SIDE: u32 = 4096;

fn state_id() -> Id {
    Id::new("oxi_image_viewer")
}

/// Decode `data` at full resolution and open it in the viewer.
pub fn open(ctx: &egui::Context, data: &[u8]) {
    let Ok(img) = image::load_from_memory(data) else {
        return;
    };
    let img = if img.width().max(img.height()) > MAX_TEXTURE_SIDE {
        img.resize(
            MAX_TEXTURE_SIDE,
            MAX_TEXTURE_SIDE,
            image::imageops::FilterType::Triangle,
        )
    } else {
        img
    };
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    let color_image = egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
    let tex = ctx.load_texture("image_viewer", color_image, egui::TextureOptions::LINEAR);
    ctx.data_mut(|d| d.insert_temp(state_id(), tex));
}

/// Render the viewer modal if an image is open.
pub fn show(ctx: &egui::Context) {
    let Some(tex) = ctx.data(|d| d.get_temp::<TextureHandle>(state_id())) else {
        return;
    };
    let screen = ctx.content_rect();
    let max = egui::vec2(screen.width() * 0.9, screen.height() * 0.9);
    let mut sz = tex.size_vec2();
    let scale = (max.x / sz.x).min(max.y / sz.y).min(1.0);
    sz *= scale;

    let mut close = false;
    let modal = egui::Modal::new(state_id())
        .frame(Frame::new().corner_radius(CornerRadius::same(6)))
        .backdrop_color(Color32::from_black_alpha(200))
        .show(ctx, |ui| {
            let resp = ui
                .add(
                    Image::new((tex.id(), sz))
                        .corner_radius(CornerRadius::same(6))
                        .sense(Sense::click()),
                )
                .on_hover_cursor(egui::CursorIcon::ZoomOut);
            close |= resp.clicked();
        });
    if close || modal.should_close() {
        ctx.data_mut(|d| d.remove::<TextureHandle>(state_id()));
    }
}
