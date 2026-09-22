//! Unified macOS title bar: content extends under the transparent title bar, the traffic
//! lights float over the sidebar, and empty space along the top edge drags the window.
//! Other platforms keep native decorations, so every inset here is zero there.

use eframe::egui::{self, Id, Rect, Sense, Ui, ViewportBuilder, ViewportCommand};

/// Height of the band the (transparent) title bar and traffic lights occupy.
pub const TITLEBAR_H: f32 = if cfg!(target_os = "macos") { 34.0 } else { 0.0 };

/// Horizontal room the traffic lights need when a column starts at the window's left edge.
pub const TRAFFIC_LIGHTS_W: f32 = if cfg!(target_os = "macos") { 76.0 } else { 0.0 };

/// Apply the platform window style to the root viewport.
pub fn configure_viewport(builder: ViewportBuilder) -> ViewportBuilder {
    if cfg!(target_os = "macos") {
        builder
            .with_fullsize_content_view(true)
            .with_titlebar_shown(false)
            .with_title_shown(false)
            .with_titlebar_buttons_shown(true)
    } else {
        builder
    }
}

/// Make empty space along the top edge behave like a title bar: drag moves the window and a
/// double-click zooms it. Call before laying out anything else so real widgets on top of this
/// strip keep their clicks (egui gives later widgets hit-test priority).
pub fn title_bar_drag_strip(ui: &Ui) {
    if TITLEBAR_H <= 0.0 {
        return;
    }
    let content = ui.ctx().content_rect();
    let rect = Rect::from_min_size(content.min, egui::vec2(content.width(), TITLEBAR_H));
    let response = ui.interact(rect, Id::new("oxi_title_bar_drag"), Sense::click_and_drag());
    if response.drag_started_by(egui::PointerButton::Primary) {
        ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
    }
    if response.double_clicked() {
        let maximized = ui.ctx().input(|i| i.viewport().maximized.unwrap_or(false));
        ui.ctx()
            .send_viewport_cmd(ViewportCommand::Maximized(!maximized));
    }
}
