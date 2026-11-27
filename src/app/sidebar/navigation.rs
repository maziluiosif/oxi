//! Daily chat visibility, history filters and asynchronous checkout labels.
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use super::*;

type Branches = HashMap<String, Option<String>>;

#[derive(Default)]
pub(crate) struct BranchCache {
    pub values: Branches,
    rx: Option<Receiver<Branches>>,
    refreshed: Option<Instant>,
}

impl OxiApp {
    pub(super) fn poll_sidebar_branches(&mut self, ctx: &egui::Context) {
        let cache = &mut self.conv.sidebar.branches;
        if let Some(rx) = &cache.rx {
            match rx.try_recv() {
                Ok(values) => {
                    cache.values = values;
                    cache.rx = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    cache.rx = None;
                }
                Err(mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(Duration::from_millis(100));
                    return;
                }
            }
        }
        let paths: Vec<_> = self
            .conv
            .workspaces
            .iter()
            .enumerate()
            .filter(|(wi, _)| !self.is_remote_workspace(*wi))
            .map(|(_, ws)| ws.root_path.clone())
            .collect();
        ctx.request_repaint_after(Duration::from_secs(5));
        let cache = &mut self.conv.sidebar.branches;
        let stale = cache
            .refreshed
            .is_none_or(|time| time.elapsed() >= Duration::from_secs(5));
        if !stale && paths.iter().all(|path| cache.values.contains_key(path)) {
            return;
        }
        cache.refreshed = Some(Instant::now());
        let (tx, rx) = mpsc::channel();
        cache.rx = Some(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let values = paths
                .into_iter()
                .map(|path| {
                    let branch = git2::Repository::discover(&path).ok().and_then(|repo| {
                        let head = repo.head().ok()?;
                        if head.is_branch() {
                            head.shorthand().ok().map(str::to_owned)
                        } else {
                            head.target()
                                .map(|id| format!("detached · {}", &id.to_string()[..8]))
                        }
                    });
                    (path, branch)
                })
                .collect();
            let _ = tx.send(values);
            ctx.request_repaint();
        });
    }

    pub(super) fn sidebar_branch_label(&self, wi: usize) -> String {
        self.conv
            .sidebar
            .branches
            .values
            .get(&self.conv.workspaces[wi].root_path)
            .and_then(|branch| branch.clone())
            .or_else(|| {
                self.conv.workspaces[wi]
                    .worktree
                    .as_ref()
                    .map(|info| info.branch.clone())
            })
            .or_else(|| {
                self.conv.workspaces.iter().find_map(|ws| {
                    ws.worktree
                        .as_ref()
                        .filter(|info| {
                            std::path::Path::new(&self.conv.workspaces[wi].root_path)
                                .starts_with(&info.main_root)
                        })
                        .map(|info| info.main_branch.clone())
                })
            })
            .unwrap_or_else(|| "Checkout".into())
    }

    pub(super) fn sidebar_session_needs_visibility(
        &self,
        wi: usize,
        si: usize,
        ctx: &egui::Context,
    ) -> bool {
        let key = self.session_key(wi, si);
        key == self.active_session_key()
            || self.split_partner_peek() == Some(key)
            || self.conv.sidebar.renaming_session == Some((wi, si))
            || self.session_row_is_running(wi, si)
            || self.run_state(key).is_some_and(|run| {
                run.completion_unseen
                    || (run.pending_approval.is_some()
                        && (key != self.active_session_key() || !self.active_chat_is_visible(ctx)))
            })
    }

    /// Rows a folded list still draws: the chat open in the main view or beside it, and one
    /// being renamed. Running and waiting chats of a folded list move to the sidebar's
    /// "Needs you" and "Running" sections instead (see `sidebar_offscreen_activity`).
    pub(super) fn sidebar_session_kept_when_folded(&self, wi: usize, si: usize) -> bool {
        let key = self.session_key(wi, si);
        key == self.active_session_key()
            || self.split_partner_peek() == Some(key)
            || self.conv.sidebar.renaming_session == Some((wi, si))
    }

    pub(super) fn sidebar_workspace_has_kept_chats(&self, wi: usize) -> bool {
        (0..self.conv.workspaces[wi].sessions.len())
            .any(|si| self.sidebar_session_kept_when_folded(wi, si))
    }

    /// Whether workspace `wi`'s chat list is unfolded in the sidebar: neither its project nor
    /// its own row (a work tree, or the main checkout beside work trees) is folded. Search and
    /// History unfold everything, but this answers for the daily list.
    pub(super) fn sidebar_chats_expanded(&self, wi: usize, parents: &[Option<usize>]) -> bool {
        let workspaces = &self.conv.workspaces;
        match parents.get(wi).copied().flatten() {
            Some(parent) => !workspaces[parent].sidebar_folded && !workspaces[wi].sidebar_folded,
            None => {
                let has_worktrees = parents.contains(&Some(wi));
                !workspaces[wi].sidebar_folded
                    && !(has_worktrees
                        && workspaces[wi]
                            .folded_groups
                            .iter()
                            .any(|key| key == "checkout"))
            }
        }
    }

    pub(super) fn sidebar_workspace_has_recent_chats(
        &self,
        wi: usize,
        ctx: &egui::Context,
    ) -> bool {
        sidebar_session_order(&self.conv.workspaces[wi])
            .iter()
            .any(|&(si, group)| {
                !self.sidebar_hides_placeholder(wi, si)
                    && (matches!(group, SidebarGroup::Pinned | SidebarGroup::Today)
                        || self.sidebar_session_needs_visibility(wi, si, ctx))
            })
            || wi == self.conv.active_workspace
    }

    pub(super) fn sidebar_session_in_history_period(&self, wi: usize, si: usize) -> bool {
        let day =
            chrono::DateTime::<chrono::Local>::from(self.conv.workspaces[wi].sessions[si].modified)
                .date_naive();
        self.conv
            .sidebar
            .history_days
            .is_none_or(|days| (chrono::Local::now().date_naive() - day).num_days() < days)
    }

    pub(super) fn render_sidebar_history_filters(&mut self, ui: &mut Ui) {
        ui.label(strong_rich("History", FS_SMALL, c_text_strong()));
        ui.add_space(4.0);
        let projects: Vec<_> = self
            .sidebar_order()
            .into_iter()
            .filter(|(_, parent)| parent.is_none())
            .map(|(wi, _)| self.conv.workspaces[wi].root_path.clone())
            .collect();
        let selected = self
            .conv
            .sidebar
            .history_project
            .as_deref()
            .map(workspace_sidebar_label)
            .unwrap_or_else(|| "All projects".into());
        let menu_w = ui.available_width().max(80.0);
        let picked = crate::ui::select_menu::SelectMenu::new("sidebar_history_project", &selected)
            .framed(menu_w)
            .show(ui, || {
                let mut opts = Vec::with_capacity(projects.len() + 1);
                opts.push(crate::ui::select_menu::SelectOption::new(
                    "All projects",
                    self.conv.sidebar.history_project.is_none(),
                ));
                for root in &projects {
                    let is_sel =
                        self.conv.sidebar.history_project.as_deref() == Some(root.as_str());
                    opts.push(crate::ui::select_menu::SelectOption::new(
                        workspace_sidebar_label(root),
                        is_sel,
                    ));
                }
                opts
            });
        if let Some(idx) = picked {
            self.conv.sidebar.history_project = if idx == 0 {
                None
            } else {
                projects.get(idx - 1).cloned()
            };
        }
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
            for (days, label) in [
                (None, "All"),
                (Some(1), "Today"),
                (Some(7), "7d"),
                (Some(30), "30d"),
            ] {
                let active = self.conv.sidebar.history_days == days;
                let (bg, border, fg) = if active {
                    (c_pill_selected_bg(), c_pill_selected_border(), c_accent())
                } else {
                    (c_bg_input(), c_border_subtle(), c_text_muted())
                };
                let btn = egui::Button::new(RichText::new(label).size(FS_TINY).color(fg))
                    .fill(bg)
                    .stroke(Stroke::new(1.0, border))
                    .corner_radius(CornerRadius::same(RADIUS_CHIP))
                    .min_size(egui::vec2(0.0, COMPACT_HEIGHT));
                if ui
                    .add(btn)
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
                    self.conv.sidebar.history_days = days;
                }
            }
        });
        ui.add_space(SPACE_SM);
    }
}
