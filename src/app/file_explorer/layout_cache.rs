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
    /// [`crate::theme::palette_generation`] the `syntax` galley's colors were taken from.
    pub(super) syntax_palette: u64,
    /// The minimap was built from uncolored text (while typing, or while highlighting ran in
    /// the background); [`super::minimap::refresh`] colors it once the document settles.
    pub(super) minimap_placeholder: bool,
    /// When the user last changed the text; `None` after a load or reload.
    pub(super) edited_at: Option<std::time::Instant>,
    /// The editor had keyboard focus last frame: keep egui's per-line layouts alive so the next
    /// keystroke re-lays only the edited line.
    pub(super) keep_warm: bool,
}
