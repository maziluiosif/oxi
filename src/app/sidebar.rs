//! Sidebar: workspace list, session rows, search.

mod inbox;
pub(super) mod navigation;
mod projects;
pub(super) mod stepping;
mod workspace_list;

use eframe::egui::scroll_area::ScrollBarVisibility;
use eframe::egui::{
    self, Align, Color32, CornerRadius, FontId, Layout, RichText, ScrollArea, Sense, Stroke, Ui,
};

use crate::theme::*;
use crate::ui::chrome::sidebar_text_field;

use super::OxiApp;
use projects::{ProjectDrag, ProjectSlot};
use stepping::Listing;

impl OxiApp {
    /// Sidebar list and controls.
    pub(crate) fn render_sidebar(&mut self, ui: &mut Ui) {
        ui.set_min_width(ui.max_rect().width());
        self.poll_sidebar_branches(ui.ctx());
        self.sidebar_follow_active_project();

        // One row on top: the toolbar, or the search field in its place while searching.
        if self.conv.sidebar.search_open || !self.conv.sidebar.search.is_empty() {
            self.render_sidebar_search(ui);
        } else {
            self.render_sidebar_toolbar(ui);
        }
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
        // Drawn this frame, or not listed at all (filtered out by the search).
        self.conv.sidebar.reveal_active = false;
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

    /// New chat (its shortcut, and a menu of the other ways to start one), then search and
    /// add project. Antigravity's "+ New Conversation" in one row with its neighbours.
    fn render_sidebar_toolbar(&mut self, ui: &mut Ui) {
        const CHEVRON_W: f32 = 18.0;
        const GAP: f32 = 2.0;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = GAP;
            ui.set_height(CONTROL_HEIGHT);
            // New chat takes only what its label (and shortcut, when there is room) needs;
            // its menu sits right next to it, search and add project at the far end.
            let key = shortcut_text("⌘N", "Ctrl+N");
            let shortcut = ui.painter().layout_job(crate::ui::chrome::shortcut_job(
                key,
                FS_TINY,
                c_text_faint(),
            ));
            let label_w = |text: &str, font: FontId| {
                ui.painter()
                    .layout_no_wrap(text.to_owned(), font, c_text_strong())
                    .rect
                    .width()
            };
            // The padding, gap and glyph of `flat_button_icon`, plus the label.
            let natural_w = 8.0
                + label_w(ICON_PLUS, FontId::new(FS_SMALL, icon_font()))
                + 5.0
                + label_w("New chat", FontId::proportional(FS_SMALL))
                + 8.0;
            let shortcut_w = 10.0 + shortcut.size().x;
            let rest_w = CHEVRON_W + 2.0 * CONTROL_HEIGHT + 3.0 * GAP;
            let show_shortcut = natural_w + shortcut_w + rest_w <= ui.available_width();
            let new_w = natural_w + if show_shortcut { shortcut_w } else { 0.0 };
            let new_chat = crate::ui::chrome::flat_button_icon(
                ui,
                ICON_PLUS,
                "New chat",
                FS_SMALL,
                egui::vec2(new_w, CONTROL_HEIGHT),
                c_text_strong(),
            )
            .on_hover_text("Start a new chat in the active workspace");
            if show_shortcut {
                let pos = egui::pos2(
                    new_chat.rect.right() - 8.0 - shortcut.size().x,
                    new_chat.rect.center().y - shortcut.size().y * 0.5,
                );
                ui.painter().galley(pos, shortcut, c_text_faint());
            }
            if new_chat.clicked() {
                self.new_chat();
            }
            new_chat.context_menu(|ui| self.render_new_chat_menu(ui));

            let more = menu_toggle(
                ui,
                egui::vec2(CHEVRON_W, CONTROL_HEIGHT),
                ICON_ANGLE_DOWN,
                "More ways to start a chat",
            );
            egui::Popup::from_toggle_button_response(&more)
                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                .show(|ui| self.render_new_chat_menu(ui));

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = GAP;
                let add_id = ui.id().with("sidebar_add_project");
                let add_open = egui::Popup::is_id_open(ui.ctx(), add_id);
                let add = crate::ui::chrome::icon_button_plain(
                    ui,
                    ICON_FOLDER_PLUS,
                    CONTROL_HEIGHT,
                    add_open,
                )
                .on_hover_text("Add a project: a folder here, or one on another machine");
                egui::Popup::from_toggle_button_response(&add)
                    .id(add_id)
                    .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                    .show(|ui| {
                        if ui
                            .button("Open folder…")
                            .on_hover_text(
                                "Add a project folder. Each project has its own chats; \
                                 tools run with that folder as cwd.",
                            )
                            .clicked()
                        {
                            self.open_workspace_folder();
                            ui.close();
                        }
                        if ui
                            .button("Connect to remote host (SSH)…")
                            .on_hover_text(
                                "Open a folder on another machine over SSH; \
                                 its oxi server runs the agents there.",
                            )
                            .clicked()
                        {
                            self.open_remote_dialog();
                            ui.close();
                        }
                    });

                let search_key = shortcut_text("⇧⌘E", "Ctrl+Shift+E");
                let search =
                    crate::ui::chrome::icon_button_plain(ui, ICON_SEARCH, CONTROL_HEIGHT, false);
                search.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Search chats")
                });
                if search
                    .on_hover_text(format!("Search chats ({search_key})"))
                    .clicked()
                {
                    self.open_sidebar_search();
                }
            });
        });
    }

    /// The other ways to start a chat, under New chat's chevron and its right-click menu.
    fn render_new_chat_menu(&mut self, ui: &mut Ui) {
        if ui
            .button("New general chat")
            .on_hover_text("A chat that isn't about any project, run in ~/oxi-general")
            .clicked()
        {
            self.new_general_chat();
            ui.close();
        }
        if ui
            .button("New chat side by side")
            .on_hover_text("Open a new chat next to this one, e.g. to ask a second agent")
            .clicked()
        {
            self.new_chat_split();
            ui.close();
        }
        if ui
            .button("New work tree chat side by side")
            .on_hover_text(
                "Start the same project in a work tree next to this chat, e.g. to give the task to a second agent",
            )
            .clicked()
        {
            self.start_worktree_split();
            ui.close();
        }
    }

    /// Shows the chat search in the toolbar's place and focuses it (the search button, or the
    /// Search Chats shortcut from anywhere).
    pub(crate) fn open_sidebar_search(&mut self) {
        self.conv.sidebar.open = true;
        self.conv.sidebar.mode = super::state::SidebarMode::Chats;
        self.conv.sidebar.search_open = true;
        self.conv.sidebar.focus_search = true;
    }

    /// The search field standing in for the toolbar. Escape or its close button clears the
    /// query and brings the toolbar back; so does leaving the field empty.
    fn render_sidebar_search(&mut self, ui: &mut Ui) {
        const GAP: f32 = 2.0;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = GAP;
            ui.set_height(CONTROL_HEIGHT);
            let search_w = (ui.available_width() - CONTROL_HEIGHT - GAP).max(48.0);
            let field = ui
                .allocate_ui_with_layout(
                    egui::vec2(search_w, CONTROL_HEIGHT),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        ui.set_width(search_w);
                        sidebar_text_field(ui, &mut self.conv.sidebar.search, "Search chats…")
                    },
                )
                .inner;
            if std::mem::take(&mut self.conv.sidebar.focus_search) {
                field.request_focus();
            }
            let escaped = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape));
            let closed =
                crate::ui::chrome::icon_button_plain(ui, ICON_CLOSE, CONTROL_HEIGHT, false)
                    .on_hover_text("Close search (Esc)")
                    .clicked();
            let left_empty = field.lost_focus() && self.conv.sidebar.search.trim().is_empty();
            if escaped || closed || left_empty {
                self.conv.sidebar.search.clear();
                self.conv.sidebar.search_open = false;
            }
        });
    }

    fn render_sidebar_session_list(&mut self, ui: &mut Ui) {
        // Workspace headers sit a step above the chat rows' default size, matching
        // the app-wide type scale (theme.rs) so it still tracks the UI density zoom.
        let q = self.conv.sidebar.search.trim().to_lowercase();
        #[cfg(test)]
        self.conv.sidebar.drawn_stops.clear();

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
        // No section titles: pinned projects come first, each with a pin, and a hairline
        // ends them. Rows take drops above and below them for their own part; only pinning
        // the first project needs a strip of its own, shown while a project is dragged.
        let in_pinned = any_pinned || dragging;
        if dragging
            && !any_pinned
            && self.render_sidebar_section_header(ui, "Pinned", Some("Drop here to pin"), true)
        {
            return;
        }
        let mut projects_header = false;
        // The active project's header row and where its block ends, for the sticky copy.
        let mut sticky = None;
        for &wi in &tops {
            if !projects_header && !self.project_is_pinned(wi) {
                if in_pinned && self.render_project_drop_end(ui, true) {
                    return;
                }
                if self.render_sidebar_section_break(ui, in_pinned) {
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
            #[cfg(test)]
            let drawn_before = self.conv.sidebar.drawn_stops.len();
            if self.render_sidebar_workspace(ui, wi, None, false, &q, &children) {
                return;
            }
            // A project row with no chats under it is a stop of its own (`sidebar_stops`).
            #[cfg(test)]
            if q.is_empty()
                && !self.conv.sidebar.history
                && self.conv.sidebar.drawn_stops.len() == drawn_before
            {
                self.conv
                    .sidebar
                    .drawn_stops
                    .push(stepping::SidebarStop::Project(wi));
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
            if self.render_sidebar_section_break(ui, in_pinned) {
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
        let parent_folded = self.sidebar_worktree_parent_folded(parent, q);
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
/// `mac` on macOS, `other` elsewhere: a shortcut as the platform writes it.
fn shortcut_text(mac: &'static str, other: &'static str) -> &'static str {
    if cfg!(target_os = "macos") {
        mac
    } else {
        other
    }
}

/// A narrow icon button that opens a menu, highlighted while its menu is open.
fn menu_toggle(ui: &mut Ui, size: egui::Vec2, icon: &str, label: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    resp.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
    });
    let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&resp));
    let radius = CornerRadius::same(RADIUS_CHIP);
    if open || resp.hovered() || resp.has_focus() {
        ui.painter().rect_filled(
            rect,
            radius,
            if open { c_row_active() } else { c_row_hover() },
        );
    }
    crate::ui::chrome::keyboard_focus_ring(ui, &resp, radius);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        icon,
        FontId::new(FS_TINY, icon_font()),
        if open || resp.hovered() {
            c_accent()
        } else {
            c_text_muted()
        },
    );
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(label)
}

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
