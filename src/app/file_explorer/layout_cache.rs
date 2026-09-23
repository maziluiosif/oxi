//! Cached editor galleys keyed by document revision, wrapping, and display scale.

use std::sync::Arc;

use eframe::egui;

#[derive(Default)]
pub(crate) struct EditorLayoutCache {
    pub(super) revision: u64,
    pub(super) wrap_width_bits: u32,
    pub(super) pixels_per_point_bits: u32,
    pub(super) geometry: Option<Arc<egui::Galley>>,
    /// Colored layout of `syntax_lines` only (see `syntax_window`).
    pub(super) syntax: Option<Arc<egui::Galley>>,
    pub(super) syntax_lines: std::ops::Range<usize>,
    /// The minimap was built from uncolored text while highlighting ran in the background.
    pub(super) minimap_placeholder: bool,
}
