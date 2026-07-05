//! Branches tab (list + create/checkout) and history tab (commit log + diff-on-click).

use eframe::egui::{self, Align, Color32, CornerRadius, Layout, RichText, ScrollArea, Sense, Ui};

use crate::git::GitOp;
use crate::theme::*;

use super::super::OxiApp;

impl OxiApp {
    pub(super) fn render_git_branches(&mut self, ui: &mut Ui) {
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
                            CornerRadius::same(crate::theme::RADIUS_ROW),
                            fill,
                        );
                        ui.scope_builder(
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

    pub(super) fn render_git_history(&mut self, ui: &mut Ui) {
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
            .rect_filled(rect, CornerRadius::same(crate::theme::RADIUS_ROW), fill);
        ui.scope_builder(
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
