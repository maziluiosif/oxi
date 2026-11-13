//! Project and checkout headers, daily chat rows and their context menus.

use super::*;

impl OxiApp {
    pub(super) fn render_sidebar_workspace(
        &mut self,
        ui: &mut Ui,
        wi: usize,
        parent: Option<usize>,
        parent_folded: bool,
        q: &str,
        worktrees: &[usize],
    ) -> bool {
        const FS_WORKSPACE: f32 = FS_SMALL;
        let mut sidebar_changed = false;
        let checkout_row = parent == Some(wi);
        let project_header = parent.is_none();
        let is_main = self.conv.workspaces[wi].worktree.is_none();
        // Work trees need a repository; the general chats folder never is one.
        let can_worktree = project_header
            && is_main
            && !crate::app_dirs::is_general_chats_dir(&self.conv.workspaces[wi].root_path)
            && !self.is_remote_workspace(wi);
        let remote_status = self.remote_status(wi);
        let active_si = self.conv.workspaces[wi].active;
        let n_sessions = self.conv.workspaces[wi].sessions.len();
        let split_partner = self.split_partner_peek();
        let root_label = match &self.conv.workspaces[wi].worktree {
            Some(_) if parent.is_some() => self.sidebar_branch_label(wi),
            Some(info) => crate::app::worktrees::worktree_label(info),
            None if checkout_row => self.sidebar_branch_label(wi),
            None => workspace_sidebar_label(&self.conv.workspaces[wi].root_path),
        };
        let kind = if !is_main {
            "Worktree"
        } else if checkout_row {
            "Checkout"
        } else {
            "Project"
        };
        let searching_all = !q.is_empty() || self.conv.sidebar.history;
        let own_folded = if checkout_row {
            self.conv.workspaces[wi]
                .folded_groups
                .iter()
                .any(|key| key == "checkout")
        } else {
            self.conv.workspaces[wi].sidebar_folded
        };
        let folded = !searching_all && (own_folded || parent_folded);
        ui.add_space(1.0);

        const ROW_H: f32 = 26.0;
        const PLUS_W: f32 = 22.0;
        const GLYPH_W: f32 = 18.0;
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H), Sense::hover());
        let response = ui.interact(
            rect,
            egui::Id::new(("sidebar_workspace_row", wi, checkout_row)),
            Sense::click(),
        );
        response.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Button,
                ui.is_enabled(),
                format!("{kind} {root_label}"),
            )
        });
        // `rect_contains_pointer` instead of `response.hovered()`: the in-place "+"
        // below steals hover from the row response, which would flicker the fill.
        let row_hovered = ui.rect_contains_pointer(rect)
            || response.has_focus()
            || response.context_menu_opened();
        if row_hovered {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            ui.painter()
                .rect_filled(rect, CornerRadius::same(RADIUS_ROW), c_row_hover());
        }
        if !is_main && wi == self.conv.active_workspace && !row_hovered {
            ui.painter()
                .rect_filled(rect, CornerRadius::same(RADIUS_ROW), c_row_active());
        }
        // Leading glyph: branch for worktrees, folder for projects; chevron on hover.
        let remote = self.is_remote_workspace(wi);
        let glyph = match (row_hovered, folded) {
            (false, _) if !is_main || checkout_row => ICON_BRANCH,
            (true, true) => ICON_CHEVRON_RIGHT,
            (true, false) => ICON_ANGLE_DOWN,
            (false, _) if remote => ICON_SERVER,
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
                rect.right() - PLUS_W * if can_worktree { 2.0 } else { 1.0 } - 2.0,
                rect.bottom(),
            ),
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(label_rect), |ui| {
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                // A remote workspace: its folder name, then the host in muted type.
                let mut job = egui::text::LayoutJob::default();
                let (name, host) = match root_label.split_once(" · ") {
                    Some((name, host)) if remote => (name, Some(host)),
                    _ => (root_label.as_str(), None),
                };
                RichText::new(name)
                    .size(FS_WORKSPACE)
                    .color(if remote_status.is_some() {
                        c_text_faint()
                    } else {
                        c_text()
                    })
                    .strong()
                    .append_to(
                        &mut job,
                        ui.style(),
                        egui::FontSelection::Default,
                        Align::Center,
                    );
                if let Some(host) = host {
                    RichText::new(format!("  {host}"))
                        .size(FS_TINY)
                        .color(c_text_faint())
                        .append_to(
                            &mut job,
                            ui.style(),
                            egui::FontSelection::Default,
                            Align::Center,
                        );
                }
                let label = ui.add(
                    egui::Label::new(job)
                        .truncate()
                        .halign(Align::LEFT)
                        .selectable(false),
                );
                if let Some(status) = &remote_status {
                    label.on_hover_text(format!("{status}\nClick the folder row to retry now."));
                }
            });
        });
        crate::ui::chrome::keyboard_focus_ring(ui, &response, CornerRadius::same(RADIUS_ROW));

        let mut worktree_hovered = false;
        if can_worktree {
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
            action.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Button,
                    ui.is_enabled(),
                    format!("New worktree for {root_label}"),
                )
            });
            if action.clicked() {
                self.start_worktree_chat(wi);
                sidebar_changed = true;
            }
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
        if plus_clicked {
            if wi != self.conv.active_workspace {
                self.select_workspace(wi);
            }
            if self.conv.active_workspace == wi {
                self.new_chat();
                sidebar_changed = true;
            }
        } else if response.clicked() && !plus_hovered && !worktree_hovered {
            if remote_status.is_some() {
                self.retry_remote(wi);
            }
            if checkout_row {
                let folds = &mut self.conv.workspaces[wi].folded_groups;
                if folded {
                    folds.retain(|key| key != "checkout");
                } else {
                    folds.push("checkout".into());
                }
            } else {
                self.conv.workspaces[wi].sidebar_folded = !folded;
            }
            if let Some(pi) = parent.filter(|pi| *pi != wi) {
                self.conv.workspaces[pi].sidebar_folded = false;
            }
            self.sync_workspaces_to_settings();
        }
        let ws_running = (0..n_sessions).any(|si| self.session_row_is_running(wi, si));
        let worktree = self.conv.workspaces[wi].worktree.clone();
        let response = if let Some(info) = &worktree {
            response.on_hover_text(format!(
                "Worktree · {}\nProject: {}\nMerge into: {}\nFolder: {}\nRight-click for merge and removal",
                info.branch, info.main_root.display(), info.main_branch,
                self.conv.workspaces[wi].root_path,
            ))
        } else {
            response
        };
        response.context_menu(|ui| {
            sidebar_menu_heading(ui, kind, &root_label, &self.conv.workspaces[wi].root_path);
            if ui.button("New chat here").clicked() {
                self.select_workspace(wi);
                if self.conv.active_workspace == wi {
                    self.new_chat();
                }
                ui.close();
            }
            match &worktree {
                None if !can_worktree => {}
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
                    let has_setup = self
                        .conv
                        .settings
                        .worktree_setup_scripts
                        .contains_key(&self.conv.workspaces[wi].root_path);
                    if ui
                        .button(if has_setup {
                            "Edit work tree setup…"
                        } else {
                            "Set up new work trees…"
                        })
                        .on_hover_text(
                            "A script run in each new work tree before its agent starts, e.g. installing dependencies",
                        )
                        .clicked()
                    {
                        self.edit_worktree_setup(wi);
                        ui.close();
                    }
                    if wi == self.conv.active_workspace
                        && ui
                            .button("New work tree chat side by side")
                            .on_hover_text(
                                "Open a work tree chat next to the current chat, e.g. to give the same task to a second agent",
                            )
                            .clicked()
                    {
                        self.start_worktree_split();
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
            if ui
                .button("Import agent session…")
                .on_hover_text(
                    "Continue a session an agent like Claude Code ran in this folder outside oxi",
                )
                .clicked()
            {
                self.open_session_import(ui.ctx(), wi);
                ui.close();
            }
            // The nested main checkout is part of the project, not a separate workspace.
            if checkout_row {
                return;
            }
            ui.separator();
            let label = if worktree.is_some() { "Close worktree in sidebar" } else { "Remove workspace" };
            let resp = ui.add_enabled(!ws_running, egui::Button::new(label));
            if ws_running {
                resp.on_disabled_hover_text("A chat in this workspace is still running");
            } else if resp.clicked() {
                self.request_confirm(crate::app::state::ConfirmAction::DeleteWorkspace {
                    wi,
                    remove_worktree: false,
                });
                ui.close();
            }
        });
        ui.add_space(1.0);
        if sidebar_changed {
            return true;
        }
        // A repository contains its main checkout and its linked checkouts at one level.
        let has_branch = self
            .conv
            .sidebar
            .branches
            .values
            .get(&self.conv.workspaces[wi].root_path)
            .is_some_and(|branch| branch.is_some());
        if project_header && is_main && (has_branch || !worktrees.is_empty()) {
            // Folding keeps only checkouts with an open, running or attention chat.
            if !folded || self.sidebar_workspace_has_active_chats(wi, ui.ctx()) {
                ui.push_id(("main_checkout", wi), |ui| {
                    ui.horizontal(|ui| {
                        ui.add_space(16.0);
                        ui.vertical(|ui| {
                            sidebar_changed =
                                self.render_sidebar_workspace(ui, wi, Some(wi), folded, q, &[]);
                        });
                    });
                });
            }
            let (visible, other): (Vec<_>, Vec<_>) =
                worktrees.iter().copied().partition(|&child| {
                    searching_all || self.sidebar_workspace_has_recent_chats(child, ui.ctx())
                });
            for child in visible {
                if self.render_sidebar_worktree_child(ui, child, wi, q) {
                    return true;
                }
            }
            if !other.is_empty() && !folded {
                egui::CollapsingHeader::new(format!("Other worktrees · {}", other.len()))
                    .id_salt(("other_worktrees", wi))
                    .show(ui, |ui| {
                        for child in other {
                            if !sidebar_changed {
                                sidebar_changed =
                                    self.render_sidebar_worktree_child(ui, child, wi, q);
                            }
                        }
                    });
            }
            return sidebar_changed;
        }
        // Folding hides the normal chat list. The chat currently open in the main view,
        // in-progress chats, and chats that need attention remain visible.
        let mut visible_sessions = 0usize;
        let mut row_advance: Option<f32> = None;
        let order = sidebar_session_order(&self.conv.workspaces[wi]);
        let searching = !q.is_empty();
        for (si, group) in order {
            if sidebar_changed || self.conv.workspaces[wi].sessions.get(si).is_none() {
                return true;
            }
            let exceptional = self.sidebar_session_needs_visibility(wi, si, ui.ctx());
            if !searching
                && !self.conv.sidebar.history
                && ((!matches!(group, SidebarGroup::Pinned | SidebarGroup::Today) || folded)
                    && !exceptional)
            {
                continue;
            }
            if self.conv.sidebar.history && !self.sidebar_session_in_history_period(wi, si) {
                continue;
            }
            if searching && !self.session_matches_search(wi, si, q) {
                continue;
            }
            visible_sessions += 1;
            // Rows are all the same height: once one has been measured, a row scrolled out
            // of view only reserves its space. Long histories otherwise cost a full layout
            // of every row on every frame (~1.5 ms at 400 chats).
            // Search results show an excerpt under some rows, so they are laid out in full.
            if let Some(advance) = row_advance
                && !searching
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
                        let selected = (wi == self.conv.active_workspace && si == active_si)
                            || split_partner == Some(self.session_key(wi, si));
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
                        response.widget_info(|| {
                            egui::WidgetInfo::selected(
                                egui::WidgetType::SelectableLabel,
                                ui.is_enabled(),
                                selected,
                                &title,
                            )
                        });
                        // Keep the row hovered while the pointer is over an
                        // overlapping action such as the hover-only trash button.
                        let hovered = ui.rect_contains_pointer(rect)
                            || response.has_focus()
                            || response.context_menu_opened();
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
                        crate::ui::chrome::keyboard_focus_ring(
                            ui,
                            &response,
                            CornerRadius::same(RADIUS_ROW),
                        );
                        if selected {
                            let rail = egui::Rect::from_min_max(
                                rect.left_top() + egui::vec2(0.0, 8.0),
                                rect.left_bottom() + egui::vec2(2.0, -8.0),
                            );
                            ui.painter().rect_filled(rail, 1.0, c_accent());
                        }
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
                            match session_rename_field(
                                ui,
                                rect.shrink2(egui::vec2(4.0, 2.0)),
                                &mut self.conv.sidebar.rename_draft,
                            ) {
                                Some(true) => {
                                    let draft = self.conv.sidebar.rename_draft.clone();
                                    self.rename_session(si, draft);
                                    self.conv.sidebar.renaming_session = None;
                                    sidebar_changed = true;
                                }
                                Some(false) => self.conv.sidebar.renaming_session = None,
                                None => {}
                            }
                        } else {
                            if response.clicked() {
                                self.close_subagent(ui.ctx());
                                if split_partner == Some(self.session_key(wi, si)) {
                                    self.focus_split_partner();
                                } else {
                                    self.select_session_in_workspace(wi, si);
                                    // Opened from a search: show where it matched.
                                    if searching && self.content_search_hit(wi, si, q).is_some() {
                                        self.find_in_chat(q);
                                    }
                                }
                            }
                            response.context_menu(|ui| {
                                sidebar_menu_heading(ui, "Chat", &title, &root_label);
                                let row_key = self.session_key(wi, si);
                                if row_key != self.active_session_key()
                                    && ui
                                        .button("Open in split view")
                                        .on_hover_text("Show this chat next to the current one")
                                        .clicked()
                                {
                                    self.open_split_with(row_key);
                                    ui.close();
                                }
                                if let Some(file) =
                                    self.conv.workspaces[wi].sessions[si].session_file.clone()
                                {
                                    let pinned = self.conv.workspaces[wi].pinned.contains(&file);
                                    let label = if pinned { "Unpin chat" } else { "Pin chat" };
                                    if ui.button(label).clicked() {
                                        let ws = &mut self.conv.workspaces[wi];
                                        if pinned {
                                            ws.pinned.retain(|f| f != &file);
                                        } else {
                                            ws.archived.retain(|f| f != &file);
                                            ws.pinned.push(file.clone());
                                        }
                                        self.sync_workspaces_to_settings();
                                        ui.close();
                                    }
                                    let archived =
                                        self.conv.workspaces[wi].archived.contains(&file);
                                    let label = if archived {
                                        "Unarchive chat"
                                    } else {
                                        "Archive chat"
                                    };
                                    if ui
                                        .button(label)
                                        .on_hover_text(
                                            "Archived chats stay in History and search; sending a message brings one back",
                                        )
                                        .clicked()
                                    {
                                        let ws = &mut self.conv.workspaces[wi];
                                        if archived {
                                            ws.archived.retain(|f| f != &file);
                                        } else {
                                            ws.pinned.retain(|f| f != &file);
                                            ws.archived.push(file);
                                        }
                                        self.sync_workspaces_to_settings();
                                        ui.close();
                                    }
                                }
                                if wi == self.conv.active_workspace && !running {
                                    if ui.button("Rename chat").clicked() {
                                        self.conv.sidebar.renaming_session = Some((wi, si));
                                        self.conv.sidebar.rename_draft =
                                            self.conv.workspaces[wi].sessions[si].title.clone();
                                        ui.close();
                                    }
                                    if ui.button("Export as Markdown…").clicked() {
                                        self.select_session_in_workspace(wi, si);
                                        self.export_active_session_markdown();
                                        ui.close();
                                    }
                                    if ui.button("Delete chat").clicked() {
                                        self.request_confirm(
                                            crate::app::state::ConfirmAction::DeleteSession {
                                                wi,
                                                si,
                                                remove_worktree: false,
                                            },
                                        );
                                        ui.close();
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
                                        crate::app::state::ConfirmAction::DeleteSession {
                                            wi,
                                            si,
                                            remove_worktree: false,
                                        },
                                    );
                                }
                            }
                        } // end else not renaming
                    });
                });
            });
            row_advance.get_or_insert(ui.cursor().min.y - row_top);
            if searching && let Some(excerpt) = self.content_search_hit(wi, si, q) {
                let response = ui
                    .horizontal(|ui| {
                        ui.add_space(14.0);
                        ui.add(
                            egui::Label::new(
                                RichText::new(excerpt).size(FS_TINY).color(c_text_muted()),
                            )
                            .truncate()
                            .sense(Sense::click()),
                        )
                        .on_hover_text(excerpt)
                    })
                    .inner;
                if response.clicked() {
                    self.select_session_in_workspace(wi, si);
                    self.find_in_chat(q);
                    return true;
                }
            }
            self.render_sidebar_subagents(ui, self.session_key(wi, si));
            if sidebar_changed {
                return true;
            }
        }
        // A folded workspace with no exceptional rows is intentionally just its header;
        // "No recent chats" would otherwise make folding look as if the workspace were empty.
        // An empty work tree is just its header too: it sits under its repository, where a
        // line per empty work tree only adds noise.
        let empty_worktree = self.conv.workspaces[wi].worktree.is_some() && q.is_empty();
        if visible_sessions == 0 && !empty_worktree && (!folded || !q.is_empty()) {
            ui.horizontal(|ui| {
                ui.add_space(10.0);
                let msg = if q.is_empty() {
                    "No recent chats"
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
                            let pinned = self.conv.workspaces[wi].sessions[si]
                                .session_file
                                .as_ref()
                                .is_some_and(|file| self.conv.workspaces[wi].pinned.contains(file));
                            if pinned && !has_error && !needs_approval && !completion_unseen {
                                ui.label(
                                    RichText::new("\u{f08d}")
                                        .font(FontId::new(FS_TINY, icon_font()))
                                        .color(c_text_muted()),
                                )
                                .on_hover_text("Pinned chat");
                            }
                            if has_error || needs_approval || completion_unseen {
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
                    // A silent agent gets a still warning dot instead of a spinner, so the
                    // sidebar does not claim progress the chat itself would deny.
                    let stall = self.stall_for(self.session_key(wi, si));
                    ui.allocate_ui_with_layout(
                        egui::vec2(spin_reserve, ROW_INNER_H),
                        egui::Layout::left_to_right(Align::Center),
                        |ui| match stall {
                            Some(quiet) => {
                                ui.label(RichText::new("•").size(FS_SMALL).color(c_warning_fg()))
                                    .on_hover_text(format!(
                                        "No updates from the agent for {}",
                                        format_stream_elapsed(quiet)
                                    ));
                                ui.ctx()
                                    .request_repaint_after(std::time::Duration::from_secs(1));
                            }
                            None => small_spinner(ui),
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

/// Inline rename field for a sidebar chat row. `Some(true)` commits on Enter,
/// `Some(false)` cancels on Escape or when focus moves elsewhere.
fn session_rename_field(ui: &mut Ui, rect: egui::Rect, draft: &mut String) -> Option<bool> {
    let edit = egui::TextEdit::singleline(draft)
        .font(egui::TextStyle::Small)
        .desired_width(rect.width() - 8.0);
    let resp = ui.put(rect, edit);
    // Focus only until the field holds it: re-requesting every frame undoes the
    // TextEdit's own surrender on Enter, so `lost_focus` never fired and Enter
    // could not commit.
    if !resp.has_focus() && !resp.lost_focus() {
        resp.request_focus();
    }
    let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    if enter {
        Some(true)
    } else if ui.input(|i| i.key_pressed(egui::Key::Escape)) || resp.lost_focus() {
        Some(false)
    } else {
        None
    }
}

/// Keep the origin visible even when the menu covers the clicked row.
fn sidebar_menu_heading(ui: &mut Ui, kind: &str, title: &str, detail: &str) {
    ui.add(
        egui::Label::new(
            RichText::new(format!("{kind} · {title}"))
                .size(FS_SMALL)
                .color(c_text_muted()),
        )
        .truncate(),
    )
    .on_hover_text(detail);
    ui.separator();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(harness: &mut egui_kittest::Harness<'_, (String, Option<bool>)>, key: egui::Key) {
        harness.key_press(key);
        harness.step();
    }

    fn rename_harness() -> egui_kittest::Harness<'static, (String, Option<bool>)> {
        egui_kittest::Harness::builder()
            .with_size(egui::vec2(240.0, 60.0))
            .build_ui_state(
                |ui, state: &mut (String, Option<bool>)| {
                    if state.1.is_some() {
                        return;
                    }
                    let rect = egui::Rect::from_min_size(ui.cursor().min, egui::vec2(200.0, 24.0));
                    state.1 = session_rename_field(ui, rect, &mut state.0);
                },
                ("Old title".to_string(), None),
            )
    }

    #[test]
    fn rename_field_commits_on_enter() {
        let mut harness = rename_harness();
        harness.run_steps(3);
        harness.event(egui::Event::Text(" 2".into()));
        harness.step();
        press(&mut harness, egui::Key::Enter);
        assert_eq!(harness.state().1, Some(true));
        assert_eq!(harness.state().0, "Old title 2");
    }

    #[test]
    fn rename_field_cancels_on_escape() {
        let mut harness = rename_harness();
        harness.run_steps(3);
        press(&mut harness, egui::Key::Escape);
        assert_eq!(harness.state().1, Some(false));
    }
}
