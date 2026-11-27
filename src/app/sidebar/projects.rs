//! Project sections: pinned projects, manual order (drag or menu), labels and the workspace actions.
//!
//! Order and pins are display only. Running chats, pending actions and every `SessionKey`
//! address a workspace by its index in `conv.workspaces`, so that vector is never reordered;
//! the sidebar sorts its rows from `settings.pinned_projects` and `settings.project_order`.

use super::super::state::Workspace;
use super::*;

/// Where [`OxiApp::place_project`] puts a project inside its section.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ProjectSlot {
    Start,
    End,
    Before(String),
    After(String),
}

/// Drag payload of a project row being reordered.
#[derive(Clone)]
pub(super) struct ProjectDrag {
    pub root: String,
    pub label: String,
}

/// The main workspace each linked checkout is drawn under (`None` for top-level rows).
pub(super) fn sidebar_parents(workspaces: &[Workspace]) -> Vec<Option<usize>> {
    workspaces
        .iter()
        .map(|ws| {
            ws.worktree.as_ref().and_then(|info| {
                workspaces.iter().position(|candidate| {
                    candidate.worktree.is_none()
                        && std::path::Path::new(&candidate.root_path).starts_with(&info.main_root)
                })
            })
        })
        .collect()
}

/// Sidebar rows in display order, each with the project it is drawn under. Pinned projects
/// come first in pin order, then projects in their manual order, then the rest in the order
/// they were added; linked checkouts follow their main workspace.
pub(super) fn sidebar_workspace_order(
    workspaces: &[Workspace],
    pinned: &[String],
    manual: &[String],
) -> Vec<(usize, Option<usize>)> {
    let parents = sidebar_parents(workspaces);
    let rank = |wi: usize| {
        let root = &workspaces[wi].root_path;
        match pinned.iter().position(|r| r == root) {
            Some(at) => (0, at),
            None => (
                1,
                manual.iter().position(|r| r == root).unwrap_or(usize::MAX),
            ),
        }
    };
    let mut tops: Vec<usize> = (0..workspaces.len())
        .filter(|&wi| parents[wi].is_none())
        .collect();
    // Stable: projects without a manual slot keep the order they were added in.
    tops.sort_by_key(|&wi| rank(wi));
    let mut order = Vec::with_capacity(workspaces.len());
    for wi in tops {
        order.push((wi, None));
        order.extend(
            parents
                .iter()
                .enumerate()
                .filter(|(_, parent)| **parent == Some(wi))
                .map(|(child, _)| (child, Some(wi))),
        );
    }
    order
}

/// `section` (root paths in display order) with `root` moved to `slot`.
fn placed(mut section: Vec<String>, root: &str, slot: &ProjectSlot) -> Vec<String> {
    section.retain(|r| r != root);
    let at = match slot {
        ProjectSlot::Start => 0,
        ProjectSlot::End => section.len(),
        ProjectSlot::Before(target) => section
            .iter()
            .position(|r| r == target)
            .unwrap_or(section.len()),
        ProjectSlot::After(target) => section
            .iter()
            .position(|r| r == target)
            .map_or(section.len(), |at| at + 1),
    };
    section.insert(at, root.to_owned());
    section
}

impl OxiApp {
    pub(crate) fn sidebar_order(&self) -> Vec<(usize, Option<usize>)> {
        sidebar_workspace_order(
            &self.conv.workspaces,
            &self.conv.settings.pinned_projects,
            &self.conv.settings.project_order,
        )
    }

    /// Project roots in sidebar order, pinned first: the rows Ctrl+1…9 and "Go to Project…"
    /// pick from. The hidden scratch workspace is left out.
    pub(crate) fn sidebar_projects(&self) -> Vec<usize> {
        self.sidebar_order()
            .into_iter()
            .filter(|&(wi, parent)| parent.is_none() && !(self.conv.no_workspace && wi == 0))
            .map(|(wi, _)| wi)
            .collect()
    }

    /// The work trees listed under project `wi`, in sidebar `order`. An Orchestrator
    /// worker's work tree shows with its lead chat instead, unless it is the open checkout.
    /// The sidebar, ⌥⌘← / → and opening a project all use this, so they agree.
    pub(crate) fn sidebar_worktrees(
        &self,
        order: &[(usize, Option<usize>)],
        wi: usize,
    ) -> Vec<usize> {
        order
            .iter()
            .filter(|(_, parent)| *parent == Some(wi))
            .map(|&(child, _)| child)
            .filter(|&child| {
                child == self.conv.active_workspace
                    || !self.hidden_worker_workspace(&self.conv.workspaces[child].root_path)
            })
            .collect()
    }

    /// Open project `wi` at the checkout last used in it and that checkout's last chat, unless
    /// the open chat is already in the project (or in one of its work trees).
    pub(crate) fn switch_to_project(&mut self, wi: usize) {
        if wi >= self.conv.workspaces.len()
            || self.sidebar_top_project(self.conv.active_workspace) == wi
        {
            return;
        }
        let remembered = self
            .conv
            .sidebar
            .last_checkout
            .get(&self.conv.workspaces[wi].root_path)
            .map(String::as_str);
        let worktrees = self.sidebar_worktrees(&self.sidebar_order(), wi);
        let target = project_checkout_to_open(&self.conv.workspaces, wi, &worktrees, remembered);
        self.select_workspace(target);
        // Switching is refused while a file has unsaved changes; the editor stays then.
        if self.conv.active_workspace == target {
            self.reveal_chat_view();
            self.focus_active_view_next_frame();
        }
    }

    /// Notes the open checkout as its project's last one. Called just before the open
    /// workspace changes, so every way of leaving a project (a chat click, Ctrl+Tab, Go to
    /// Chat, another project) is covered.
    pub(crate) fn remember_project_checkout(&mut self) {
        let wi = self.conv.active_workspace;
        let Some(checkout) = self.conv.workspaces.get(wi).map(|ws| ws.root_path.clone()) else {
            return;
        };
        let project = self.conv.workspaces[self.sidebar_top_project(wi)]
            .root_path
            .clone();
        self.conv.sidebar.last_checkout.insert(project, checkout);
    }

    pub(super) fn project_is_pinned(&self, wi: usize) -> bool {
        self.conv
            .settings
            .pinned_projects
            .contains(&self.conv.workspaces[wi].root_path)
    }

    /// The project row `wi` is listed under: itself for a project, its repository for a work
    /// tree.
    pub(crate) fn sidebar_top_project(&self, wi: usize) -> usize {
        // Only work trees have a parent row; skip the scan for everything else.
        if self
            .conv
            .workspaces
            .get(wi)
            .is_none_or(|ws| ws.worktree.is_none())
        {
            return wi;
        }
        sidebar_parents(&self.conv.workspaces)
            .get(wi)
            .copied()
            .flatten()
            .unwrap_or(wi)
    }

    /// Project roots in one sidebar section, in display order.
    fn sidebar_section_roots(&self, pinned: bool) -> Vec<String> {
        self.sidebar_order()
            .into_iter()
            .filter(|&(wi, parent)| {
                parent.is_none()
                    && !(self.conv.no_workspace && wi == 0)
                    && self.project_is_pinned(wi) == pinned
            })
            .map(|(wi, _)| self.conv.workspaces[wi].root_path.clone())
            .collect()
    }

    /// Move project `root` into the pinned or the unpinned section, at `slot`.
    pub(super) fn place_project(&mut self, root: &str, pinned: bool, slot: ProjectSlot) {
        let section = placed(self.sidebar_section_roots(pinned), root, &slot);
        let settings = &mut self.conv.settings;
        if pinned {
            settings.pinned_projects = section;
        } else {
            settings.pinned_projects.retain(|r| r != root);
            settings.project_order = section;
        }
        self.sync_workspaces_to_settings();
    }

    pub(super) fn set_project_pinned(&mut self, wi: usize, pinned: bool) {
        let root = self.conv.workspaces[wi].root_path.clone();
        if pinned {
            self.place_project(&root, true, ProjectSlot::End);
        } else {
            // Back to the slot it had before it was pinned, if it was ever placed.
            self.conv.settings.pinned_projects.retain(|r| r != &root);
            self.sync_workspaces_to_settings();
        }
    }

    /// Neighbours of project `wi` within its section: (previous, next) root paths.
    pub(super) fn project_neighbours(&self, wi: usize) -> (Option<String>, Option<String>) {
        let root = &self.conv.workspaces[wi].root_path;
        let section = self.sidebar_section_roots(self.project_is_pinned(wi));
        let Some(at) = section.iter().position(|r| r == root) else {
            return (None, None);
        };
        (
            at.checked_sub(1).map(|prev| section[prev].clone()),
            section.get(at + 1).cloned(),
        )
    }

    /// The project's name as the sidebar shows it: the folder name alone, like the breadcrumb,
    /// with its parent folder added only when another open project has the same name.
    pub(crate) fn sidebar_project_label(&self, wi: usize) -> String {
        let root = &self.conv.workspaces[wi].root_path;
        let short = workspace_short_label(root);
        let clash = self.conv.workspaces.iter().enumerate().any(|(other, ws)| {
            other != wi
                && ws.worktree.is_none()
                && !(self.conv.no_workspace && other == 0)
                && workspace_short_label(&ws.root_path) == short
        });
        if clash {
            workspace_sidebar_label(root)
        } else {
            short
        }
    }

    /// Project `wi`'s color. Projects claim their preferred hue in the order they were added;
    /// one already taken moves on to the next free hue, so up to eight open projects never
    /// share a color, and a project's color only changes if one added before it closes.
    pub(super) fn sidebar_project_color(&self, wi: usize) -> egui::Color32 {
        let colors = project_tag_colors();
        let mut taken = [false; 8];
        let mut mine = project_tag_slot(&self.conv.workspaces[wi].root_path);
        for (index, ws) in self.conv.workspaces.iter().enumerate() {
            if ws.worktree.is_some() && index != wi {
                continue;
            }
            let mut slot = project_tag_slot(&ws.root_path);
            if taken.iter().all(|&t| t) {
                taken = [false; 8];
            }
            while taken[slot] {
                slot = (slot + 1) % colors.len();
            }
            taken[slot] = true;
            if index == wi {
                mine = slot;
                break;
            }
        }
        colors[mine]
    }

    /// With "Collapse other projects when switching" on, opening a chat in another project
    /// folds the one left behind and unfolds the new one. Runs once per switch, so a project
    /// unfolded by hand stays open until the next switch away from it.
    pub(super) fn sidebar_follow_active_project(&mut self) {
        let Some(top) = self
            .conv
            .workspaces
            .get(self.conv.active_workspace)
            .map(|_| self.sidebar_top_project(self.conv.active_workspace))
        else {
            return;
        };
        let root = self.conv.workspaces[top].root_path.clone();
        let previous = self.conv.sidebar.focused_project.replace(root.clone());
        if !self.conv.settings.sidebar_focus_active_project {
            return;
        }
        let Some(previous) = previous.filter(|p| *p != root) else {
            return;
        };
        let mut changed = false;
        if let Some(left) = self
            .conv
            .workspaces
            .iter_mut()
            .find(|w| w.root_path == previous)
            && !left.sidebar_folded
        {
            left.sidebar_folded = true;
            changed = true;
        }
        if self.conv.workspaces[top].sidebar_folded {
            self.conv.workspaces[top].sidebar_folded = false;
            changed = true;
        }
        if changed {
            self.sync_workspaces_to_settings();
        }
    }

    /// Where the unpinned projects start: a hairline under the pinned ones (nothing when none
    /// are pinned). While a project is dragged the hairline takes drops for the top of the
    /// unpinned projects; it keeps its height, so rows don't jump when a drag starts.
    pub(super) fn render_sidebar_section_break(&mut self, ui: &mut Ui, after_pinned: bool) -> bool {
        if !after_pinned {
            return false;
        }
        let (rect, target) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), 2.0 * SPACE_XS + 1.0),
            Sense::hover(),
        );
        ui.painter().hline(
            egui::Rangef::new(rect.left() + 6.0, rect.right() - 6.0),
            rect.center().y,
            Stroke::new(1.0, c_border_subtle()),
        );
        self.project_drop_target(ui, &target, rect.center().y, false, ProjectSlot::Start)
    }

    /// Small caps section title, shown as "Pinned · Drop here to pin" while a project is
    /// dragged and none is pinned yet: the drop target that pins it.
    pub(super) fn render_sidebar_section_header(
        &mut self,
        ui: &mut Ui,
        label: &str,
        hint: Option<&str>,
        pinned: bool,
    ) -> bool {
        ui.add_space(SPACE_XS);
        let row = ui.horizontal(|ui| {
            ui.set_height(SIDEBAR_HEADER_H);
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.add_space(6.0);
            ui.label(
                RichText::new(label)
                    .size(FS_TINY)
                    .color(c_sidebar_section()),
            );
            if let Some(hint) = hint {
                ui.label(RichText::new(hint).size(FS_TINY).color(c_text_faint()));
            }
        });
        let rect = row.response.rect;
        let target = ui.interact(
            rect,
            ui.id().with(("sidebar_section_drop", pinned)),
            Sense::hover(),
        );
        self.project_drop_target(ui, &target, rect.bottom(), pinned, ProjectSlot::Start)
    }

    /// Thin strip after a section's last project, so a project can be dropped at the end.
    pub(super) fn render_project_drop_end(&mut self, ui: &mut Ui, pinned: bool) -> bool {
        if !egui::DragAndDrop::has_payload_of_type::<ProjectDrag>(ui.ctx()) {
            return false;
        }
        let (rect, target) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 10.0), Sense::hover());
        self.project_drop_target(ui, &target, rect.top() + 1.0, pinned, ProjectSlot::End)
    }

    /// Drop target over the chats of unfolded project `wi`: a project released there goes
    /// right after it, marked by a line under its last row.
    pub(super) fn project_drop_after_block(
        &mut self,
        ui: &Ui,
        wi: usize,
        chats: egui::Rect,
    ) -> bool {
        let root = self.conv.workspaces[wi].root_path.clone();
        let pinned = self.project_is_pinned(wi);
        let target = ui.interact(
            chats,
            egui::Id::new(("sidebar_project_block_drop", wi)),
            Sense::hover(),
        );
        self.project_drop_target(
            ui,
            &target,
            chats.bottom() - 0.5,
            pinned,
            ProjectSlot::After(root),
        )
    }

    /// While a project is dragged near the top or bottom edge of the list, scroll toward it,
    /// faster the closer the pointer gets, so a long list never has to be dropped short.
    pub(super) fn project_drag_autoscroll(&self, ui: &mut Ui) {
        const EDGE: f32 = 28.0;
        const MAX_SPEED: f32 = 14.0;
        if !egui::DragAndDrop::has_payload_of_type::<ProjectDrag>(ui.ctx()) {
            return;
        }
        let clip = ui.clip_rect();
        let Some(pos) = ui.ctx().pointer_hover_pos() else {
            return;
        };
        if !clip.x_range().contains(pos.x) {
            return;
        }
        // Positive delta scrolls the content down, revealing what is above.
        let delta = if pos.y < clip.top() + EDGE {
            (clip.top() + EDGE - pos.y).min(EDGE * 2.0) / EDGE * MAX_SPEED
        } else if pos.y > clip.bottom() - EDGE {
            -(pos.y - (clip.bottom() - EDGE)).min(EDGE * 2.0) / EDGE * MAX_SPEED
        } else {
            return;
        };
        ui.scroll_with_delta(egui::vec2(0.0, delta));
        ui.ctx().request_repaint();
    }

    /// Shared drop handling: an accent line at `line_y` while a project hovers `target`, and
    /// the move on release. Returns whether the order changed.
    pub(super) fn project_drop_target(
        &mut self,
        ui: &Ui,
        target: &egui::Response,
        line_y: f32,
        pinned: bool,
        slot: ProjectSlot,
    ) -> bool {
        let excluded = |drag: &ProjectDrag| match &slot {
            ProjectSlot::Before(root) | ProjectSlot::After(root) => *root == drag.root,
            ProjectSlot::Start | ProjectSlot::End => false,
        };
        if target
            .dnd_hover_payload::<ProjectDrag>()
            .is_some_and(|drag| !excluded(&drag))
        {
            ui.painter().hline(
                target.rect.x_range().shrink(2.0),
                line_y,
                Stroke::new(2.0, c_accent()),
            );
        }
        if let Some(drag) = target.dnd_release_payload::<ProjectDrag>()
            && !excluded(&drag)
        {
            self.place_project(&drag.root, pinned, slot);
            return true;
        }
        false
    }

    /// The project name following the pointer while a project row is dragged.
    /// While the active project's header row is scrolled out of view and its chats are still
    /// on screen, a copy of the row stays pinned to the top of the list; clicking it scrolls
    /// back to the real one. The next project's rows push it off as they arrive.
    pub(super) fn render_sticky_project_header(
        &self,
        ui: &mut Ui,
        wi: usize,
        header: egui::Rect,
        block_bottom: f32,
    ) {
        let clip = ui.clip_rect();
        if header.top() >= clip.top()
            || block_bottom <= clip.top() + header.height()
            || egui::DragAndDrop::has_payload_of_type::<ProjectDrag>(ui.ctx())
        {
            return;
        }
        let top = clip.top().min(block_bottom - header.height());
        let rect = egui::Rect::from_min_size(egui::pos2(header.left(), top), header.size());
        let label = self.sidebar_project_label(wi);
        let response = ui
            .interact(
                rect,
                egui::Id::new("sidebar_sticky_project"),
                Sense::click(),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text("Scroll to the project");
        response.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Button,
                ui.is_enabled(),
                format!("Scroll to project {label}"),
            )
        });
        let painter = ui.painter().with_clip_rect(clip);
        // Opaque, a little wider than the row so scrolled rows can't peek out at its sides.
        painter.rect_filled(rect.expand2(egui::vec2(2.0, 0.0)), 0.0, c_bg_sidebar());
        if response.hovered() {
            painter.rect_filled(rect, CornerRadius::same(RADIUS_ROW), c_row_hover());
        }
        painter.hline(
            rect.x_range(),
            rect.bottom() - 0.5,
            Stroke::new(1.0, c_border_subtle()),
        );
        painter.rect_filled(
            egui::Rect::from_min_size(
                rect.left_top() + egui::vec2(0.0, 5.0),
                egui::vec2(2.0, rect.height() - 10.0),
            ),
            CornerRadius::same(1),
            c_accent(),
        );
        const GLYPH_W: f32 = 18.0;
        painter.text(
            egui::pos2(rect.left() + 4.0 + GLYPH_W * 0.5, rect.center().y),
            egui::Align2::CENTER_CENTER,
            if self.is_remote_workspace(wi) {
                ICON_SERVER
            } else {
                ICON_FOLDER_OPEN
            },
            FontId::new(FS_TINY, icon_font()),
            self.sidebar_project_color(wi),
        );
        let name = label
            .split_once(" · ")
            .map_or(label.as_str(), |(name, _)| name);
        let mut job = egui::text::LayoutJob::simple_singleline(
            name.to_string(),
            FontId::new(FS_SMALL, strong_font()),
            c_text_strong(),
        );
        job.wrap = egui::text::TextWrapping::truncate_at_width(rect.width() - GLYPH_W - 16.0);
        let galley = painter.layout_job(job);
        painter.galley(
            egui::pos2(
                rect.left() + 4.0 + GLYPH_W + 4.0,
                rect.center().y - galley.size().y * 0.5,
            ),
            galley,
            c_text_strong(),
        );
        if response.clicked() {
            ui.scroll_to_rect(header, Some(egui::Align::TOP));
        }
    }

    pub(super) fn render_project_drag_preview(&self, ctx: &egui::Context) {
        let Some(drag) = egui::DragAndDrop::payload::<ProjectDrag>(ctx) else {
            return;
        };
        ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
        let Some(pos) = ctx.pointer_interact_pos() else {
            return;
        };
        egui::Area::new(egui::Id::new("sidebar_project_drag_preview"))
            .order(egui::Order::Tooltip)
            .fixed_pos(pos + egui::vec2(14.0, -8.0))
            .interactable(false)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style())
                    .inner_margin(egui::Margin::symmetric(8, 3))
                    .show(ui, |ui| {
                        // Extend: a floating area near the window edge would wrap the name.
                        ui.add(
                            egui::Label::new(strong_rich(
                                drag.label.as_str(),
                                FS_SMALL,
                                c_text_strong(),
                            ))
                            .wrap_mode(egui::TextWrapMode::Extend),
                        );
                    });
            });
    }
}

/// The checkout to open for project `wi`: the `remembered` one if it still exists, else the
/// one (project or one of its listed `worktrees`) with the newest saved chat, else the project
/// itself.
fn project_checkout_to_open(
    workspaces: &[Workspace],
    wi: usize,
    worktrees: &[usize],
    remembered: Option<&str>,
) -> usize {
    let checkouts = || std::iter::once(wi).chain(worktrees.iter().copied());
    if let Some(root) = remembered
        && let Some(w) = checkouts().find(|&w| workspaces[w].root_path == root)
    {
        return w;
    }
    // Unsaved chats (the empty stand-in every checkout has) say nothing about recent use.
    let newest = |w: usize| {
        workspaces[w]
            .sessions
            .iter()
            .filter(|s| s.session_file.is_some())
            .map(|s| s.modified)
            .max()
    };
    checkouts()
        .filter_map(|w| newest(w).map(|t| (t, w)))
        .reduce(|best, next| if next.0 > best.0 { next } else { best })
        .map_or(wi, |(_, w)| w)
}

#[cfg(test)]
mod tests {
    use super::super::super::state::Workspace;
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

    fn saved_chat(age_secs: u64) -> crate::model::Session {
        let mut session = crate::app::OxiApp::blank_session("Saved");
        session.session_file = Some(format!("/chats/{age_secs}.jsonl"));
        session.modified = std::time::SystemTime::now() - std::time::Duration::from_secs(age_secs);
        session
    }

    #[test]
    fn a_project_reopens_the_checkout_last_used_in_it() {
        let mut workspaces = vec![
            workspace("/repo", None),
            workspace("/other", None),
            workspace("/trees/old", Some("/repo")),
            workspace("/trees/new", Some("/repo")),
            workspace("/trees/elsewhere", Some("/other")),
        ];
        let parents = sidebar_parents(&workspaces);
        let trees: Vec<usize> = (0..workspaces.len())
            .filter(|&w| parents[w] == Some(0) && w != 0)
            .collect();
        let pick =
            |ws: &[Workspace], remembered| project_checkout_to_open(ws, 0, &trees, remembered);
        assert_eq!(
            pick(&workspaces, None),
            0,
            "nothing saved: the project itself"
        );
        assert_eq!(
            pick(&workspaces, Some("/trees/old")),
            2,
            "the remembered work tree"
        );
        assert_eq!(
            pick(&workspaces, Some("/trees/elsewhere")),
            0,
            "another project's work tree is never picked"
        );
        workspaces[0].sessions.push(saved_chat(300));
        workspaces[2].sessions.push(saved_chat(200));
        workspaces[3].sessions.push(saved_chat(100));
        workspaces[4].sessions.push(saved_chat(10));
        let unsaved = crate::app::OxiApp::blank_session("New chat");
        workspaces[2].sessions.push(unsaved);
        assert_eq!(
            pick(&workspaces, None),
            3,
            "else the newest saved chat in the project"
        );
        assert_eq!(
            pick(&workspaces, Some("/trees/removed")),
            3,
            "a removed work tree falls back the same way"
        );
    }

    fn roots(list: &[&str]) -> Vec<String> {
        list.iter().map(|r| r.to_string()).collect()
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
            sidebar_workspace_order(&workspaces, &[], &[]),
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
            sidebar_workspace_order(&workspaces, &[], &[]),
            vec![(0, None), (1, None), (3, None), (2, Some(3)),]
        );
    }

    #[test]
    fn pinned_projects_lead_then_manual_order_then_the_rest() {
        let workspaces = vec![
            workspace("/a", None),
            workspace("/b", None),
            workspace("/c", None),
            workspace("/trees/c", Some("/c")),
            workspace("/d", None),
        ];
        let pinned = roots(&["/d", "/c"]);
        // "/a" has no manual slot, so it follows the placed projects.
        let manual = roots(&["/b", "/d"]);
        assert_eq!(
            sidebar_workspace_order(&workspaces, &pinned, &manual),
            vec![(4, None), (2, None), (3, Some(2)), (1, None), (0, None)],
            "work trees travel with their pinned project; indices never change"
        );
    }

    #[test]
    fn placing_moves_one_root_within_its_section() {
        let section = roots(&["/a", "/b", "/c"]);
        let place = |root, slot| placed(section.clone(), root, &slot);
        assert_eq!(place("/c", ProjectSlot::Start), roots(&["/c", "/a", "/b"]));
        assert_eq!(place("/a", ProjectSlot::End), roots(&["/b", "/c", "/a"]));
        assert_eq!(
            place("/a", ProjectSlot::After("/b".into())),
            roots(&["/b", "/a", "/c"])
        );
        assert_eq!(
            place("/c", ProjectSlot::Before("/b".into())),
            roots(&["/a", "/c", "/b"])
        );
        // Dropped in from the other section: inserted, nothing else moves.
        assert_eq!(
            place("/x", ProjectSlot::Before("/b".into())),
            roots(&["/a", "/x", "/b", "/c"])
        );
    }
}
