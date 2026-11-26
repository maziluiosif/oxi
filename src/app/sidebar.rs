//! Sidebar: workspace list, session rows, search.

mod inbox;
pub(super) mod navigation;
mod projects;
mod workspace_list;

use eframe::egui::scroll_area::ScrollBarVisibility;
use eframe::egui::{
    self, Align, Color32, CornerRadius, FontId, Layout, RichText, ScrollArea, Sense, Stroke, Ui,
};

use crate::theme::*;
use crate::ui::chrome::sidebar_text_field;

use super::OxiApp;
use projects::{ProjectDrag, ProjectSlot};

impl OxiApp {
    /// Sidebar list and controls.
    pub(crate) fn render_sidebar(&mut self, ui: &mut Ui) {
        ui.set_min_width(ui.max_rect().width());
        self.poll_sidebar_branches(ui.ctx());
        self.sidebar_follow_active_project();

        // Primary action first, with its shortcut, like Antigravity's "+ New Conversation".
        let new_chat = crate::ui::chrome::flat_button_icon(
            ui,
            ICON_PLUS,
            "New chat",
            FS_SMALL,
            egui::vec2(ui.available_width(), CONTROL_HEIGHT),
            c_text_strong(),
        )
        .on_hover_text("Start a new chat in the active workspace");
        if new_chat.rect.width() >= 170.0 {
            let key = if cfg!(target_os = "macos") {
                "⌘N"
            } else {
                "Ctrl+N"
            };
            let galley = ui.painter().layout_job(crate::ui::chrome::shortcut_job(
                key,
                FS_TINY,
                c_text_faint(),
            ));
            let pos = egui::pos2(
                new_chat.rect.right() - 8.0 - galley.size().x,
                new_chat.rect.center().y - galley.size().y * 0.5,
            );
            ui.painter().galley(pos, galley, c_text_faint());
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

        // Full-width quiet search field (with inline clear when active).
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.set_height(CONTROL_HEIGHT);
            let clear_w = if self.conv.sidebar.search.is_empty() {
                0.0
            } else {
                22.0
            };
            let search_w = (ui.available_width()
                - clear_w
                - if clear_w > 0.0 {
                    ui.spacing().item_spacing.x
                } else {
                    0.0
                })
            .max(48.0);
            ui.allocate_ui_with_layout(
                egui::vec2(search_w, CONTROL_HEIGHT),
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
        });

        ui.add_space(SPACE_SM);

        if let Some(notice) = self.conv.sidebar.notice.clone() {
            if crate::ui::chrome::dismissible_notice(ui, "sidebar_notice", &notice) {
                self.conv.sidebar.notice = None;
            }
            ui.add_space(8.0);
        }

        if self.conv.sidebar.history {
            self.render_sidebar_history_filters(ui);
        }
        // Reserve room for the bottom footer (History).
        let footer_h = 14.0 + CONTROL_HEIGHT;
        ScrollArea::vertical()
            .id_salt("sidebar_main_scroll")
            .max_height((ui.available_height() - footer_h).max(40.0))
            .auto_shrink([false, false])
            .scroll_bar_visibility(ScrollBarVisibility::VisibleWhenNeeded)
            .show(ui, |ui| {
                self.render_sidebar_session_list(ui);
            });
        self.render_project_drag_preview(ui.ctx());

        ui.add_space(6.0);
        crate::ui::chrome::hairline(ui);
        ui.add_space(6.0);

        let history = self.conv.sidebar.history;
        let label = if history {
            "Back to recent chats"
        } else {
            "History"
        };
        if crate::ui::chrome::flat_button_icon(
            ui,
            ICON_UNDO,
            label,
            FS_SMALL,
            egui::vec2(ui.available_width(), CONTROL_HEIGHT),
            c_text_muted(),
        )
        .on_hover_text("Browse all saved chats, including older and archived conversations")
        .clicked()
        {
            self.conv.sidebar.history = !history;
        }
        // No Settings entry here: the status bar's gear is always there, even with the
        // sidebar hidden, and a second one only added a row.

        // The outer sidebar allocation already owns the full fixed width/height. Expanding
        // again from content made Conversations report a subtly different size than Explorer.
    }

    fn render_sidebar_session_list(&mut self, ui: &mut Ui) {
        // Workspace headers sit a step above the chat rows' default size, matching
        // the app-wide type scale (theme.rs) so it still tracks the UI density zoom.
        let q = self.conv.sidebar.search.trim().to_lowercase();

        if self.conv.no_workspace && self.conv.workspaces.len() == 1 {
            ui.add_space(12.0);
            ui.label(
                egui::RichText::new("No workspaces. Open a folder to start.")
                    .size(FS_SMALL)
                    .color(c_text_muted()),
            );
            ui.add_space(6.0);
            if crate::ui::chrome::primary_button(ui, "Open folder…").clicked() {
                self.open_workspace_folder();
            }
            if crate::ui::chrome::ghost_button(ui, "Start a general chat", false)
                .on_hover_text("A chat that isn't about any project, run in ~/oxi-general")
                .clicked()
            {
                self.new_general_chat();
            }
            return;
        }
        // History unfolds every list, so the sections would only repeat rows shown below;
        // the badges still need this frame's counts either way.
        let show_inbox = q.is_empty() && !self.conv.sidebar.history;
        if self.render_sidebar_inbox(ui, show_inbox) {
            return;
        }
        let order = self.sidebar_order();
        let tops = self.sidebar_projects();
        let dragging = egui::DragAndDrop::has_payload_of_type::<ProjectDrag>(ui.ctx());
        let any_pinned = tops.iter().any(|&wi| self.project_is_pinned(wi));
        // Sections: "Pinned" (only when something is pinned, or as a drop target while a
        // project is dragged), then "Projects", which holds the workspace actions.
        let mut in_pinned = false;
        if any_pinned || dragging {
            let hint = (!any_pinned).then_some("Drop here to pin");
            if self.render_sidebar_section_header(ui, "Pinned", hint, true, false) {
                return;
            }
            in_pinned = true;
        }
        let mut projects_header = false;
        // The active project's header row and where its block ends, for the sticky copy.
        let mut sticky = None;
        for &wi in &tops {
            if !projects_header && !self.project_is_pinned(wi) {
                if in_pinned && self.render_project_drop_end(ui, true) {
                    return;
                }
                if self.render_sidebar_section_header(ui, "Projects", None, false, true) {
                    return;
                }
                projects_header = true;
            }
            // Orchestrator workers' work trees show with their lead chat instead.
            let children: Vec<usize> = order
                .iter()
                .filter(|(_, p)| *p == Some(wi))
                .map(|&(child, _)| child)
                .filter(|&child| {
                    child == self.conv.active_workspace
                        || !self.hidden_worker_workspace(&self.conv.workspaces[child].root_path)
                })
                .collect();
            if self.conv.sidebar.history
                && self
                    .conv
                    .sidebar
                    .history_project
                    .as_ref()
                    .is_some_and(|root| root != &self.conv.workspaces[wi].root_path)
            {
                continue;
            }
            let top = ui.cursor().min;
            let width = ui.available_width();
            if self.render_sidebar_workspace(ui, wi, None, false, &q, &children) {
                return;
            }
            // The header row takes drops above it; the chats under an unfolded project take
            // drops after it, so it never has to be folded first.
            let chats = egui::Rect::from_min_max(
                top + egui::vec2(0.0, 1.0 + SIDEBAR_ROW_H),
                egui::pos2(top.x + width, ui.cursor().min.y),
            );
            if dragging && chats.height() > 4.0 && self.project_drop_after_block(ui, wi, chats) {
                return;
            }
            if self.sidebar_top_project(self.conv.active_workspace) == wi {
                // `render_sidebar_workspace` leads with 1px of space before the header row.
                let header = egui::Rect::from_min_size(
                    top + egui::vec2(0.0, 1.0),
                    egui::vec2(width, SIDEBAR_ROW_H),
                );
                sticky = Some((wi, header, ui.cursor().min.y));
            }
        }
        if !projects_header {
            if in_pinned && self.render_project_drop_end(ui, true) {
                return;
            }
            if self.render_sidebar_section_header(ui, "Projects", None, false, true) {
                return;
            }
        }
        self.render_project_drop_end(ui, false);
        self.project_drag_autoscroll(ui);
        if let Some((wi, header, block_bottom)) = sticky {
            self.render_sticky_project_header(ui, wi, header, block_bottom);
        }
    }

    /// Linked checkout `wi` of workspace `parent`, indented under it with a tree guide.
    fn render_sidebar_worktree_child(
        &mut self,
        ui: &mut egui::Ui,
        wi: usize,
        parent: usize,
        q: &str,
    ) -> bool {
        let parent_folded = self.conv.workspaces[parent].sidebar_folded
            && q.is_empty()
            && !self.conv.sidebar.history;
        if parent_folded && !self.sidebar_workspace_has_kept_chats(wi) {
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

/// Ordering and daily visibility, independent of the saved archive state.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum SidebarGroup {
    Pinned,
    Today,
    Older,
    Archived,
}

impl SidebarGroup {
    fn for_time(modified: std::time::SystemTime, today: chrono::NaiveDate) -> Self {
        let day = chrono::DateTime::<chrono::Local>::from(modified).date_naive();
        if day >= today {
            Self::Today
        } else {
            Self::Older
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
            .filter(|&si| !self.sidebar_hides_placeholder(wi, si))
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
