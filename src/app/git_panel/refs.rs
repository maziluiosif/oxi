//! Branches tab (list + create/checkout) and history tab (commit log + diff-on-click).

use eframe::egui::{
    self, Align, Color32, CornerRadius, FontId, Layout, RichText, ScrollArea, Sense, Ui,
};

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
            ui.spacing_mut().item_spacing.x = 6.0;
            // The field's frame adds its own padding and border around the text.
            let resp = crate::ui::chrome::settings_text_field_width(
                ui,
                &mut self.conv.git_ui.new_branch,
                "branch name…",
                (ui.available_width() - 96.0).max(60.0),
            );
            let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let name = self.conv.git_ui.new_branch.trim().to_string();
            let create = ui
                .add_enabled_ui(!name.is_empty(), |ui| {
                    crate::ui::chrome::primary_button(ui, "Create")
                })
                .inner;
            if (create.clicked() || enter) && !name.is_empty() {
                self.request(GitOp::NewBranch(name));
                self.conv.git_ui.new_branch.clear();
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
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded)
            .show(ui, |ui| {
                if branches.is_empty() {
                    ui.label(
                        RichText::new("No branches yet — create one above.")
                            .size(FS_SMALL)
                            .color(c_text_muted()),
                    );
                    return;
                }
                for (i, b) in branches.iter().enumerate() {
                    ui.push_id(("branch", i), |ui| {
                        self.render_branch_row(ui, b, b == &current);
                    });
                }
            });
    }

    fn render_branch_row(&mut self, ui: &mut Ui, branch: &str, is_current: bool) {
        let full_w = ui.available_width();
        let (rect, response) = ui.allocate_exact_size(egui::vec2(full_w, 22.0), Sense::click());
        // Geometric hover: the trash button on top of the row takes the pointer away from
        // the row response, which would otherwise hide the button under the cursor.
        let row_hot = ui
            .input(|i| i.pointer.hover_pos())
            .is_some_and(|p| rect.contains(p));
        let deleting = self.conv.git_ui.pending_branch_delete.as_deref() == Some(branch);
        let fill = if is_current {
            c_row_active()
        } else if row_hot {
            c_row_hover()
        } else {
            Color32::TRANSPARENT
        };
        ui.painter()
            .rect_filled(rect, CornerRadius::same(crate::theme::RADIUS_ROW), fill);
        let inner = rect.shrink2(egui::vec2(6.0, 0.0));
        let (icon, icon_ink) = if is_current {
            (ICON_CHECK, c_accent())
        } else {
            (ICON_BRANCH, c_text_faint())
        };
        let icon_rect = ui.painter().text(
            egui::pos2(inner.left(), inner.center().y),
            egui::Align2::LEFT_CENTER,
            icon,
            FontId::new(FS_TINY, icon_font()),
            icon_ink,
        );
        let name_left = inner.left().max(icon_rect.right()) + 7.0;
        let action_w = 22.0;
        let name = single_line_galley(
            ui,
            branch,
            FontId::monospace(FS_SMALL),
            if is_current {
                c_text_strong()
            } else {
                c_text()
            },
            inner.right() - action_w - name_left,
        );
        let name_y = inner.center().y - name.size().y / 2.0;
        ui.painter()
            .galley(egui::pos2(name_left, name_y), name, c_text());

        let mut delete = false;
        if deleting {
            let spin = egui::Rect::from_center_size(
                egui::pos2(inner.right() - 8.0, inner.center().y),
                egui::vec2(12.0, 12.0),
            );
            ui.scope_builder(egui::UiBuilder::new().max_rect(spin), |ui| {
                crate::theme::spinner(ui, 12.0, c_text_muted());
            });
        } else if row_hot && !is_current {
            let slot = egui::Rect::from_min_max(
                egui::pos2(inner.right() - action_w, rect.top()),
                egui::pos2(inner.right(), rect.bottom()),
            );
            ui.scope_builder(
                egui::UiBuilder::new()
                    .max_rect(slot)
                    .layout(Layout::right_to_left(Align::Center)),
                |ui| {
                    delete = crate::ui::chrome::icon_button_inline(
                        ui,
                        ICON_TRASH,
                        FS_TINY,
                        c_text_faint(),
                    )
                    .on_hover_text("Delete branch")
                    .clicked();
                },
            );
        }

        let mut force_delete = false;
        response.context_menu(|ui| {
            if ui
                .add_enabled(!is_current, egui::Button::new("Checkout"))
                .clicked()
            {
                self.request(GitOp::Checkout(branch.to_string()));
                ui.close();
            }
            if ui.button("Copy name").clicked() {
                ui.ctx().copy_text(branch.to_string());
                ui.close();
            }
            ui.separator();
            let delete_btn = ui
                .add_enabled(!is_current, egui::Button::new("Delete"))
                .on_disabled_hover_text("The checked-out branch can't be deleted");
            if delete_btn.clicked() {
                delete = true;
                ui.close();
            }
            if ui
                .add_enabled(!is_current, egui::Button::new("Force delete…"))
                .on_hover_text("Delete even if its commits are not merged")
                .clicked()
            {
                force_delete = true;
                ui.close();
            }
        });

        if delete {
            self.delete_branch(branch);
        } else if force_delete {
            self.request_confirm(crate::app::state::ConfirmAction::GitForceDeleteBranch {
                name: branch.to_string(),
            });
        } else if response.clicked() && !is_current {
            self.request(GitOp::Checkout(branch.to_string()));
        }
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            response.on_hover_text(if is_current {
                "Current branch · Right-click for actions"
            } else {
                "Click: checkout · Right-click for actions"
            });
        }
    }

    /// Safe delete (`git branch -d`); an unmerged branch comes back as a force-delete
    /// confirmation (see `settle_branch_delete`).
    fn delete_branch(&mut self, branch: &str) {
        self.conv.git.error = None;
        self.conv.git_ui.pending_branch_delete = Some(branch.to_string());
        self.request(GitOp::DeleteBranch {
            name: branch.to_string(),
            force: false,
        });
    }

    pub(super) fn render_git_history(&mut self, ui: &mut Ui) {
        let log = self.conv.git.log.clone();
        ScrollArea::vertical()
            .id_salt("git_history_scroll")
            .max_height(ui.available_height())
            .auto_shrink([false, true])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded)
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

    pub(super) fn render_commit_row(&mut self, ui: &mut Ui, commit: &crate::git::GitCommit) {
        let full_w = ui.available_width();
        let (rect, response) = ui.allocate_exact_size(egui::vec2(full_w, 40.0), Sense::click());
        let hovered = response.hovered();
        // The commit whose diff is open (ShowCommit records its hash as the diff path).
        let selected = self.conv.diff_view.open
            && self.conv.git.current_diff_path.as_deref() == Some(commit.hash.as_str());
        let fill = if selected {
            c_row_active()
        } else if hovered {
            c_row_hover()
        } else {
            Color32::TRANSPARENT
        };
        ui.painter()
            .rect_filled(rect, CornerRadius::same(crate::theme::RADIUS_ROW), fill);
        // Text is painted, not added as labels: labels sense the pointer themselves, which
        // dropped the row's hover fill and click whenever the pointer was over the text.
        let inner = rect.shrink2(egui::vec2(6.0, 4.0));
        let hash = &commit.hash[..7.min(commit.hash.len())];
        let hash_galley =
            ui.painter()
                .layout_no_wrap(hash.to_owned(), FontId::monospace(FS_TINY), c_accent());
        let line_h = inner.height() / 2.0;
        let first_y = inner.top() + line_h / 2.0;
        ui.painter().galley(
            egui::pos2(inner.left(), first_y - hash_galley.size().y / 2.0),
            hash_galley.clone(),
            c_accent(),
        );
        let message_left = inner.left() + hash_galley.size().x + 6.0;
        let message = single_line_galley(
            ui,
            &commit.message,
            FontId::proportional(FS_SMALL),
            if selected { c_text_strong() } else { c_text() },
            inner.right() - message_left,
        );
        ui.painter().galley(
            egui::pos2(message_left, first_y - message.size().y / 2.0),
            message,
            c_text(),
        );
        let meta = single_line_galley(
            ui,
            &format!("{} · {}", commit.author, commit.date),
            FontId::proportional(FS_TINY),
            c_text_muted(),
            inner.width(),
        );
        ui.painter().galley(
            egui::pos2(inner.left(), first_y + line_h - meta.size().y / 2.0),
            meta,
            c_text_muted(),
        );
        if response.clicked() {
            self.request(GitOp::ShowCommit(commit.hash.clone()));
            self.conv.diff_view.open = true;
            // Open the commit diff as an editor tab, like working-tree file diffs.
            self.conv.editor.diff_tab_active = true;
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

/// One line of text elided with `…` to `max_width`.
fn single_line_galley(
    ui: &Ui,
    text: &str,
    font: FontId,
    color: Color32,
    max_width: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap.max_width = max_width.max(0.0);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job.wrap.overflow_character = Some('…');
    ui.fonts_mut(|f| f.layout_job(job))
}
