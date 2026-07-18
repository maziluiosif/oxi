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

        // Search row + add-workspace button.
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.set_height(24.0);

            let add_w = 22.0;
            let clear_w = if self.conv.sidebar_search.is_empty() {
                0.0
            } else {
                22.0
            };
            let search_w = (ui.available_width()
                - add_w
                - clear_w
                - ui.spacing().item_spacing.x * if clear_w > 0.0 { 2.0 } else { 1.0 })
            .max(48.0);
            ui.allocate_ui_with_layout(
                egui::vec2(search_w, 24.0),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.set_width(search_w);
                    sidebar_text_field(ui, &mut self.conv.sidebar_search, "Search chats…");
                },
            );

            if clear_w > 0.0
                && crate::ui::chrome::icon_button_plain(ui, ICON_CLOSE, clear_w, false)
                    .on_hover_text("Clear chat search")
                    .clicked()
            {
                self.conv.sidebar_search.clear();
            }

            if crate::ui::chrome::icon_button_plain(ui, ICON_FOLDER_PLUS, add_w, false)
                .on_hover_text(
                    "Add a project folder. Each workspace has its own chats; \
                     tools run with that folder as cwd.",
                )
                .clicked()
            {
                self.open_workspace_folder();
            }
        });

        ui.add_space(8.0);

        const FOOTER_H: f32 = 36.0;
        let scroll_h = (ui.available_height() - FOOTER_H).max(48.0);
        ScrollArea::vertical()
            .id_salt("sidebar_main_scroll")
            .max_height(scroll_h)
            .auto_shrink([false, false])
            .scroll_bar_visibility(ScrollBarVisibility::VisibleWhenNeeded)
            .show(ui, |ui| {
                self.render_sidebar_session_list(ui);
            });

        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            ui.add_space(4.0);
            if crate::ui::chrome::flat_button_icon(
                ui,
                ICON_SETTINGS,
                "Settings",
                FS_SMALL,
                egui::vec2(ui.available_width(), 28.0),
                c_text_muted(),
            )
            .on_hover_text("Open settings")
            .clicked()
            {
                self.open_settings_page();
            }
        });

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
                    .rect_filled(rect, CornerRadius::same(RADIUS_ROW), c_row_hover());
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
                    .on_hover_text("New chat in this workspace (Cmd/Ctrl+N)");
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
                let ws_running = (0..n_sessions).any(|si| self.session_row_is_running(wi, si));
                response.context_menu(|ui| {
                    let resp = ui.add_enabled(!ws_running, egui::Button::new("Remove workspace"));
                    if ws_running {
                        resp.on_disabled_hover_text("A chat in this workspace is still running");
                    } else if resp.clicked() {
                        self.request_confirm(super::state::ConfirmAction::DeleteWorkspace { wi });
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
                if !q.is_empty() {
                    let title_hit = session.title.to_lowercase().contains(&q);
                    let msg_hit = session.messages.iter().any(|m| {
                        m.text.to_lowercase().contains(&q)
                            || m.blocks.iter().any(|b| match b {
                                crate::model::AssistantBlock::Answer(t)
                                | crate::model::AssistantBlock::Thinking(t) => {
                                    t.to_lowercase().contains(&q)
                                }
                                crate::model::AssistantBlock::Tool { output, .. } => {
                                    output.to_lowercase().contains(&q)
                                }
                            })
                    });
                    if !title_hit && !msg_hit {
                        continue;
                    }
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
                            let has_error = !running && self.session_row_has_error(wi, si);
                            let title = row_title.clone();
                            const ROW_INNER_H: f32 = 22.0;
                            const ROW_VMARGIN: f32 = 4.0;
                            let row_outer_h = ROW_INNER_H + ROW_VMARGIN * 2.0;
                            let (rect, response) = ui.allocate_exact_size(
                                egui::vec2(row_w, row_outer_h),
                                Sense::click(),
                            );
                            // Keep the row hovered while the pointer is over an
                            // overlapping action such as the hover-only trash button.
                            let hovered = ui.rect_contains_pointer(rect);
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
                            ui.painter()
                                .rect_filled(rect, CornerRadius::same(RADIUS_ROW), fill);
                            if self.conv.renaming_session == Some((wi, si)) {
                                let edit = egui::TextEdit::singleline(&mut self.conv.rename_draft)
                                    .font(egui::TextStyle::Small)
                                    .desired_width(rect.width() - 8.0);
                                let resp = ui.put(rect.shrink2(egui::vec2(4.0, 2.0)), edit);
                                resp.request_focus();
                                let enter = resp.lost_focus()
                                    && ui.input(|i| i.key_pressed(egui::Key::Enter));
                                let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
                                if enter {
                                    let draft = self.conv.rename_draft.clone();
                                    self.rename_session(si, draft);
                                    self.conv.renaming_session = None;
                                    sidebar_changed = true;
                                } else if escape || (resp.lost_focus() && !enter) {
                                    self.conv.renaming_session = None;
                                }
                            } else {
                                if response.clicked() {
                                    self.select_session_in_workspace(wi, si);
                                }
                                response.context_menu(|ui| {
                                    if wi == self.conv.active_workspace && !running {
                                        if ui.button("Rename chat").clicked() {
                                            self.conv.renaming_session = Some((wi, si));
                                            self.conv.rename_draft =
                                                self.conv.workspaces[wi].sessions[si].title.clone();
                                        }
                                        if ui.button("Export as Markdown…").clicked() {
                                            self.select_session_in_workspace(wi, si);
                                            self.export_active_session_markdown();
                                        }
                                        if ui.button("Delete chat").clicked() {
                                            self.request_confirm(
                                                super::state::ConfirmAction::DeleteSession {
                                                    wi,
                                                    si,
                                                },
                                            );
                                        }
                                    }
                                });
                                // Hover-only delete button, mirroring the context-menu action.
                                // `rect_contains_pointer`, not `response.hovered()`: the
                                // trash button interacted below overlaps this rect and
                                // would otherwise steal hover from the row response,
                                // flickering show/hide every other frame.
                                let can_delete = wi == self.conv.active_workspace && !running;
                                let show_trash = can_delete && ui.rect_contains_pointer(rect);
                                self.render_session_row_inner(
                                    ui, rect, wi, si, running, has_error, selected, title,
                                    show_trash,
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
                                        CornerRadius::same(RADIUS_ROW),
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
                                        .interact(
                                            trash_rect,
                                            ui.id().with("row_trash"),
                                            Sense::click(),
                                        )
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
                                        self.request_confirm(
                                            super::state::ConfirmAction::DeleteSession { wi, si },
                                        );
                                    }
                                }
                            } // end else not renaming
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
                    if !q.is_empty()
                        && crate::ui::chrome::ghost_button(ui, "Clear", false).clicked()
                    {
                        self.conv.sidebar_search.clear();
                    }
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
        has_error: bool,
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
            let bullet_col = if has_error {
                c_danger()
            } else if selected {
                c_accent()
            } else {
                c_text_muted()
            };

            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = sx;
                if !running {
                    ui.allocate_ui_with_layout(
                        egui::vec2(lead_w, ROW_INNER_H),
                        egui::Layout::left_to_right(Align::Center),
                        |ui| {
                            if selected || has_error {
                                let resp =
                                    ui.label(RichText::new("•").size(FS_SMALL).color(bullet_col));
                                if has_error {
                                    resp.on_hover_text(
                                        "This chat has an error — open it to see details",
                                    );
                                }
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
                        let resp = ui.add(
                            Label::new(
                                RichText::new(title.as_str())
                                    .size(FS_SMALL)
                                    .color(title_color),
                            )
                            .truncate()
                            .halign(Align::LEFT),
                        );
                        let full_w = ui.fonts_mut(|f| {
                            f.layout_no_wrap(
                                title.clone(),
                                FontId::proportional(FS_SMALL),
                                title_color,
                            )
                            .rect
                            .width()
                        });
                        if full_w > title_w {
                            resp.on_hover_text(title.as_str());
                        }
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
                                if self.conv.sidebar_mode == super::state::SidebarMode::Explorer {
                                    self.render_file_explorer(ui);
                                } else {
                                    self.render_sidebar(ui);
                                }
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
                    let editor_open = self.conv.editor.active_document().is_some();
                    Frame::new()
                        .fill(c_bg_main())
                        .inner_margin(Margin {
                            // The editor gutter starts directly at the sidebar boundary; the chat
                            // view keeps its usual breathing room.
                            left: if editor_open {
                                0
                            } else {
                                CHAT_VIEW_MARGIN_LEFT as i8
                            },
                            right: CHAT_VIEW_MARGIN_RIGHT as i8,
                            top: CHAT_FRAME_TOP as i8,
                            bottom: CHAT_FRAME_BOTTOM as i8,
                        })
                        .show(ui, |ui| {
                            if editor_open {
                                self.render_text_editor(ui);
                                return;
                            }

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

                            // Floating composer always stays available — even over a diff —
                            // so you can discuss the change without leaving the view.
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
                                    if show_diff {
                                        if let Some((title, diff_text)) = self.conv.git.diff.clone()
                                        {
                                            self.render_diff_view(
                                                ui,
                                                &title,
                                                &diff_text,
                                                column_center_w,
                                            );
                                        }
                                    } else {
                                        self.render_conversation(
                                            ui,
                                            column_center_w,
                                            conversation_h,
                                            composer_overlay_h,
                                        );
                                    }
                                },
                            );

                            // Soft scrim so transcript text doesn't compete with the input.
                            // If TextEdit changes height while rendering, `render_composer` asks
                            // egui for a second layout pass in the same frame. That keeps this
                            // previous measurement safe for hard newlines, wrapping, deletion,
                            // sending, attachments, and notices without predicting individual keys.
                            let composer_h = self.conv.composer_measured_full_h.max(80.0);
                            let scrim_h = (composer_h + 28.0).min(conversation_h * 0.45);
                            let scrim_top = chat_rect.bottom() - scrim_h;
                            let scrim_rect = egui::Rect::from_min_max(
                                egui::pos2(chat_rect.left(), scrim_top),
                                egui::pos2(chat_rect.right(), chat_rect.bottom()),
                            );
                            paint_composer_scrim(ui, scrim_rect);

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
        if sep.dragged()
            && let Some(pos) = ui.input(|i| i.pointer.interact_pos())
        {
            // Position-based like the sidebar sep (see there for why deltas jitter).
            // The panel's right edge is pinned to the window, so width = right - pointer.
            self.conv.git_width = (ui.max_rect().right() - pos.x).clamp(
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
        let col = if sep.dragged() {
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
        if sep.dragged()
            && let Some(pos) = ui.input(|i| i.pointer.interact_pos())
        {
            // Track the pointer's absolute position, not per-frame deltas: deltas keep
            // applying while the width is clamped, so over-dragging past the minimum
            // desyncs the edge from the pointer and the sidebar jitters on any
            // back-and-forth pointer movement.
            self.conv.sidebar_width = (pos.x - ui.min_rect().left()).clamp(min_w, max_w);
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
        let col = if sep.dragged() {
            c_accent()
        } else {
            crate::theme::c_border_subtle()
        };
        ui.painter()
            .vline(boundary_x, sep_rect.y_range(), Stroke::new(1.0, col));
    }
}

/// Soft vertical fade behind the floating composer so transcript text doesn't compete
/// with the input card.
fn paint_composer_scrim(ui: &mut Ui, rect: egui::Rect) {
    if rect.height() < 4.0 {
        return;
    }
    let base = c_bg_main();
    let steps = 12usize;
    let step_h = rect.height() / steps as f32;
    for i in 0..steps {
        let t = (i as f32 + 0.5) / steps as f32;
        // Ease-in: transparent at the top, opaque near the composer.
        let alpha = (t * t * 220.0) as u8;
        let y0 = rect.top() + i as f32 * step_h;
        let band = egui::Rect::from_min_max(
            egui::pos2(rect.left(), y0),
            egui::pos2(rect.right(), y0 + step_h + 0.5),
        );
        ui.painter().rect_filled(
            band,
            0.0,
            Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), alpha),
        );
    }
}
