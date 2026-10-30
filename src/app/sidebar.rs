//! Sidebar: workspace list, session rows, search.

mod workspace_list;

use eframe::egui::scroll_area::ScrollBarVisibility;
use eframe::egui::{
    self, Align, Color32, CornerRadius, FontFamily, FontId, Layout, RichText, ScrollArea, Sense,
    Stroke, Ui,
};

use crate::theme::*;
use crate::ui::chrome::sidebar_text_field;

use super::OxiApp;

impl OxiApp {
    /// Sidebar list and controls.
    pub(crate) fn render_sidebar(&mut self, ui: &mut Ui) {
        ui.set_min_width(ui.max_rect().width());

        // Primary action first, with its shortcut, like other chat apps.
        let new_chat = crate::ui::chrome::flat_button_icon(
            ui,
            ICON_PLUS,
            "New chat",
            FS_SMALL,
            egui::vec2(ui.available_width(), 30.0),
            c_text(),
        )
        .on_hover_text("Start a new chat in the active workspace");
        if new_chat.rect.width() >= 170.0 {
            ui.painter().text(
                new_chat.rect.right_center() - egui::vec2(8.0, 0.0),
                egui::Align2::RIGHT_CENTER,
                if cfg!(target_os = "macos") {
                    "⌘N"
                } else {
                    "Ctrl+N"
                },
                FontId::proportional(FS_TINY),
                c_text_faint(),
            );
        }
        if new_chat.clicked() {
            self.new_chat();
        }
        new_chat.context_menu(|ui| {
            if ui
                .button("New general chat")
                .on_hover_text("A chat that isn't about any project, run in ~/oxi-general")
                .clicked()
            {
                self.new_general_chat();
            }
            if ui
                .button("New chat side by side")
                .on_hover_text("Open a new chat next to this one, e.g. to ask a second agent")
                .clicked()
            {
                self.new_chat_split();
            }
            if ui
                .button("New work tree chat side by side")
                .on_hover_text(
                    "Start the same project in a work tree next to this chat, e.g. to give the task to a second agent",
                )
                .clicked()
            {
                self.start_worktree_split();
            }
        });
        ui.add_space(SPACE_SM);

        // Search row + add-workspace button.
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.set_height(28.0);

            let add_w = 22.0;
            let general_w = 22.0;
            let remote_w = 22.0;
            let clear_w = if self.conv.sidebar.search.is_empty() {
                0.0
            } else {
                22.0
            };
            let search_w = (ui.available_width()
                - add_w
                - general_w
                - remote_w
                - clear_w
                - ui.spacing().item_spacing.x * if clear_w > 0.0 { 4.0 } else { 3.0 })
            .max(48.0);
            ui.allocate_ui_with_layout(
                egui::vec2(search_w, 28.0),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.set_width(search_w);
                    let _ = sidebar_text_field(ui, &mut self.conv.sidebar.search, "Search chats…");
                },
            );

            if clear_w > 0.0
                && crate::ui::chrome::icon_button_plain(ui, ICON_CLOSE, clear_w, false)
                    .on_hover_text("Clear chat search")
                    .clicked()
            {
                self.conv.sidebar.search.clear();
            }

            if crate::ui::chrome::icon_button_plain(ui, ICON_GLOBE, general_w, false)
                .on_hover_text("New general chat: not about any project, run in ~/oxi-general")
                .clicked()
            {
                self.new_general_chat();
            }

            if crate::ui::chrome::icon_button_plain(ui, ICON_SERVER, remote_w, false)
                .on_hover_text(
                    "Connect to a remote host: open a folder on another machine over SSH; \
                     its oxi server runs the agents there.",
                )
                .clicked()
            {
                self.open_remote_dialog();
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

        ui.add_space(SPACE_MD);

        if let Some(notice) = self.conv.sidebar.notice.clone() {
            if crate::ui::chrome::dismissible_notice(ui, "sidebar_notice", &notice) {
                self.conv.sidebar.notice = None;
            }
            ui.add_space(8.0);
        }

        ScrollArea::vertical()
            .id_salt("sidebar_main_scroll")
            .auto_shrink([false, false])
            .scroll_bar_visibility(ScrollBarVisibility::VisibleWhenNeeded)
            .show(ui, |ui| {
                self.render_sidebar_session_list(ui);
            });

        // The outer sidebar allocation already owns the full fixed width/height. Expanding
        // again from content made Conversations report a subtly different size than Explorer.
    }

    fn render_sidebar_session_list(&mut self, ui: &mut Ui) {
        // Workspace headers sit a step above the chat rows' default size, matching
        // the app-wide type scale (theme.rs) so it still tracks the UI density zoom.
        let q = self.conv.sidebar.search.trim().to_lowercase();
        let mut sidebar_changed = false;

        if self.conv.no_workspace && self.conv.workspaces.len() == 1 {
            ui.add_space(12.0);
            ui.label(
                egui::RichText::new("No workspaces. Open a folder to start.")
                    .size(FS_SMALL)
                    .color(c_text_muted()),
            );
            ui.add_space(6.0);
            if ui.button("Open folder…").clicked() {
                self.open_workspace_folder();
            }
            if ui
                .button("Start a general chat")
                .on_hover_text("A chat that isn't about any project, run in ~/oxi-general")
                .clicked()
            {
                self.new_general_chat();
            }
            return;
        }
        if q.is_empty() && self.render_sidebar_running(ui) {
            return;
        }
        let order = sidebar_workspace_order(&self.conv.workspaces);
        for &(wi, parent) in &order {
            if sidebar_changed {
                return;
            }
            // Linked checkouts are drawn by their parent, right under its header.
            if parent.is_some() || (self.conv.no_workspace && wi == 0) {
                continue;
            }
            let children: Vec<usize> = order
                .iter()
                .filter(|(_, p)| *p == Some(wi))
                .map(|&(child, _)| child)
                .collect();
            sidebar_changed = self.render_sidebar_workspace(ui, wi, None, false, &q, &children);
        }
    }

    /// Chats working (or waiting for approval) in any workspace, above the workspaces, so work
    /// running elsewhere stays one click away. Shown when it adds something: two or more of
    /// them, or one outside the open workspace. Returns whether the selection changed.
    fn render_sidebar_running(&mut self, ui: &mut Ui) -> bool {
        let mut chats = Vec::new();
        for (wi, workspace) in self.conv.workspaces.iter().enumerate() {
            for si in 0..workspace.sessions.len() {
                let approval = self
                    .run_state(self.session_key(wi, si))
                    .is_some_and(|run| run.pending_approval.is_some());
                if approval || self.session_row_is_running(wi, si) {
                    chats.push((wi, si, approval));
                }
            }
        }
        let active = self.conv.active_workspace;
        if chats.len() < 2 && chats.iter().all(|&(wi, ..)| wi == active) {
            return false;
        }
        ui.add_space(4.0);
        ui.label(
            RichText::new(format!("Running · {}", chats.len()))
                .size(FS_TINY)
                .color(c_text_muted()),
        );
        let mut open = None;
        for &(wi, si, approval) in &chats {
            let title = sidebar_session_title_display(&self.conv.workspaces[wi].sessions[si].title);
            let place = std::path::Path::new(&self.conv.workspaces[wi].root_path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let selected = wi == active && si == self.conv.workspaces[wi].active;
            let response = ui
                .horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    ui.add_space(4.0);
                    if approval {
                        ui.label(
                            RichText::new(ICON_WARNING)
                                .font(FontId::new(FS_TINY, icon_font()))
                                .color(c_accent()),
                        )
                        .on_hover_text("Waiting for your approval");
                    } else {
                        small_spinner(ui);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(&place).size(FS_TINY).color(c_text_faint()));
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&title).size(FS_SMALL).color(if selected {
                                        c_text_strong()
                                    } else {
                                        c_text()
                                    }),
                                )
                                .truncate()
                                .sense(Sense::click()),
                            )
                        })
                        .inner
                    })
                    .inner
                })
                .inner
                .on_hover_text(format!("{title} — {place}"));
            if response.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if response.clicked() {
                open = Some((wi, si));
            }
        }
        ui.add_space(6.0);
        if let Some((wi, si)) = open {
            self.close_subagent(ui.ctx());
            self.select_session_in_workspace(wi, si);
            return true;
        }
        false
    }

    /// Linked checkout `wi` of workspace `parent`, indented under it with a tree guide.
    fn render_sidebar_worktree_child(
        &mut self,
        ui: &mut egui::Ui,
        wi: usize,
        parent: usize,
        q: &str,
    ) -> bool {
        let parent_folded = self.conv.workspaces[parent].sidebar_folded && q.is_empty();
        if parent_folded && !self.sidebar_workspace_needs_visibility(wi, ui.ctx()) {
            return false;
        }
        let mut sidebar_changed = false;
        ui.horizontal(|ui| {
            let (guide, _) = ui.allocate_exact_size(egui::vec2(16.0, 22.0), Sense::hover());
            ui.painter().line_segment(
                [
                    guide.left_top() + egui::vec2(8.0, 0.0),
                    guide.left_bottom() + egui::vec2(8.0, 0.0),
                ],
                Stroke::new(1.0, c_border_subtle()),
            );
            ui.painter().line_segment(
                [
                    guide.left_center() + egui::vec2(8.0, 0.0),
                    guide.right_center(),
                ],
                Stroke::new(1.0, c_border_subtle()),
            );
            ui.vertical(|ui| {
                sidebar_changed =
                    self.render_sidebar_workspace(ui, wi, Some(parent), parent_folded, q, &[]);
            });
        });
        sidebar_changed
    }

    fn sidebar_workspace_needs_visibility(&self, wi: usize, ctx: &egui::Context) -> bool {
        wi == self.conv.active_workspace
            || (0..self.conv.workspaces[wi].sessions.len()).any(|si| {
                let key = self.session_key(wi, si);
                self.session_row_is_running(wi, si)
                    || self.run_state(key).is_some_and(|run| {
                        run.completion_unseen
                            || (run.pending_approval.is_some()
                                && (key != self.active_session_key()
                                    || !self.active_chat_is_visible(ctx)))
                    })
            })
    }

    /// Whether a chat's title or loaded messages contain `query` (already lowercased). The
    /// answer is cached per chat until the query or the chat changes: lowercasing every
    /// message of every loaded chat on each frame made typing in the search box crawl once a
    /// long conversation was open.
    fn session_matches_search(&mut self, wi: usize, si: usize, query: &str) -> bool {
        let session = &self.conv.workspaces[wi].sessions[si];
        let stamp = (
            session.messages.len(),
            session.messages.last().map_or(0, |m| {
                m.text.len()
                    + m.blocks
                        .iter()
                        .map(|b| match b {
                            crate::model::AssistantBlock::Answer(t)
                            | crate::model::AssistantBlock::Thinking(t) => t.len(),
                            crate::model::AssistantBlock::Tool { output, .. } => output.len(),
                        })
                        .sum::<usize>()
            }),
        );
        // Found in its file by the background search (chats not loaded yet included).
        if self.content_search_hit(wi, si, query).is_some() {
            return true;
        }
        if let Some(cached) = self.conv.sidebar.search_cache.get(&(wi, si))
            && cached.query == query
            && cached.title == session.title
            && cached.stamp == stamp
        {
            return cached.hit;
        }
        let contains = |text: &str| text.to_lowercase().contains(query);
        let hit = contains(&session.title)
            || session.messages.iter().any(|m| {
                contains(&m.text)
                    || m.blocks.iter().any(|b| match b {
                        crate::model::AssistantBlock::Answer(t)
                        | crate::model::AssistantBlock::Thinking(t) => contains(t),
                        crate::model::AssistantBlock::Tool { output, .. } => contains(output),
                    })
            });
        let title = session.title.clone();
        self.conv.sidebar.search_cache.insert(
            (wi, si),
            super::state::SidebarSearchHit {
                query: query.to_owned(),
                title,
                stamp,
                hit,
            },
        );
        hit
    }
}

/// Soft vertical fade behind the floating composer so transcript text doesn't compete
/// with the input card.
/// Sidebar section a chat row is listed under.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
enum SidebarGroup {
    Pinned,
    Today,
    Yesterday,
    Week,
    Month,
    Older,
    /// Archived chats, last and folded until opened.
    Archived,
}

impl SidebarGroup {
    /// Stable id persisted in settings for a folded group.
    fn key(self) -> &'static str {
        match self {
            Self::Pinned => "pinned",
            Self::Today => "today",
            Self::Yesterday => "yesterday",
            Self::Week => "week",
            Self::Month => "month",
            Self::Older => "older",
            // Stored when the group is open: it starts folded.
            Self::Archived => "archived",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Pinned => "Pinned",
            Self::Today => "Today",
            Self::Yesterday => "Yesterday",
            Self::Week => "Previous 7 days",
            Self::Month => "Previous 30 days",
            Self::Older => "Older",
            Self::Archived => "Archived",
        }
    }

    /// Bucket by local calendar day, so "Yesterday" means the previous date, not 24–48 h ago.
    fn for_time(modified: std::time::SystemTime, today: chrono::NaiveDate) -> Self {
        let day = chrono::DateTime::<chrono::Local>::from(modified).date_naive();
        match (today - day).num_days() {
            ..=0 => Self::Today,
            1 => Self::Yesterday,
            2..=7 => Self::Week,
            8..=30 => Self::Month,
            _ => Self::Older,
        }
    }
}

impl OxiApp {
    /// Ctrl+Tab / Ctrl+Shift+Tab: the next or previous chat of the active workspace, in the
    /// order the sidebar lists them, wrapping around.
    pub(crate) fn cycle_chat(&mut self, back: bool) {
        let wi = self.conv.active_workspace;
        let Some(workspace) = self.conv.workspaces.get(wi) else {
            return;
        };
        let order: Vec<usize> = sidebar_session_order(workspace)
            .into_iter()
            .map(|(si, _)| si)
            .collect();
        let Some(next) = cycled(&order, workspace.active, back) else {
            return;
        };
        self.select_session_in_workspace(wi, next);
        self.reveal_chat_view();
    }
}

/// The entry after (or before) `current` in `order`, wrapping; `None` with nothing to move to.
fn cycled(order: &[usize], current: usize, back: bool) -> Option<usize> {
    if order.len() < 2 {
        return None;
    }
    let n = order.len();
    let at = order.iter().position(|&si| si == current).unwrap_or(0);
    Some(order[if back { (at + n - 1) % n } else { (at + 1) % n }])
}

/// Rows in display order: pinned chats first, then by recency bucket. The sort is stable, so
/// within a bucket chats keep their stored (most-recent-first) order.
fn sidebar_session_order(ws: &super::state::Workspace) -> Vec<(usize, SidebarGroup)> {
    let today = chrono::Local::now().date_naive();
    let mut order: Vec<(usize, SidebarGroup)> = ws
        .sessions
        .iter()
        .enumerate()
        .map(|(si, session)| {
            let pinned = session
                .session_file
                .as_ref()
                .is_some_and(|f| ws.pinned.contains(f));
            let archived = session
                .session_file
                .as_ref()
                .is_some_and(|f| ws.archived.contains(f));
            let group = if archived {
                SidebarGroup::Archived
            } else if pinned {
                SidebarGroup::Pinned
            } else {
                SidebarGroup::for_time(session.modified, today)
            };
            (si, group)
        })
        .collect();
    order.sort_by_key(|&(_, group)| group);
    order
}

/// Display linked checkouts immediately after their main workspace, without changing
/// persisted indices (running sessions and pending actions use those indices).
fn sidebar_workspace_order(workspaces: &[super::state::Workspace]) -> Vec<(usize, Option<usize>)> {
    let parents: Vec<_> = workspaces
        .iter()
        .map(|ws| {
            ws.worktree.as_ref().and_then(|info| {
                workspaces.iter().position(|candidate| {
                    candidate.worktree.is_none()
                        && std::path::Path::new(&candidate.root_path).starts_with(&info.main_root)
                })
            })
        })
        .collect();
    let mut order = Vec::with_capacity(workspaces.len());
    for (wi, parent) in parents.iter().enumerate() {
        if parent.is_some() {
            continue;
        }
        order.push((wi, None));
        for (child, child_parent) in parents.iter().enumerate() {
            if *child_parent == Some(wi) {
                order.push((child, Some(wi)));
            }
        }
    }
    order
}

#[cfg(test)]
mod cycle_tests {
    use super::cycled;

    #[test]
    fn ctrl_tab_walks_the_listed_order_and_wraps() {
        // Sidebar order (pinned first) differs from storage order.
        let order = [2, 0, 1];
        assert_eq!(cycled(&order, 2, false), Some(0));
        assert_eq!(cycled(&order, 1, false), Some(2), "wraps to the top");
        assert_eq!(cycled(&order, 2, true), Some(1), "wraps to the bottom");
        assert_eq!(cycled(&[0], 0, false), None, "nothing to switch to");
    }
}

#[cfg(test)]
mod worktree_sidebar_tests {
    use super::super::state::Workspace;
    use super::*;
    use crate::git::worktree::WorktreeInfo;

    fn workspace(root: &str, main_root: Option<&str>) -> Workspace {
        Workspace {
            root_path: root.into(),
            sessions: Vec::new(),
            active: 0,
            sidebar_folded: false,
            pinned: Vec::new(),
            archived: Vec::new(),
            folded_groups: Vec::new(),
            worktree: main_root.map(|main| WorktreeInfo {
                branch: "oxi/task".into(),
                main_root: main.into(),
                main_branch: "dev".into(),
            }),
        }
    }

    #[test]
    fn linked_workspaces_follow_their_parent_without_changing_indices() {
        let workspaces = vec![
            workspace("/repo-a", None),
            workspace("/repo-b", None),
            workspace("/trees/b", Some("/repo-b")),
            workspace("/trees/a-one", Some("/repo-a")),
            workspace("/trees/a-two", Some("/repo-a")),
        ];
        assert_eq!(
            sidebar_workspace_order(&workspaces),
            vec![
                (0, None),
                (3, Some(0)),
                (4, Some(0)),
                (1, None),
                (2, Some(1)),
            ]
        );
    }

    #[test]
    fn subfolder_parents_and_orphan_worktrees_remain_accessible() {
        let workspaces = vec![
            workspace("/trees/orphan", Some("/unopened")),
            workspace("/repo-extra", None),
            workspace("/trees/child", Some("/repo")),
            workspace("/repo/src", None),
        ];
        assert_eq!(
            sidebar_workspace_order(&workspaces),
            vec![(0, None), (1, None), (3, None), (2, Some(3)),]
        );
    }
}
