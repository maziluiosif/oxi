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

use eframe::egui::{self, Id, Ui};

/// Default max width for message/composer column (left-aligned; extra space stays on the
/// right). The user can widen this via the "Chat width" slider in Appearance settings; see
/// [`chat_column_max_width`].
pub const CHAT_COLUMN_MAX_DEFAULT: f32 = 720.0;

/// Slider bounds for the user-configurable chat column width (see
/// [`crate::settings::AppSettings::chat_column_max_width`]).
pub const CHAT_COLUMN_WIDTH_MIN: f32 = 480.0;
pub const CHAT_COLUMN_WIDTH_MAX: f32 = 1400.0;

fn chat_column_max_width_id() -> Id {
    Id::new("oxi_chat_column_max_width")
}

/// Publishes the configured chat column width for this frame. Called once per frame from
/// the top-level `App::ui` (which has the live [`AppSettings`](crate::settings::AppSettings)),
/// so every downstream renderer — including free functions like [`content_wrap_width`] that
/// only see a `&Ui` — can read the current value via [`chat_column_max_width`].
pub fn set_chat_column_max_width(ctx: &egui::Context, width: f32) {
    ctx.data_mut(|d| d.insert_temp(chat_column_max_width_id(), width));
}

/// Current max width for the message/composer column. Falls back to
/// [`CHAT_COLUMN_MAX_DEFAULT`] if [`set_chat_column_max_width`] hasn't run yet this frame.
pub fn chat_column_max_width(ctx: &egui::Context) -> f32 {
    ctx.data(|d| d.get_temp(chat_column_max_width_id()))
        .unwrap_or(CHAT_COLUMN_MAX_DEFAULT)
}

/// Shared corner radii — every rounded surface should use one of these tokens.
/// List rows (sidebar chats, settings nav, git changes).
pub const RADIUS_ROW: u8 = 6;
/// Buttons, inputs, small inline banners.
pub const RADIUS_BUTTON: u8 = 7;
/// Chips, pills, toolbar icons, nested panels.
pub const RADIUS_CHIP: u8 = 8;
/// Cards and bubbles (settings cards, user bubbles, code blocks' outer frame).
pub const RADIUS_CARD: u8 = 10;
/// Large floating surfaces (composer card).
pub const RADIUS_PANEL: u8 = 14;

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
/// [`Ui::max_rect`] and [`chat_column_max_width`] so [`eframe::egui::text::LayoutJob`] wrap and frames
/// stay inside the chat.
pub fn content_wrap_width(ui: &Ui) -> f32 {
    let max_w = chat_column_max_width(ui.ctx());
    let avail = ui.available_width();
    if !avail.is_finite() || avail <= 0.0 {
        return max_w.max(48.0);
    }
    let rect_w = ui.max_rect().width();
    let bounded = if rect_w.is_finite() && rect_w > 1.0 {
        avail.min(rect_w)
    } else {
        avail
    };
    bounded.clamp(48.0, max_w)
}

pub const MAX_IMAGE_ATTACHMENT_BYTES: usize = 6 * 1024 * 1024;
pub const MAX_PENDING_IMAGES: usize = 8;
