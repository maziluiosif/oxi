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

/// A panel-shaped toggle beside the traffic lights, independent of the sidebar view.
pub fn sidebar_toggle(ui: &mut Ui, open: bool) -> bool {
    if TITLEBAR_H <= 0.0 {
        return false;
    }
    let origin = ui.ctx().content_rect().min;
    let rect = Rect::from_min_size(
        origin + egui::vec2(TRAFFIC_LIGHTS_W + 4.0, 3.0),
        egui::vec2(28.0, 28.0),
    );
    let label = if open { "Hide sidebar" } else { "Show sidebar" };
    panel_toggle(
        ui,
        rect,
        "titlebar_sidebar_toggle",
        label,
        open,
        PanelSide::Left,
    )
}

/// Mirror of [`sidebar_toggle`] in the window's top-right corner, for the git panel.
pub fn git_panel_toggle(ui: &mut Ui, open: bool) -> bool {
    if TITLEBAR_H <= 0.0 {
        return false;
    }
    let content = ui.ctx().content_rect();
    let rect = Rect::from_min_size(
        egui::pos2(content.right() - 4.0 - 28.0, content.top() + 3.0),
        egui::vec2(28.0, 28.0),
    );
    let label = if open {
        "Hide git panel"
    } else {
        "Show git panel"
    };
    panel_toggle(
        ui,
        rect,
        "titlebar_git_panel_toggle",
        label,
        open,
        PanelSide::Right,
    )
}

#[derive(Clone, Copy, PartialEq)]
enum PanelSide {
    Left,
    Right,
}

/// Window-layout glyph: a rounded frame split by a divider, the `side` column filled while open.
fn panel_toggle(
    ui: &mut Ui,
    rect: Rect,
    id: &str,
    label: &str,
    open: bool,
    side: PanelSide,
) -> bool {
    let response = ui.interact(rect, Id::new(id), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
    });
    if response.hovered() || response.has_focus() {
        ui.painter()
            .rect_filled(rect, 4, crate::theme::c_row_hover());
    }
    let icon = rect.shrink2(egui::vec2(5.0, 7.0));
    let color = if open {
        crate::theme::c_text()
    } else {
        crate::theme::c_text_muted()
    };
    ui.painter().rect_stroke(
        icon,
        2,
        egui::Stroke::new(1.2, color),
        egui::StrokeKind::Inside,
    );
    let divider = match side {
        PanelSide::Left => icon.left() + 6.0,
        PanelSide::Right => icon.right() - 6.0,
    };
    if open {
        let column = match side {
            PanelSide::Left => Rect::from_min_max(
                icon.min + egui::vec2(1.5, 1.5),
                egui::pos2(divider, icon.bottom() - 1.5),
            ),
            PanelSide::Right => Rect::from_min_max(
                egui::pos2(divider, icon.top() + 1.5),
                icon.max - egui::vec2(1.5, 1.5),
            ),
        };
        ui.painter()
            .rect_filled(column, 0, crate::theme::c_text_faint());
    }
    ui.painter().line_segment(
        [
            egui::pos2(divider, icon.top()),
            egui::pos2(divider, icon.bottom()),
        ],
        egui::Stroke::new(1.0, color),
    );
    response.on_hover_text(label).clicked()
}

/// Space for native controls and the sidebar toggle when editor tabs reach the left edge.
pub const WINDOW_CONTROLS_W: f32 = if cfg!(target_os = "macos") {
    112.0
} else {
    0.0
};

/// Room the top-right git panel toggle needs when a strip reaches the window's right edge.
pub const RIGHT_CONTROLS_W: f32 = if cfg!(target_os = "macos") { 30.0 } else { 0.0 };
