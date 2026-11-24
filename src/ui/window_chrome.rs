//! Top window chrome: unified transparent title bar on macOS (traffic lights + drag strip),
//! and consistent top-corner sidebar / git panel toggles on every platform.

use eframe::egui::{self, Id, Rect, Sense, Ui, ViewportBuilder, ViewportCommand};

/// Height of the top chrome band (transparent title bar + traffic lights on macOS,
/// and the matching top-corner toggle band on Windows and Linux).
pub const TITLEBAR_H: f32 = 38.0;

/// Height of the chat header strip (matches [`TITLEBAR_H`] so top-corner toggles,
/// breadcrumb and quick actions share one crisp horizontal band).
pub const CHAT_HEADER_H: f32 = TITLEBAR_H;

/// Horizontal room the macOS traffic lights occupy at the window's left edge.
const MACOS_TRAFFIC_LIGHTS_W: f32 = 76.0;

const TOGGLE_SIZE: f32 = 28.0;
const RIGHT_TOGGLE_MARGIN: f32 = 8.0;

const fn sidebar_toggle_x(is_macos: bool) -> f32 {
    if is_macos {
        MACOS_TRAFFIC_LIGHTS_W + 4.0
    } else {
        8.0
    }
}

const fn window_controls_width(is_macos: bool) -> f32 {
    sidebar_toggle_x(is_macos) + TOGGLE_SIZE + 6.0
}

const SIDEBAR_TOGGLE_X: f32 = sidebar_toggle_x(cfg!(target_os = "macos"));

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

/// Make empty space along the top edge behave like a title bar on macOS: drag moves the window
/// and a double-click zooms it. Call before laying out anything else so real widgets on top of
/// this strip keep their clicks (egui gives later widgets hit-test priority).
/// Other platforms keep native window decorations, so this is a no-op there.
pub fn title_bar_drag_strip(ui: &Ui) {
    if !cfg!(target_os = "macos") {
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

/// A panel-shaped toggle in the window's top-left corner (beside the traffic lights on macOS),
/// independent of the sidebar view.
pub fn sidebar_toggle(ui: &mut Ui, open: bool) -> bool {
    let origin = ui.ctx().content_rect().min;
    let rect = Rect::from_min_size(
        origin + egui::vec2(SIDEBAR_TOGGLE_X, (TITLEBAR_H - TOGGLE_SIZE) * 0.5),
        egui::vec2(TOGGLE_SIZE, TOGGLE_SIZE),
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
    let content = ui.ctx().content_rect();
    let rect = Rect::from_min_size(
        egui::pos2(
            content.right() - RIGHT_TOGGLE_MARGIN - TOGGLE_SIZE,
            content.top() + (TITLEBAR_H - TOGGLE_SIZE) * 0.5,
        ),
        egui::vec2(TOGGLE_SIZE, TOGGLE_SIZE),
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
    let hovered = response.hovered() || response.has_focus();
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let pill_radius = egui::CornerRadius::same(14);
    if open {
        ui.painter()
            .rect_filled(rect, pill_radius, crate::theme::c_bg_elevated_2());
        ui.painter().rect_stroke(
            rect,
            pill_radius,
            egui::Stroke::new(
                1.0,
                if hovered {
                    crate::theme::c_text_faint()
                } else {
                    crate::theme::c_border()
                },
            ),
            egui::StrokeKind::Inside,
        );
    } else if hovered {
        ui.painter()
            .rect_filled(rect, pill_radius, crate::theme::c_row_hover());
    }
    let icon = rect.shrink2(egui::vec2(6.5, 7.5));
    let color = if open || hovered {
        crate::theme::c_text_strong()
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
        PanelSide::Left => icon.left() + 5.5,
        PanelSide::Right => icon.right() - 5.5,
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

/// Space for native controls (macOS traffic lights) and the sidebar toggle when a top strip
/// reaches the window's left edge.
pub const WINDOW_CONTROLS_W: f32 = window_controls_width(cfg!(target_os = "macos"));

/// Room the top-right git panel toggle needs when a strip reaches the window's right edge.
pub const RIGHT_CONTROLS_W: f32 = RIGHT_TOGGLE_MARGIN + TOGGLE_SIZE + 2.0;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_insets_clear_top_bar_buttons_on_all_platforms() {
        const {
            assert!(TOGGLE_SIZE < TITLEBAR_H);
            assert!(RIGHT_CONTROLS_W > RIGHT_TOGGLE_MARGIN + TOGGLE_SIZE);
        }
        assert_eq!(TITLEBAR_H, CHAT_HEADER_H);

        // macOS leaves room for the traffic lights plus the sidebar toggle.
        assert_eq!(sidebar_toggle_x(true), 80.0);
        assert_eq!(window_controls_width(true), 114.0);
        assert!(window_controls_width(true) > sidebar_toggle_x(true) + TOGGLE_SIZE);

        // Windows and Linux have no traffic lights in content_rect, only the sidebar toggle.
        assert_eq!(sidebar_toggle_x(false), 8.0);
        assert_eq!(window_controls_width(false), 42.0);
        assert!(window_controls_width(false) > sidebar_toggle_x(false) + TOGGLE_SIZE);

        // Right-side git panel toggle inset matches across every platform.
        assert_eq!(RIGHT_CONTROLS_W, 38.0);
    }
}
