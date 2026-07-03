//! Source-control panel (right side): changes, commit, branch switching, history, diff.

use eframe::egui::text::{LayoutJob, TextFormat, TextWrapping};
use eframe::egui::{
    self, Align, Color32, FontId, Frame, Layout, Margin, RichText, Rounding, ScrollArea, Sense, Ui,
};

use crate::git::{GitEntry, GitOp, GitState};
use crate::theme::*;

use super::OxiApp;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GitTab {
    #[default]
    Changes,
    Branches,
    History,
}

pub const GIT_W_MIN: f32 = 240.0;
pub const GIT_W_MAX: f32 = 640.0;

impl OxiApp {
    pub(crate) fn toggle_git_panel(&mut self) {
        self.conv.git_open = !self.conv.git_open;
        self.conv.settings.git_open = self.conv.git_open;
        if let Err(e) = self.conv.settings.save() {
            self.run_state_mut(self.active_session_key()).stream_error =
                Some(format!("Save settings: {e}"));
        }
        if self.conv.git_open {
            // Ensure the worker exists and request an initial snapshot.
            self.ensure_git_channels();
            let _ = self.conv.git_tx.as_ref().map(|t| t.send(GitOp::Refresh));
        }
    }

    pub(crate) fn bind_git_ctx(&mut self, ctx: &egui::Context) {
        self.conv.git_ctx = ctx.clone();
    }

    /// Tell the git worker the active workspace changed, so it re-roots and
    /// refreshes. Called from `select_workspace` / new-workspace flows.
    pub(crate) fn refresh_git_cwd(&mut self) {
        if self.conv.git_rx.is_none() {
            return;
        }
        let cwd = self.active_workspace().root_path.clone();
        let _ = self
            .conv
            .git_tx
            .as_ref()
            .map(|t| t.send(GitOp::SetCwd(cwd)));
    }

    /// Make sure the git worker thread exists and is rooted at the active workspace.
    /// `ensure_git_channels` lazily creates it using the real egui context.
    fn ensure_git_channels(&mut self) {
        if self.conv.git_rx.is_none() {
            let cwd = self.active_workspace().root_path.clone();
            let chan = crate::git::GitChannels::new(cwd, self.conv.git_ctx.clone());
            self.conv.git_tx = Some(chan.tx);
            self.conv.git_rx = Some(chan.rx);
            // Optimistic busy marker so the panel doesn't flash "not a repo" before
            // the first snapshot arrives.
            self.conv.git.busy = true;
            self.conv.git.last_op = Some("refresh".to_string());
        }
    }

    pub(crate) fn drain_git(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.conv.git_rx.as_ref() else {
            return;
        };
        let mut latest: Option<GitState> = None;
        // The diff collected for the commit-message generator can arrive on any drained
        // state, so capture it across all of them rather than only the last one.
        let mut collected_diff: Option<String> = None;
        while let Ok(state) = rx.try_recv() {
            if let Some(diff) = &state.commit_diff {
                collected_diff = Some(diff.clone());
            }
            latest = Some(state);
        }
        if let Some(state) = latest {
            self.conv.git = state;
            ctx.request_repaint();
        }
        if let Some(diff) = collected_diff {
            if self.conv.commit_gen_pending {
                self.start_commit_gen(&diff);
                ctx.request_repaint();
            }
        }
    }

    /// Kick off the LLM completion for the commit message once the diff is in hand.
    fn start_commit_gen(&mut self, diff: &str) {
        self.conv.commit_gen_pending = false;
        let Some(profile) = self.conv.settings.commit_msg_profile().cloned() else {
            self.conv.commit_gen_error =
                Some("No provider profile configured for commit messages.".to_string());
            return;
        };
        let system_prompt = self.conv.settings.commit_msg_system_prompt.clone();
        let user_prompt =
            format!("Write a git commit message for the following diff.\n\n```diff\n{diff}\n```");
        let (rx, _handle) = crate::agent::spawn_completion(crate::agent::CompleteRequest {
            profile,
            system_prompt,
            user_prompt,
            max_chars: Some(1500),
        });
        self.conv.git_commit_message.clear();
        self.conv.commit_gen_error = None;
        self.conv.commit_gen_rx = Some(rx);
    }

    /// Drain streamed commit-message deltas into the composer. Called each frame.
    pub(crate) fn drain_commit_gen(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.conv.commit_gen_rx.as_ref() else {
            return;
        };
        let mut done = false;
        loop {
            match rx.try_recv() {
                Ok(crate::agent::CompleteEvent::Delta(d)) => {
                    self.conv.git_commit_message.push_str(&d);
                    ctx.request_repaint();
                }
                Ok(crate::agent::CompleteEvent::Done(result)) => {
                    match result {
                        Ok(text) => {
                            let trimmed = text.trim();
                            if !trimmed.is_empty() {
                                self.conv.git_commit_message = trimmed.to_string();
                            }
                        }
                        Err(e) => self.conv.commit_gen_error = Some(e),
                    }
                    done = true;
                    ctx.request_repaint();
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    done = true;
                    break;
                }
            }
        }
        if done {
            self.conv.commit_gen_rx = None;
        }
    }

    /// True while a commit message is being collected or generated.
    fn commit_gen_active(&self) -> bool {
        self.conv.commit_gen_pending || self.conv.commit_gen_rx.is_some()
    }

    /// Render the right-side git column (allocated after the chat area).
    pub(crate) fn render_git_panel(&mut self, ui: &mut Ui, full_h: f32) {
        let _ = full_h;

        // The panel can come up open straight from settings (git_open persisted), in
        // which case no one went through `toggle_git_panel` — make sure the worker
        // exists so we don't sit on a stale "Not a git repository" default state.
        if self.conv.git_rx.is_none() {
            self.ensure_git_channels();
            let _ = self.conv.git_tx.as_ref().map(|t| t.send(GitOp::Refresh));
        }
        ui.set_min_width(ui.max_rect().width());
        ui.set_min_height(ui.max_rect().height());

        Frame::none()
            .fill(c_bg_sidebar())
            .inner_margin(Margin {
                left: 8.0,
                right: 8.0,
                top: 8.0,
                bottom: 8.0,
            })
            .show(ui, |ui| {
                ui.set_min_width(ui.max_rect().width());
                ui.set_min_height(ui.max_rect().height());

                self.render_git_header(ui);
                ui.add_space(8.0);
                self.render_git_tabs(ui);
                ui.add_space(6.0);

                if !self.conv.git.repo && !self.conv.git.busy {
                    ui.add_space(12.0);
                    ui.label(
                        RichText::new("Not a git repository")
                            .size(FS_SMALL)
                            .color(c_text_muted()),
                    );
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(format!(
                            "Workspace root isn't inside a git worktree:\n{}",
                            self.active_workspace().root_path
                        ))
                        .size(FS_TINY)
                        .color(c_text_faint()),
                    );
                    return;
                }

                if let Some(err) = self.conv.git.error.clone() {
                    crate::ui::chrome::alert_banner(ui, &err, true);
                    ui.add_space(6.0);
                }

                if self.conv.git.busy {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new().size(12.0).color(c_text_muted()));
                        ui.label(
                            RichText::new(
                                self.conv
                                    .git
                                    .last_op
                                    .clone()
                                    .unwrap_or_else(|| "working".to_string()),
                            )
                            .size(FS_TINY)
                            .color(c_text_muted()),
                        );
                    });
                    ui.add_space(4.0);
                }

                match self.conv.git_tab {
                    GitTab::Changes => self.render_git_changes(ui),
                    GitTab::Branches => self.render_git_branches(ui),
                    GitTab::History => self.render_git_history(ui),
                }
            });
        ui.expand_to_include_rect(ui.max_rect());
    }

    fn render_git_header(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.label(
                RichText::new(ICON_GIT)
                    .font(FontId::new(FS_H3, icon_font()))
                    .color(c_accent())
                    .strong(),
            );
            ui.label(
                RichText::new("Source Control")
                    .size(FS_H3)
                    .color(c_text())
                    .strong(),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if crate::ui::chrome::icon_button_plain(ui, ICON_REFRESH, 22.0, false)
                    .on_hover_text("Refresh")
                    .clicked()
                {
                    self.ensure_git_channels();
                    let _ = self.conv.git_tx.as_ref().map(|t| t.send(GitOp::Refresh));
                }
                if crate::ui::chrome::icon_button_plain(ui, ICON_CHEVRON_RIGHT, 22.0, false)
                    .on_hover_text("Hide git panel")
                    .clicked()
                {
                    self.toggle_git_panel();
                }
            });
        });

        // Branch + ahead/behind line
        if self.conv.git.repo {
            ui.add_space(2.0);
            let branch = self.conv.git.branch.clone();
            let (ahead, behind) = (self.conv.git.ahead, self.conv.git.behind);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.label(
                    RichText::new(ICON_BRANCH)
                        .font(FontId::new(FS_TINY, icon_font()))
                        .color(c_text_muted()),
                );
                ui.label(
                    RichText::new(branch)
                        .size(FS_TINY)
                        .color(c_text_muted())
                        .monospace(),
                );
                if ahead > 0 {
                    ui.label(
                        RichText::new(format!("↑{ahead}"))
                            .size(FS_TINY)
                            .color(c_success())
                            .monospace(),
                    )
                    .on_hover_text("Commits ahead of upstream");
                }
                if behind > 0 {
                    ui.label(
                        RichText::new(format!("↓{behind}"))
                            .size(FS_TINY)
                            .color(c_warning_fg())
                            .monospace(),
                    )
                    .on_hover_text("Commits behind upstream");
                }
            });
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
                if crate::ui::chrome::mini_button_icon(ui, ICON_DOWNLOAD, "Pull")
                    .on_hover_text("Pull (fast-forward only)")
                    .clicked()
                {
                    self.request(GitOp::Pull);
                }
                if crate::ui::chrome::mini_button_icon(ui, ICON_UPLOAD, "Push")
                    .on_hover_text("Push")
                    .clicked()
                {
                    self.request(GitOp::Push);
                }
                if crate::ui::chrome::mini_button_icon(ui, ICON_REFRESH, "Fetch")
                    .on_hover_text("Fetch")
                    .clicked()
                {
                    self.request(GitOp::Fetch);
                }
            });
        }
    }

    fn render_git_tabs(&mut self, ui: &mut Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
            for (tab, label) in [
                (GitTab::Changes, "Changes"),
                (GitTab::Branches, "Branches"),
                (GitTab::History, "History"),
            ] {
                let selected = self.conv.git_tab == tab;
                let n = match tab {
                    GitTab::Changes => self.conv.git.staged.len() + self.conv.git.unstaged.len(),
                    GitTab::Branches => self.conv.git.branches.len(),
                    GitTab::History => self.conv.git.log.len(),
                };
                let label = if n > 0 {
                    format!("{label} {n}")
                } else {
                    label.to_string()
                };
                if crate::ui::chrome::pill_tab(ui, &label, selected) {
                    self.conv.git_tab = tab;
                }
            }
        });
    }

    fn request(&self, op: GitOp) {
        let _ = self.conv.git_tx.as_ref().map(|t| t.send(op));
    }

    fn render_git_changes(&mut self, ui: &mut Ui) {
        // Commit composer
        ui.label(
            RichText::new("Message")
                .size(FS_TINY)
                .color(c_text_muted())
                .strong(),
        );
        ui.add_space(2.0);
        let resp = egui::TextEdit::multiline(&mut self.conv.git_commit_message)
            .frame(true)
            .hint_text(
                RichText::new("Commit message…")
                    .size(FS_SMALL)
                    .color(c_text_faint()),
            )
            .desired_rows(3)
            .desired_width(ui.available_width())
            .font(FontId::proportional(FS_SMALL));
        ui.add(resp);
        ui.add_space(4.0);

        let gen_active = self.commit_gen_active();
        let staged_empty = self.conv.git.staged.is_empty();
        let msg_empty = self.conv.git_commit_message.trim().is_empty();
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
            let commit_resp = ui
                .add_enabled(
                    !staged_empty && !msg_empty,
                    crate::ui::chrome::primary_button_icon_widget(ICON_CHECK, "Commit"),
                )
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text("Commit staged changes")
                .on_disabled_hover_text(if staged_empty {
                    "Stage some changes first"
                } else {
                    "Write or generate a commit message"
                });
            if commit_resp.clicked() {
                let msg = self.conv.git_commit_message.clone();
                self.request(GitOp::Commit(msg));
                self.conv.git_commit_message.clear();
            }
            let (gen_icon, gen_label) = if gen_active {
                ("", "Generating…")
            } else {
                (ICON_MAGIC, "Generate")
            };
            if crate::ui::chrome::ghost_button_icon_enabled(
                ui,
                gen_icon,
                gen_label,
                false,
                !gen_active,
            )
            .on_hover_text(
                "Generate a commit message from the staged diff with the configured model",
            )
            .clicked()
            {
                // Ask the worker for the diff; the response handler starts the LLM run.
                self.conv.commit_gen_error = None;
                self.conv.commit_gen_pending = true;
                self.request(GitOp::CollectCommitDiff);
            }
            let has_unstaged = !self.conv.git.unstaged.is_empty();
            if ui
                .add_enabled(
                    has_unstaged,
                    crate::ui::chrome::ghost_button_widget("Stage all", false),
                )
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
            {
                let paths: Vec<String> = self
                    .conv
                    .git
                    .unstaged
                    .iter()
                    .map(|e| e.path.clone())
                    .collect();
                self.request(GitOp::Stage(paths));
            }
            if ui
                .add_enabled(
                    !staged_empty,
                    crate::ui::chrome::ghost_button_widget("Unstage all", false),
                )
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
            {
                let paths: Vec<String> = self
                    .conv
                    .git
                    .staged
                    .iter()
                    .map(|e| e.path.clone())
                    .collect();
                self.request(GitOp::Unstage(paths));
            }
        });

        if let Some(err) = self.conv.commit_gen_error.clone() {
            ui.add_space(4.0);
            ui.label(
                RichText::new(format!("Generate failed: {err}"))
                    .size(FS_TINY)
                    .color(crate::theme::c_error_fg()),
            );
        }

        ui.add_space(8.0);
        crate::ui::chrome::hairline(ui);
        ui.add_space(6.0);

        // The diff is now shown as an overlay over the chat window instead of
        // inline in the sidebar; nothing to render here.

        let available_h = ui.available_height();
        let (staged, unstaged) = (self.conv.git.staged.clone(), self.conv.git.unstaged.clone());
        ScrollArea::vertical()
            .id_salt("git_changes_scroll")
            .max_height(available_h)
            .auto_shrink([false, true])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
            .show(ui, |ui| {
                if !staged.is_empty() {
                    self.render_section(ui, "Staged Changes", &staged, true);
                    ui.add_space(6.0);
                }
                if !unstaged.is_empty() {
                    self.render_section(ui, "Changes", &unstaged, false);
                }
                if staged.is_empty() && unstaged.is_empty() {
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        ui.label(
                            RichText::new(ICON_CHECK_CIRCLE)
                                .font(FontId::new(FS_SMALL, icon_font()))
                                .color(c_success()),
                        );
                        ui.label(
                            RichText::new("Working tree clean")
                                .size(FS_SMALL)
                                .color(c_text_muted()),
                        );
                    });
                }
            });
    }

    fn render_section(&mut self, ui: &mut Ui, title: &str, entries: &[GitEntry], staged: bool) {
        ui.label(
            RichText::new(title.to_uppercase())
                .size(FS_TINY)
                .color(c_text_faint())
                .strong(),
        );
        ui.add_space(2.0);
        let total = entries.len();
        for (i, entry) in entries.iter().enumerate() {
            ui.push_id((title, i), |ui| {
                self.render_change_row(ui, entry, staged, i, total);
            });
        }
    }

    fn render_change_row(
        &mut self,
        ui: &mut Ui,
        entry: &GitEntry,
        staged: bool,
        _i: usize,
        _total: usize,
    ) {
        let full_w = ui.available_width();
        let (rect, response) = ui.allocate_exact_size(egui::vec2(full_w, 22.0), Sense::click());
        let hovered = response.hovered();
        // Pure geometric hover test — `Ui::rect_contains_pointer` also checks layer
        // and clip, which the nested child layouts below fail even with the pointer
        // visually on the row.
        let row_hot = ui
            .input(|i| i.pointer.hover_pos())
            .is_some_and(|p| rect.contains(p));

        // Selection highlight if this is the currently-viewed diff.
        let selected = self
            .conv
            .git
            .current_diff_path
            .as_deref()
            .map(|p| p == entry.path)
            .unwrap_or(false)
            && self.conv.git.current_diff_staged.unwrap_or(false) == staged;

        let fill = if selected {
            c_row_active()
        } else if hovered {
            c_row_hover()
        } else {
            Color32::TRANSPARENT
        };
        ui.painter()
            .rect_filled(rect, Rounding::same(crate::theme::RADIUS_ROW), fill);

        ui.allocate_new_ui(
            egui::UiBuilder::new().max_rect(rect.shrink2(egui::vec2(6.0, 0.0))),
            |ui| {
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    // Action button: stage (when unstaged) / unstage (when staged)
                    if staged {
                        if crate::ui::chrome::icon_button_inline(
                            ui,
                            ICON_CLOSE,
                            FS_TINY,
                            c_text_muted(),
                        )
                        .on_hover_text("Unstage")
                        .clicked()
                        {
                            self.request(GitOp::Unstage(vec![entry.path.clone()]));
                        }
                    } else if crate::ui::chrome::icon_button_inline(
                        ui,
                        ICON_PLUS,
                        FS_TINY,
                        c_text_muted(),
                    )
                    .on_hover_text("Stage")
                    .clicked()
                    {
                        self.request(GitOp::Stage(vec![entry.path.clone()]));
                    }
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(format!("{:<1}", entry.status))
                            .size(FS_SMALL)
                            .color(status_color(entry.status))
                            .strong(),
                    );
                    ui.add_space(6.0);
                    // Filename first, parent dir faint after — the name is what you scan for.
                    let (dir, file) = match entry.path.rsplit_once('/') {
                        Some((d, f)) => (Some(d), f),
                        None => (None, entry.path.as_str()),
                    };
                    let mut job = LayoutJob::default();
                    job.append(
                        file,
                        0.0,
                        TextFormat::simple(FontId::proportional(FS_SMALL), c_text()),
                    );
                    if let Some(d) = dir {
                        job.append(
                            d,
                            8.0,
                            TextFormat::simple(FontId::proportional(FS_TINY), c_text_faint()),
                        );
                    }
                    // Always reserve a fixed slot on the right for the hover-only
                    // discard button — a truncating label otherwise swallows the
                    // whole row width and the button never gets space on narrow
                    // panels. Reserving it on staged rows too keeps both sections
                    // truncating at the same column.
                    const ACTION_W: f32 = 22.0;
                    let label_w = (ui.available_width() - ACTION_W).max(0.0);
                    ui.allocate_ui_with_layout(
                        egui::vec2(label_w, ui.available_height()),
                        Layout::left_to_right(Align::Center),
                        |ui| {
                            ui.set_width(label_w);
                            ui.add(egui::Label::new(job).truncate());
                        },
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        // Discard is destructive — keep it hover-only so it doesn't
                        // read as part of every row.
                        if !staged
                            && row_hot
                            && crate::ui::chrome::icon_button_inline(
                                ui,
                                ICON_TRASH,
                                FS_TINY,
                                c_text_faint(),
                            )
                            .on_hover_text("Discard changes")
                            .clicked()
                        {
                            self.request(GitOp::Discard(vec![entry.path.clone()]));
                        }
                    });
                });
            },
        );

        // File diff on click.
        if response.clicked() {
            self.request(GitOp::ShowDiff {
                path: entry.path.clone(),
                staged,
            });
            self.conv.diff_view_open = true;
        }
        if hovered {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }

    /// Full-area diff viewer that replaces the chat window while a diff is open.
    /// Constrained to the same centered column as the chat header/transcript so it
    /// stays aligned when the side panels are closed. Easy to close via the close
    /// button or Esc.
    pub(crate) fn render_diff_view(
        &mut self,
        ui: &mut Ui,
        title: &str,
        diff_text: &str,
        column_center_w: f32,
    ) {
        // Close the viewer on Esc.
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.request(GitOp::ClearDiff);
            self.conv.diff_view_open = false;
        }

        let col_w = column_center_w.min(CHAT_COLUMN_MAX);
        let pad = ((column_center_w - col_w) * 0.5).max(0.0);

        // Header bar: title + metadata on the left, close button on the right,
        // constrained to the centered chat column like the chat header above it.
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            if pad > 0.0 {
                ui.add_space(pad);
            }
            ui.allocate_ui_with_layout(
                egui::vec2(col_w, 24.0),
                egui::Layout::left_to_right(Align::Center),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    ui.label(RichText::new("Diff").size(FS_H3).color(c_text()).strong());
                    ui.label(
                        RichText::new(title)
                            .size(FS_SMALL)
                            .color(c_text_muted())
                            .monospace(),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if crate::ui::chrome::icon_button_plain(ui, ICON_CLOSE, 24.0, false)
                            .on_hover_text("Close diff (Esc)")
                            .clicked()
                        {
                            self.request(GitOp::ClearDiff);
                            self.conv.diff_view_open = false;
                        }
                    });
                },
            );
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            if pad > 0.0 {
                ui.add_space(pad);
            }
            ui.allocate_ui(egui::vec2(col_w, 1.0), |ui| {
                crate::ui::chrome::hairline(ui);
            });
        });
        ui.add_space(6.0);

        // Full-size scroll area; the diff text is centered inside it (same pattern
        // as the transcript in `render_conversation`).
        let avail_h = ui.available_height();
        ScrollArea::vertical()
            .id_salt("diff_view_scroll")
            .max_height(avail_h)
            .auto_shrink([false, false])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
            .show(ui, |ui| {
                let viewport_w = ui.max_rect().width();
                ui.set_max_width(viewport_w);
                let wrap_width = col_w.max(200.0);
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                std::hash::Hash::hash(title, &mut hasher);
                std::hash::Hash::hash(diff_text, &mut hasher);
                let key = (std::hash::Hasher::finish(&hasher), wrap_width.to_bits());
                let cached = self
                    .conv
                    .diff_job_cache
                    .as_ref()
                    .is_some_and(|(h, w, _)| (*h, *w) == key);
                if !cached {
                    let job = colorize_diff(diff_text, wrap_width);
                    self.conv.diff_job_cache = Some((key.0, key.1, job));
                }
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    if pad > 0.0 {
                        ui.add_space(pad);
                    }
                    ui.vertical(|ui| {
                        ui.set_width(wrap_width);
                        if let Some((_, _, job)) = &self.conv.diff_job_cache {
                            ui.label(job.clone());
                        }
                    });
                });
            });
    }

    fn render_git_branches(&mut self, ui: &mut Ui) {
        // New branch input
        ui.label(
            RichText::new("Create branch from current")
                .size(FS_TINY)
                .color(c_text_muted())
                .strong(),
        );
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.conv.git_new_branch)
                    .hint_text("branch name…")
                    .desired_width(ui.available_width() - 78.0),
            );
            let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if crate::ui::chrome::primary_button(ui, "Create").clicked() || enter {
                let name = self.conv.git_new_branch.trim().to_string();
                if !name.is_empty() {
                    self.request(GitOp::NewBranch(name));
                    self.conv.git_new_branch.clear();
                }
            }
        });
        ui.add_space(8.0);
        crate::ui::chrome::hairline(ui);
        ui.add_space(6.0);

        let branches = self.conv.git.branches.clone();
        let current = self.conv.git.branch.clone();
        ScrollArea::vertical()
            .id_salt("git_branches_scroll")
            .max_height(ui.available_height())
            .auto_shrink([false, true])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
            .show(ui, |ui| {
                for (i, b) in branches.iter().enumerate() {
                    ui.push_id(("branch", i), |ui| {
                        let is_current = b == &current;
                        let full_w = ui.available_width();
                        let (rect, response) =
                            ui.allocate_exact_size(egui::vec2(full_w, 22.0), Sense::click());
                        let hovered = response.hovered();
                        let fill = if is_current {
                            c_row_active()
                        } else if hovered {
                            c_row_hover()
                        } else {
                            Color32::TRANSPARENT
                        };
                        ui.painter().rect_filled(
                            rect,
                            Rounding::same(crate::theme::RADIUS_ROW),
                            fill,
                        );
                        ui.allocate_new_ui(
                            egui::UiBuilder::new().max_rect(rect.shrink2(egui::vec2(6.0, 0.0))),
                            |ui| {
                                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                    ui.label(
                                        RichText::new(if is_current { "●" } else { "○" })
                                            .size(FS_TINY)
                                            .color(if is_current {
                                                c_accent()
                                            } else {
                                                c_text_faint()
                                            }),
                                    );
                                    ui.add_space(6.0);
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(b.clone())
                                                .size(FS_SMALL)
                                                .color(c_text())
                                                .monospace(),
                                        )
                                        .truncate(),
                                    );
                                });
                            },
                        );
                        if response.clicked() && !is_current {
                            self.request(GitOp::Checkout(b.clone()));
                        }
                        if hovered {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                    });
                }
            });
    }

    fn render_git_history(&mut self, ui: &mut Ui) {
        let log = self.conv.git.log.clone();
        ScrollArea::vertical()
            .id_salt("git_history_scroll")
            .max_height(ui.available_height())
            .auto_shrink([false, true])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
            .show(ui, |ui| {
                if log.is_empty() {
                    ui.label(
                        RichText::new("No commits yet")
                            .size(FS_SMALL)
                            .color(c_text_muted()),
                    );
                    return;
                }
                for (i, commit) in log.iter().enumerate() {
                    ui.push_id(("commit", i), |ui| {
                        self.render_commit_row(ui, commit);
                    });
                }
            });
    }

    fn render_commit_row(&mut self, ui: &mut Ui, commit: &crate::git::GitCommit) {
        let full_w = ui.available_width();
        let (rect, response) = ui.allocate_exact_size(egui::vec2(full_w, 40.0), Sense::click());
        let hovered = response.hovered();
        let fill = if hovered {
            c_row_hover()
        } else {
            Color32::TRANSPARENT
        };
        ui.painter()
            .rect_filled(rect, Rounding::same(crate::theme::RADIUS_ROW), fill);
        ui.allocate_new_ui(
            egui::UiBuilder::new().max_rect(rect.shrink2(egui::vec2(6.0, 4.0))),
            |ui| {
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(&commit.hash[..7.min(commit.hash.len())])
                                .size(FS_TINY)
                                .color(c_accent())
                                .monospace(),
                        );
                        ui.add_space(6.0);
                        ui.add(
                            egui::Label::new(
                                RichText::new(commit.message.clone())
                                    .size(FS_SMALL)
                                    .color(c_text()),
                            )
                            .truncate(),
                        );
                    });
                    ui.add_space(1.0);
                    ui.label(
                        RichText::new(format!("{} · {}", commit.author, commit.date))
                            .size(FS_TINY)
                            .color(c_text_muted()),
                    );
                });
            },
        );
        if response.clicked() {
            self.request(GitOp::ShowCommit(commit.hash.clone()));
            self.conv.diff_view_open = true;
        } else if response.secondary_clicked() {
            let hash = commit.hash.clone();
            ui.ctx().copy_text(hash.clone());
        }
        if hovered {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            response.on_hover_ui(|ui| {
                ui.label(
                    RichText::new(format!(
                        "{}\n{} · {}\n\nClick: show full diff · Right-click: copy hash",
                        commit.message, commit.author, commit.date
                    ))
                    .size(FS_SMALL)
                    .color(c_text()),
                );
            });
        }
    }
}

fn status_color(status: char) -> Color32 {
    match status {
        'M' => c_accent(),
        'A' => c_diff_add_fg(),
        'D' => c_diff_del_fg(),
        'R' | 'C' => c_accent(),
        'U' => c_danger(),
        '?' => c_text_muted(),
        _ => c_text(),
    }
}

/// Colorize a unified diff: added/removed/context lines painted with their bg color.
fn colorize_diff(text: &str, wrap_width: f32) -> LayoutJob {
    let mut job = LayoutJob {
        wrap: TextWrapping {
            max_width: wrap_width,
            ..Default::default()
        },
        break_on_newline: true,
        ..Default::default()
    };

    let context_color = c_text_muted();

    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        let start = job.text.len();
        job.text.push_str(line);
        // Keep the newline inside this section's byte range — egui only lays out
        // bytes covered by a section, so a newline left in a gap gets dropped.
        if lines.peek().is_some() {
            job.text.push('\n');
        }
        let end = job.text.len();
        let (color, bg) = if line.starts_with("+++") || line.starts_with("---") {
            (c_text(), c_bg_elevated())
        } else if line.starts_with('+') {
            (c_diff_add_fg(), c_diff_add_bg())
        } else if line.starts_with('-') {
            (c_diff_del_fg(), c_diff_del_bg())
        } else if line.starts_with("@@") {
            (c_accent(), Color32::TRANSPARENT)
        } else {
            (context_color, Color32::TRANSPARENT)
        };
        job.sections.push(egui::text::LayoutSection {
            leading_space: 0.0,
            byte_range: start..end,
            format: TextFormat {
                font_id: FontId::monospace(FS_CODE),
                color,
                background: bg,
                ..Default::default()
            },
        });
    }

    job
}
