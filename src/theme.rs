//! Theme: color palette + registry, font/style setup, and shared formatting helpers.
//!
//! Split by responsibility into submodules ([`palette`], [`catalog`], [`style`],
//! [`format`]) and re-exported here, so every existing `crate::theme::X` / `use
//! crate::theme::*;` call site keeps working unchanged.

mod catalog;
mod format;
mod palette;
mod style;

pub use catalog::*;
pub use format::*;
pub use palette::*;
pub use style::*;

use eframe::egui::{self, Ui};

/// Max width for message/composer column (left-aligned; extra space stays on the right).
pub const CHAT_COLUMN_MAX: f32 = 720.0;

/// Shared corner radii — every rounded surface should use one of these tokens.
pub const RADIUS_ROW: f32 = 5.0;
pub const RADIUS_BUTTON: f32 = 7.0;
pub const RADIUS_CHIP: f32 = 8.0;
pub const RADIUS_PANEL: f32 = 14.0;

/// Draggable strip between sidebar and chat (must match `render_main_area`).
pub const SIDEBAR_RESIZE_SEP_W: f32 = 5.0;
pub const CHAT_VIEW_MARGIN_LEFT: f32 = 12.0;
pub const CHAT_VIEW_MARGIN_RIGHT: f32 = 8.0;
/// Inner margin of the chat [`Frame`] (transcript + composer stack).
pub const CHAT_FRAME_TOP: f32 = 10.0;
pub const CHAT_FRAME_BOTTOM: f32 = 10.0;

/// Usable width for the chat column (transcript + bottom composer). Subtracts scrollbar gutter so
/// input and transcript line up.
pub fn chat_column_center_width(available: f32, style: &egui::Style) -> f32 {
    let g = style.spacing.scroll.allocated_width();
    (available - g).max(1.0)
}

/// Wrap width for transcript text (thinking, markdown, tool bodies). `available_width()` alone can
/// exceed the visible column inside [`egui::CollapsingHeader`] and similar; clamp to the current
/// [`Ui::max_rect`] and [`CHAT_COLUMN_MAX`] so [`eframe::egui::text::LayoutJob`] wrap and frames
/// stay inside the chat.
pub fn content_wrap_width(ui: &Ui) -> f32 {
    let avail = ui.available_width();
    if !avail.is_finite() || avail <= 0.0 {
        return CHAT_COLUMN_MAX.max(48.0);
    }
    let rect_w = ui.max_rect().width();
    let bounded = if rect_w.is_finite() && rect_w > 1.0 {
        avail.min(rect_w)
    } else {
        avail
    };
    bounded.clamp(48.0, CHAT_COLUMN_MAX)
}

pub const MAX_IMAGE_ATTACHMENT_BYTES: usize = 6 * 1024 * 1024;
pub const MAX_PENDING_IMAGES: usize = 8;
