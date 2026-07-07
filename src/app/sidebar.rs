//! Sidebar: workspace list, session rows, search, settings button.

use eframe::egui::scroll_area::ScrollBarVisibility;
use eframe::egui::{
    self, Align, Color32, CornerRadius, FontFamily, FontId, Frame, Layout, Margin, RichText,
    ScrollArea, Sense, Stroke, Ui,
};

use crate::theme::*;
use crate::ui::chrome::sidebar_text_field;

use super::OxiApp;

impl OxiApp {
    /// Sidebar list and controls.
    pub(crate) fn render_sidebar(&mut self, ui: &mut Ui) {
        ui.set_min_width(ui.max_rect().width());

        // Top row: app title + collapse button
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.label(
                RichText::new("oxi")
                    .size(FS_H3)
                    .color(crate::theme::c_accent())
                    .strong(),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if crate::ui::chrome::icon_button_plain(ui, ICON_CHEVRON_LEFT, 22.0, false)
                    .on_hover_text("Hide sidebar")
                    .clicked()
                {
                    self.conv.sidebar_open = false;
                }
                if crate::ui::chrome::icon_button_plain(ui, ICON_FOLDER_PLUS, 22.0, false)
                    .on_hover_text(
                        "Add a project folder. Each workspace has its own chats; \
                         tools run with that folder as cwd.",
                    )
                    .clicked()
                {
                    self.open_workspace_folder();
                }
            });
        });

        ui.add_space(8.0);

        sidebar_text_field(ui, &mut self.conv.sidebar_search, "Search chats…");

        ui.add_space(8.0);

        let scroll_h = (ui.available_height() - 38.0).max(48.0);
        ScrollArea::vertical()
            .id_salt("sidebar_main_scroll")
            .max_height(scroll_h)
            .auto_shrink([false, false])
            .scroll_bar_visibility(ScrollBarVisibility::VisibleWhenNeeded)
            .show(ui, |ui| {
                self.render_sidebar_session_list(ui);
            });

        ui.add_space(8.0);
        // Settings footer row: same rounded pill styling
        let settings_resp = crate::ui::chrome::row_button_icon(
            ui,
            ICON_SETTINGS,
            "Settings",
            egui::vec2(ui.available_width(), 30.0),
        );
        // Quiet accent dot on the row while a newer release is available.
        let settings_resp = if self.update_available().is_some() {
            let dot = egui::pos2(
                settings_resp.rect.right() - 10.0,
                settings_resp.rect.center().y,
            );
            ui.painter().circle_filled(dot, 3.0, c_accent());
            settings_resp.on_hover_text("Open settings — update available")
        } else {
            settings_resp.on_hover_text("Open settings")
        };
        if settings_resp.clicked() {
            self.conv.settings_open = true;
        }
        ui.expand_to_include_rect(ui.max_rect());
    }

    fn render_sidebar_session_list(&mut self, ui: &mut Ui) {
        // Workspace headers sit a step above the chat rows' default size, matching
        // the app-wide type scale (theme.rs) so it still tracks the UI density zoom.
        const FS_WORKSPACE: f32 = FS_SMALL + 1.5;
        let q = self.conv.sidebar_search.trim().to_lowercase();
        let mut sidebar_changed = false;

        for wi in 0..self.conv.workspaces.len() {
            if sidebar_changed {
                return;
            }
            let active_si = self.conv.workspaces[wi].active;
            let n_sessions = self.conv.workspaces[wi].sessions.len();
            let root_label = workspace_sidebar_label(&self.conv.workspaces[wi].root_path);
            let folded = self.conv.workspaces[wi].sidebar_folded;
            ui.add_space(1.0);

            const ROW_H: f32 = 22.0;
            const PLUS_W: f32 = 22.0;
            const GLYPH_W: f32 = 18.0;
            let (rect, response) =
                ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H), Sense::click());
            // `rect_contains_pointer` instead of `response.hovered()`: the in-place "+"
            // below steals hover from the row response, which would flicker the fill.
            let row_hovered = ui.rect_contains_pointer(rect);
            if row_hovered {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                ui.painter()
                    .rect_filled(rect, CornerRadius::same(6), c_row_hover());
            }
            // Leading glyph: folder open/closed at rest, fold chevron on hover.
            let glyph = match (row_hovered, folded) {
                (true, true) => ICON_CHEVRON_RIGHT,
                (true, false) => ICON_ANGLE_DOWN,
                (false, true) => ICON_FOLDER,
                (false, false) => ICON_FOLDER_OPEN,
            };
            ui.painter().text(
                egui::pos2(rect.left() + 4.0 + GLYPH_W * 0.5, rect.center().y),
                egui::Align2::CENTER_CENTER,
                glyph,
                FontId::new(FS_TINY, icon_font()),
                c_sidebar_section(),
            );
            let label_rect = egui::Rect::from_min_max(
                egui::pos2(rect.left() + 4.0 + GLYPH_W + 4.0, rect.top()),
                egui::pos2(rect.right() - PLUS_W - 2.0, rect.bottom()),
            );
            ui.scope_builder(egui::UiBuilder::new().max_rect(label_rect), |ui| {
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.add(
                        egui::Label::new(
                            RichText::new(&root_label)
                                .size(FS_WORKSPACE)
                                .color(c_sidebar_section()),
                        )
                        .truncate()
                        .halign(Align::LEFT)
                        .selectable(false),
                    );
                });
            });
            // Hover-only "+" at the right edge, painted + interacted in place (never
            // allocated) so its appearance can't shift the layout.
            let mut plus_hovered = false;
            let mut plus_clicked = false;
            if row_hovered {
                let plus_rect = egui::Rect::from_min_max(
                    egui::pos2(rect.right() - PLUS_W - 2.0, rect.top()),
                    egui::pos2(rect.right() - 2.0, rect.bottom()),
                );
                let plus_resp = ui
                    .interact(plus_rect, ui.id().with(("ws_plus", wi)), Sense::click())
                    .on_hover_text("New chat in this workspace");
                plus_hovered = plus_resp.hovered();
                plus_clicked = plus_resp.clicked();
                ui.painter().text(
                    plus_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    ICON_PLUS,
                    FontId::new(FS_TINY, icon_font()),
                    if plus_hovered {
                        c_accent()
                    } else {
                        c_text_faint()
                    },
                );
            }
            if plus_clicked {
                if wi != self.conv.active_workspace {
                    self.select_workspace(wi);
                }
                self.new_chat();
                sidebar_changed = true;
            } else if response.clicked() && !plus_hovered {
                self.conv.workspaces[wi].sidebar_folded = !folded;
                self.sync_workspaces_to_settings();
            }
            // The cwd workspace (index 0) is always present, so it gets no delete option.
            if wi != 0 {
                response.context_menu(|ui| {
                    if ui.button("Delete workspace").clicked() {
                        self.delete_workspace(wi);
                        sidebar_changed = true;
                    }
                });
            }
            ui.add_space(1.0);
            if sidebar_changed {
                return;
            }
            if self.conv.workspaces[wi].sidebar_folded {
                continue;
            }

            let mut visible_sessions = 0usize;
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
                visible_sessions += 1;
                let row_title = sidebar_session_title_display(&session.title);
                ui.horizontal(|ui| {
                    ui.add_space(7.0);
                    ui.vertical(|ui| {
                        let row_w = ui.available_width();
                        ui.push_id((wi, si), |ui| {
                            let selected = wi == self.conv.active_workspace && si == active_si;
                            let running = self.session_row_is_running(wi, si);
                            let title = row_title.clone();
                            const ROW_INNER_H: f32 = 22.0;
                            const ROW_VMARGIN: f32 = 4.0;
                            let row_outer_h = ROW_INNER_H + ROW_VMARGIN * 2.0;
                            let (rect, response) = ui.allocate_exact_size(
                                egui::vec2(row_w, row_outer_h),
                                Sense::click(),
                            );
                            let hovered = response.hovered();
                            if hovered {
                                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                            }
                            let fill = if selected {
                                c_row_active()
                            } else if hovered {
                                c_row_hover()
                            } else {
                                Color32::TRANSPARENT
                            };
                            ui.painter().rect_filled(rect, CornerRadius::same(7), fill);
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
                                }
                            });
                            // Hover-only delete button, mirroring the context-menu action.
                            // `rect_contains_pointer`, not `response.hovered()`: the
                            // trash button interacted below overlaps this rect and
                            // would otherwise steal hover from the row response,
                            // flickering show/hide every other frame.
                            let can_delete =
                                wi == self.conv.active_workspace && n_sessions > 1 && !running;
                            let show_trash = can_delete && ui.rect_contains_pointer(rect);
                            self.render_session_row_inner(
                                ui, rect, wi, si, running, selected, title, show_trash,
                            );

                            if show_trash {
                                // Flush against the same right edge the time label
                                // sits at, so it swaps in instead of crowding it.
                                const TIME_W: f32 = 34.0;
                                let trash_rect = egui::Rect::from_min_max(
                                    egui::pos2(rect.right() - 3.0 - TIME_W, rect.top() + 2.0),
                                    egui::pos2(rect.right() - 3.0, rect.bottom() - 2.0),
                                );
                                // Backing fill keeps the icon legible over long titles.
                                ui.painter().rect_filled(
                                    trash_rect,
                                    CornerRadius::same(7),
                                    if selected {
                                        c_row_active()
                                    } else {
                                        c_row_hover()
                                    },
                                );
                                // Painted + interacted in place, never allocated: a
                                // hover-only widget that allocates nudges the layout
                                // every time it appears.
                                let trash_resp = ui
                                    .interact(trash_rect, ui.id().with("row_trash"), Sense::click())
                                    .on_hover_text("Delete chat");
                                ui.painter().text(
                                    trash_rect.center(),
                                    egui::Align2::CENTER_CENTER,
                                    ICON_TRASH,
                                    FontId::new(FS_TINY, icon_font()),
                                    if trash_resp.hovered() {
                                        c_accent()
                                    } else {
                                        c_text_faint()
                                    },
                                );
                                if trash_resp.hovered() {
                                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                                }
                                if trash_resp.clicked() {
                                    self.delete_session(si);
                                    sidebar_changed = true;
                                }
                            }
                        });
                    });
                });
                if sidebar_changed {
                    return;
                }
            }
            if visible_sessions == 0 {
                ui.horizontal(|ui| {
                    ui.add_space(10.0);
                    let msg = if q.is_empty() {
                        "No chats yet"
                    } else {
                        "No chats found"
                    };
                    ui.label(RichText::new(msg).size(FS_TINY).color(c_text_muted()));
                });
                ui.add_space(4.0);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_session_row_inner(
        &self,
        ui: &mut Ui,
        rect: egui::Rect,
        wi: usize,
        si: usize,
        running: bool,
        selected: bool,
        title: String,
        hide_time: bool,
    ) {
        const ROW_INNER_H: f32 = 22.0;
        const ROW_VMARGIN: f32 = 4.0;
        const BULLET_GAP: f32 = 4.0;
        const SPINNER_GAP: f32 = 4.0;

        let inner = rect.shrink2(egui::vec2(3.0, ROW_VMARGIN));
        ui.scope_builder(egui::UiBuilder::new().max_rect(inner), |ui| {
            ui.set_min_width(inner.width());
            let lead_w = if running { 0.0 } else { 14.0 };
            let time_w = if running { 40.0 } else { 34.0 };
            let spin_reserve = if running { 14.0 } else { 0.0 };
            let sx = ui.spacing().item_spacing.x;
            // Space is always reserved for the time label so the title never
            // reflows; when hidden the delete button is painted over that
            // same slot instead.
            let time_label = if hide_time {
                None
            } else if running {
                self.stream_started_at_for(wi, si)
                    .map(|t| format_stream_elapsed(t.elapsed()))
            } else {
                Some(format_relative_time(
                    self.conv.workspaces[wi].sessions[si].modified,
                ))
            };
            let fixed = lead_w
                + if running { 0.0 } else { BULLET_GAP }
                + if running { SPINNER_GAP } else { 0.0 }
                + time_w
                + spin_reserve
                + sx * if running { 4.0 } else { 3.0 };
            let title_w = (ui.available_width() - fixed).max(24.0);
            let bullet_col = if selected { c_accent() } else { c_text_muted() };

            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = sx;
                if !running {
                    ui.allocate_ui_with_layout(
                        egui::vec2(lead_w, ROW_INNER_H),
                        egui::Layout::left_to_right(Align::Center),
                        |ui| {
                            // Dot only on the active chat — a bullet on every row is noise.
                            if selected {
                                ui.label(RichText::new("•").size(FS_SMALL).color(bullet_col));
                            }
                        },
                    );
                    ui.add_space(BULLET_GAP);
                }
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
                        let title_color = if selected { c_text() } else { c_text_muted() };
                        ui.add(
                            Label::new(
                                RichText::new(title.as_str())
                                    .size(FS_SMALL)
                                    .color(title_color),
                            )
                            .truncate()
                            .halign(Align::LEFT),
                        );
                    },
                );
            });
            // Painted at an absolute rect (flush against `inner`'s right edge)
            // rather than placed in the sequential layout, so it lines up
            // pixel-for-pixel with the hover-only trash button that swaps
            // into this same spot.
            if let Some(ref s) = time_label {
                let time_rect = egui::Rect::from_min_max(
                    egui::pos2(inner.right() - time_w, inner.top()),
                    egui::pos2(inner.right(), inner.bottom()),
                );
                // Nudged left off the flush-right edge (~2 monospace chars)
                // so it doesn't sit exactly under the trash icon's center.
                const TEXT_NUDGE: f32 = 7.0;
                ui.painter().text(
                    time_rect.right_center() - egui::vec2(TEXT_NUDGE, 0.0),
                    egui::Align2::RIGHT_CENTER,
                    s,
                    FontId::new(FS_TINY, FontFamily::Monospace),
                    c_text_muted(),
                );
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
                        Frame::new()
                            .fill(c_bg_sidebar())
                            .inner_margin(Margin {
                                left: 12,
                                right: 10,
                                top: 12,
                                bottom: 12,
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

            let git_open = self.conv.git_open;
            let git_w = if git_open {
                self.conv.git_width.clamp(
                    crate::app::git_panel::GIT_W_MIN,
                    crate::app::git_panel::GIT_W_MAX,
                )
            } else {
                0.0
            };
            let chat_w = (ui.available_width() - git_w).max(60.0);
            ui.allocate_ui_with_layout(
                egui::vec2(chat_w, full_h),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    Frame::new()
                        .fill(c_bg_main())
                        .inner_margin(Margin {
                            left: CHAT_VIEW_MARGIN_LEFT as i8,
                            right: CHAT_VIEW_MARGIN_RIGHT as i8,
                            top: CHAT_FRAME_TOP as i8,
                            bottom: CHAT_FRAME_BOTTOM as i8,
                        })
                        .show(ui, |ui| {
                            let style = (*ui.style()).clone();
                            let column_center_w = crate::theme::chat_column_center_width(
                                ui.available_width(),
                                &style,
                            );

                            const HEADER_H: f32 = 38.0;
                            const HEADER_GAP: f32 = 6.0;
                            let show_diff =
                                self.conv.diff_view_open && self.conv.git.diff.is_some();
                            self.render_chat_header(ui, column_center_w);
                            ui.add_space(HEADER_GAP);

                            if show_diff {
                                // Diff viewer replaces the chat transcript + composer.
                                let diff_h =
                                    (ui.available_height() - HEADER_H - HEADER_GAP).max(48.0);
                                ui.allocate_ui_with_layout(
                                    egui::vec2(ui.available_width(), diff_h),
                                    egui::Layout::top_down(egui::Align::Min),
                                    |ui| {
                                        if let Some((title, diff_text)) = self.conv.git.diff.clone()
                                        {
                                            self.render_diff_view(
                                                ui,
                                                &title,
                                                &diff_text,
                                                column_center_w,
                                            );
                                        }
                                    },
                                );
                            } else {
                                // Floating composer: the transcript uses the full remaining height,
                                // while the input is painted as an overlay pinned to the bottom of the
                                // chat column. The transcript adds matching tail padding internally so
                                // bottom content can still be scrolled into view.
                                const COMPOSER_GAP: f32 = 8.0;
                                let composer_overlay_h =
                                    (self.conv.composer_measured_full_h + COMPOSER_GAP).max(88.0);
                                let conversation_h =
                                    (ui.available_height() - HEADER_H - HEADER_GAP).max(48.0);
                                let chat_rect = ui.max_rect();
                                ui.allocate_ui_with_layout(
                                    egui::vec2(ui.available_width(), conversation_h),
                                    egui::Layout::top_down(egui::Align::Min),
                                    |ui| {
                                        self.render_conversation(
                                            ui,
                                            column_center_w,
                                            conversation_h,
                                            composer_overlay_h,
                                        );
                                    },
                                );

                                let composer_h = self.conv.composer_measured_full_h.max(80.0);
                                let composer_top = chat_rect.bottom() - composer_h;
                                let composer_rect = egui::Rect::from_min_size(
                                    egui::pos2(chat_rect.left(), composer_top),
                                    egui::vec2(chat_rect.width(), composer_h),
                                );
                                ui.scope_builder(
                                    egui::UiBuilder::new().max_rect(composer_rect),
                                    |ui| {
                                        self.render_composer(ui, column_center_w);
                                    },
                                );
                            }
                        });
                    ui.expand_to_include_rect(ui.max_rect());
                },
            );

            // Right git panel
            if git_open {
                self.render_git_resize_sep(ui, full_h);
                ui.allocate_ui_with_layout(
                    egui::vec2(git_w, full_h),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_min_height(full_h);
                        Frame::new()
                            .fill(c_bg_sidebar())
                            .inner_margin(Margin {
                                left: 0,
                                right: 0,
                                top: 0,
                                bottom: 0,
                            })
                            .show(ui, |ui| {
                                self.render_git_panel(ui, full_h);
                            });
                        ui.expand_to_include_rect(ui.max_rect());
                    },
                );
            }
        });
    }

    fn render_git_resize_sep(&mut self, ui: &mut Ui, full_h: f32) {
        const SEP_W: f32 = 6.0;
        let boundary_x = ui.cursor().min.x;
        let sep_rect = egui::Rect::from_min_max(
            egui::pos2(boundary_x - SEP_W * 0.5, ui.min_rect().top()),
            egui::pos2(boundary_x + SEP_W * 0.5, ui.min_rect().top() + full_h),
        );
        let sep = ui.interact(sep_rect, ui.id().with("git_sep"), Sense::drag());
        if sep.hovered() || sep.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
        if sep.dragged() {
            // Dragging the left edge left (negative dx) grows the panel.
            let dx = ui.input(|i| i.pointer.delta().x);
            self.conv.git_width = (self.conv.git_width - dx).clamp(
                crate::app::git_panel::GIT_W_MIN,
                crate::app::git_panel::GIT_W_MAX,
            );
            self.conv.settings.git_width = self.conv.git_width;
        }
        if sep.drag_stopped()
            && let Err(e) = self.conv.settings.save()
        {
            self.run_state_mut(self.active_session_key()).stream_error =
                Some(format!("Save settings: {e}"));
        }
        let col = if sep.hovered() || sep.dragged() {
            c_accent()
        } else {
            c_border_subtle()
        };
        ui.painter().vline(
            sep_rect.center().x,
            sep_rect.y_range(),
            Stroke::new(1.0, col),
        );
    }

    fn render_sidebar_resize_sep(&mut self, ui: &mut Ui, full_h: f32, min_w: f32, max_w: f32) {
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
            let delta_x = ui.input(|i| i.pointer.delta().x);
            self.conv.sidebar_width = (self.conv.sidebar_width + delta_x).clamp(min_w, max_w);
            self.conv.settings.sidebar_width = self.conv.sidebar_width;
        }
        if sep.drag_stopped()
            && let Err(e) = self.conv.settings.save()
        {
            self.run_state_mut(self.active_session_key()).stream_error =
                Some(format!("Save settings: {e}"));
        }
        if sep.hovered() || sep.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
        let col = if sep.hovered() || sep.dragged() {
            c_accent()
        } else {
            crate::theme::c_border_subtle()
        };
        ui.painter()
            .vline(boundary_x, sep_rect.y_range(), Stroke::new(1.0, col));
    }
}
