//! One workspace in the sidebar: its header row, grouped chat rows and their context menus.

use super::*;

impl OxiApp {
    pub(super) fn render_sidebar_workspace(
        &mut self,
        ui: &mut Ui,
        wi: usize,
        parent: Option<usize>,
        parent_folded: bool,
        q: &str,
    ) -> bool {
        const FS_WORKSPACE: f32 = FS_SMALL + 1.5;
        let mut sidebar_changed = false;
        let is_main = self.conv.workspaces[wi].worktree.is_none();
        let active_si = self.conv.workspaces[wi].active;
        let n_sessions = self.conv.workspaces[wi].sessions.len();
        let root_label = match &self.conv.workspaces[wi].worktree {
            Some(info) if parent.is_some() => info.branch.clone(),
            Some(info) => crate::app::worktrees::worktree_label(info),
            None => workspace_sidebar_label(&self.conv.workspaces[wi].root_path),
        };
        let folded = self.conv.workspaces[wi].sidebar_folded || parent_folded;
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
        let glyph = match (row_hovered || parent.is_some(), folded) {
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
            egui::pos2(
                rect.right() - PLUS_W * if is_main { 2.0 } else { 1.0 } - 2.0,
                rect.bottom(),
            ),
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
        let mut worktree_clicked = false;
        let mut worktree_hovered = false;
        if is_main {
            let action_rect = egui::Rect::from_min_max(
                egui::pos2(rect.right() - PLUS_W * 2.0 - 2.0, rect.top()),
                egui::pos2(rect.right() - PLUS_W - 2.0, rect.bottom()),
            );
            let action = ui
                .interact(
                    action_rect,
                    ui.id().with(("ws_worktree", wi)),
                    Sense::click(),
                )
                .on_hover_text("New worktree chat — create an isolated branch and start chatting");
            worktree_clicked = action.clicked();
            worktree_hovered = action.hovered();
            ui.painter().text(
                action_rect.center(),
                egui::Align2::CENTER_CENTER,
                ICON_BRANCH,
                FontId::new(FS_TINY, icon_font()),
                if worktree_hovered {
                    c_accent()
                } else {
                    c_text_muted()
                },
            );
        }
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
        if worktree_clicked {
            self.start_worktree_chat(wi);
            sidebar_changed = true;
        } else if plus_clicked {
            if wi != self.conv.active_workspace {
                self.select_workspace(wi);
            }
            self.new_chat();
            sidebar_changed = true;
        } else if response.clicked() && !plus_hovered && !worktree_hovered {
            self.conv.workspaces[wi].sidebar_folded = !folded;
            if let Some(pi) = parent {
                self.conv.workspaces[pi].sidebar_folded = false;
            }
            self.sync_workspaces_to_settings();
        }
        let ws_running = (0..n_sessions).any(|si| self.session_row_is_running(wi, si));
        let worktree = self.conv.workspaces[wi].worktree.clone();
        response.context_menu(|ui| {
            match &worktree {
                None => {
                    if ui
                        .button("New chat in a work tree")
                        .on_hover_text(
                            "Make a new branch in its own folder and chat there, so the agent's changes stay off this checkout until you merge them",
                        )
                        .clicked()
                    {
                        self.start_worktree_chat(wi);
                        ui.close();
                    }
                }
                Some(info) => {
                    let merge = ui
                        .add_enabled(
                            !ws_running,
                            egui::Button::new(format!("Merge into {}", info.main_branch)),
                        )
                        .on_hover_text(
                            "Commit everything in this work tree and merge its branch into the main checkout",
                        );
                    if merge.clicked() {
                        self.merge_worktree(wi);
                        ui.close();
                    }
                    let remove =
                        ui.add_enabled(!ws_running, egui::Button::new("Remove work tree…"));
                    if remove.clicked() {
                        self.request_confirm(crate::app::state::ConfirmAction::RemoveWorktree {
                            wi,
                        });
                        ui.close();
                    }
                }
            }
            let resp = ui.add_enabled(!ws_running, egui::Button::new("Remove workspace"));
            if ws_running {
                resp.on_disabled_hover_text("A chat in this workspace is still running");
            } else if resp.clicked() {
                self.request_confirm(crate::app::state::ConfirmAction::DeleteWorkspace { wi });
            }
        });
        ui.add_space(1.0);
        if sidebar_changed {
            return true;
        }
        // Folding hides the normal chat list. The chat currently open in the main view,
        // in-progress chats, and chats that need attention remain visible.
        let mut visible_sessions = 0usize;
        let mut row_advance: Option<f32> = None;
        let order = sidebar_session_order(&self.conv.workspaces[wi]);
        let mut group_counts = std::collections::HashMap::new();
        for (_, group) in &order {
            *group_counts.entry(*group).or_insert(0usize) += 1;
        }
        let searching = !q.is_empty();
        let mut last_group = None;
        let mut hidden_by_group_fold = false;
        for (si, group) in order {
            if sidebar_changed {
                return true;
            }
            if self.conv.workspaces[wi].sessions.get(si).is_none() {
                return true;
            }
            // While searching, every match is listed and date groups ignore their fold.
            let group_folded = !folded
                && !searching
                && self.conv.workspaces[wi]
                    .folded_groups
                    .iter()
                    .any(|key| key == group.key());
            // Headers of a folded group still show (that is how it unfolds); while searching
            // only groups with a match get one.
            if !folded && !searching && last_group != Some(group) {
                last_group = Some(group);
                if sidebar_group_header(ui, group, group_folded, group_counts[&group]) {
                    let list = &mut self.conv.workspaces[wi].folded_groups;
                    if group_folded {
                        list.retain(|key| key != group.key());
                    } else {
                        list.push(group.key().to_string());
                    }
                    self.sync_workspaces_to_settings();
                    sidebar_changed = true;
                    continue;
                }
            }
            if folded || group_folded {
                let key = self.session_key(wi, si);
                let globally_selected = wi == self.conv.active_workspace && si == active_si;
                let running = self.session_row_is_running(wi, si);
                let needs_attention = self.run_state(key).is_some_and(|run| {
                    run.completion_unseen
                        || (run.pending_approval.is_some()
                            && (key != self.active_session_key()
                                || !self.active_chat_is_visible(ui.ctx())))
                });
                if !globally_selected && !running && !needs_attention {
                    hidden_by_group_fold |= group_folded;
                    continue;
                }
            }
            if searching && !self.session_matches_search(wi, si, q) {
                continue;
            }
            visible_sessions += 1;
            if !folded && searching && last_group != Some(group) {
                last_group = Some(group);
                sidebar_group_header(ui, group, false, group_counts[&group]);
            }
            // Rows are all the same height: once one has been measured, a row scrolled out
            // of view only reserves its space. Long histories otherwise cost a full layout
            // of every row on every frame (~1.5 ms at 400 chats).
            if let Some(advance) = row_advance
                && self.conv.sidebar.renaming_session != Some((wi, si))
                && !ui.is_rect_visible(egui::Rect::from_min_size(
                    ui.cursor().min,
                    egui::vec2(ui.available_width(), advance),
                ))
            {
                ui.add_space(advance);
                continue;
            }
            let row_top = ui.cursor().min.y;
            let session = &self.conv.workspaces[wi].sessions[si];
            let row_title = sidebar_session_title_display(&session.title);
            ui.horizontal(|ui| {
                ui.add_space(7.0);
                ui.vertical(|ui| {
                    let row_w = ui.available_width();
                    // Explicit id: rows scrolled out of view are skipped, and a
                    // `push_id` child's widget ids would shift with every skipped row
                    // (egui salts them with the parent's child count), dropping the
                    // rename field's focus or an open context menu while scrolling.
                    let row_id = egui::Id::new(("sidebar_session_row", wi, si));
                    ui.scope_builder(egui::UiBuilder::new().id(row_id), |ui| {
                        let selected = wi == self.conv.active_workspace && si == active_si;
                        let running = self.session_row_is_running(wi, si);
                        let has_error = !running && self.session_row_has_error(wi, si);
                        let key = self.session_key(wi, si);
                        let run_state = self.run_state(key);
                        let needs_approval = run_state.is_some_and(|run| {
                            run.pending_approval.is_some()
                                && (key != self.active_session_key()
                                    || !self.active_chat_is_visible(ui.ctx()))
                        });
                        let completion_unseen = run_state.is_some_and(|run| run.completion_unseen);
                        let needs_attention = needs_approval || completion_unseen;
                        let title = row_title.clone();
                        const ROW_INNER_H: f32 = 22.0;
                        const ROW_VMARGIN: f32 = 4.0;
                        let row_outer_h = ROW_INNER_H + ROW_VMARGIN * 2.0;
                        let (rect, response) =
                            ui.allocate_exact_size(egui::vec2(row_w, row_outer_h), Sense::click());
                        // Keep the row hovered while the pointer is over an
                        // overlapping action such as the hover-only trash button.
                        let hovered = ui.rect_contains_pointer(rect);
                        if hovered {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                        // Only a pending approval pulses (it blocks the run); an unseen
                        // completion keeps a static highlight. Animate only in a focused
                        // window, and ask for frames only while a pulse is on screen.
                        let attention_pulse = if needs_approval && ui.input(|input| input.focused) {
                            ui.ctx()
                                .request_repaint_after(std::time::Duration::from_millis(50));
                            let time = ui.input(|input| input.time);
                            (0.5 + 0.5 * (time * 3.6).sin()) as f32
                        } else if needs_attention {
                            0.6
                        } else {
                            0.0
                        };
                        let fill = if needs_attention {
                            blend_color(
                                if selected {
                                    c_row_active()
                                } else {
                                    c_info_bg()
                                },
                                c_accent(),
                                0.10 + attention_pulse * 0.14,
                            )
                        } else if selected {
                            c_row_active()
                        } else if hovered {
                            c_row_hover()
                        } else {
                            Color32::TRANSPARENT
                        };
                        ui.painter()
                            .rect_filled(rect, CornerRadius::same(RADIUS_ROW), fill);
                        if needs_attention {
                            ui.painter().rect_stroke(
                                rect,
                                CornerRadius::same(RADIUS_ROW),
                                Stroke::new(
                                    1.0,
                                    c_accent().gamma_multiply(0.45 + attention_pulse * 0.55),
                                ),
                                egui::StrokeKind::Inside,
                            );
                        }
                        if self.conv.sidebar.renaming_session == Some((wi, si)) {
                            let edit =
                                egui::TextEdit::singleline(&mut self.conv.sidebar.rename_draft)
                                    .font(egui::TextStyle::Small)
                                    .desired_width(rect.width() - 8.0);
                            let resp = ui.put(rect.shrink2(egui::vec2(4.0, 2.0)), edit);
                            resp.request_focus();
                            let enter =
                                resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                            let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
                            if enter {
                                let draft = self.conv.sidebar.rename_draft.clone();
                                self.rename_session(si, draft);
                                self.conv.sidebar.renaming_session = None;
                                sidebar_changed = true;
                            } else if escape || (resp.lost_focus() && !enter) {
                                self.conv.sidebar.renaming_session = None;
                            }
                        } else {
                            if response.clicked() {
                                self.select_session_in_workspace(wi, si);
                            }
                            response.context_menu(|ui| {
                                if let Some(file) =
                                    self.conv.workspaces[wi].sessions[si].session_file.clone()
                                {
                                    let pinned = self.conv.workspaces[wi].pinned.contains(&file);
                                    let label = if pinned { "Unpin chat" } else { "Pin chat" };
                                    if ui.button(label).clicked() {
                                        let list = &mut self.conv.workspaces[wi].pinned;
                                        if pinned {
                                            list.retain(|f| f != &file);
                                        } else {
                                            list.push(file);
                                        }
                                        self.sync_workspaces_to_settings();
                                    }
                                }
                                if wi == self.conv.active_workspace && !running {
                                    if ui.button("Rename chat").clicked() {
                                        self.conv.sidebar.renaming_session = Some((wi, si));
                                        self.conv.sidebar.rename_draft =
                                            self.conv.workspaces[wi].sessions[si].title.clone();
                                    }
                                    if ui.button("Export as Markdown…").clicked() {
                                        self.select_session_in_workspace(wi, si);
                                        self.export_active_session_markdown();
                                    }
                                    if ui.button("Delete chat").clicked() {
                                        self.request_confirm(
                                            crate::app::state::ConfirmAction::DeleteSession {
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
                                ui,
                                rect,
                                wi,
                                si,
                                running,
                                has_error,
                                selected,
                                needs_approval,
                                completion_unseen,
                                title,
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
                                    self.request_confirm(
                                        crate::app::state::ConfirmAction::DeleteSession { wi, si },
                                    );
                                }
                            }
                        } // end else not renaming
                    });
                });
            });
            row_advance.get_or_insert(ui.cursor().min.y - row_top);
            if sidebar_changed {
                return true;
            }
        }
        // A folded workspace with no exceptional rows is intentionally just its header;
        // "No chats yet" would otherwise make folding look as if the workspace were empty.
        if visible_sessions == 0 && !hidden_by_group_fold && (!folded || !q.is_empty()) {
            ui.horizontal(|ui| {
                ui.add_space(10.0);
                let msg = if q.is_empty() {
                    "No chats yet"
                } else {
                    "No chats found"
                };
                ui.label(RichText::new(msg).size(FS_TINY).color(c_text_muted()));
                if !q.is_empty() && crate::ui::chrome::ghost_button(ui, "Clear", false).clicked() {
                    self.conv.sidebar.search.clear();
                }
            });
            ui.add_space(4.0);
        }
        sidebar_changed
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_session_row_inner(
        &self,
        ui: &mut Ui,
        rect: egui::Rect,
        wi: usize,
        si: usize,
        running: bool,
        has_error: bool,
        selected: bool,
        needs_approval: bool,
        completion_unseen: bool,
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
            let mut time_w: f32 = if running { 40.0 } else { 34.0 };
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
            // Nudged left off the flush-right edge (~2 monospace chars)
            // so it doesn't sit exactly under the trash icon's center.
            const TEXT_NUDGE: f32 = 7.0;
            // Gap kept between the title's "…" and the time label.
            const TIME_GAP: f32 = 6.0;
            let time_galley = time_label.map(|s| {
                ui.painter().layout_no_wrap(
                    s,
                    FontId::new(FS_TINY, FontFamily::Monospace),
                    c_text_muted(),
                )
            });
            // Long labels ("12m 34s" while running, "120d") are wider than the default
            // slot; widen it so the truncated title stops before the label instead of
            // running underneath it.
            if let Some(galley) = &time_galley {
                time_w = time_w.max(galley.size().x + TEXT_NUDGE + TIME_GAP);
            }
            let fixed = lead_w
                + if running { 0.0 } else { BULLET_GAP }
                + if running { SPINNER_GAP } else { 0.0 }
                + time_w
                + spin_reserve
                + sx * if running { 4.0 } else { 3.0 };
            let title_w = (ui.available_width() - fixed).max(24.0);
            let bullet_col = if has_error {
                c_danger()
            } else if needs_approval || completion_unseen || selected {
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
                            if selected || has_error || needs_approval || completion_unseen {
                                let resp =
                                    ui.label(RichText::new("•").size(FS_SMALL).color(bullet_col));
                                if has_error {
                                    resp.on_hover_text(
                                        "This chat has an error — open it to see details",
                                    );
                                } else if needs_approval {
                                    resp.on_hover_text("This chat needs tool approval");
                                } else if completion_unseen {
                                    resp.on_hover_text("This chat finished while out of focus");
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
                        // `Label::truncate` already provides the complete text on hover.
                        // Adding another tooltip here caused two differently styled tooltip
                        // layers, especially when the hover-only delete action overlapped it.
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
            if let Some(galley) = time_galley {
                let anchor = egui::pos2(inner.right() - TEXT_NUDGE, inner.center().y);
                ui.painter().galley(
                    anchor - egui::vec2(galley.size().x, galley.size().y / 2.0),
                    galley,
                    c_text_muted(),
                );
            }
        });
    }
}

/// Clickable date-group header: chevron, label, and the chat count while folded. Returns
/// whether it was clicked (the caller toggles the fold).
pub(super) fn sidebar_group_header(
    ui: &mut Ui,
    group: SidebarGroup,
    group_folded: bool,
    count: usize,
) -> bool {
    const H: f32 = 20.0;
    ui.add_space(4.0);
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), H), Sense::click());
    let response = response.on_hover_text(if group_folded {
        "Show these chats"
    } else {
        "Hide these chats"
    });
    let hovered = response.hovered();
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        ui.painter()
            .rect_filled(rect, CornerRadius::same(RADIUS_ROW), c_row_hover());
    }
    let color = if hovered {
        c_text_muted()
    } else {
        c_text_faint()
    };
    let chevron_x = rect.left() + 13.0;
    ui.painter().text(
        egui::pos2(chevron_x, rect.center().y),
        egui::Align2::CENTER_CENTER,
        if group_folded {
            ICON_CHEVRON_RIGHT
        } else {
            ICON_ANGLE_DOWN
        },
        FontId::new(FS_TINY - 2.0, icon_font()),
        color,
    );
    let label = if group_folded {
        format!("{}  ·  {count}", group.label())
    } else {
        group.label().to_string()
    };
    ui.painter().text(
        egui::pos2(chevron_x + 10.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        FontId::proportional(FS_TINY),
        color,
    );
    ui.add_space(1.0);
    response.clicked()
}
