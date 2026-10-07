//! Cached editor galleys keyed by document revision, wrapping, and display scale.

use std::sync::Arc;

use eframe::egui;

#[derive(Default)]
pub(crate) struct EditorLayoutCache {
    pub(super) revision: u64,
    pub(super) wrap_width_bits: u32,
    pub(super) pixels_per_point_bits: u32,
    /// [`crate::theme::fonts_generation`] `geometry` was laid out with.
    pub(super) fonts_generation: u64,
    /// [`super::diff_editor::DocumentDiff::layout_key`] `geometry` was laid out for; 0 outside
    /// diff mode.
    pub(super) diff_key: u64,
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
    /// Per-paragraph galleys of the last laid-out text; kept across edits so a keystroke
    /// re-lays only the edited paragraph (see [`super::line_layout`]).
    pub(super) lines: Option<super::line_layout::LineLayout>,
    /// Bracket pair (char indices) touching the caret at the given char index; recomputed
    /// only when the caret moves (the whole cache resets on edit).
    pub(super) bracket_pair: Option<(usize, Option<(usize, usize)>)>,
    /// Git gutter markers projected onto the edited text, and the on-disk markers they came from.
    pub(super) live_git_lines: Option<(
        Vec<crate::git::GitLineChange>,
        Vec<crate::git::GitLineChange>,
    )>,
}
