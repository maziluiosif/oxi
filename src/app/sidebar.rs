//! Sidebar: workspace list, session rows, search, settings button.

use eframe::egui::{
    self, Align, Button, Color32, Frame, Layout, Margin, Order, RichText, Rounding,
    ScrollArea, Sense, Stroke, Ui,
};
use eframe::egui::scroll_area::ScrollBarVisibility;

use crate::theme::{
    format_stream_elapsed, sidebar_session_title_display, small_spinner, workspace_sidebar_label,
    C_ACCENT, C_BG_ELEVATED, C_BG_SIDEBAR, C_BORDER, C_BORDER_SUBTLE, C_ROW_ACTIVE, C_ROW_HOVER,
    C_SIDEBAR_SECTION, C_TEXT, C_TEXT_MUTED, FS_SMALL, FS_TINY, SIDEBAR_RESIZE_SEP_W,
    CHAT_VIEW_MARGIN_LEFT, CHAT_VIEW_MARGIN_RIGHT, CHAT_FRAME_TOP, CHAT_FRAME_BOTTOM,
    C_BG_MAIN,
};
use crate::ui::chrome::sidebar_text_field;

use super::OxiApp;

impl OxiApp {
    /// Sidebar list and controls.
    pub(crate) fn render_sidebar(&mut self, ui: &mut Ui) {
        let (status_text, status_color) = self.connection_status();
        ui.set_min_width(ui.max_rect().width());
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.label(RichText::new("CHATS").size(FS_TINY).color(C_TEXT_MUTED));
            ui.label(RichText::new("·").size(FS_TINY).color(C_SIDEBAR_SECTION));
            ui.label(RichText::new(status_text).size(FS_TINY).color(status_color));
            ui.add_space(ui.available_width());
            if ui
                .add(
                    Button::new(RichText::new("◀").size(FS_TINY).color(C_SIDEBAR_SECTION))
                        .frame(false)
                        .fill(Color32::TRANSPARENT),
                )
                .on_hover_text("Hide sidebar")
                .clicked()
            {
                self.conv.sidebar_open = false;
            }
        });
        ui.add_space(6.0);

        sidebar_text_field(ui, &mut self.conv.sidebar_search, "Search chats…");

        ui.add_space(6.0);
        self.render_sidebar_add_workspace(ui);
        ui.add_space(7.0);

        let scroll_h = (ui.available_height() - 34.0).max(48.0);
        ScrollArea::vertical()
            .id_salt("sidebar_main_scroll")
            .max_height(scroll_h)
            .auto_shrink([false, false])
            .scroll_bar_visibility(ScrollBarVisibility::VisibleWhenNeeded)
            .show(ui, |ui| {
                self.render_sidebar_session_list(ui);
            });

        ui.add_space(6.0);
        if ui
            .add_sized(
                [ui.available_width(), 28.0],
                Button::new(RichText::new("⚙ Settings").size(FS_SMALL).color(C_TEXT))
                    .fill(C_BG_ELEVATED)
                    .stroke(Stroke::new(1.0, C_BORDER_SUBTLE))
                    .rounding(6.0),
            )
            .on_hover_text("Open settings")
            .clicked()
        {
            self.conv.settings_open = true;
        }
        ui.expand_to_include_rect(ui.max_rect());
    }

    fn render_sidebar_add_workspace(&mut self, ui: &mut Ui) {
        const H: f32 = 26.0;
        const R: f32 = 6.0;
        let full_w = ui.available_width();
        let (rect, response) = ui.allocate_exact_size(egui::vec2(full_w, H), Sense::click());
        let hovered = response.hovered();
        let fill = if hovered { C_ROW_HOVER } else { C_BG_ELEVATED };
        let rounding = Rounding::same(R);
        ui.painter().rect_filled(rect, rounding, fill);
        ui.painter().rect_stroke(
            rect,
            rounding,
            Stroke::new(1.0, if hovered { C_BORDER } else { C_BORDER_SUBTLE }),
        );
        ui.allocate_new_ui(
            egui::UiBuilder::new().max_rect(rect.shrink2(egui::vec2(10.0, 4.0))),
            |ui| {
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.label(RichText::new("+  Add workspace").size(FS_SMALL).color(C_TEXT));
                });
            },
        );
        if hovered {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if response.clicked() {
            self.open_workspace_folder();
        }
        response.on_hover_text(
            "Add a project folder. Each workspace has its own chats; tools run with that folder as cwd.",
        );
    }

    fn render_sidebar_session_list(&mut self, ui: &mut Ui) {
        let q = self.conv.sidebar_search.trim().to_lowercase();
        let mut sidebar_changed = false;

        for wi in 0..self.conv.workspaces.len() {
            if sidebar_changed {
                return;
            }
            let root_label = workspace_sidebar_label(&self.conv.workspaces[wi].root_path);
            let active_si = self.conv.workspaces[wi].active;
            let n_sessions = self.conv.workspaces[wi].sessions.len();
            let folded = self.conv.workspaces[wi].sidebar_folded;
            ui.add_space(1.0);

            let chev = if folded { "▸" } else { "▾" };
            let row_w = ui.available_width();
            if ui
                .add_sized(
                    [row_w, 0.0],
                    Button::new(
                        RichText::new(format!("{chev}  {root_label}"))
                            .size(FS_TINY)
                            .color(C_SIDEBAR_SECTION),
                    )
                    .frame(false)
                    .fill(Color32::TRANSPARENT)
                    .min_size(egui::vec2(row_w, 17.0)),
                )
                .on_hover_text("Fold or unfold chats. Each folder is a workspace (agent cwd).")
                .clicked()
            {
                self.conv.workspaces[wi].sidebar_folded = !folded;
            }
            ui.add_space(1.0);
            if folded {
                continue;
            }

            for si in 0..n_sessions {
                if sidebar_changed {
                    return;
                }
                let Some(session) = self.conv.workspaces[wi].sessions.get(si) else {
                    return;
                };
                if !q.is_empty() && !session.title.to_lowercase().contains(&q) {
                    continue;
                }
                let row_title = sidebar_session_title_display(&session.title);
                ui.horizontal(|ui| {
                    ui.add_space(7.0);
                    ui.vertical(|ui| {
                        let row_w = ui.available_width();
                        ui.push_id((wi, si), |ui| {
                            let selected =
                                wi == self.conv.active_workspace && si == active_si;
                            let running = self.session_row_is_running(wi, si);
                            let title = row_title.clone();
                            const ROW_INNER_H: f32 = 17.0;
                            const ROW_VMARGIN: f32 = 1.0;
                            let row_outer_h = ROW_INNER_H + ROW_VMARGIN * 2.0;
                            let (rect, response) = ui.allocate_exact_size(
                                egui::vec2(row_w, row_outer_h),
                                Sense::click(),
                            );
                            let hovered = response.hovered();
                            let fill = if selected {
                                C_ROW_ACTIVE
                            } else if hovered {
                                C_ROW_HOVER
                            } else {
                                Color32::TRANSPARENT
                            };
                            ui.painter().rect_filled(rect, Rounding::same(4.0), fill);
                            if response.clicked() {
                                self.select_session_in_workspace(wi, si);
                            }
                            response.context_menu(|ui| {
                                if wi == self.conv.active_workspace
                                    && n_sessions > 1
                                    && ui.button("Delete chat").clicked()
                                {
                                    self.delete_session(si);
                                    sidebar_changed = true;
                                    ui.close_menu();
                                }
                            });
                            self.render_session_row_inner(
                                ui, rect, wi, si, running, selected, title,
                            );
                        });
                    });
                });
                ui.add_space(1.0);
                if sidebar_changed {
                    return;
                }
            }
        }
    }

    fn render_session_row_inner(
        &self,
        ui: &mut Ui,
        rect: egui::Rect,
        wi: usize,
        si: usize,
        running: bool,
        selected: bool,
        title: String,
    ) {
        const ROW_INNER_H: f32 = 17.0;
        const ROW_VMARGIN: f32 = 1.0;
        const BULLET_GAP: f32 = 4.0;
        const SPINNER_GAP: f32 = 4.0;

        let inner = rect.shrink2(egui::vec2(3.0, ROW_VMARGIN));
        ui.allocate_new_ui(
            egui::UiBuilder::new().max_rect(inner),
            |ui| {
                ui.set_min_width(inner.width());
                let lead_w = 14.0;
                let time_w = if running { 40.0 } else { 0.0 };
                let spin_reserve = if running { 14.0 } else { 0.0 };
                let sx = ui.spacing().item_spacing.x;
                let time_label = if running {
                    self.stream_started_at_for(wi, si)
                        .map(|t| format_stream_elapsed(t.elapsed()))
                } else {
                    None
                };
                let fixed = lead_w
                    + BULLET_GAP
                    + if running { SPINNER_GAP } else { 0.0 }
                    + time_w
                    + spin_reserve
                    + sx * if running { 4.0 } else { 2.0 };
                let title_w = (ui.available_width() - fixed).max(24.0);
                let bullet_col = if selected { C_ACCENT } else { C_TEXT_MUTED };

                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = sx;
                    ui.allocate_ui_with_layout(
                        egui::vec2(lead_w, ROW_INNER_H),
                        egui::Layout::left_to_right(Align::Center),
                        |ui| {
                            ui.label(
                                RichText::new("•").size(FS_SMALL).color(bullet_col),
                            );
                        },
                    );
                    ui.add_space(BULLET_GAP);
                    if running {
                        ui.allocate_ui_with_layout(
                            egui::vec2(spin_reserve, ROW_INNER_H),
                            egui::Layout::left_to_right(Align::Center),
                            |ui| {
                                small_spinner(ui);
                            },
                        );
                        ui.add_space(SPINNER_GAP);
                    }
                    ui.allocate_ui_with_layout(
                        egui::vec2(title_w, ROW_INNER_H),
                        egui::Layout::left_to_right(Align::Center),
                        |ui| {
                            use eframe::egui::Label;
                            ui.add(
                                Label::new(
                                    RichText::new(title.as_str())
                                        .size(FS_SMALL)
                                        .color(C_TEXT),
                                )
                                .truncate()
                                .halign(Align::LEFT),
                            );
                        },
                    );
                    if let Some(ref s) = time_label {
                        ui.allocate_ui_with_layout(
                            egui::vec2(time_w, ROW_INNER_H),
                            egui::Layout::right_to_left(Align::Center),
                            |ui| {
                                ui.label(
                                    RichText::new(s)
                                        .size(FS_TINY)
                                        .color(C_TEXT_MUTED)
                                        .monospace(),
                                );
                            },
                        );
                    }
                });
            },
        );
    }

    /// Top-right floating "New chat" button over the chat column.
    pub(crate) fn render_floating_new_chat_button(&mut self, ui: &Ui, chat_panel: egui::Rect) {
        const M: f32 = 8.0;
        const BW: f32 = 98.0;
        const BH: f32 = 27.0;
        let pos = chat_panel.right_top() + egui::vec2(-M - BW, M);
        egui::Area::new(ui.id().with("floating_new_chat"))
            .order(Order::Foreground)
            .fixed_pos(pos)
            .show(ui.ctx(), |ui| {
                if ui
                    .add_sized(
                        [BW, BH],
                        Button::new(RichText::new("New chat").size(FS_SMALL).color(C_TEXT))
                            .fill(C_BG_ELEVATED)
                            .stroke(Stroke::new(1.0, C_BORDER_SUBTLE))
                            .rounding(8.0),
                    )
                    .on_hover_text("New chat tab in the active workspace.")
                    .clicked()
                {
                    self.new_chat();
                }
            });
    }

    /// Central region manual split: sidebar | chat.
    pub(crate) fn render_main_area(&mut self, ui: &mut Ui) {
        const SIDEBAR_W_MIN: f32 = 120.0;
        const SIDEBAR_W_MAX: f32 = 520.0;
        let full_h = ui.available_height();

        ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
            ui.set_min_height(full_h);
            ui.spacing_mut().item_spacing.x = 0.0;

            if self.conv.sidebar_open {
                let w = self.conv.sidebar_width.clamp(SIDEBAR_W_MIN, SIDEBAR_W_MAX);
                ui.allocate_ui_with_layout(
                    egui::vec2(w, full_h),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        Frame::none()
                            .fill(C_BG_SIDEBAR)
                            .inner_margin(Margin {
                                left: 8.0,
                                right: 6.0,
                                top: 6.0,
                                bottom: 8.0,
                            })
                            .show(ui, |ui| {
                                ui.set_min_width(ui.max_rect().width());
                                ui.set_min_height(ui.max_rect().height());
                                self.render_sidebar(ui);
                            });
                        ui.expand_to_include_rect(ui.max_rect());
                    },
                );
                self.render_sidebar_resize_sep(ui, full_h, SIDEBAR_W_MIN, SIDEBAR_W_MAX);
            }

            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), full_h),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    Frame::none()
                        .fill(C_BG_MAIN)
                        .inner_margin(Margin {
                            left: CHAT_VIEW_MARGIN_LEFT,
                            right: CHAT_VIEW_MARGIN_RIGHT,
                            top: CHAT_FRAME_TOP,
                            bottom: CHAT_FRAME_BOTTOM,
                        })
                        .show(ui, |ui| {
                            let chat_panel_rect = ui.max_rect();
                            let style = (*ui.ctx().style()).clone();
                            let column_center_w =
                                crate::theme::chat_column_center_width(ui.available_width(), &style);

                            // Top-down split: reserve composer height from the last frame so the
                            // transcript never overlaps or pushes the input below the clip rect.
                            const COMPOSER_GAP: f32 = 8.0;
                            let composer_reserve =
                                (self.conv.composer_measured_full_h + COMPOSER_GAP).max(88.0);
                            let conversation_h =
                                (ui.available_height() - composer_reserve).max(48.0);
                            ui.allocate_ui_with_layout(
                                egui::vec2(ui.available_width(), conversation_h),
                                egui::Layout::top_down(egui::Align::Min),
                                |ui| {
                                    self.render_conversation(ui, column_center_w, conversation_h);
                                },
                            );
                            ui.add_space(COMPOSER_GAP);
                            self.render_composer(ui, column_center_w);

                            self.render_floating_new_chat_button(ui, chat_panel_rect);
                        });
                    ui.expand_to_include_rect(ui.max_rect());
                },
            );
        });
    }

    fn render_sidebar_resize_sep(
        &mut self,
        ui: &mut Ui,
        full_h: f32,
        min_w: f32,
        max_w: f32,
    ) {
        let boundary_x = ui.cursor().min.x;
        let sep_rect = egui::Rect::from_min_max(
            egui::pos2(boundary_x - SIDEBAR_RESIZE_SEP_W * 0.5, ui.min_rect().top()),
            egui::pos2(
                boundary_x + SIDEBAR_RESIZE_SEP_W * 0.5,
                ui.min_rect().top() + full_h,
            ),
        );
        let sep = ui.interact(sep_rect, ui.id().with("sidebar_sep"), Sense::drag());
        if sep.dragged() {
            self.conv.sidebar_width =
                (self.conv.sidebar_width + sep.drag_delta().x).clamp(min_w, max_w);
        }
        if sep.hovered() || sep.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
        ui.painter().vline(
            boundary_x,
            sep_rect.y_range(),
            Stroke::new(1.0, crate::theme::C_BORDER_SUBTLE),
        );
    }
}
