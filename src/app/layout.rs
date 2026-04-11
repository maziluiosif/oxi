use eframe::egui::scroll_area::ScrollBarVisibility;
use eframe::egui::text_edit::TextEditState;
use eframe::egui::{
    self, Align, Button, Color32, FontId, Frame, Id, Key, Label, Layout, Margin, Modifiers, Order,
    RichText, Rounding, ScrollArea, Sense, Stroke, TextEdit, Ui,
};

use crate::model::MsgRole;
use crate::oauth::{clear_codex, clear_copilot, load_oauth_store, save_oauth_store, OAuthUiMsg};
use crate::settings::{LlmProviderKind, ALL_TOOL_NAMES};

use super::task_runner::spawn_async_task;

use super::SettingsTab;
use crate::theme::{
    chat_column_center_width, format_stream_elapsed, sidebar_session_title_display, small_spinner,
    workspace_sidebar_label, CHAT_COLUMN_MAX, CHAT_FRAME_BOTTOM, CHAT_FRAME_TOP,
    CHAT_VIEW_MARGIN_LEFT, CHAT_VIEW_MARGIN_RIGHT, C_ACCENT, C_BG_ELEVATED, C_BG_INPUT, C_BG_MAIN,
    C_BG_SIDEBAR, C_BORDER, C_BORDER_SUBTLE, C_ROW_ACTIVE, C_ROW_HOVER, C_SIDEBAR_SECTION, C_TEXT,
    C_TEXT_MUTED, FS_SMALL, FS_TINY, SIDEBAR_RESIZE_SEP_W,
};
use crate::ui::chrome::{render_empty_state, sidebar_text_field};
use crate::ui::messages::{render_assistant_message_run, render_message};

use super::PiChatApp;

impl PiChatApp {
    /// Sidebar list and controls (drawn inside a padded frame by [`Self::render_main_area`]).
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
        const SIDEBAR_ACTION_H: f32 = 26.0;
        const SIDEBAR_ACTION_R: f32 = 6.0;
        {
            let full_w = ui.available_width();
            let (rect, response) =
                ui.allocate_exact_size(egui::vec2(full_w, SIDEBAR_ACTION_H), Sense::click());
            let hovered = response.hovered();
            let fill = if hovered { C_ROW_HOVER } else { C_BG_ELEVATED };
            let rounding = Rounding::same(SIDEBAR_ACTION_R);
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
                        ui.label(
                            RichText::new("+  Add workspace")
                                .size(FS_SMALL)
                                .color(C_TEXT),
                        );
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

        ui.add_space(7.0);

        let scroll_h = ui.available_height().max(48.0);
        ScrollArea::vertical()
            .id_salt("sidebar_main_scroll")
            .max_height(scroll_h)
            .auto_shrink([false, false])
            .scroll_bar_visibility(ScrollBarVisibility::VisibleWhenNeeded)
            .show(ui, |ui| {
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
                    let chev_ws = if folded { "▸" } else { "▾" };
                    let row_w = ui.available_width();
                    if ui
                        .add_sized(
                            [row_w, 0.0],
                            Button::new(
                                RichText::new(format!("{chev_ws}  {root_label}"))
                                    .size(FS_TINY)
                                    .color(C_SIDEBAR_SECTION),
                            )
                            .frame(false)
                            .fill(Color32::TRANSPARENT)
                            .min_size(egui::vec2(row_w, 17.0)),
                        )
                        .on_hover_text(
                            "Fold or unfold chats. Each folder is a workspace (agent cwd).",
                        )
                        .clicked()
                    {
                        self.conv.workspaces[wi].sidebar_folded =
                            !self.conv.workspaces[wi].sidebar_folded;
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
                                    let inner = rect.shrink2(egui::vec2(3.0, ROW_VMARGIN));
                                    ui.allocate_new_ui(
                                        egui::UiBuilder::new().max_rect(inner),
                                        |ui| {
                                            ui.set_min_width(inner.width());
                                            let row_h = ROW_INNER_H;
                                            let lead_w = 14.0;
                                            const BULLET_GAP: f32 = 4.0;
                                            const SPINNER_GAP: f32 = 4.0;
                                            let time_w = if running { 40.0 } else { 0.0 };
                                            let spin_reserve = if running { 14.0 } else { 0.0 };
                                            let sx = ui.spacing().item_spacing.x;
                                            let time_label = if running {
                                                self.flow
                                                    .stream_started_at
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
                                            let bullet_col =
                                                if selected { C_ACCENT } else { C_TEXT_MUTED };
                                            ui.with_layout(
                                                Layout::left_to_right(Align::Center),
                                                |ui| {
                                                    ui.spacing_mut().item_spacing.x = sx;
                                                    ui.allocate_ui_with_layout(
                                                        egui::vec2(lead_w, row_h),
                                                        egui::Layout::left_to_right(Align::Center),
                                                        |ui| {
                                                            ui.label(
                                                                RichText::new("•")
                                                                    .size(FS_SMALL)
                                                                    .color(bullet_col),
                                                            );
                                                        },
                                                    );
                                                    ui.add_space(BULLET_GAP);
                                                    if running {
                                                        ui.allocate_ui_with_layout(
                                                            egui::vec2(spin_reserve, row_h),
                                                            egui::Layout::left_to_right(
                                                                Align::Center,
                                                            ),
                                                            |ui| {
                                                                small_spinner(ui);
                                                            },
                                                        );
                                                        ui.add_space(SPINNER_GAP);
                                                    }
                                                    ui.allocate_ui_with_layout(
                                                        egui::vec2(title_w, row_h),
                                                        egui::Layout::left_to_right(Align::Center),
                                                        |ui| {
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
                                                            egui::vec2(time_w, row_h),
                                                            egui::Layout::right_to_left(
                                                                Align::Center,
                                                            ),
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
                                                },
                                            );
                                        },
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
            });
        ui.expand_to_include_rect(ui.max_rect());
    }

    /// Top-right over the chat column (does not live in the sidebar).
    fn render_floating_new_chat_button(&mut self, ui: &Ui, chat_panel: egui::Rect) {
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

    /// Central region: manual split (sidebar | chat). Avoids egui `SidePanel` / `PanelState` width bugs.
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
                        // `allocate_new_ui` advances the parent by `child_ui.min_rect()`, not by the
                        // requested `w`. If the frame reported a narrow rect, the separator would start
                        // too early and the chat column would overlap the intended sidebar width.
                        ui.expand_to_include_rect(ui.max_rect());
                    },
                );

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
                    self.conv.sidebar_width = (self.conv.sidebar_width + sep.drag_delta().x)
                        .clamp(SIDEBAR_W_MIN, SIDEBAR_W_MAX);
                }
                if sep.hovered() || sep.dragged() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                }
                ui.painter().vline(
                    boundary_x,
                    sep_rect.y_range(),
                    Stroke::new(1.0, C_BORDER_SUBTLE),
                );
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
                                chat_column_center_width(ui.available_width(), &style);
                            const COMPOSER_GAP: f32 = 4.0;
                            ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                                self.render_composer(ui, column_center_w);
                                ui.add_space(COMPOSER_GAP);
                                let scroll_h = ui.available_height().max(48.0);
                                ui.allocate_ui_with_layout(
                                    egui::vec2(ui.available_width(), scroll_h),
                                    egui::Layout::top_down(egui::Align::Min),
                                    |ui| {
                                        self.render_conversation(ui, column_center_w, scroll_h);
                                        // `allocate_ui_with_layout` advances the parent by the
                                        // child UI's used rect, not by the requested height.
                                        // Keep the transcript block pinned to the remaining chat
                                        // viewport after the bottom composer takes its real size.
                                        ui.expand_to_include_rect(ui.max_rect());
                                    },
                                );
                            });
                            ui.expand_to_include_rect(ui.max_rect());
                            self.render_floating_new_chat_button(ui, chat_panel_rect);
                        });
                    // Same sizing issue as the sidebar child: report the full requested chat pane
                    // height to the parent horizontal layout, otherwise egui may vertically align
                    // this pane against the sidebar content height and create a fake top gap.
                    ui.expand_to_include_rect(ui.max_rect());
                },
            );
        });
    }

    pub(crate) fn render_composer(&mut self, ui: &mut Ui, column_center_w: f32) {
        let full_w = column_center_w;
        let col_w = full_w.min(CHAT_COLUMN_MAX);
        let pad = ((full_w - col_w) * 0.5).max(0.0);
        ui.horizontal(|ui| {
            if pad > 0.0 {
                ui.add_space(pad);
            }
            ui.vertical(|ui| {
                ui.set_width(col_w);
                let model_id: Option<String> = Some(self.conv.settings.model_id.clone());
                const MODEL_CHIP_CAP: f32 = 116.0;
                const MODEL_CHIP_GAP: f32 = 6.0;
                const COMPOSER_CONTROL_H: f32 = 20.0;
                let model_slot_w = MODEL_CHIP_CAP + MODEL_CHIP_GAP;
                Frame::none()
                    .fill(C_BG_INPUT)
                    .stroke(Stroke::new(1.0, C_BORDER_SUBTLE))
                    .rounding(16.0)
                    .inner_margin(Margin {
                        left: 9.0,
                        right: 9.0,
                        top: 2.0,
                        bottom: 2.0,
                    })
                    .show(ui, |ui| {
                        ui.with_layout(egui::Layout::bottom_up(Align::Min), |ui| {
                            ui.spacing_mut().item_spacing.y = 1.0;

                            // Do not use `horizontal_centered` here: it allocates the parent's full height to
                            // vertically center children, which can expand this row to eat the entire chat
                            // region when combined with the bottom panel's vertical layout.
                            ui.with_layout(egui::Layout::left_to_right(Align::Center), |ui| {
                                ui.spacing_mut().item_spacing.x = 4.0;
                                if ui
                                    .add_sized(
                                        [22.0, COMPOSER_CONTROL_H],
                                        Button::new(
                                            RichText::new("+").size(14.0).color(C_TEXT_MUTED),
                                        )
                                        .rounding(5.0),
                                    )
                                    .on_hover_text(
                                        "Attach image from disk. Paste image: Ctrl/Cmd+V.",
                                    )
                                    .clicked()
                                {
                                    self.pick_image_attachment();
                                }

                                const COMPOSER_ACTION: f32 = 27.0;
                                let te_id = ui.id().with("composer_multiline");
                                let ctx = ui.ctx().clone();
                                if ui.memory(|m| m.focused() == Some(te_id)) {
                                    if ctx.input(|i| {
                                        i.key_pressed(Key::ArrowUp) && !i.modifiers.any()
                                    }) && self.try_input_history_up(te_id, &ctx)
                                    {
                                        ctx.input_mut(|i| {
                                            i.consume_key(Modifiers::NONE, Key::ArrowUp);
                                        });
                                    } else if ctx.input(|i| {
                                        i.key_pressed(Key::ArrowDown) && !i.modifiers.any()
                                    }) && self.try_input_history_down(te_id, &ctx)
                                    {
                                        ctx.input_mut(|i| {
                                            i.consume_key(Modifiers::NONE, Key::ArrowDown);
                                        });
                                    }
                                }
                                let has_text = !self.conv.input.trim().is_empty();
                                let has_attachments = !self.conv.pending_images.is_empty();
                                let can_send = has_text || has_attachments;
                                let slot_gap = ui.spacing().item_spacing.x;
                                let editor_w = (ui.available_width()
                                    - model_slot_w
                                    - COMPOSER_ACTION
                                    - slot_gap * 2.0)
                                    .max(80.0);
                                // `TextEdit` must not sit directly in a horizontal row: egui expands
                                // horizontal children downward (see egui `Layout::next_frame_ignore_wrap`).
                                // A bottom-up vertical strip anchors the editor to the row baseline so
                                // extra lines grow upward into the transcript.
                                let editor_response =
                                    ui.with_layout(egui::Layout::bottom_up(Align::Min), |ui| {
                                        ui.set_width(editor_w);
                                        ui.add(
                                            TextEdit::multiline(&mut self.conv.input)
                                                .id(te_id)
                                                .frame(false)
                                                .margin(Margin::symmetric(1.0, 0.0))
                                                .desired_width(editor_w)
                                                .desired_rows(1)
                                                .min_size(egui::vec2(0.0, 17.0))
                                                .vertical_align(Align::Center)
                                                .font(FontId::proportional(FS_SMALL))
                                                .text_color(C_TEXT)
                                                .hint_text(
                                                    RichText::new("Message…")
                                                        .size(FS_TINY)
                                                        .color(C_TEXT_MUTED),
                                                )
                                                .return_key(egui::KeyboardShortcut::new(
                                                    egui::Modifiers::SHIFT,
                                                    egui::Key::Enter,
                                                )),
                                        )
                                    });
                                let editor = editor_response.inner;
                                if editor.changed()
                                    && !self.conv.input_history_ignore_next_edit_change
                                {
                                    self.conv.input_history_index = None;
                                }
                                if self.conv.input_history_ignore_next_edit_change {
                                    self.conv.input_history_ignore_next_edit_change = false;
                                }
                                if editor.has_focus() {
                                    let c = ui.ctx().clone();
                                    let paste_img = c.input(|i| {
                                        i.key_pressed(Key::V)
                                            && (i.modifiers.command || i.modifiers.ctrl)
                                            && !i.modifiers.shift
                                    });
                                    if paste_img && self.try_paste_image_from_clipboard() {
                                        c.input_mut(|i| {
                                            if i.modifiers.command {
                                                i.consume_key(Modifiers::COMMAND, Key::V);
                                            } else {
                                                i.consume_key(Modifiers::CTRL, Key::V);
                                            }
                                        });
                                    }
                                }
                                let enter_to_send = ui.input(|i| {
                                    i.key_pressed(egui::Key::Enter)
                                        && !i.modifiers.shift
                                        && !i.modifiers.ctrl
                                        && !i.modifiers.alt
                                        && !i.modifiers.command
                                });
                                if (editor.has_focus() || editor.lost_focus())
                                    && enter_to_send
                                    && can_send
                                {
                                    self.send_message();
                                    editor.request_focus();
                                }
                                ui.allocate_ui_with_layout(
                                    egui::vec2(model_slot_w, COMPOSER_CONTROL_H),
                                    egui::Layout::right_to_left(Align::Center),
                                    |ui| {
                                        let chip_label = model_id.as_deref().unwrap_or("Model");
                                        ui.set_max_width(MODEL_CHIP_CAP);
                                        let chip = Button::new(
                                            RichText::new(chip_label)
                                                .size(FS_TINY - 0.25)
                                                .color(C_TEXT_MUTED),
                                        )
                                        .fill(C_BG_MAIN)
                                        .stroke(Stroke::new(1.0, C_BORDER))
                                        .rounding(6.0)
                                        .min_size(egui::vec2(0.0, COMPOSER_CONTROL_H - 6.0));
                                        let chip = ui.add(chip).on_hover_text(
                                            "Open settings: providers, system prompt, tools",
                                        );
                                        if chip.clicked() {
                                            self.conv.settings_open = true;
                                        }
                                    },
                                );
                                let waiting = self.flow.waiting_response;
                                let (fill, fg, enabled, icon, hover) = if waiting {
                                    (C_ACCENT, Color32::WHITE, true, "■", "Stop generation")
                                } else if can_send {
                                    (C_ACCENT, Color32::WHITE, true, "▶", "Send")
                                } else {
                                    (
                                        Color32::from_rgb(0x35, 0x37, 0x3d),
                                        C_TEXT_MUTED,
                                        false,
                                        "▶",
                                        "Message is empty",
                                    )
                                };
                                let clicked = ui
                                    .add_enabled(
                                        enabled,
                                        Button::new(RichText::new(icon).size(13.5).color(fg))
                                            .min_size(egui::vec2(
                                                COMPOSER_ACTION - 2.0,
                                                COMPOSER_CONTROL_H - 1.0,
                                            ))
                                            .fill(fill)
                                            .stroke(Stroke::NONE)
                                            .rounding(10.0),
                                    )
                                    .on_hover_text(hover)
                                    .clicked();
                                if clicked {
                                    if waiting {
                                        self.send_abort();
                                    } else if can_send {
                                        self.send_message();
                                    }
                                }
                            });

                            if !self.conv.pending_images.is_empty() {
                                let mut remove_attachment: Option<usize> = None;
                                ui.add_space(4.0);
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 5.0;
                                    ui.label(
                                        RichText::new("Attachments")
                                            .size(FS_TINY)
                                            .color(C_TEXT_MUTED),
                                    );
                                    ui.horizontal_wrapped(|ui| {
                                        ui.spacing_mut().item_spacing.x = 4.0;
                                        for (i, (mime, _)) in
                                            self.conv.pending_images.iter().enumerate()
                                        {
                                            let short = mime
                                                .strip_prefix("image/")
                                                .unwrap_or(mime.as_str());
                                            ui.horizontal(|ui| {
                                                Frame::none()
                                                    .fill(C_BG_MAIN)
                                                    .stroke(Stroke::new(1.0, C_BORDER))
                                                    .rounding(4.0)
                                                    .inner_margin(Margin::symmetric(4.0, 1.0))
                                                    .show(ui, |ui| {
                                                        ui.label(
                                                            RichText::new(short)
                                                                .size(FS_TINY)
                                                                .color(C_ACCENT),
                                                        );
                                                        if ui
                                                            .add(
                                                                Button::new(
                                                                    RichText::new("×")
                                                                        .size(11.0)
                                                                        .color(C_TEXT_MUTED),
                                                                )
                                                                .frame(false)
                                                                .fill(Color32::TRANSPARENT)
                                                                .min_size(egui::vec2(14.0, 14.0)),
                                                            )
                                                            .on_hover_text("Remove image")
                                                            .clicked()
                                                        {
                                                            remove_attachment = Some(i);
                                                        }
                                                    });
                                            });
                                        }
                                    });
                                });
                                if let Some(i) = remove_attachment {
                                    self.remove_pending_image_at(i);
                                }
                            }
                        });
                    });
            });
            if pad > 0.0 {
                ui.add_space(pad);
            }
        });
    }

    /// Connection / RPC / stream strip above the transcript (inside the scroll area).
    fn render_status_banner(&mut self, ui: &mut Ui) {
        let has_err = self.conn.connect_error.is_some() || self.flow.stream_error.is_some();
        if !has_err {
            return;
        }
        ui.spacing_mut().item_spacing.y = 4.0;
        if let Some(ref e) = self.conn.connect_error {
            Frame::none()
                .fill(Color32::from_rgb(0x32, 0x18, 0x18))
                .stroke(Stroke::new(1.0, Color32::from_rgb(0x70, 0x38, 0x38)))
                .rounding(Rounding::same(6.0))
                .inner_margin(Margin::symmetric(8.0, 6.0))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(
                        RichText::new(format!("Connection: {e}"))
                            .size(FS_SMALL)
                            .color(Color32::from_rgb(0xff, 0xb0, 0xb0)),
                    );
                });
            ui.add_space(4.0);
        }
        if let Some(ref e) = self.flow.stream_error {
            Frame::none()
                .fill(Color32::from_rgb(0x38, 0x28, 0x14))
                .stroke(Stroke::new(1.0, Color32::from_rgb(0x78, 0x58, 0x28)))
                .rounding(Rounding::same(6.0))
                .inner_margin(Margin::symmetric(8.0, 6.0))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(
                        RichText::new(format!("Agent: {e}"))
                            .size(FS_SMALL)
                            .color(Color32::from_rgb(0xff, 0xd0, 0xa0)),
                    );
                });
            ui.add_space(4.0);
        }
    }

    /// `scroll_budget` is the exact height of the region containing the transcript (from
    /// `allocate_ui_with_layout`). `ScrollArea::max_height` must not exceed this or the composer is
    /// pushed below the visible area.
    pub(crate) fn render_conversation(
        &mut self,
        ui: &mut Ui,
        column_center_w: f32,
        scroll_budget: f32,
    ) {
        let show_sidebar_button_pos = if self.conv.sidebar_open {
            None
        } else {
            Some(ui.min_rect().min + egui::vec2(0.0, 0.0))
        };

        let transcript_h = scroll_budget.max(40.0);

        let wi = self.conv.active_workspace;
        let si = self.conv.workspaces[wi].active;
        let agent_ack = self.flow.agent_ack;

        // Full-width scroll area so the vertical scrollbar sits at the chat pane edge.
        let scroll_outer_w = ui.available_width();
        let force_scroll_bottom = self.conv.scroll_to_bottom_once;
        let stick_bottom = force_scroll_bottom
            || self.flow.waiting_response
            || self.conv.workspaces[wi].sessions[si]
                .messages
                .last()
                .is_some_and(|m| m.role == MsgRole::Assistant && m.streaming);

        ScrollArea::vertical()
            .max_width(scroll_outer_w)
            .id_salt(self.conv.chat_scroll_id)
            .max_height(transcript_h)
            // Vertical `true`: shrink viewport when content is shorter than `max_height` (avoids a
            // huge empty band with `stick_to_bottom`). Horizontal `false`: keep stable width with
            // scrollbar (see egui `ScrollArea` `auto_shrink` table).
            .auto_shrink([false, true])
            // Reserve scrollbar width even when content fits so the transcript column width does not
            // jump when streaming grows past the viewport (matches Cursor-style stability).
            .scroll_bar_visibility(ScrollBarVisibility::AlwaysVisible)
            // Dragging the viewport to scroll fights with drag-to-select on labels.
            .drag_to_scroll(false)
            .stick_to_bottom(stick_bottom)
            .show(ui, |ui| {
                let viewport_w = ui.max_rect().width();
                ui.set_max_width(viewport_w);
                let full_w = column_center_w;
                let col_w = full_w.min(CHAT_COLUMN_MAX);
                let pad = ((full_w - col_w) * 0.5).max(0.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    if pad > 0.0 {
                        ui.add_space(pad);
                    }
                    ui.vertical(|ui| {
                        ui.set_width(col_w);
                        self.render_status_banner(ui);
                    });
                    if pad > 0.0 {
                        ui.add_space(pad);
                    }
                });
                ui.add_space(38.0);
                // egui 0.29 label selection does not mark the selected label as `dragged_id`, so
                // the transcript would stop scrolling as soon as the pointer drag left the current
                // viewport. Keep wheel/trackpad scrolling alive during selection drags, and
                // auto-scroll when the pointer is held near the transcript edges.
                let (selection_scroll_delta, consume_scroll_delta) =
                    conversation_selection_scroll_delta(ui);
                if selection_scroll_delta != egui::Vec2::ZERO {
                    ui.scroll_with_delta(selection_scroll_delta);
                    if consume_scroll_delta {
                        ui.ctx().input_mut(|i| {
                            i.smooth_scroll_delta = egui::Vec2::ZERO;
                        });
                    }
                    ui.ctx().request_repaint();
                }
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    if pad > 0.0 {
                        ui.add_space(pad);
                    }
                    ui.vertical(|ui| {
                        ui.set_width(col_w);
                        let messages = &self.conv.workspaces[wi].sessions[si].messages;
                        if messages.is_empty() {
                            render_empty_state(ui);
                        } else {
                            let mut mi = 0;
                            while mi < messages.len() {
                                let msg = &messages[mi];
                                if msg.role == MsgRole::Assistant {
                                    let start = mi;
                                    mi += 1;
                                    while mi < messages.len()
                                        && messages[mi].role == MsgRole::Assistant
                                    {
                                        mi += 1;
                                    }
                                    render_assistant_message_run(
                                        ui,
                                        start,
                                        &messages[start..mi],
                                        agent_ack,
                                    );
                                } else {
                                    render_message(ui, mi, msg, agent_ack);
                                    mi += 1;
                                }
                            }
                        }
                    });
                    if pad > 0.0 {
                        ui.add_space(pad);
                    }
                });
                if force_scroll_bottom {
                    ui.scroll_to_cursor(Some(Align::BOTTOM));
                }
            });
        if let Some(pos) = show_sidebar_button_pos {
            egui::Area::new(ui.id().with("show_sidebar_button"))
                .order(egui::Order::Foreground)
                .fixed_pos(pos)
                .show(ui.ctx(), |ui| {
                    if ui
                        .add_sized(
                            [28.0, 28.0],
                            Button::new(RichText::new("☰").size(14.0).color(C_TEXT_MUTED))
                                .fill(C_BG_ELEVATED)
                                .stroke(Stroke::new(1.0, C_BORDER_SUBTLE))
                                .rounding(6.0),
                        )
                        .on_hover_text("Show sidebar")
                        .clicked()
                    {
                        self.conv.sidebar_open = true;
                    }
                });
        }
        if self.conv.scroll_to_bottom_once {
            self.conv.scroll_to_bottom_once = false;
        };
    }

    fn spawn_github_oauth(&mut self, ctx: &egui::Context) {
        if self.conv.oauth_busy {
            return;
        }
        self.conv.oauth_busy = true;
        self.conv.oauth_last_message = None;
        let (tx, rx) = std::sync::mpsc::channel();
        self.conn.oauth_rx = Some(rx);
        let ctx = ctx.clone();
        let ent = self.conv.copilot_enterprise_domain.clone();
        spawn_async_task(
            {
                let tx = tx.clone();
                let ctx = ctx.clone();
                move |err| {
                    let _ = tx.send(OAuthUiMsg::GitHubDone(Err(err)));
                    ctx.request_repaint();
                }
            },
            move |rt| {
                let tx2 = tx.clone();
                let r = rt.block_on(crate::oauth::login_github_copilot(&ent, tx2));
                let _ = tx.send(OAuthUiMsg::GitHubDone(r));
                ctx.request_repaint();
            },
        );
    }

    fn spawn_codex_oauth(&mut self, ctx: &egui::Context) {
        if self.conv.oauth_busy {
            return;
        }
        self.conv.oauth_busy = true;
        self.conv.oauth_last_message = None;
        let (tx, rx) = std::sync::mpsc::channel();
        self.conn.oauth_rx = Some(rx);
        let ctx = ctx.clone();
        spawn_async_task(
            {
                let tx = tx.clone();
                let ctx = ctx.clone();
                move |err| {
                    let _ = tx.send(OAuthUiMsg::CodexDone(Err(err)));
                    ctx.request_repaint();
                }
            },
            move |rt| {
                let tx2 = tx.clone();
                let r = rt.block_on(crate::oauth::login_openai_codex(tx2));
                let _ = tx.send(OAuthUiMsg::CodexDone(r));
                ctx.request_repaint();
            },
        );
    }

    pub(crate) fn render_settings_page(&mut self, ui: &mut Ui) {
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
                                self.render_settings_sidebar(ui);
                            });
                        ui.expand_to_include_rect(ui.max_rect());
                    },
                );

                let boundary_x = ui.cursor().min.x;
                let sep_rect = egui::Rect::from_min_max(
                    egui::pos2(boundary_x - SIDEBAR_RESIZE_SEP_W * 0.5, ui.min_rect().top()),
                    egui::pos2(
                        boundary_x + SIDEBAR_RESIZE_SEP_W * 0.5,
                        ui.min_rect().top() + full_h,
                    ),
                );
                let sep = ui.interact(
                    sep_rect,
                    ui.id().with("settings_sidebar_sep"),
                    Sense::drag(),
                );
                if sep.dragged() {
                    self.conv.sidebar_width = (self.conv.sidebar_width + sep.drag_delta().x)
                        .clamp(SIDEBAR_W_MIN, SIDEBAR_W_MAX);
                }
                if sep.hovered() || sep.dragged() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                }
                ui.painter().vline(
                    boundary_x,
                    sep_rect.y_range(),
                    Stroke::new(1.0, C_BORDER_SUBTLE),
                );
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
                            ScrollArea::vertical()
                                .id_salt("settings_page_scroll")
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    self.render_settings_body(ui);
                                });
                            ui.expand_to_include_rect(ui.max_rect());
                        });
                    ui.expand_to_include_rect(ui.max_rect());
                },
            );
        });
    }

    fn render_settings_sidebar(&mut self, ui: &mut Ui) {
        ui.set_min_width(ui.max_rect().width());
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 5.0;
            if ui
                .add(
                    Button::new(RichText::new("← Back").size(FS_TINY).color(C_TEXT))
                        .frame(false)
                        .fill(Color32::TRANSPARENT),
                )
                .on_hover_text("Back to chat")
                .clicked()
            {
                self.conv.settings_open = false;
            }
        });
        ui.add_space(8.0);
        ui.label(RichText::new("SETTINGS").size(FS_TINY).color(C_TEXT_MUTED));
        ui.add_space(8.0);

        for (tab, label) in [
            (SettingsTab::Providers, "Providers"),
            (SettingsTab::SystemPrompt, "System prompt"),
        ] {
            let selected = self.conv.settings_tab == tab;
            let row_w = ui.available_width();
            let (rect, response) = ui.allocate_exact_size(egui::vec2(row_w, 28.0), Sense::click());
            let hovered = response.hovered();
            let fill = if selected {
                C_ROW_ACTIVE
            } else if hovered {
                C_ROW_HOVER
            } else {
                Color32::TRANSPARENT
            };
            ui.painter().rect_filled(rect, Rounding::same(6.0), fill);
            ui.allocate_new_ui(
                egui::UiBuilder::new().max_rect(rect.shrink2(egui::vec2(10.0, 4.0))),
                |ui| {
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        ui.label(RichText::new(label).size(FS_SMALL).color(C_TEXT));
                    });
                },
            );
            if hovered {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if response.clicked() {
                self.conv.settings_tab = tab;
            }
            ui.add_space(4.0);
        }

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(8.0);
        if ui.button("Save to disk").clicked() {
            if let Err(e) = self.conv.settings.save() {
                self.flow.stream_error = Some(format!("Save settings: {e}"));
            }
        }
        ui.label(
            RichText::new("Writes ~/.config/oxi/settings.json")
                .size(FS_TINY)
                .color(C_TEXT_MUTED),
        );
        ui.expand_to_include_rect(ui.max_rect());
    }

    fn render_settings_body(&mut self, ui: &mut Ui) {
        match self.conv.settings_tab {
            SettingsTab::Providers => {
                self.render_settings_providers_panel(ui);
            }
            SettingsTab::SystemPrompt => {
                self.render_settings_system_prompt_panel(ui);
            }
        }
    }

    fn render_settings_providers_panel(&mut self, ui: &mut Ui) {
        ui.label(
            RichText::new("Providers")
                .size(FS_SMALL)
                .color(C_TEXT)
                .strong(),
        );
        ui.add_space(8.0);
        ui.label(RichText::new("Provider").size(FS_TINY).color(C_TEXT_MUTED));
        egui::ComboBox::from_id_salt("llm_provider_combo")
            .selected_text(match self.conv.settings.provider {
                LlmProviderKind::OpenAi => "OpenAI",
                LlmProviderKind::OpenRouter => "OpenRouter",
                LlmProviderKind::GptCodex => "GPT Codex (OAuth or API key)",
                LlmProviderKind::GitHubCopilot => "GitHub Copilot",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut self.conv.settings.provider,
                    LlmProviderKind::OpenAi,
                    "OpenAI",
                );
                ui.selectable_value(
                    &mut self.conv.settings.provider,
                    LlmProviderKind::OpenRouter,
                    "OpenRouter",
                );
                ui.selectable_value(
                    &mut self.conv.settings.provider,
                    LlmProviderKind::GptCodex,
                    "GPT Codex (ChatGPT OAuth or API key)",
                );
                ui.selectable_value(
                    &mut self.conv.settings.provider,
                    LlmProviderKind::GitHubCopilot,
                    "GitHub Copilot",
                );
            });
        ui.add_space(8.0);
        ui.label(RichText::new("Model id").size(FS_TINY).color(C_TEXT_MUTED));
        ui.add(
            TextEdit::singleline(&mut self.conv.settings.model_id)
                .desired_width(f32::INFINITY)
                .hint_text("e.g. gpt-4o-mini or vendor/model")
                .margin(Margin::symmetric(4.0, 2.0)),
        );
        ui.add_space(6.0);
        ui.label(
            RichText::new("Base URL (optional)")
                .size(FS_TINY)
                .color(C_TEXT_MUTED),
        );
        ui.add(
            TextEdit::singleline(&mut self.conv.settings.base_url)
                .desired_width(f32::INFINITY)
                .hint_text("Leave empty for provider default")
                .margin(Margin::symmetric(4.0, 2.0)),
        );
        ui.add_space(10.0);
        ui.label(RichText::new("Tools").size(FS_TINY).color(C_TEXT_MUTED));
        for (i, name) in ALL_TOOL_NAMES.iter().enumerate() {
            ui.checkbox(&mut self.conv.settings.tools_enabled[i], *name);
        }
        ui.add_space(12.0);
        ui.separator();
        ui.label(
            RichText::new("OAuth sign-in")
                .size(FS_SMALL)
                .color(C_TEXT)
                .strong(),
        );
        ui.add_space(6.0);
        let oauth = load_oauth_store();
        ui.label(
            RichText::new(format!(
                "Tokens file: {}",
                crate::oauth::oauth_config_path().display()
            ))
            .size(FS_TINY)
            .color(C_TEXT_MUTED),
        );
        ui.add_space(6.0);
        ui.label(
            RichText::new("GitHub Copilot (device flow)")
                .size(FS_TINY)
                .color(C_TEXT_MUTED),
        );
        ui.label(
            RichText::new("Optional Enterprise hostname (blank = github.com)")
                .size(FS_TINY)
                .color(C_TEXT_MUTED),
        );
        ui.add(
            TextEdit::singleline(&mut self.conv.copilot_enterprise_domain)
                .desired_width(f32::INFINITY)
                .hint_text("e.g. company.ghe.com")
                .margin(Margin::symmetric(4.0, 2.0)),
        );
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!self.conv.oauth_busy, Button::new("Sign in with GitHub"))
                .clicked()
            {
                self.spawn_github_oauth(ui.ctx());
            }
            if ui
                .add_enabled(oauth.github_copilot.is_some(), Button::new("Sign out"))
                .clicked()
            {
                let mut s = load_oauth_store();
                clear_copilot(&mut s);
                let _ = save_oauth_store(&s);
                self.conv.oauth_last_message = Some("Signed out GitHub Copilot.".into());
            }
        });
        if let Some((ref url, ref code)) = self.conv.oauth_device_copilot {
            ui.label(
                RichText::new(format!("Open {url}\nEnter code: {code}"))
                    .size(FS_TINY)
                    .color(C_ACCENT),
            );
        }
        ui.add_space(8.0);
        ui.label(
            RichText::new("ChatGPT / Codex (browser + localhost:1455)")
                .size(FS_TINY)
                .color(C_TEXT_MUTED),
        );
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!self.conv.oauth_busy, Button::new("Sign in with ChatGPT"))
                .clicked()
            {
                self.spawn_codex_oauth(ui.ctx());
            }
            if ui
                .add_enabled(oauth.openai_codex.is_some(), Button::new("Sign out"))
                .clicked()
            {
                let mut s = load_oauth_store();
                clear_codex(&mut s);
                let _ = save_oauth_store(&s);
                self.conv.oauth_last_message = Some("Signed out Codex OAuth.".into());
            }
        });
        if let Some(ref msg) = self.conv.oauth_last_message {
            ui.label(RichText::new(msg).size(FS_TINY).color(C_TEXT));
        }
        ui.add_space(10.0);
        ui.label(
            RichText::new("API keys (fallback if OAuth not used)")
                .size(FS_TINY)
                .color(C_TEXT_MUTED),
        );
        ui.label(
            RichText::new(
                "OpenAI / Codex (no OAuth): OPENAI_API_KEY\n\
                 OpenRouter: OPENROUTER_API_KEY\n\
                 Copilot (no OAuth): COPILOT_GITHUB_TOKEN, GH_TOKEN, or GITHUB_TOKEN",
            )
            .size(FS_TINY)
            .color(C_TEXT_MUTED),
        );
        ui.add_space(6.0);
        ui.label(
            RichText::new("Optional: OPENROUTER_HTTP_REFERER, OPENROUTER_TITLE for OpenRouter.")
                .size(FS_TINY)
                .color(C_TEXT_MUTED),
        );
    }

    fn render_settings_system_prompt_panel(&mut self, ui: &mut Ui) {
        ui.label(
            RichText::new("System prompt")
                .size(FS_SMALL)
                .color(C_TEXT)
                .strong(),
        );
        ui.add_space(6.0);
        ui.label(
            RichText::new(
                "Optional override. If filled, it fully replaces the built-in agent prompt body.",
            )
            .size(FS_TINY)
            .color(C_TEXT_MUTED),
        );
        ui.add_space(8.0);
        ui.label(
            RichText::new("System prompt override")
                .size(FS_TINY)
                .color(C_TEXT_MUTED),
        );
        ui.add(
            TextEdit::multiline(&mut self.conv.settings.system_prompt)
                .desired_width(f32::INFINITY)
                .desired_rows(8)
                .margin(Margin::symmetric(4.0, 4.0)),
        );
        ui.add_space(10.0);
        ui.label(
            RichText::new(
                "Built-in agent prompt template, editable on the fly. Use {tools_list} to inject enabled tools automatically.",
            )
            .size(FS_TINY)
            .color(C_TEXT_MUTED),
        );
        ui.add_space(6.0);
        ui.label(
            RichText::new("Agent system prompt template")
                .size(FS_TINY)
                .color(C_TEXT_MUTED),
        );
        ui.add(
            TextEdit::multiline(&mut self.conv.settings.agent_system_prompt)
                .desired_width(f32::INFINITY)
                .desired_rows(16)
                .margin(Margin::symmetric(4.0, 4.0))
                .hint_text(crate::agent::prompt::DEFAULT_AGENT_SYSTEM_PROMPT),
        );
    }

    fn try_input_history_up(&mut self, te_id: Id, ctx: &egui::Context) -> bool {
        let hist = &self.conv.input_history;
        if hist.is_empty() {
            return false;
        }
        let char_idx = TextEditState::load(ctx, te_id)
            .and_then(|s| s.cursor.char_range())
            .map(|r| r.primary.index)
            .unwrap_or(0);
        let text = &self.conv.input;
        let empty = text.trim().is_empty();
        let on_first = char_idx_on_first_line(text, char_idx);

        match self.conv.input_history_index {
            None => {
                if !empty && !on_first {
                    return false;
                }
                self.conv.input_history_draft = text.clone();
                self.conv.input = hist[0].clone();
                self.conv.input_history_index = Some(0);
                self.conv.input_history_ignore_next_edit_change = true;
                true
            }
            Some(i) => {
                if !on_first {
                    return false;
                }
                if i + 1 < hist.len() {
                    self.conv.input = hist[i + 1].clone();
                    self.conv.input_history_index = Some(i + 1);
                    self.conv.input_history_ignore_next_edit_change = true;
                    true
                } else {
                    false
                }
            }
        }
    }

    fn try_input_history_down(&mut self, te_id: Id, ctx: &egui::Context) -> bool {
        let hist = &self.conv.input_history;
        if hist.is_empty() {
            return false;
        }
        let char_idx = TextEditState::load(ctx, te_id)
            .and_then(|s| s.cursor.char_range())
            .map(|r| r.primary.index)
            .unwrap_or(0);
        let text = &self.conv.input;
        let on_last = char_idx_on_last_line(text, char_idx);

        match self.conv.input_history_index {
            None => false,
            Some(0) if !on_last => false,
            Some(0) => {
                self.conv.input = std::mem::take(&mut self.conv.input_history_draft);
                self.conv.input_history_index = None;
                self.conv.input_history_ignore_next_edit_change = true;
                true
            }
            Some(_i) if !on_last => false,
            Some(i) => {
                self.conv.input = hist[i - 1].clone();
                self.conv.input_history_index = Some(i - 1);
                self.conv.input_history_ignore_next_edit_change = true;
                true
            }
        }
    }
}

fn char_idx_on_first_line(s: &str, char_idx: usize) -> bool {
    s.chars().take(char_idx).filter(|c| *c == '\n').count() == 0
}

fn char_idx_on_last_line(s: &str, char_idx: usize) -> bool {
    s.chars().skip(char_idx).filter(|c| *c == '\n').count() == 0
}

fn conversation_selection_scroll_delta(ui: &Ui) -> (egui::Vec2, bool) {
    let ctx = ui.ctx();
    let widget_dragging = ctx.dragged_id().is_some();
    let label_selection_dragging = ctx.input(|i| i.pointer.primary_down())
        && egui::text_selection::LabelSelectionState::load(ctx).has_selection();

    if !widget_dragging && !label_selection_dragging {
        return (egui::Vec2::ZERO, false);
    }

    let mut delta = egui::Vec2::ZERO;
    let wheel_delta = ctx.input(|i| i.smooth_scroll_delta);
    if wheel_delta != egui::Vec2::ZERO {
        delta += wheel_delta;
    }

    if label_selection_dragging {
        if let Some(pointer_pos) = ctx.pointer_interact_pos() {
            let viewport = ui.clip_rect();
            let horizontal_margin = 64.0;
            let edge_zone = 28.0;
            let edge_range = edge_zone * 2.0;
            let within_transcript_x = pointer_pos.x >= viewport.left() - horizontal_margin
                && pointer_pos.x <= viewport.right() + horizontal_margin;

            if within_transcript_x {
                let top_zone = viewport.top() + edge_zone;
                if pointer_pos.y < top_zone {
                    let strength = ((top_zone - pointer_pos.y) / edge_range).clamp(0.0, 1.0);
                    delta.y += 6.0 + strength * 22.0;
                }

                let bottom_zone = viewport.bottom() - edge_zone;
                if pointer_pos.y > bottom_zone {
                    let strength = ((pointer_pos.y - bottom_zone) / edge_range).clamp(0.0, 1.0);
                    delta.y -= 6.0 + strength * 22.0;
                }
            }
        }
    }

    (delta, wheel_delta != egui::Vec2::ZERO)
}
