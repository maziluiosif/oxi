//! Tab-style switching, from anywhere in the app: ⌥⌘→ / ⌥⌘← (Ctrl+PgDn / PgUp elsewhere) open
//! the next or previous row the sidebar shows, top to bottom across projects: each chat, and a
//! folded project as one stop. It wraps around, as browser and editor tabs do. While a
//! document shows in the editor, the keys switch editor tabs instead (`handle_editor_tab_keys`
//! takes them first).

use super::*;
use crate::app::state::SessionKey;

/// Whether a chat gets a row in its list, given the list's fold state and the search.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Listing {
    Listed,
    /// Older than today in an unfolded list: counted in its "Show N older chats" link.
    Older,
    Hidden,
}

impl OxiApp {
    /// Whether the chat list of row `wi` is folded by the row itself: the project or work
    /// tree's own fold, or for the main checkout beside work trees, its "checkout" group.
    pub(super) fn sidebar_row_own_folded(&self, wi: usize, checkout_row: bool) -> bool {
        let workspace = &self.conv.workspaces[wi];
        if checkout_row {
            workspace.folded_groups.iter().any(|key| key == "checkout")
        } else {
            workspace.sidebar_folded
        }
    }

    /// Whether project `parent`'s fold hides the work trees under it. Search and History
    /// unfold everything.
    pub(super) fn sidebar_worktree_parent_folded(&self, parent: usize, q: &str) -> bool {
        self.conv.workspaces[parent].sidebar_folded && q.is_empty() && !self.conv.sidebar.history
    }

    /// Whether chat `si` of workspace `wi` gets a row: `folded` is its list's fold, `q` the
    /// lowercased search.
    pub(super) fn sidebar_session_listing(
        &mut self,
        wi: usize,
        si: usize,
        group: SidebarGroup,
        folded: bool,
        q: &str,
        ctx: &egui::Context,
    ) -> Listing {
        if self.sidebar_hides_placeholder(wi, si) {
            return Listing::Hidden;
        }
        // An orchestrator worker's chat shows above its lead's composer instead.
        let open_here = wi == self.conv.active_workspace && si == self.conv.workspaces[wi].active;
        if !open_here
            && self.hidden_worker_chat(
                self.conv.workspaces[wi].sessions[si]
                    .session_file
                    .as_deref(),
            )
        {
            return Listing::Hidden;
        }
        let searching = !q.is_empty();
        let history = self.conv.sidebar.history;
        // Folded: only the open chat stays; its running and waiting neighbours are listed
        // under "Needs you" / "Running" at the top instead. Unfolded, in-progress chats and
        // chats that need attention stay visible.
        let exceptional = if folded {
            self.sidebar_session_kept_when_folded(wi, si)
        } else {
            self.sidebar_session_needs_visibility(wi, si, ctx)
        };
        if !searching
            && !history
            && (!matches!(group, SidebarGroup::Pinned | SidebarGroup::Today) || folded)
            && !exceptional
        {
            return if !folded && group == SidebarGroup::Older {
                Listing::Older
            } else {
                Listing::Hidden
            };
        }
        if history && !self.sidebar_session_in_history_period(wi, si) {
            return Listing::Hidden;
        }
        if searching && !self.session_matches_search(wi, si, q) {
            return Listing::Hidden;
        }
        Listing::Listed
    }

    /// The stops ⌥⌘← / → walk, top to bottom: the "Needs you" and "Running" chats, then each
    /// project's chats, and a project row that shows no chats (folded, say) as one stop.
    /// Search and History walk only the chats they list. Walks the rows the way
    /// `render_sidebar_session_list` and `render_sidebar_workspace` draw them, so it answers
    /// the same with the sidebar hidden; `stepping_walks_the_rows_the_sidebar_draws` keeps the
    /// two in step.
    pub(crate) fn sidebar_stops(&mut self, ctx: &egui::Context) -> Vec<SidebarStop> {
        let mut stops = Vec::new();
        if self.conv.no_workspace && self.conv.workspaces.len() == 1 {
            return stops;
        }
        let q = self.conv.sidebar.search.trim().to_lowercase();
        let history = self.conv.sidebar.history;
        let daily = q.is_empty() && !history;
        if daily {
            stops.extend(
                self.sidebar_offscreen_activity()
                    .into_iter()
                    .map(|item| SidebarStop::Chat(self.session_key(item.wi, item.si))),
            );
        }
        let order = self.sidebar_order();
        for wi in self.sidebar_projects() {
            if history
                && self
                    .conv
                    .sidebar
                    .history_project
                    .as_ref()
                    .is_some_and(|root| root != &self.conv.workspaces[wi].root_path)
            {
                continue;
            }
            let worktrees: Vec<usize> = order
                .iter()
                .filter(|(_, parent)| *parent == Some(wi))
                .map(|&(child, _)| child)
                .collect();
            let before = stops.len();
            self.list_row_chats(ctx, wi, None, false, &q, &worktrees, &mut stops);
            if daily && stops.len() == before {
                stops.push(SidebarStop::Project(wi));
            }
        }
        let mut seen = std::collections::HashSet::new();
        stops.retain(|stop| seen.insert(*stop));
        stops
    }

    /// The chats under row `wi`, as `render_sidebar_workspace` lists them.
    #[allow(clippy::too_many_arguments)]
    fn list_row_chats(
        &mut self,
        ctx: &egui::Context,
        wi: usize,
        parent: Option<usize>,
        parent_folded: bool,
        q: &str,
        worktrees: &[usize],
        out: &mut Vec<SidebarStop>,
    ) {
        let checkout_row = parent == Some(wi);
        let searching_all = !q.is_empty() || self.conv.sidebar.history;
        let folded =
            !searching_all && (self.sidebar_row_own_folded(wi, checkout_row) || parent_folded);
        let is_main = self.conv.workspaces[wi].worktree.is_none();
        if parent.is_none() && is_main && !worktrees.is_empty() {
            if !folded || self.sidebar_workspace_has_kept_chats(wi) {
                self.list_row_chats(ctx, wi, Some(wi), folded, q, &[], out);
            }
            let (visible, other): (Vec<_>, Vec<_>) =
                worktrees.iter().copied().partition(|&child| {
                    searching_all || self.sidebar_workspace_has_recent_chats(child, ctx)
                });
            let other_open = !other.is_empty()
                && !folded
                && ctx.data_mut(|d| d.get_persisted::<bool>(other_worktrees_id(wi))) == Some(true);
            for child in visible
                .into_iter()
                .chain(other.into_iter().filter(|_| other_open))
            {
                let parent_folded = self.sidebar_worktree_parent_folded(wi, q);
                if parent_folded && !self.sidebar_workspace_has_kept_chats(child) {
                    continue;
                }
                self.list_row_chats(ctx, child, Some(wi), parent_folded, q, &[], out);
            }
            return;
        }
        for (si, group) in sidebar_session_order(&self.conv.workspaces[wi]) {
            if self.sidebar_session_listing(wi, si, group, folded, q, ctx) == Listing::Listed {
                out.push(SidebarStop::Chat(self.session_key(wi, si)));
            }
        }
    }

    /// Open the stop after (or before) the open chat, wrapping: the next chat, or the next
    /// project that shows none, at the chat last used there (as clicking its row does). The
    /// chat beside the open one in split view is skipped: opening it would close the split.
    pub(crate) fn step_sidebar(&mut self, ctx: &egui::Context, back: bool) {
        let partner = self.split_partner_peek().map(SidebarStop::Chat);
        let stops: Vec<SidebarStop> = self
            .sidebar_stops(ctx)
            .into_iter()
            .filter(|&stop| Some(stop) != partner)
            .collect();
        let chat = SidebarStop::Chat(self.active_session_key());
        let current = if stops.contains(&chat) {
            chat
        } else {
            SidebarStop::Project(self.sidebar_top_project(self.conv.active_workspace))
        };
        let Some(next) = stepped(&stops, current, back) else {
            return;
        };
        self.close_subagent(ctx);
        match next {
            SidebarStop::Chat(key) => {
                self.select_session_in_workspace(key.workspace_idx, key.session_idx)
            }
            SidebarStop::Project(wi) => self.switch_to_project(wi),
        }
        self.conv.sidebar.reveal_active = true;
    }
}

/// Where ⌥⌘← / → can land.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum SidebarStop {
    Chat(SessionKey),
    /// A project row with no chat rows under it: folded, or nothing from today.
    Project(usize),
}

/// A tab-style step through the sidebar.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SidebarStep {
    Next,
    Previous,
}

/// Takes the first tab-style step key out of this frame's input, so a focused composer,
/// editor or terminal never sees it.
pub(crate) fn take_sidebar_step(input: &mut egui::InputState) -> Option<SidebarStep> {
    let mac = cfg!(target_os = "macos");
    let step = |event: &egui::Event| match event {
        egui::Event::Key {
            key,
            physical_key,
            pressed: true,
            modifiers,
            ..
        } => step_key(*key, *physical_key, *modifiers, mac),
        _ => None,
    };
    let position = input
        .events
        .iter()
        .position(|event| step(event).is_some())?;
    step(&input.events.remove(position))
}

/// The step `key` with `modifiers` stands for: VS Code's and the browsers' tab keys (⌥⌘→ / ⌥⌘←
/// and ⇧⌘] / ⇧⌘[ on macOS, Ctrl+PgDn / PgUp elsewhere).
fn step_key(
    key: egui::Key,
    physical_key: Option<egui::Key>,
    modifiers: egui::Modifiers,
    mac: bool,
) -> Option<SidebarStep> {
    use SidebarStep::{Next, Previous};
    use egui::Key;
    if mac {
        let cmd = modifiers.mac_cmd && !modifiers.ctrl;
        if cmd && modifiers.alt && !modifiers.shift {
            return match key {
                Key::ArrowRight => Some(Next),
                Key::ArrowLeft => Some(Previous),
                _ => None,
            };
        }
        if cmd && modifiers.shift && !modifiers.alt {
            // Shift+] arrives as `}` on a US layout; the physical key is still `]`.
            return match physical_key.unwrap_or(key) {
                Key::CloseBracket => Some(Next),
                Key::OpenBracket => Some(Previous),
                _ => None,
            };
        }
        return None;
    }
    if !modifiers.ctrl || modifiers.alt || modifiers.shift {
        return None;
    }
    match key {
        Key::PageDown => Some(Next),
        Key::PageUp => Some(Previous),
        _ => None,
    }
}

/// Where the "Other worktrees" link under project `wi` keeps whether it is open.
pub(super) fn other_worktrees_id(wi: usize) -> egui::Id {
    egui::Id::new(("sidebar_other_worktrees", wi))
}

/// The entry after (or before) `current` in `order`, wrapping. From outside the list (the
/// open chat is filtered out by the search, say) forward starts at the top, back at the
/// bottom. `None` when there is nothing else to go to.
fn stepped<T: Copy + PartialEq>(order: &[T], current: T, back: bool) -> Option<T> {
    let n = order.len();
    let next = match order.iter().position(|&item| item == current) {
        Some(_) if n < 2 => return None,
        Some(at) if back => (at + n - 1) % n,
        Some(at) => (at + 1) % n,
        None if n == 0 => return None,
        None if back => n - 1,
        None => 0,
    };
    Some(order[next])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::Workspace;

    #[test]
    fn browser_tab_keys_step_through_the_sidebar() {
        use SidebarStep::{Next, Previous};
        use egui::{Key, Modifiers};
        let cmd_alt = Modifiers::MAC_CMD
            .plus(Modifiers::COMMAND)
            .plus(Modifiers::ALT);
        let mac = |key, modifiers| step_key(key, None, modifiers, true);
        assert_eq!(mac(Key::ArrowRight, cmd_alt), Some(Next));
        assert_eq!(mac(Key::ArrowLeft, cmd_alt), Some(Previous));
        assert_eq!(mac(Key::ArrowDown, cmd_alt), None);
        let cmd_shift = Modifiers::MAC_CMD
            .plus(Modifiers::COMMAND)
            .plus(Modifiers::SHIFT);
        assert_eq!(
            step_key(
                Key::CloseCurlyBracket,
                Some(Key::CloseBracket),
                cmd_shift,
                true
            ),
            Some(Next)
        );
        assert_eq!(
            mac(Key::ArrowRight, Modifiers::MAC_CMD.plus(Modifiers::COMMAND)),
            None,
            "⌘→ is end of line"
        );
        assert_eq!(
            mac(Key::ArrowRight, Modifiers::ALT),
            None,
            "⌥→ is next word"
        );

        let other = |key, modifiers| step_key(key, None, modifiers, false);
        let ctrl = Modifiers::CTRL.plus(Modifiers::COMMAND);
        assert_eq!(other(Key::PageDown, ctrl), Some(Next));
        assert_eq!(other(Key::PageUp, ctrl), Some(Previous));
        assert_eq!(other(Key::PageDown, ctrl.plus(Modifiers::SHIFT)), None);
        assert_eq!(other(Key::PageDown, Modifiers::NONE), None);
    }

    #[test]
    fn steps_wrap_and_enter_from_outside_the_list() {
        let order = [4, 7, 2];
        assert_eq!(stepped(&order, 7, false), Some(2));
        assert_eq!(stepped(&order, 2, false), Some(4), "wraps to the top");
        assert_eq!(stepped(&order, 4, true), Some(2), "wraps to the bottom");
        assert_eq!(stepped(&order, 9, false), Some(4), "from outside: the top");
        assert_eq!(
            stepped(&order, 9, true),
            Some(2),
            "from outside: the bottom"
        );
        assert_eq!(stepped(&[4], 4, false), None, "nothing else to open");
        assert_eq!(stepped(&[4], 9, false), Some(4));
        assert_eq!(stepped::<usize>(&[], 9, false), None);
    }

    fn chat(title: &str, file: &str, age_secs: u64) -> crate::model::Session {
        let mut session = OxiApp::blank_session(title);
        session.session_file = Some(format!("/chats/{file}.jsonl"));
        session.modified = std::time::SystemTime::now() - std::time::Duration::from_secs(age_secs);
        session
    }

    fn workspace(root: &str, sessions: Vec<crate::model::Session>) -> Workspace {
        Workspace {
            root_path: root.into(),
            sessions,
            active: 0,
            sidebar_folded: false,
            pinned: Vec::new(),
            archived: Vec::new(),
            folded_groups: Vec::new(),
            worktree: None,
        }
    }

    fn key(workspace_idx: usize, session_idx: usize) -> SessionKey {
        SessionKey {
            workspace_idx,
            session_idx,
        }
    }

    fn chat_stop(workspace_idx: usize, session_idx: usize) -> SidebarStop {
        SidebarStop::Chat(key(workspace_idx, session_idx))
    }

    /// Three projects: `/a` (two chats from today, one from last week), `/b` folded with a
    /// reply nobody has seen yet, and `/c` with one chat.
    fn app() -> OxiApp {
        crate::secrets::use_mock_store();
        let mut app = OxiApp::new();
        const WEEK: u64 = 7 * 24 * 3600;
        app.conv.workspaces = vec![
            workspace(
                "/tmp/oxi-step-a",
                vec![
                    chat("A1", "a1", 10),
                    chat("A2", "a2", 20),
                    chat("Old", "a3", WEEK),
                ],
            ),
            workspace(
                "/tmp/oxi-step-b",
                vec![chat("B1", "b1", 10), chat("B2", "b2", 20)],
            ),
            workspace("/tmp/oxi-step-c", vec![chat("C1", "c1", 10)]),
        ];
        app.conv.workspaces[1].sidebar_folded = true;
        app.conv.active_workspace = 0;
        app.conv.no_workspace = false;
        app.conv.settings.pinned_projects.clear();
        app.conv.settings.project_order = vec![
            "/tmp/oxi-step-a".into(),
            "/tmp/oxi-step-b".into(),
            "/tmp/oxi-step-c".into(),
        ];
        app.conv.settings.sidebar_focus_active_project = false;
        app.flow.sessions.clear();
        app.run_state_mut(key(1, 1)).completion_unseen = true;
        app
    }

    #[test]
    fn walks_needs_you_then_each_projects_chats_with_folded_projects_as_one_stop() {
        let mut app = app();
        let ctx = egui::Context::default();
        assert_eq!(
            app.sidebar_stops(&ctx),
            [
                chat_stop(1, 1),
                chat_stop(0, 0),
                chat_stop(0, 1),
                SidebarStop::Project(1),
                chat_stop(2, 0),
            ],
            "B2 finished unseen in folded B; last week's chat is not shown, B is one stop"
        );

        app.step_sidebar(&ctx, false);
        assert_eq!(app.active_session_key(), key(0, 1));
        app.step_sidebar(&ctx, false);
        assert_eq!(
            app.active_session_key(),
            key(1, 0),
            "folded B opens at its last chat"
        );
        assert!(app.conv.workspaces[1].sidebar_folded, "and stays folded");
        assert!(
            app.sidebar_stops(&ctx).contains(&chat_stop(1, 0)),
            "the open chat shows under the folded row"
        );
        app.step_sidebar(&ctx, false);
        assert_eq!(
            app.active_session_key(),
            key(2, 0),
            "on to the next project"
        );

        app.step_sidebar(&ctx, true);
        assert_eq!(app.active_session_key(), key(1, 0), "back into folded B");
        app.step_sidebar(&ctx, true);
        assert_eq!(app.active_session_key(), key(0, 1), "and out of it");
        app.step_sidebar(&ctx, true);
        app.step_sidebar(&ctx, true);
        assert_eq!(app.active_session_key(), key(1, 1), "Needs you comes first");
    }

    #[test]
    fn search_walks_only_the_matching_chats() {
        let mut app = app();
        let ctx = egui::Context::default();
        app.conv.sidebar.search = "2".into();
        assert_eq!(
            app.sidebar_stops(&ctx),
            [chat_stop(0, 1), chat_stop(1, 1)],
            "A2 and B2 match; no project stops while searching"
        );
    }

    #[test]
    fn stepping_walks_the_rows_the_sidebar_draws() {
        let mut app = app();
        let ctx = egui::Context::default();
        crate::theme::apply_theme(&ctx, "dark");
        let check = |app: &mut OxiApp, case: &str| {
            for _ in 0..2 {
                let _ = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(320.0, 2000.0),
                        )),
                        ..Default::default()
                    },
                    |ui| app.render_sidebar(ui),
                );
            }
            let drawn = app.conv.sidebar.drawn_stops.clone();
            assert!(!drawn.is_empty(), "{case}: rows were drawn");
            assert_eq!(app.sidebar_stops(&ctx), drawn, "{case}");
        };
        // A work tree of `/a`, so `/a` lists its main checkout and the work tree as rows.
        let mut tree = workspace("/tmp/oxi-step-a-tree", vec![chat("T1", "t1", 10)]);
        tree.worktree = Some(crate::git::worktree::WorktreeInfo {
            branch: "oxi/task".into(),
            main_root: "/tmp/oxi-step-a".into(),
            main_branch: "dev".into(),
        });
        app.conv.workspaces.push(tree);
        check(&mut app, "daily list");
        assert!(
            app.conv.sidebar.drawn_stops.contains(&chat_stop(3, 0)),
            "the work tree's chat is listed"
        );
        assert!(
            app.conv
                .sidebar
                .drawn_stops
                .contains(&SidebarStop::Project(1)),
            "folded B is a stop"
        );
        app.conv.workspaces[0].folded_groups.push("checkout".into());
        check(&mut app, "the main checkout folded");
        app.conv.workspaces[0].sidebar_folded = true;
        check(&mut app, "the open project folded");
        app.conv.sidebar.search = "1".into();
        check(&mut app, "searching");
        app.conv.sidebar.search.clear();
        app.conv.sidebar.history = true;
        check(&mut app, "history");
    }
}
