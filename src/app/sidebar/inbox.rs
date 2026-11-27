//! "Needs you" and "Running": chats across all projects that are working or waiting on the
//! user and are not already visible in an unfolded project below. Each row carries its
//! project's color and name, so work running elsewhere stays one click away without repeating
//! rows the sidebar already shows.

use super::projects::sidebar_parents;
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Activity {
    /// A tool call is waiting for approval; the run is blocked.
    Approval,
    /// The last run failed.
    Error,
    /// A reply finished while the chat was out of view.
    Done,
    Running,
}

impl Activity {
    pub(super) fn needs_you(self) -> bool {
        self != Self::Running
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ActivityItem {
    pub wi: usize,
    pub si: usize,
    pub activity: Activity,
}

impl OxiApp {
    /// Working or waiting chats whose rows are not drawn in place: their project or work tree
    /// is folded, and they are not the chat open in the main view.
    pub(crate) fn sidebar_offscreen_activity(&self) -> Vec<ActivityItem> {
        let parents = sidebar_parents(&self.conv.workspaces);
        let mut items = Vec::new();
        for (wi, workspace) in self.conv.workspaces.iter().enumerate() {
            if self.sidebar_chats_expanded(wi, &parents) {
                continue;
            }
            for (si, session) in workspace.sessions.iter().enumerate() {
                if self.sidebar_session_kept_when_folded(wi, si) {
                    continue;
                }
                // An orchestrator worker's chat shows above its lead's composer instead.
                if self.hidden_worker_chat(session.session_file.as_deref()) {
                    continue;
                }
                let run = self.run_state(self.session_key(wi, si));
                let activity = if run.is_some_and(|run| run.pending_approval.is_some()) {
                    Activity::Approval
                } else if self.session_row_is_running(wi, si) {
                    Activity::Running
                } else if self.session_row_has_error(wi, si) {
                    Activity::Error
                } else if run.is_some_and(|run| run.completion_unseen) {
                    Activity::Done
                } else {
                    continue;
                };
                items.push(ActivityItem { wi, si, activity });
            }
        }
        // Blocked runs first, then failures, then finished replies; stable within each.
        items.sort_by_key(|item| item.activity as u8);
        items
    }

    /// The two sections above the projects. Returns whether the selection changed.
    pub(super) fn render_sidebar_inbox(&mut self, ui: &mut Ui, show: bool) -> bool {
        let items = self.sidebar_offscreen_activity();
        let hidden = &mut self.conv.sidebar.hidden_activity;
        hidden.clear();
        for item in &items {
            let counts = hidden.entry(item.wi).or_default();
            if item.activity.needs_you() {
                counts.0 += 1;
            } else {
                counts.1 += 1;
            }
        }
        if !show || items.is_empty() {
            return false;
        }
        let (needs, running): (Vec<_>, Vec<_>) = items
            .into_iter()
            .partition(|item| item.activity.needs_you());
        let mut open = None;
        for (label, list) in [("Needs you", needs), ("Running", running)] {
            if list.is_empty() {
                continue;
            }
            ui.add_space(SPACE_XS);
            ui.horizontal(|ui| {
                ui.set_height(SIDEBAR_HEADER_H);
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!("{label} · {}", list.len()))
                        .size(FS_TINY)
                        .color(c_sidebar_section()),
                );
            });
            for item in list {
                #[cfg(test)]
                self.conv
                    .sidebar
                    .drawn_stops
                    .push(super::stepping::SidebarStop::Chat(
                        self.session_key(item.wi, item.si),
                    ));
                if self.render_inbox_row(ui, item) {
                    open = Some((item.wi, item.si));
                }
                ui.add_space(1.0);
            }
        }
        ui.add_space(SPACE_SM);
        if let Some((wi, si)) = open {
            self.close_subagent(ui.ctx());
            self.select_session_in_workspace(wi, si);
            return true;
        }
        false
    }

    /// One chat: status glyph, title, and its project tag (color dot + name) on the right.
    fn render_inbox_row(&self, ui: &mut Ui, item: ActivityItem) -> bool {
        let ActivityItem { wi, si, activity } = item;
        let title = sidebar_session_title_display(&self.conv.workspaces[wi].sessions[si].title);
        let top = self.sidebar_top_project(wi);
        let project = self.sidebar_project_label(top);
        let tag_color = self.sidebar_project_color(top);
        let place = match &self.conv.workspaces[wi].worktree {
            Some(info) if top != wi => format!("{project} · {}", info.branch),
            _ => project.clone(),
        };
        let (rect, response) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), SIDEBAR_ROW_H),
            Sense::click(),
        );
        response.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Button,
                ui.is_enabled(),
                format!("{title} — {place}"),
            )
        });
        let hovered = ui.rect_contains_pointer(rect) || response.has_focus();
        let radius = CornerRadius::same(RADIUS_ROW);
        if hovered {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            ui.painter().rect_filled(rect, radius, c_row_hover());
        }
        crate::ui::chrome::keyboard_focus_ring(ui, &response, radius);

        // Leading status glyph in a fixed slot so titles line up.
        const LEAD_W: f32 = 16.0;
        let lead = egui::Rect::from_center_size(
            egui::pos2(rect.left() + 6.0 + LEAD_W * 0.5, rect.center().y),
            egui::vec2(12.0, 12.0),
        );
        match activity {
            Activity::Approval => {
                ui.painter().text(
                    lead.center(),
                    egui::Align2::CENTER_CENTER,
                    ICON_WARNING,
                    FontId::new(FS_TINY, icon_font()),
                    c_accent(),
                );
            }
            Activity::Error => {
                ui.painter().circle_filled(lead.center(), 3.5, c_danger());
            }
            Activity::Done => {
                ui.painter().circle_filled(lead.center(), 3.5, c_accent());
            }
            Activity::Running => match self.stall_for(self.session_key(wi, si)) {
                Some(_) => {
                    ui.painter()
                        .circle_filled(lead.center(), 3.5, c_warning_fg());
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_secs(1));
                }
                None => paint_spinner(ui, lead, c_text_muted()),
            },
        }

        // Project tag on the right: never wider than ~40% of the row.
        let tag_galley = ui.painter().layout_no_wrap(
            project.clone(),
            FontId::proportional(FS_TINY),
            c_text_faint(),
        );
        let tag_max = (rect.width() * 0.4).max(40.0);
        let tag_w = tag_galley.size().x.min(tag_max);
        let tag_right = rect.right() - 8.0;
        let tag_rect = egui::Rect::from_min_max(
            egui::pos2(tag_right - tag_w, rect.top()),
            egui::pos2(tag_right, rect.bottom()),
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(tag_rect), |ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add(
                    egui::Label::new(RichText::new(&project).size(FS_TINY).color(c_text_faint()))
                        .truncate()
                        .selectable(false),
                );
            });
        });
        ui.painter().circle_filled(
            egui::pos2(tag_rect.left() - 7.0, rect.center().y),
            3.0,
            tag_color,
        );

        let title_rect = egui::Rect::from_min_max(
            egui::pos2(rect.left() + 6.0 + LEAD_W + 4.0, rect.top()),
            egui::pos2(tag_rect.left() - 16.0, rect.bottom()),
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(title_rect), |ui| {
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.add(
                    egui::Label::new(RichText::new(&title).size(FS_SMALL).color(c_text()))
                        .truncate()
                        .selectable(false),
                );
            });
        });

        let status = match activity {
            Activity::Approval => "Waiting for your approval",
            Activity::Error => "Stopped with an error",
            Activity::Done => "Finished while you were away",
            Activity::Running => "Working",
        };
        response
            .on_hover_text(format!("{title}\n{place} · {status}"))
            .clicked()
    }
}
