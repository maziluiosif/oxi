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
                        left: 12,
                        right: 10,
                        top: 12 + crate::ui::window_chrome::TITLEBAR_H as i8,
                        bottom: 12,
                    })
                    .show(&mut sidebar_ui, |ui| {
                        ui.set_min_width(ui.max_rect().width());
                        ui.set_min_height(ui.max_rect().height());
                        if self.conv.sidebar.mode == super::state::SidebarMode::Explorer {
                            self.render_file_explorer(ui);
                        } else {
                            self.render_sidebar(ui);
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
                    || (self.conv.editor.diff_tab_active
                        && self.conv.diff_view.open
                        && self.conv.git.diff.is_some());
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
                        top: if editor_open { 0 } else { CHAT_FRAME_TOP as i8 },
                        // The editor owns a bottom-docked find panel, so it must meet the
                        // status bar without the chat view's composer breathing room.
                        bottom: if editor_open {
                            0
                        } else {
                            CHAT_FRAME_BOTTOM as i8
                        },
                    })
                    .show(ui, |ui| {
                        if editor_open {
                            self.render_text_editor(ui);
                            return;
                        }

                        let style = (*ui.style()).clone();
                        let column_center_w =
                            crate::theme::chat_column_center_width(ui.available_width(), &style);

                        let show_diff = self.conv.diff_view.open && self.conv.git.diff.is_some();

                        // Floating composer always stays available — even over a diff —
                        // so you can discuss the change without leaving the view.
                        const COMPOSER_GAP: f32 = 8.0;
                        let composer_overlay_h =
                            (self.conv.composer.measured_full_h + COMPOSER_GAP).max(88.0);
                        let conversation_h = ui.available_height().max(48.0);
                        let chat_rect = ui.max_rect();
                        ui.allocate_ui_with_layout(
                            egui::vec2(ui.available_width(), conversation_h),
                            egui::Layout::top_down(egui::Align::Min),
                            |ui| {
                                if show_diff {
                                    if let Some(title) =
                                        self.conv.git.diff.as_ref().map(|(title, _)| title.clone())
                                    {
                                        self.render_diff_view(ui, &title, column_center_w);
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
                    });
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
        let col = if sep.dragged() {
            c_accent()
        } else {
            crate::theme::c_border_subtle()
        };
        ui.painter()
            .vline(boundary_x, sep_rect.y_range(), Stroke::new(1.0, col));
    }
}

pub(super) fn paint_composer_scrim(ui: &mut Ui, rect: egui::Rect) {
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
