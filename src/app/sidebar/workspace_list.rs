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
        let mut sidebar_changed = false;
        let checkout_row = parent == Some(wi);
        let project_header = parent.is_none();
        let is_main = self.conv.workspaces[wi].worktree.is_none();
        let root_path = self.conv.workspaces[wi].root_path.clone();
        // Work trees need a repository; the general chats folder never is one.
        let can_worktree = project_header
            && is_main
            && !crate::app_dirs::is_general_chats_dir(&root_path)
            && !self.is_remote_workspace(wi);
        let remote_status = self.remote_status(wi);
        let active_si = self.conv.workspaces[wi].active;
        let n_sessions = self.conv.workspaces[wi].sessions.len();
        let split_partner = self.split_partner_peek();
        let root_label = match &self.conv.workspaces[wi].worktree {
            Some(_) if parent.is_some() => self.sidebar_branch_label(wi),
            Some(info) => crate::app::worktrees::worktree_label(info),
            None if checkout_row => self.sidebar_branch_label(wi),
            None => self.sidebar_project_label(wi),
        };
        let kind = if !is_main {
            "Worktree"
        } else if checkout_row {
            "Checkout"
        } else {
            "Project"
        };
        let searching_all = !q.is_empty() || self.conv.sidebar.history;
        let own_folded = self.sidebar_row_own_folded(wi, checkout_row);
        let folded = !searching_all && (own_folded || parent_folded);
        // The project holding the open chat gets the accent bar; the open checkout or work
        // tree under it gets the strongest branch text.
        let active_project =
            project_header && self.sidebar_top_project(self.conv.active_workspace) == wi;
        let active_checkout = !project_header && wi == self.conv.active_workspace;
        let pinned = project_header && self.project_is_pinned(wi);
        // A repository without work trees shows its branch as a tag on the project row,
        // instead of a nested checkout row that costs a whole level.
        let branch_tag = if project_header && is_main && worktrees.is_empty() {
            self.conv
                .sidebar
                .branches
                .values
                .get(&root_path)
                .cloned()
                .flatten()
        } else {
            None
        };
        // A folded row summarizes the chats it hides: (need you, running).
        let (needs_you, running_hidden) = if folded {
            let hidden = &self.conv.sidebar.hidden_activity;
            std::iter::once(wi)
                .chain(worktrees.iter().copied().filter(|_| project_header))
                .filter_map(|w| hidden.get(&w))
                .fold((0, 0), |(needs, running), &(n, r)| (needs + n, running + r))
        } else {
            (0, 0)
        };
        ui.add_space(1.0);

        const ROW_H: f32 = SIDEBAR_ROW_H;
        const ACTION_W: f32 = 22.0;
        const GLYPH_W: f32 = 18.0;
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H), Sense::hover());
        // Project rows drag to reorder; a click opens the project, the leading chevron folds it.
        let sense = if project_header {
            Sense::click_and_drag()
        } else {
            Sense::click()
        };
        let response = ui.interact(
            rect,
            egui::Id::new(("sidebar_workspace_row", wi, checkout_row)),
            sense,
        );
        response.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Button,
                ui.is_enabled(),
                format!("{kind} {root_label}"),
            )
        });
        let dragging_any = egui::DragAndDrop::has_payload_of_type::<ProjectDrag>(ui.ctx());
        if project_header && response.drag_started() {
            egui::DragAndDrop::set_payload(
                ui.ctx(),
                ProjectDrag {
                    root: root_path.clone(),
                    label: root_label.clone(),
                },
            );
        }
        let dragging_this = egui::DragAndDrop::payload::<ProjectDrag>(ui.ctx())
            .is_some_and(|drag| drag.root == root_path);
        // `rect_contains_pointer` instead of `response.hovered()`: the in-place actions
        // below steal hover from the row response, which would flicker the fill.
        let row_hovered = ui.rect_contains_pointer(rect)
            || response.has_focus()
            || response.context_menu_opened();
        if (row_hovered && !dragging_any) || dragging_this {
            if !dragging_any {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            ui.painter()
                .rect_filled(rect, CornerRadius::same(RADIUS_ROW), c_row_hover());
        }
        if active_project {
            let bar = egui::Rect::from_min_size(
                rect.left_top() + egui::vec2(0.0, 5.0),
                egui::vec2(2.0, rect.height() - 10.0),
            );
            ui.painter()
                .rect_filled(bar, CornerRadius::same(1), c_accent());
        }
        // Leading glyph: branch for checkouts, folder (in the project's color) for projects;
        // on hover it turns into the fold chevron, its own click target so the rest of the row
        // can open the project.
        let remote = self.is_remote_workspace(wi);
        let show_actions = row_hovered && !dragging_any;
        let fold_rect = egui::Rect::from_min_max(
            rect.left_top(),
            egui::pos2(rect.left() + 4.0 + GLYPH_W + 2.0, rect.bottom()),
        );
        let fold_toggle = show_actions.then(|| {
            let toggle = ui.interact(
                fold_rect,
                ui.id().with(("ws_fold", wi, checkout_row)),
                Sense::click(),
            );
            toggle.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Button,
                    ui.is_enabled(),
                    format!("{} {root_label}", if folded { "Unfold" } else { "Fold" }),
                )
            });
            toggle
        });
        let fold_hovered = fold_toggle.as_ref().is_some_and(egui::Response::hovered);
        let glyph = match (show_actions, folded) {
            (false, _) if !is_main || checkout_row => ICON_BRANCH,
            (true, true) => ICON_CHEVRON_RIGHT,
            (true, false) => ICON_ANGLE_DOWN,
            (false, _) if remote => ICON_SERVER,
            (false, true) => ICON_FOLDER,
            (false, false) => ICON_FOLDER_OPEN,
        };
        let glyph_color = if fold_hovered {
            c_text_strong()
        } else if show_actions {
            c_text_muted()
        } else if project_header {
            self.sidebar_project_color(wi)
        } else if active_checkout {
            c_accent()
        } else {
            c_sidebar_section()
        };
        ui.painter().text(
            egui::pos2(rect.left() + 4.0 + GLYPH_W * 0.5, rect.center().y),
            egui::Align2::CENTER_CENTER,
            glyph,
            FontId::new(FS_TINY, icon_font()),
            glyph_color,
        );
        let n_actions = 1 + usize::from(can_worktree) + usize::from(project_header);
        let count_galley = (running_hidden > 1).then(|| {
            ui.painter().layout_no_wrap(
                running_hidden.to_string(),
                FontId::proportional(FS_TINY),
                c_text_muted(),
            )
        });
        // Pinned projects keep a quiet pin at the right edge: with no section labels, it is
        // what sets them apart from the rest.
        let pin_badge = pinned;
        let badge_w = if pin_badge { 14.0 } else { 0.0 }
            + if needs_you > 0 { 12.0 } else { 0.0 }
            + if running_hidden > 0 {
                16.0 + count_galley.as_ref().map_or(0.0, |g| g.size().x + 2.0)
            } else {
                0.0
            };
        let actions_reserve = if show_actions {
            ACTION_W * n_actions as f32 + 2.0
        } else {
            badge_w + 4.0
        };
        let label_rect = egui::Rect::from_min_max(
            egui::pos2(rect.left() + 4.0 + GLYPH_W + 4.0, rect.top()),
            egui::pos2(rect.right() - actions_reserve, rect.bottom()),
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(label_rect), |ui| {
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                let style = ui.style().clone();
                let mut job = egui::text::LayoutJob::default();
                let mut append = |text: RichText| {
                    text.append_to(
                        &mut job,
                        &style,
                        egui::FontSelection::Default,
                        Align::Center,
                    );
                };
                // A remote workspace: its folder name, then the host in muted type.
                let (name, host) = match root_label.split_once(" · ") {
                    Some((name, host)) if remote => (name, Some(host)),
                    _ => (root_label.as_str(), None),
                };
                if project_header {
                    let color = if remote_status.is_some() {
                        c_text_faint()
                    } else if active_project {
                        c_text_strong()
                    } else {
                        c_text()
                    };
                    append(strong_rich(name, FS_SMALL, color));
                } else {
                    // Branches read as code: small monospace, quieter than the project.
                    append(RichText::new(name).font(FontId::monospace(FS_TINY)).color(
                        if active_checkout {
                            c_text_strong()
                        } else {
                            c_text_muted()
                        },
                    ));
                }
                if let Some(branch) = &branch_tag {
                    append(
                        RichText::new(format!("  {branch}"))
                            .font(FontId::monospace(FS_TINY))
                            .color(c_text_faint()),
                    );
                }
                if let Some(host) = host {
                    append(
                        RichText::new(format!("  {host}"))
                            .size(FS_TINY)
                            .color(c_text_faint()),
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

        // Folded summary at the right edge: accent dot for chats that need you, spinner and
        // count for running ones. The hover actions take its place.
        if !show_actions && badge_w > 0.0 {
            let cy = rect.center().y;
            let mut x = rect.right() - 6.0;
            if pin_badge {
                ui.painter().text(
                    egui::pos2(x, cy),
                    egui::Align2::RIGHT_CENTER,
                    ICON_PIN,
                    FontId::new(FS_TINY, icon_font()),
                    c_text_faint(),
                );
                x -= 14.0;
            }
            if needs_you > 0 {
                ui.painter()
                    .circle_filled(egui::pos2(x - 3.5, cy), 3.5, c_accent());
                x -= 12.0;
            }
            if running_hidden > 0 {
                if let Some(galley) = count_galley {
                    let width = galley.size().x;
                    ui.painter().galley(
                        egui::pos2(x - width, cy - galley.size().y * 0.5),
                        galley,
                        c_text_muted(),
                    );
                    x -= width + 2.0;
                }
                paint_spinner(
                    ui,
                    egui::Rect::from_center_size(egui::pos2(x - 6.0, cy), egui::vec2(11.0, 11.0)),
                    c_text_muted(),
                );
            }
        }

        // Hover-only actions at the right edge ("+", work tree, pin), painted and interacted
        // in place (never allocated) so their appearance can't shift the layout.
        let action_rect = |slot: usize| {
            egui::Rect::from_min_max(
                egui::pos2(
                    rect.right() - 2.0 - ACTION_W * (slot + 1) as f32,
                    rect.top(),
                ),
                egui::pos2(rect.right() - 2.0 - ACTION_W * slot as f32, rect.bottom()),
            )
        };
        let mut action_hovered = fold_hovered;
        let fold_clicked = fold_toggle.as_ref().is_some_and(egui::Response::clicked);
        if let Some(toggle) = fold_toggle {
            toggle.on_hover_text(if folded { "Show chats" } else { "Hide chats" });
        }
        let (mut plus_clicked, mut worktree_clicked, mut pin_clicked) = (false, false, false);
        if show_actions {
            let mut slot = 0;
            let plus = row_action(
                ui,
                action_rect(slot),
                ui.id().with(("ws_plus", wi)),
                ICON_PLUS,
                false,
                &format!("New chat in {root_label}"),
                "New chat in this workspace (Cmd/Ctrl+N)",
            );
            slot += 1;
            action_hovered |= plus.hovered();
            plus_clicked = plus.clicked();
            if can_worktree {
                let action = row_action(
                    ui,
                    action_rect(slot),
                    ui.id().with(("ws_worktree", wi)),
                    ICON_BRANCH,
                    false,
                    &format!("New worktree for {root_label}"),
                    "New worktree chat — create an isolated branch and start chatting",
                );
                slot += 1;
                action_hovered |= action.hovered();
                worktree_clicked = action.clicked();
            }
            if project_header {
                let pin = row_action(
                    ui,
                    action_rect(slot),
                    ui.id().with(("ws_pin", wi)),
                    ICON_PIN,
                    pinned,
                    &format!("{} {root_label}", if pinned { "Unpin" } else { "Pin" }),
                    if pinned {
                        "Unpin project"
                    } else {
                        "Pin project to the top of the sidebar"
                    },
                );
                action_hovered |= pin.hovered();
                pin_clicked = pin.clicked();
            }
        }
        if plus_clicked {
            if wi != self.conv.active_workspace {
                self.select_workspace(wi);
            }
            if self.conv.active_workspace == wi {
                self.new_chat();
                sidebar_changed = true;
            }
        } else if worktree_clicked {
            self.start_worktree_chat(wi);
            sidebar_changed = true;
        } else if pin_clicked {
            self.set_project_pinned(wi, !pinned);
            sidebar_changed = true;
        } else if response.clicked()
            && !action_hovered
            && !self.sidebar_row_is_open(wi, project_header)
        {
            if remote_status.is_some() {
                self.retry_remote(wi);
            }
            // Opening a project goes to its last chat; a work tree or checkout row opens that
            // checkout. Panels that follow the open project (files, source control) follow.
            if project_header {
                self.switch_to_project(wi);
            } else {
                self.select_workspace(wi);
                if self.conv.active_workspace == wi {
                    self.reveal_chat_view();
                    self.focus_active_view_next_frame();
                }
            }
            sidebar_changed = true;
        } else if fold_clicked || (response.clicked() && !action_hovered) {
            // The chevron folds; so does clicking the row that is already open.
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
        // Dropping another project here puts it above this one, or below a folded one when
        // released over its lower half (an unfolded project's chats follow its row).
        if project_header && !dragging_this && !sidebar_changed {
            let after = folded
                && ui
                    .ctx()
                    .pointer_interact_pos()
                    .is_some_and(|pos| pos.y > rect.center().y);
            let (line_y, slot) = if after {
                (rect.bottom() + 0.5, ProjectSlot::After(root_path.clone()))
            } else {
                (rect.top() - 0.5, ProjectSlot::Before(root_path.clone()))
            };
            if self.project_drop_target(ui, &response, line_y, pinned, slot) {
                return true;
            }
        }
        let ws_running = (0..n_sessions).any(|si| self.session_row_is_running(wi, si));
        let worktree = self.conv.workspaces[wi].worktree.clone();
        let mut tip = match &worktree {
            Some(info) => format!(
                "Worktree · {}\nProject: {}\nMerge into: {}\nFolder: {}\nRight-click for merge and removal",
                info.branch,
                info.main_root.display(),
                info.main_branch,
                root_path,
            ),
            None if project_header => root_path.clone(),
            None => String::new(),
        };
        if needs_you + running_hidden > 0 {
            let mut parts = Vec::new();
            if needs_you > 0 {
                parts.push(format!("{needs_you} need you"));
            }
            if running_hidden > 0 {
                parts.push(format!("{running_hidden} running"));
            }
            tip.push_str(&format!("\n{}", parts.join(" · ")));
        }
        if project_header {
            tip.push_str("\nClick to open · drag to reorder · right-click to pin or move");
        }
        let response = if tip.is_empty() || dragging_any {
            response
        } else {
            response.on_hover_text(tip)
        };
        response.context_menu(|ui| {
            sidebar_menu_heading(ui, kind, &root_label, &root_path);
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
                        .contains_key(&root_path);
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
            if project_header {
                ui.separator();
                if ui
                    .button(if pinned { "Unpin project" } else { "Pin project" })
                    .on_hover_text("Pinned projects stay in their own section at the top")
                    .clicked()
                {
                    self.set_project_pinned(wi, !pinned);
                    ui.close();
                }
                let (prev, next) = self.project_neighbours(wi);
                let moves = [
                    ("Move up", prev.clone().map(ProjectSlot::Before)),
                    ("Move down", next.map(ProjectSlot::After)),
                    ("Move to top", prev.map(|_| ProjectSlot::Start)),
                ];
                for (label, slot) in moves {
                    let enabled = slot.is_some();
                    if ui.add_enabled(enabled, egui::Button::new(label)).clicked()
                        && let Some(slot) = slot
                    {
                        self.place_project(&root_path, pinned, slot);
                        ui.close();
                    }
                }
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
        // A repository with work trees lists its main checkout and its linked checkouts at one
        // level; without work trees its chats sit right under the project row.
        if project_header && is_main && !worktrees.is_empty() {
            // Folding keeps only checkouts with the open chat.
            if !folded || self.sidebar_workspace_has_kept_chats(wi) {
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
                let id = super::stepping::other_worktrees_id(wi);
                let mut open = ui.data_mut(|d| *d.get_persisted_mut_or_insert_with(id, || false));
                if sidebar_quiet_link(
                    ui,
                    &format!("Other worktrees · {}", other.len()),
                    Some(open),
                )
                .clicked()
                {
                    open = !open;
                    ui.data_mut(|d| d.insert_persisted(id, open));
                }
                if open {
                    for child in other {
                        if !sidebar_changed {
                            sidebar_changed = self.render_sidebar_worktree_child(ui, child, wi, q);
                        }
                    }
                }
            }
            return sidebar_changed;
        }
        // Folding hides the normal chat list; the chat currently open in the main view stays.
        // Unfolded, in-progress chats and chats that need attention stay visible too.
        let mut visible_sessions = 0usize;
        let mut hidden_older_sessions = 0usize;
        let mut row_advance: Option<f32> = None;
        let order = sidebar_session_order(&self.conv.workspaces[wi]);
        let searching = !q.is_empty();
        for (si, group) in order {
            if sidebar_changed || self.conv.workspaces[wi].sessions.get(si).is_none() {
                return true;
            }
            match self.sidebar_session_listing(wi, si, group, folded, q, ui.ctx()) {
                Listing::Listed => {}
                Listing::Older => {
                    hidden_older_sessions += 1;
                    continue;
                }
                Listing::Hidden => continue,
            }
            visible_sessions += 1;
            #[cfg(test)]
            self.conv
                .sidebar
                .drawn_stops
                .push(super::stepping::SidebarStop::Chat(self.session_key(wi, si)));
            // A shortcut just opened this chat: lay it out even off screen, to scroll to it.
            let reveal = self.conv.sidebar.reveal_active
                && wi == self.conv.active_workspace
                && si == active_si;
            // Rows are all the same height: once one has been measured, a row scrolled out
            // of view only reserves its space. Long histories otherwise cost a full layout
            // of every row on every frame (~1.5 ms at 400 chats).
            // Search results show an excerpt under some rows, so they are laid out in full.
            if let Some(advance) = row_advance
                && !searching
                && !reveal
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
                        const ROW_VMARGIN: f32 = 2.0;
                        let row_outer_h = ROW_INNER_H + ROW_VMARGIN * 2.0;
                        let (rect, response) =
                            ui.allocate_exact_size(egui::vec2(row_w, row_outer_h), Sense::click());
                        if reveal {
                            ui.scroll_to_rect(rect, None);
                            self.conv.sidebar.reveal_active = false;
                        }
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
                        let radius = CornerRadius::same(RADIUS_ROW);
                        ui.painter().rect_filled(rect, radius, fill);
                        // Selection is a fill plus a short accent bar, never bold type,
                        // so the open chat can't be mistaken for a project or branch header.
                        if selected {
                            paint_selection_bar(ui, rect);
                        }
                        crate::ui::chrome::keyboard_focus_ring(ui, &response, radius);
                        if needs_attention {
                            ui.painter().rect_stroke(
                                rect,
                                radius,
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
                                        c_danger()
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
                            egui::Label::new(search_highlight_job(
                                excerpt,
                                q,
                                FontId::proportional(FS_TINY),
                                c_text_muted(),
                            ))
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
        if hidden_older_sessions > 0 && !folded && !searching && !self.conv.sidebar.history {
            let label = format!(
                "Show {hidden_older_sessions} older chat{}…",
                if hidden_older_sessions == 1 { "" } else { "s" }
            );
            if sidebar_quiet_link(ui, &label, None)
                .on_hover_text("Open History filtered to this project")
                .clicked()
            {
                self.conv.sidebar.history = true;
                let project_root = match parent {
                    Some(pi) => self.conv.workspaces[pi].root_path.clone(),
                    None => self.conv.workspaces[wi].root_path.clone(),
                };
                self.conv.sidebar.history_project = Some(project_root);
                self.conv.sidebar.history_days = None;
            }
            ui.add_space(2.0);
        }
        // A project with no chats to list is just its header: its open/closed folder already
        // says whether it is unfolded. Only an empty search explains itself, with a way out.
        if visible_sessions == 0 && hidden_older_sessions == 0 && searching {
            ui.horizontal(|ui| {
                ui.add_space(10.0);
                ui.label(
                    RichText::new("No chats found")
                        .size(FS_TINY)
                        .color(c_text_muted()),
                );
                if crate::ui::chrome::ghost_button(ui, "Clear", false).clicked() {
                    self.conv.sidebar.search.clear();
                    self.conv.sidebar.search_open = false;
                }
            });
            ui.add_space(4.0);
        }
        sidebar_changed
    }

    /// Whether clicking this project, checkout or work tree row would land where you already
    /// are: the open chat is in the project (or one of its work trees), or in that checkout.
    fn sidebar_row_is_open(&self, wi: usize, project_header: bool) -> bool {
        if project_header {
            self.sidebar_top_project(self.conv.active_workspace) == wi
        } else {
            self.conv.active_workspace == wi
        }
    }

    /// Every project holds at least one chat, so one with none saved gets an unsaved, empty
    /// "New chat" stand-in. Listing it under every project only adds a row that opens an empty
    /// chat; it shows once it is the open chat, or once it has a draft or messages.
    pub(in crate::app) fn sidebar_hides_placeholder(&self, wi: usize, si: usize) -> bool {
        let session = &self.conv.workspaces[wi].sessions[si];
        let untouched = session.session_file.is_none()
            && session.messages.is_empty()
            && session.input_text.trim().is_empty()
            && session.pending_images.is_empty()
            && session.pending_texts.is_empty();
        if !untouched {
            return false;
        }
        let key = self.session_key(wi, si);
        let open = key == self.active_session_key() || self.split_partner_peek() == Some(key);
        let busy = self
            .run_state(key)
            .is_some_and(|state| state.waiting_response);
        !open && !busy
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
        const ROW_VMARGIN: f32 = 2.0;
        const BULLET_GAP: f32 = 4.0;
        const SPINNER_GAP: f32 = 4.0;

        let inner = rect.shrink2(egui::vec2(4.0, ROW_VMARGIN));
        ui.scope_builder(egui::UiBuilder::new().max_rect(inner), |ui| {
            ui.set_min_width(inner.width());
            let lead_w = if running { 0.0 } else { 10.0 };
            let mut time_w: f32 = if running { 40.0 } else { 32.0 };
            let spin_reserve = if running { 14.0 } else { 0.0 };
            let sx = ui.spacing().item_spacing.x;
            // Space is always reserved for the time label so the title never
            // reflows; when hidden the delete button is painted over that
            // same slot instead.
            let ws = &self.conv.workspaces[wi];
            // A pinned chat is kept on purpose; its age ("328d") is only noise.
            let pinned = ws.sessions[si]
                .session_file
                .as_ref()
                .is_some_and(|file| ws.pinned.contains(file));
            let time_label = if hide_time || (pinned && !running) {
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
                ui.painter()
                    .layout_no_wrap(s, FontId::proportional(FS_TINY), c_text_faint())
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
                            if pinned && !has_error && !needs_approval && !completion_unseen {
                                ui.label(
                                    RichText::new(ICON_PIN)
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
                        let q = self.conv.sidebar.search.trim();
                        // `Label::truncate` already provides the complete text on hover.
                        // Adding another tooltip here caused two differently styled tooltip
                        // layers, especially when the hover-only delete action overlapped it.
                        if !q.is_empty() && find_ignore_case(&title, q).is_some() {
                            let (font, color) = if selected {
                                (FontId::proportional(FS_SMALL), c_text_strong())
                            } else {
                                (FontId::proportional(FS_SMALL), c_text_muted())
                            };
                            let job = search_highlight_job(&title, q, font, color);
                            ui.add(Label::new(job).truncate().halign(Align::LEFT));
                        } else {
                            let title_text =
                                RichText::new(title.as_str())
                                    .size(FS_SMALL)
                                    .color(if selected {
                                        c_text_strong()
                                    } else {
                                        c_text_muted()
                                    });
                            ui.add(Label::new(title_text).truncate().halign(Align::LEFT));
                        }
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

/// A quiet link row under a project's chats ("Show 4 older chats…", "Other worktrees · 2"):
/// small faint text that only brightens on hover, so it never reads as a section header.
/// `open` adds a disclosure chevron.
fn sidebar_quiet_link(ui: &mut Ui, label: &str, open: Option<bool>) -> egui::Response {
    const INDENT: f32 = 12.0;
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 20.0), Sense::click());
    response.widget_info(|| match open {
        Some(open) => egui::WidgetInfo::selected(
            egui::WidgetType::CollapsingHeader,
            ui.is_enabled(),
            open,
            label,
        ),
        None => egui::WidgetInfo::labeled(egui::WidgetType::Link, ui.is_enabled(), label),
    });
    let hovered = response.hovered() || response.has_focus();
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    crate::ui::chrome::keyboard_focus_ring(ui, &response, CornerRadius::same(RADIUS_ROW));
    let ink = if hovered {
        c_text_muted()
    } else {
        c_text_faint()
    };
    let mut x = rect.left() + INDENT;
    if let Some(open) = open {
        ui.painter().text(
            egui::pos2(x, rect.center().y),
            egui::Align2::LEFT_CENTER,
            if open {
                ICON_ANGLE_DOWN
            } else {
                ICON_CHEVRON_RIGHT
            },
            FontId::new(FS_TINY - 2.5, icon_font()),
            ink,
        );
        x += 11.0;
    }
    ui.painter().text(
        egui::pos2(x, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        FontId::proportional(FS_TINY),
        ink,
    );
    response
}

/// Inline rename field for a sidebar chat row. `Some(true)` commits on Enter,
/// `Some(false)` cancels on Escape or when focus moves elsewhere.
fn session_rename_field(ui: &mut Ui, rect: egui::Rect, draft: &mut String) -> Option<bool> {
    let resp = ui
        .scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
            egui::Frame::new()
                .fill(c_bg_input())
                .stroke(Stroke::new(1.0, c_composer_focus_border()))
                .corner_radius(RADIUS_ROW)
                .inner_margin(egui::Margin::symmetric(6, 2))
                .show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::singleline(draft)
                            .frame(egui::Frame::NONE)
                            .font(FontId::proportional(FS_SMALL))
                            .desired_width((rect.width() - 14.0).max(24.0)),
                    )
                })
                .inner
        })
        .inner;
    // Focus only until the field holds it: re-requesting every frame undoes the
    // TextEdit's own surrender on Enter, so `lost_focus` never fired and Enter
    // could not commit.
    if !resp.has_focus() && !resp.lost_focus() {
        resp.request_focus();
    }
    let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
    if !(enter || escape || resp.lost_focus()) {
        return None;
    }
    // Take the key the field let go on, or the approval card drawn later this frame reads
    // it as Approve / Deny.
    ui.input_mut(|i| {
        i.consume_key(egui::Modifiers::NONE, egui::Key::Enter);
        i.consume_key(egui::Modifiers::NONE, egui::Key::Escape);
    });
    Some(enter)
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

    /// What the approval card, drawn after the sidebar, would see of the key that ended the
    /// rename: it answers only when no field has focus.
    #[test]
    fn the_key_that_ends_a_rename_does_not_reach_widgets_drawn_later() {
        for key in [egui::Key::Enter, egui::Key::Escape] {
            let mut harness = egui_kittest::Harness::builder()
                .with_size(egui::vec2(240.0, 60.0))
                .build_ui_state(
                    |ui, state: &mut (String, Option<bool>, bool)| {
                        if state.1.is_none() {
                            let rect =
                                egui::Rect::from_min_size(ui.cursor().min, egui::vec2(200.0, 24.0));
                            state.1 = session_rename_field(ui, rect, &mut state.0);
                        }
                        let free = !ui.ctx().egui_wants_keyboard_input();
                        let pressed = ui.input(|i| {
                            i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Escape)
                        });
                        state.2 |= free && pressed;
                    },
                    ("Old title".to_string(), None, false),
                );
            harness.run_steps(3);
            harness.key_press(key);
            harness.step();
            assert!(harness.state().1.is_some(), "{key:?} should end the rename");
            assert!(!harness.state().2, "{key:?} leaked past the rename field");
        }
    }
}

/// Byte range of the first case-insensitive occurrence of `needle` in `haystack`, on char
/// boundaries of `haystack` (lowercasing can change byte lengths, so no index arithmetic on a
/// lowercased copy).
pub(super) fn find_ignore_case(haystack: &str, needle: &str) -> Option<std::ops::Range<usize>> {
    if needle.is_empty() {
        return None;
    }
    let lower = |c: char| c.to_lowercase();
    haystack.char_indices().find_map(|(start, _)| {
        let mut rest = haystack[start..].char_indices();
        let mut end = start;
        let mut wanted = needle.chars().flat_map(lower);
        let mut pending = None::<char>;
        loop {
            let next_wanted = pending.take().or_else(|| wanted.next());
            let Some(w) = next_wanted else {
                return Some(start..end);
            };
            let (offset, c) = rest.next()?;
            let mut got = lower(c);
            if got.next() != Some(w) {
                return None;
            }
            // Multi-char lowercase forms must match the following needle chars too.
            for g in got {
                if wanted.next() != Some(g) {
                    return None;
                }
            }
            end = start + offset + c.len_utf8();
        }
    })
}

/// `text` laid out in `font`/`color`, with every case-insensitive match of `query` marked like
/// the project search marks its hits.
pub(super) fn search_highlight_job(
    text: &str,
    query: &str,
    font: FontId,
    color: Color32,
) -> egui::text::LayoutJob {
    let plain = egui::text::TextFormat::simple(font.clone(), color);
    let mut marked = egui::text::TextFormat::simple(font, c_text_strong());
    marked.background = crate::theme::blend_color(c_bg_sidebar(), c_warning_fg(), 0.38);
    let mut job = egui::text::LayoutJob::default();
    let mut rest = 0;
    while let Some(hit) = find_ignore_case(&text[rest..], query) {
        let (start, end) = (rest + hit.start, rest + hit.end);
        if end == start {
            break;
        }
        job.append(&text[rest..start], 0.0, plain.clone());
        job.append(&text[start..end], 0.0, marked.clone());
        rest = end;
    }
    job.append(&text[rest..], 0.0, plain);
    job
}

#[cfg(test)]
mod search_highlight_tests {
    use super::find_ignore_case;

    #[test]
    fn finds_case_insensitive_ranges_on_original_boundaries() {
        assert_eq!(find_ignore_case("Add CSV export", "csv"), Some(4..7));
        assert_eq!(find_ignore_case("Ștergere ȘIR", "șir"), Some(10..14));
        assert_eq!(find_ignore_case("abc", "abcd"), None);
        assert_eq!(find_ignore_case("abc", ""), None);
        assert_eq!(find_ignore_case("xx İstanbul", "i̇st"), Some(3..7));
    }
}

/// A hover-only icon action on a sidebar row, painted and interacted in place.
fn row_action(
    ui: &mut Ui,
    rect: egui::Rect,
    id: egui::Id,
    icon: &str,
    on: bool,
    label: &str,
    tooltip: &str,
) -> egui::Response {
    let response = ui.interact(rect, id, Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
    });
    let hovered = response.hovered();
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        icon,
        FontId::new(FS_TINY, icon_font()),
        if hovered || on {
            c_accent()
        } else {
            c_text_faint()
        },
    );
    response.on_hover_text(tooltip)
}

/// The selected chat's marker: a 2px accent bar inset at the row's left edge.
pub(super) fn paint_selection_bar(ui: &Ui, rect: egui::Rect) {
    let bar = egui::Rect::from_min_size(
        rect.left_top() + egui::vec2(1.0, 6.0),
        egui::vec2(2.0, (rect.height() - 12.0).max(4.0)),
    );
    ui.painter()
        .rect_filled(bar, CornerRadius::same(1), c_accent());
}
