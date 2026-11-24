//! Central region layout: sidebar | chat (or editor, diff, settings) | git panel, with the
//! draggable separators between them.

use eframe::egui::{self, Color32, Frame, Margin, Sense, Stroke, Ui};

use crate::theme::*;

use super::OxiApp;

impl OxiApp {
    /// Central region manual split: sidebar | chat.
    pub(super) fn render_main_area(&mut self, ui: &mut Ui) {
        const SIDEBAR_W_MIN: f32 = 120.0;
        const SIDEBAR_W_MAX: f32 = 520.0;
        let full_h = ui.available_height();

        ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
            ui.set_min_height(full_h);
            ui.spacing_mut().item_spacing.x = 0.0;

            if self.conv.sidebar.open {
                let w = self.conv.sidebar.width.clamp(SIDEBAR_W_MIN, SIDEBAR_W_MAX);
                // Like the chat/git split below, the sidebar owns an exact column. A long session
                // list can report a wider `min_rect` when its floating scrollbar wakes on hover;
                // letting that propagate through `allocate_ui_with_layout` steals a few pixels
                // from the chat and re-wraps borderline transcript lines.
                let sidebar_rect =
                    egui::Rect::from_min_size(ui.cursor().min, egui::vec2(w, full_h));
                let mut sidebar_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .id_salt("fixed_left_sidebar")
                        .max_rect(sidebar_rect)
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                sidebar_ui.shrink_clip_rect(sidebar_rect);
                Frame::new()
                    .fill(c_bg_sidebar())
                    .inner_margin(Margin {
                        left: 10,
                        right: 10,
                        top: 8 + crate::ui::window_chrome::TITLEBAR_H as i8,
                        bottom: 10,
                    })
                    .show(&mut sidebar_ui, |ui| {
                        ui.set_min_width(ui.max_rect().width());
                        ui.set_min_height(ui.max_rect().height());
                        match self.conv.sidebar.mode {
                            super::state::SidebarMode::Explorer => self.render_file_explorer(ui),
                            super::state::SidebarMode::Search => self.render_project_search(ui),
                            super::state::SidebarMode::Chats => self.render_sidebar(ui),
                        }
                    });
                drop(sidebar_ui);
                ui.advance_cursor_after_rect(sidebar_rect);
                self.render_sidebar_resize_sep(ui, full_h, SIDEBAR_W_MIN, SIDEBAR_W_MAX);
            }

            let git_open = self.conv.git_ui.open;
            let git_w = if git_open {
                self.conv.git_ui.width.clamp(
                    crate::app::git_panel::GIT_W_MIN,
                    crate::app::git_panel::GIT_W_MAX,
                )
            } else {
                0.0
            };
            let chat_w = (ui.available_width() - git_w).max(60.0);
            // `allocate_ui_with_layout` grows its parent allocation when a child reports a wider
            // `min_rect`. That is normally useful, but here it lets a narrow transcript (notably
            // while its scrollbar appears and text re-wraps) push the fixed-width git panel to
            // the right. Keep the split geometry authoritative: chat content may reflow inside
            // this rect, but it must never participate in sizing the sibling git column.
            let chat_rect = egui::Rect::from_min_size(ui.cursor().min, egui::vec2(chat_w, full_h));
            let mut chat_ui = ui.new_child(
                egui::UiBuilder::new()
                    .id_salt("fixed_chat_column")
                    .max_rect(chat_rect)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            chat_ui.shrink_clip_rect(chat_rect);
            {
                let ui = &mut chat_ui;
                // An active git-diff tab is also an "editor open" state, even with no file
                // document behind it: the diff should render in the editor pane (as its own tab)
                // rather than falling back to the in-chat diff view. This makes the editor the
                // default home for every diff, whether or not a file was already open.
                let editor_open = self.conv.editor.active_document().is_some()
                    || (self.conv.diff_view.open && self.conv.git.diff.is_some());
                let show_split_chat_editor =
                    editor_open && self.conv.editor.split_with_chat && chat_w >= 640.0;
                self.conv.editor.split_rendered = show_split_chat_editor;
                if show_split_chat_editor {
                    let ratio = if self.conv.editor.split_ratio > 0.0 {
                        self.conv.editor.split_ratio.clamp(0.28, 0.72)
                    } else {
                        0.46
                    };
                    let sep_w = SIDEBAR_RESIZE_SEP_W;
                    let left_w = ((chat_w - sep_w) * ratio).clamp(260.0, chat_w - sep_w - 280.0);
                    let right_w = (chat_w - sep_w - left_w).max(200.0);
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        let left_rect =
                            egui::Rect::from_min_size(ui.cursor().min, egui::vec2(left_w, full_h));
                        let mut left_ui = ui.new_child(
                            egui::UiBuilder::new()
                                .id_salt("split_chat_pane")
                                .max_rect(left_rect)
                                .layout(egui::Layout::top_down(egui::Align::Min)),
                        );
                        left_ui.shrink_clip_rect(left_rect);
                        Frame::new()
                            .fill(c_bg_main())
                            .inner_margin(Margin::same(0))
                            .show(&mut left_ui, |ui| {
                                self.render_chat_header(ui);
                                Frame::new()
                                    .fill(c_bg_main())
                                    .inner_margin(Margin {
                                        left: CHAT_VIEW_MARGIN_LEFT as i8,
                                        right: crate::theme::CHAT_VIEW_MARGIN_RIGHT_CHAT as i8,
                                        top: CHAT_FRAME_TOP as i8,
                                        bottom: CHAT_FRAME_BOTTOM as i8,
                                    })
                                    .show(ui, |ui| {
                                        self.render_chat_column(ui);
                                    });
                            });
                        drop(left_ui);
                        ui.advance_cursor_after_rect(left_rect);

                        // Draggable splitter between Chat and Editor.
                        let boundary_x = ui.cursor().min.x;
                        let sep_rect = egui::Rect::from_min_size(
                            egui::pos2(boundary_x, left_rect.top()),
                            egui::vec2(sep_w, full_h),
                        );
                        let sep =
                            ui.interact(sep_rect, ui.id().with("chat_editor_sep"), Sense::drag());
                        if sep.hovered() || sep.dragged() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                        }
                        if sep.dragged()
                            && let Some(pos) = ui.input(|i| i.pointer.interact_pos())
                        {
                            let new_left =
                                (pos.x - left_rect.left()).clamp(260.0, chat_w - sep_w - 280.0);
                            self.conv.editor.split_ratio =
                                (new_left / (chat_w - sep_w)).clamp(0.28, 0.72);
                        }
                        let sep_col = if sep.hovered() || sep.dragged() {
                            c_accent()
                        } else {
                            c_border_subtle()
                        };
                        ui.painter().vline(
                            sep_rect.center().x,
                            sep_rect.y_range(),
                            Stroke::new(1.0, sep_col),
                        );
                        ui.advance_cursor_after_rect(sep_rect);

                        let right_rect =
                            egui::Rect::from_min_size(ui.cursor().min, egui::vec2(right_w, full_h));
                        let mut right_ui = ui.new_child(
                            egui::UiBuilder::new()
                                .id_salt("split_editor_pane")
                                .max_rect(right_rect)
                                .layout(egui::Layout::top_down(egui::Align::Min)),
                        );
                        right_ui.shrink_clip_rect(right_rect);
                        Frame::new()
                            .fill(c_bg_main())
                            .inner_margin(Margin {
                                left: 0,
                                right: CHAT_VIEW_MARGIN_RIGHT as i8,
                                top: 0,
                                bottom: 0,
                            })
                            .show(&mut right_ui, |ui| {
                                self.render_text_editor(ui);
                            });
                        drop(right_ui);
                        ui.advance_cursor_after_rect(right_rect);
                    });
                } else if editor_open {
                    Frame::new()
                        .fill(c_bg_main())
                        .inner_margin(Margin {
                            left: 0,
                            right: CHAT_VIEW_MARGIN_RIGHT as i8,
                            top: 0,
                            bottom: 0,
                        })
                        .show(ui, |ui| {
                            self.render_text_editor(ui);
                        });
                } else {
                    Frame::new()
                        .fill(c_bg_main())
                        .inner_margin(Margin::same(0))
                        .show(ui, |ui| {
                            self.render_chat_header(ui);
                            Frame::new()
                                .fill(c_bg_main())
                                .inner_margin(Margin {
                                    left: CHAT_VIEW_MARGIN_LEFT as i8,
                                    right: crate::theme::CHAT_VIEW_MARGIN_RIGHT_CHAT as i8,
                                    top: CHAT_FRAME_TOP as i8,
                                    bottom: CHAT_FRAME_BOTTOM as i8,
                                })
                                .show(ui, |ui| {
                                    if !self.render_split_chat(ui) {
                                        self.render_chat_column(ui);
                                    }
                                });
                        });
                }
                ui.expand_to_include_rect(ui.max_rect());
            }
            drop(chat_ui);
            ui.advance_cursor_after_rect(chat_rect);

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

    /// Top breadcrumb bar over the chat column (`workspace / conversation title`) with quick
    /// toggles for real oxi workspace panels, aligned with the top title bar strip.
    fn render_chat_header(&mut self, ui: &mut Ui) {
        let h = crate::ui::window_chrome::CHAT_HEADER_H;
        let full_w = ui.available_width();
        let (rect, _) = ui.allocate_exact_size(egui::vec2(full_w, h), Sense::hover());
        ui.painter().rect_filled(rect, 0.0, c_bg_main());
        ui.painter().hline(
            rect.x_range(),
            rect.bottom() - 0.5,
            Stroke::new(1.0, c_border_subtle()),
        );

        let left_inset = if self.conv.sidebar.open {
            16.0
        } else {
            crate::ui::window_chrome::WINDOW_CONTROLS_W + 8.0
        };
        let right_inset = if self.conv.git_ui.open || self.conv.editor.split_rendered {
            10.0
        } else {
            crate::ui::window_chrome::RIGHT_CONTROLS_W + 4.0
        };
        let inner = egui::Rect::from_min_max(
            egui::pos2(rect.left() + left_inset, rect.top()),
            egui::pos2(
                (rect.right() - right_inset).max(rect.left() + left_inset),
                rect.bottom(),
            ),
        );

        let ws_root = self.active_workspace().root_path.clone();
        let ws_name = header_workspace_name(&ws_root);
        let session_title = sidebar_session_title_display(&self.active_session().title);
        let find_open = self.conv.transcript.find.is_some();
        let split_open = self.conv.split.is_some();
        let has_hidden_editor = !self.conv.editor.split_rendered
            && (self.conv.editor.hidden_active.is_some()
                || !self.conv.editor.documents.is_empty());

        let rename_id = egui::Id::new("chat_header_rename_state");
        let active_key = self.active_session_key();
        let mut renaming = ui
            .ctx()
            .data(|d| d.get_temp::<(super::state::SessionKey, String)>(rename_id))
            .filter(|(k, _)| *k == active_key);

        let mut toggle_find = false;
        let mut toggle_explorer = false;
        let mut open_palette = false;
        let mut toggle_split = false;
        let mut reopen_editor = false;
        let mut commit_rename: Option<String> = None;
        let mut cancel_rename = false;
        let mut start_rename = false;

        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(inner)
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
            |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                if crate::ui::chrome::icon_button_plain(ui, ICON_SEARCH, 26.0, find_open)
                    .on_hover_text(if find_open {
                        "Close find in chat (Esc)"
                    } else {
                        "Find in chat (Cmd/Ctrl+F)"
                    })
                    .clicked()
                {
                    toggle_find = true;
                }
                if !self.conv.editor.split_rendered
                    && header_split_button(ui, split_open)
                        .on_hover_text(if split_open {
                            "Close split chat"
                        } else {
                            "New chat side by side (compare two agents)"
                        })
                        .clicked()
                {
                    toggle_split = true;
                }
                if has_hidden_editor
                    && crate::ui::chrome::icon_button_plain(ui, ICON_FILE, 26.0, false)
                        .on_hover_text("Return to open editor tabs")
                        .clicked()
                {
                    reopen_editor = true;
                }
                if header_palette_button(ui)
                    .on_hover_text(if cfg!(target_os = "macos") {
                        "Command palette (⇧⌘P)"
                    } else {
                        "Command palette (Ctrl+Shift+P)"
                    })
                    .clicked()
                {
                    open_palette = true;
                }
                ui.add_space(6.0);
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let ws_resp = ui
                        .add(
                            egui::Label::new(
                                egui::RichText::new(&ws_name)
                                    .size(FS_SMALL)
                                    .color(c_text_muted()),
                            )
                            .sense(Sense::click())
                            .selectable(false),
                        )
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .on_hover_text("Open workspace explorer (Cmd/Ctrl+E)");
                    if ws_resp.clicked() {
                        toggle_explorer = true;
                    }
                    ui.label(
                        egui::RichText::new("/")
                            .size(FS_SMALL)
                            .color(c_text_faint()),
                    );
                    if let Some((_, ref mut draft)) = renaming {
                        let field_id = egui::Id::new("chat_header_rename_input");
                        let edit = ui.add(
                            egui::TextEdit::singleline(draft)
                                .id(field_id)
                                .font(egui::FontId::proportional(FS_SMALL))
                                .desired_width((ui.available_width() - 8.0).clamp(80.0, 260.0))
                                .margin(Margin::symmetric(6, 2)),
                        );
                        if !edit.has_focus() && !edit.lost_focus() {
                            edit.request_focus();
                        }
                        let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                        let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
                        if esc {
                            cancel_rename = true;
                        } else if enter || edit.lost_focus() {
                            commit_rename = Some(draft.clone());
                        }
                    } else {
                        let title_resp = ui
                            .add(
                                egui::Label::new(
                                    egui::RichText::new(&session_title)
                                        .size(FS_SMALL)
                                        .color(c_text()),
                                )
                                .truncate()
                                .sense(Sense::click())
                                .selectable(false),
                            )
                            .on_hover_cursor(egui::CursorIcon::Text)
                            .on_hover_text(format!("{session_title}\nDouble-click to rename"));
                        if title_resp.double_clicked() {
                            start_rename = true;
                        }
                    }
                });
            },
        );

        if cancel_rename {
            ui.ctx()
                .data_mut(|d| d.remove::<(super::state::SessionKey, String)>(rename_id));
        } else if let Some(new_title) = commit_rename {
            ui.ctx()
                .data_mut(|d| d.remove::<(super::state::SessionKey, String)>(rename_id));
            let trimmed = new_title.trim().to_owned();
            if !trimmed.is_empty() && trimmed != self.active_session().title {
                let si = self.active_workspace().active;
                self.rename_session(si, trimmed);
                crate::ui::chrome::show_toast(ui.ctx(), "Chat renamed");
            }
        } else if start_rename {
            let current = self.active_session().title.clone();
            ui.ctx()
                .data_mut(|d| d.insert_temp(rename_id, (active_key, current)));
        } else if let Some(state) = renaming {
            ui.ctx().data_mut(|d| d.insert_temp(rename_id, state));
        }

        if toggle_find {
            if find_open {
                self.close_chat_find();
            } else {
                self.open_chat_find();
            }
        }
        if toggle_explorer {
            self.request_settings_exit(super::state::SettingsExitAction::ToggleExplorer);
        }
        if open_palette {
            self.open_file_picker_with(">");
        }
        if toggle_split {
            if split_open {
                self.close_split();
            } else {
                self.new_chat_split();
            }
        }
        if reopen_editor {
            let idx = self
                .conv
                .editor
                .hidden_active
                .take()
                .filter(|&i| i < self.conv.editor.documents.len())
                .or_else(|| (!self.conv.editor.documents.is_empty()).then_some(0));
            if let Some(i) = idx {
                self.conv.editor.active = Some(i);
                self.conv.editor.focus_editor_next_frame = true;
            }
        }
    }

    /// The active chat's transcript with the composer floating over its bottom edge.
    pub(super) fn render_chat_column(&mut self, ui: &mut Ui) {
        if self.render_subagent_chat(ui, ui.available_height()) {
            return;
        }
        let style = (*ui.style()).clone();
        let column_center_w = crate::theme::chat_column_center_width(ui.available_width(), &style);

        const COMPOSER_GAP: f32 = 8.0;
        let composer_overlay_h = (self.conv.composer.measured_full_h + COMPOSER_GAP).max(88.0);
        let conversation_h = ui.available_height().max(48.0);
        let chat_rect = ui.max_rect();
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), conversation_h),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                self.render_conversation(ui, column_center_w, conversation_h, composer_overlay_h);
            },
        );

        // Soft scrim so transcript text doesn't compete with the input.
        // If TextEdit changes height while rendering, `render_composer` asks
        // egui for a second layout pass in the same frame. That keeps this
        // previous measurement safe for hard newlines, wrapping, deletion,
        // sending, attachments, and notices without predicting individual keys.
        let composer_h = self.conv.composer.measured_full_h.max(80.0);
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
        ui.scope_builder(egui::UiBuilder::new().max_rect(composer_rect), |ui| {
            self.render_composer(ui, column_center_w);
        });
    }

    pub(super) fn render_git_resize_sep(&mut self, ui: &mut Ui, full_h: f32) {
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
            self.conv.git_ui.width = (ui.max_rect().right() - pos.x).clamp(
                crate::app::git_panel::GIT_W_MIN,
                crate::app::git_panel::GIT_W_MAX,
            );
            self.conv.settings.git_width = self.conv.git_ui.width;
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

    pub(super) fn render_sidebar_resize_sep(
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
        if sep.dragged()
            && let Some(pos) = ui.input(|i| i.pointer.interact_pos())
        {
            // Track the pointer's absolute position, not per-frame deltas: deltas keep
            // applying while the width is clamped, so over-dragging past the minimum
            // desyncs the edge from the pointer and the sidebar jitters on any
            // back-and-forth pointer movement.
            self.conv.sidebar.width = (pos.x - ui.min_rect().left()).clamp(min_w, max_w);
            self.conv.settings.sidebar_width = self.conv.sidebar.width;
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

/// Short single-segment project name for the top breadcrumb (`oxi` instead of `Projects/oxi`).
fn header_workspace_name(root_path: &str) -> String {
    if crate::app_dirs::is_general_chats_dir(root_path) {
        return "General".to_string();
    }
    if let Some((host, path)) = crate::remote::split(root_path) {
        let trimmed = path.trim_end_matches('/');
        let name = trimmed
            .rsplit('/')
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or(trimmed);
        return format!("{name} · {host}");
    }
    std::path::Path::new(root_path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| workspace_sidebar_label(root_path))
}

pub(super) fn paint_composer_scrim(ui: &mut Ui, rect: egui::Rect) {
    if rect.height() < 4.0 || rect.width() <= 0.0 {
        return;
    }
    let base = c_bg_main();
    // Smooth GPU-interpolated vertical gradient (ease-in stops from transparent at the top to
    // opaque behind the composer), avoiding visible horizontal step bands on dark themes.
    const STOPS: usize = 5;
    let mut mesh = egui::Mesh::default();
    for i in 0..=STOPS {
        let t = i as f32 / STOPS as f32;
        let alpha = (t * t * 235.0).round() as u8;
        let color = Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), alpha);
        let y = egui::lerp(rect.top()..=rect.bottom(), t);
        mesh.colored_vertex(egui::pos2(rect.left(), y), color);
        mesh.colored_vertex(egui::pos2(rect.right(), y), color);
        if i > 0 {
            let row = (i as u32) * 2;
            let prev = row - 2;
            mesh.add_triangle(prev, prev + 1, row + 1);
            mesh.add_triangle(prev, row + 1, row);
        }
    }
    ui.painter().add(egui::Shape::mesh(mesh));
}

/// Compact dual-pane split icon button for the chat header.
fn header_split_button(ui: &mut Ui, active: bool) -> egui::Response {
    let size = egui::vec2(26.0, 26.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let hovered = resp.hovered() || resp.has_focus();
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let radius = egui::CornerRadius::same(RADIUS_ROW);
    if active {
        ui.painter().rect_filled(rect, radius, c_row_active());
    } else if hovered {
        ui.painter().rect_filled(rect, radius, c_row_hover());
    }
    let icon = rect.shrink2(egui::vec2(6.5, 7.5));
    let color = if active {
        c_accent()
    } else if hovered {
        c_text_strong()
    } else {
        c_text_muted()
    };
    ui.painter().rect_stroke(
        icon,
        2,
        Stroke::new(1.15, color),
        egui::StrokeKind::Inside,
    );
    ui.painter().line_segment(
        [
            egui::pos2(icon.center().x, icon.top()),
            egui::pos2(icon.center().x, icon.bottom()),
        ],
        Stroke::new(1.0, color),
    );
    resp
}

/// Compact `>_` / command-palette icon button for the chat header.
fn header_palette_button(ui: &mut Ui) -> egui::Response {
    let size = egui::vec2(26.0, 26.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let hovered = resp.hovered() || resp.has_focus();
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        ui.painter()
            .rect_filled(rect, egui::CornerRadius::same(RADIUS_ROW), c_row_hover());
    }
    let color = if hovered {
        c_text_strong()
    } else {
        c_text_muted()
    };
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "⌘",
        egui::FontId::proportional(FS_SMALL),
        color,
    );
    resp
}
