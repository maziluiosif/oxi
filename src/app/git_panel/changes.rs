//! Header (branch status, pull/push/fetch), tab strip, the "Changes" tab (commit
//! composer, staged/unstaged lists), and the full-area diff viewer.

use eframe::egui::text::{LayoutJob, TextFormat};
use eframe::egui::{
    self, Align, Color32, CornerRadius, FontId, Layout, RichText, ScrollArea, Sense, Ui,
};

use crate::git::{GitEntry, GitOp};
use crate::theme::*;

use super::super::OxiApp;
use super::GitTab;

impl OxiApp {
    pub(super) fn render_git_header(&mut self, ui: &mut Ui) {
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
            if self.conv.git.busy {
                ui.add(egui::Spinner::new().size(12.0).color(c_text_muted()));
                let op = self
                    .conv
                    .git
                    .last_op
                    .clone()
                    .unwrap_or_else(|| "working".to_string());
                ui.label(RichText::new(op).size(FS_TINY).color(c_text_muted()))
                    .on_hover_text("Git operation in progress");
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let busy = self.conv.git.busy;
                let refresh = crate::ui::chrome::icon_button_plain(ui, ICON_REFRESH, 22.0, false);
                if busy {
                    refresh.on_hover_text("Git operation in progress…");
                } else if refresh.on_hover_text("Refresh").clicked() {
                    self.ensure_git_channels();
                    self.request(GitOp::Refresh);
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
            // Disabled while the worker is busy so an impatient double-click can't
            // queue the same network operation twice.
            let busy = self.conv.git.busy;
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
                for (icon, label, hover, op) in [
                    (
                        ICON_DOWNLOAD,
                        "Pull",
                        "Fetch and integrate upstream changes",
                        GitOp::Pull,
                    ),
                    (ICON_UPLOAD, "Push", "Sync remote changes safely, then push", GitOp::Push),
                    (ICON_REFRESH, "Fetch", "Fetch", GitOp::Fetch),
                ] {
                    let resp = crate::ui::chrome::mini_button_icon_enabled(ui, icon, label, !busy);
                    if busy {
                        resp.on_hover_text("Git operation in progress…");
                    } else if resp.on_hover_text(hover).clicked() {
                        self.request(op);
                    }
                }
            });
        }
    }

    pub(super) fn render_git_tabs(&mut self, ui: &mut Ui) {
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

    pub(super) fn render_git_changes(&mut self, ui: &mut Ui) {
        // Commit composer
        ui.label(
            RichText::new("Message")
                .size(FS_TINY)
                .color(c_text_muted())
                .strong(),
        );
        ui.add_space(2.0);
        crate::ui::chrome::settings_text_area(
            ui,
            &mut self.conv.git_commit_message,
            "Commit message…",
            3,
        );
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
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded)
            .show(ui, |ui| {
                if !staged.is_empty() {
                    self.render_section(ui, "Staged", &staged, true);
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
        self.render_section_header(ui, title, entries, staged);
        ui.add_space(2.0);
        let total = entries.len();
        for (i, entry) in entries.iter().enumerate() {
            ui.push_id((title, i), |ui| {
                self.render_change_row(ui, entry, staged, i, total);
            });
        }
    }

    fn render_section_header(
        &mut self,
        ui: &mut Ui,
        title: &str,
        entries: &[GitEntry],
        staged: bool,
    ) {
        let full_w = ui.available_width();
        ui.allocate_ui_with_layout(
            egui::vec2(full_w, 20.0),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.label(
                    RichText::new(title.to_uppercase())
                        .size(FS_TINY)
                        .color(c_text_faint())
                        .strong(),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if staged {
                        if crate::ui::chrome::icon_button_inline(
                            ui,
                            ICON_CLOSE,
                            FS_TINY,
                            c_text_faint(),
                        )
                        .on_hover_text("Unstage all")
                        .clicked()
                        {
                            let paths: Vec<String> =
                                entries.iter().map(|e| e.path.clone()).collect();
                            self.request(GitOp::Unstage(paths));
                        }
                    } else {
                        if crate::ui::chrome::icon_button_inline(
                            ui,
                            ICON_TRASH,
                            FS_TINY,
                            c_text_faint(),
                        )
                        .on_hover_text("Discard all changes")
                        .clicked()
                        {
                            self.request_confirm(crate::app::state::ConfirmAction::GitDiscard {
                                paths: entries.iter().map(|e| e.path.clone()).collect(),
                            });
                        }
                        if crate::ui::chrome::icon_button_inline(
                            ui,
                            ICON_PLUS,
                            FS_TINY,
                            c_text_faint(),
                        )
                        .on_hover_text("Stage all")
                        .clicked()
                        {
                            let paths: Vec<String> =
                                entries.iter().map(|e| e.path.clone()).collect();
                            self.request(GitOp::Stage(paths));
                        }
                    }
                });
            },
        );
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
            .rect_filled(rect, CornerRadius::same(crate::theme::RADIUS_ROW), fill);

        ui.scope_builder(
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
                            self.request_confirm(crate::app::state::ConfirmAction::GitDiscard {
                                paths: vec![entry.path.clone()],
                            });
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
            self.conv.focus_chat_input_next_frame = true;
        }

        let col_w = column_center_w.min(crate::theme::chat_column_max_width(ui.ctx()));
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
                            self.conv.focus_chat_input_next_frame = true;
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
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded)
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
                    let job = crate::ui::diff::diff_layout_job(diff_text, wrap_width);
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
