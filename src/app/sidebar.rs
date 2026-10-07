//! Sidebar: workspace list, session rows, search, settings button.

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
        if new_chat.clicked() {
            self.new_chat();
        }
        ui.add_space(6.0);

        // Search row + add-workspace button.
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.set_height(28.0);

            let add_w = 22.0;
            let clear_w = if self.conv.sidebar.search.is_empty() {
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

        if let Some(notice) = self.conv.sidebar.notice.clone() {
            if crate::ui::chrome::dismissible_notice(ui, "sidebar_notice", &notice) {
                self.conv.sidebar.notice = None;
            }
            ui.add_space(8.0);
        }

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
            return;
        }
        for (wi, parent) in sidebar_workspace_order(&self.conv.workspaces) {
            if sidebar_changed {
                return;
            }
            if self.conv.no_workspace && wi == 0 {
                continue;
            }
            let parent_folded =
                parent.is_some_and(|pi| self.conv.workspaces[pi].sidebar_folded) && q.is_empty();
            if parent_folded && !self.sidebar_workspace_needs_visibility(wi, ui.ctx()) {
                continue;
            }
            ui.horizontal(|ui| {
                if parent.is_some() {
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
                }
                ui.vertical(|ui| {
                    sidebar_changed =
                        self.render_sidebar_workspace(ui, wi, parent, parent_folded, &q);
                });
            });
        }
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
            let group = if pinned {
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
