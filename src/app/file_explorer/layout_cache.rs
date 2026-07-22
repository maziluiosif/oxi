//! Cached editor galleys keyed by document revision, wrapping, and display scale.

use std::sync::Arc;

use eframe::egui;

#[derive(Default)]
pub(crate) struct EditorLayoutCache {
    pub(super) revision: u64,
    pub(super) wrap_width_bits: u32,
    pub(super) pixels_per_point_bits: u32,
    pub(super) geometry: Option<Arc<egui::Galley>>,
    pub(super) syntax: Option<Arc<egui::Galley>>,
}
