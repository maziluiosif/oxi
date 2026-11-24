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
                crate::theme::spinner(ui, 12.0, c_text_muted());
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
                // The top-right git panel toggle floats over this corner.
                ui.add_space(crate::ui::window_chrome::RIGHT_CONTROLS_W);
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

        // Compact branch + inline Pull/Push/Fetch row (reclaims vertical height for file list).
        if self.conv.git.repo {
            ui.add_space(3.0);
            let branch = self.conv.git.branch.clone();
            let (ahead, behind) = (self.conv.git.ahead, self.conv.git.behind);
            let busy = self.conv.git.busy;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                // Sync buttons laid out first from the right edge so long branch names truncate
                // cleanly without pushing sync controls off-panel.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 3.0;
                    let fetch_resp =
                        crate::ui::chrome::icon_button_plain(ui, ICON_REFRESH, 20.0, false);
                    if !busy && fetch_resp.on_hover_text("Fetch from remote").clicked() {
                        self.request(GitOp::Fetch);
                    }
                    let push_label = if ahead > 0 {
                        format!("{ahead}")
                    } else {
                        String::new()
                    };
                    let push_resp = crate::ui::chrome::mini_button_icon_enabled(
                        ui,
                        ICON_UPLOAD,
                        &push_label,
                        !busy,
                    )
                    .on_hover_text(if ahead > 0 {
                        format!(
                            "Push {ahead} commit{} to upstream",
                            if ahead == 1 { "" } else { "s" }
                        )
                    } else {
                        "Sync remote changes safely, then push".to_owned()
                    });
                    if push_resp.clicked() {
                        self.request(GitOp::Push);
                    }
                    let pull_label = if behind > 0 {
                        format!("{behind}")
                    } else {
                        String::new()
                    };
                    let pull_resp = crate::ui::chrome::mini_button_icon_enabled(
                        ui,
                        ICON_DOWNLOAD,
                        &pull_label,
                        !busy,
                    )
                    .on_hover_text(if behind > 0 {
                        format!(
                            "Pull {behind} commit{} from upstream",
                            if behind == 1 { "" } else { "s" }
                        )
                    } else {
                        "Fetch and integrate upstream changes".to_owned()
                    });
                    if pull_resp.clicked() {
                        self.request(GitOp::Pull);
                    }
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(ICON_BRANCH)
                                .font(FontId::new(FS_TINY, icon_font()))
                                .color(c_text_muted()),
                        );
                        ui.add(
                            egui::Label::new(
                                RichText::new(&branch)
                                    .size(FS_TINY)
                                    .color(c_text_muted())
                                    .monospace(),
                            )
                            .truncate(),
                        )
                        .on_hover_text(&branch);
                    });
                });
            });
        }
    }

    pub(super) fn render_git_tabs(&mut self, ui: &mut Ui) {
        // Counts only where they say something: pending changes and the compare commits.
        // The branch and history list lengths just crowd a narrow panel.
        let tabs = [
            (
                GitTab::Changes,
                "Changes",
                self.conv.git.staged.len() + self.conv.git.unstaged.len(),
            ),
            (GitTab::Branches, "Branches", 0),
            (GitTab::History, "History", 0),
            (
                GitTab::Compare,
                "Compare",
                self.conv
                    .git_ui
                    .compare
                    .data
                    .as_ref()
                    .map_or(0, |data| data.commits.len()),
            ),
        ];
        let strip: Vec<_> = tabs
            .iter()
            .map(|(tab, label, n)| (*label, *n, self.conv.git_ui.tab == *tab))
            .collect();
        if let Some(i) = crate::ui::chrome::underline_tabs(ui, &strip) {
            self.conv.git_ui.tab = tabs[i].0;
        }
    }

    pub(super) fn render_git_changes(&mut self, ui: &mut Ui) {
        // Commit composer
        let commit_hint = if cfg!(target_os = "macos") {
            "Commit message… (⌘Enter to commit)"
        } else {
            "Commit message… (Ctrl+Enter to commit)"
        };
        let commit_edit = crate::ui::chrome::settings_text_area(
            ui,
            &mut self.conv.git_ui.commit_message,
            commit_hint,
            3,
        );
        ui.add_space(4.0);

        let gen_active = self.commit_gen_active();
        let staged_empty = self.conv.git.staged.is_empty();
        let unstaged_empty = self.conv.git.unstaged.is_empty();
        let stage_all_on_commit = staged_empty && !unstaged_empty;
        let msg_empty = self.conv.git_ui.commit_message.trim().is_empty();
        let can_commit = (!staged_empty || !unstaged_empty) && !msg_empty;
        let cmd_enter = commit_edit.has_focus()
            && ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Enter));
        let mut do_commit = cmd_enter && can_commit;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
            let commit_label = if stage_all_on_commit {
                "Stage all & Commit"
            } else {
                "Commit"
            };
            let commit_resp = ui
                .add_enabled(
                    can_commit,
                    crate::ui::chrome::primary_button_icon_widget(ICON_CHECK, commit_label),
                )
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text(if stage_all_on_commit {
                    "Stage all unstaged files and commit (Cmd/Ctrl+Enter)"
                } else {
                    "Commit staged changes (Cmd/Ctrl+Enter)"
                })
                .on_disabled_hover_text(if staged_empty && unstaged_empty {
                    "No changes to commit"
                } else {
                    "Write or generate a commit message"
                });
            if commit_resp.clicked() {
                do_commit = true;
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
                self.conv.git_ui.commit_gen_error = None;
                self.conv.git_ui.commit_gen_pending = true;
                self.request(GitOp::CollectCommitDiff);
            }
        });
        if do_commit {
            if stage_all_on_commit {
                let paths: Vec<String> = self
                    .conv
                    .git
                    .unstaged
                    .iter()
                    .map(|e| e.path.clone())
                    .collect();
                if !paths.is_empty() {
                    self.request(GitOp::Stage(paths));
                }
            }
            let msg = self.conv.git_ui.commit_message.trim().to_owned();
            self.request(GitOp::Commit(msg));
            self.conv.git_ui.commit_message.clear();
            crate::ui::chrome::show_toast(ui.ctx(), "Committed changes");
        }

        if let Some(err) = self.conv.git_ui.commit_gen_error.clone() {
            ui.add_space(4.0);
            if crate::ui::chrome::dismissible_notice(
                ui,
                "commit_gen_error",
                &format!("Generate failed: {err}"),
            ) {
                self.conv.git_ui.commit_gen_error = None;
            }
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
        let (rect, _) = ui.allocate_exact_size(egui::vec2(full_w, 20.0), Sense::hover());
        fixed_row_ui(ui, rect, Layout::right_to_left(Align::Center), |ui| {
            if staged {
                if crate::ui::chrome::icon_button_inline(ui, ICON_CLOSE, FS_TINY, c_text_faint())
                    .on_hover_text("Unstage all")
                    .clicked()
                {
                    let paths: Vec<String> = entries.iter().map(|e| e.path.clone()).collect();
                    self.request(GitOp::Unstage(paths));
                }
            } else {
                if crate::ui::chrome::icon_button_inline(ui, ICON_TRASH, FS_TINY, c_text_faint())
                    .on_hover_text("Discard all changes")
                    .clicked()
                {
                    self.request_confirm(crate::app::state::ConfirmAction::GitDiscard {
                        paths: entries.iter().map(|e| e.path.clone()).collect(),
                    });
                }
                if crate::ui::chrome::icon_button_inline(ui, ICON_PLUS, FS_TINY, c_text_faint())
                    .on_hover_text("Stage all")
                    .clicked()
                {
                    let paths: Vec<String> = entries.iter().map(|e| e.path.clone()).collect();
                    self.request(GitOp::Stage(paths));
                }
            }
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.add(
                    egui::Label::new(
                        RichText::new(title.to_uppercase())
                            .size(FS_TINY)
                            .color(c_text_faint())
                            .strong(),
                    )
                    .truncate(),
                );
            });
        });
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
        let mut open_file = false;
        let (rect, response) = ui.allocate_exact_size(egui::vec2(full_w, 22.0), Sense::click());
        let hovered = response.hovered();
        // Pure geometric hover test — `Ui::rect_contains_pointer` also checks layer
        // and clip, which the nested child layouts below fail even with the pointer
        // visually on the row.
        let row_hot = ui
            .input(|i| i.pointer.hover_pos())
            .is_some_and(|p| rect.contains(p));

        // Selection highlight if this is the currently-viewed diff.
        let selected = match self.active_diff_target() {
            Some((path, source)) => {
                path == entry.path
                    && matches!(
                        (source, staged),
                        (crate::app::file_explorer::DiffSource::Staged, true)
                            | (crate::app::file_explorer::DiffSource::WorkTree, false)
                    )
            }
            None => {
                self.conv.diff_view.open
                    && self.conv.editor.diff_tab_active
                    && self.conv.git.current_diff_path.as_deref() == Some(entry.path.as_str())
                    && self.conv.git.current_diff_staged.unwrap_or(false) == staged
            }
        };

        let fill = if selected {
            c_row_active()
        } else if hovered {
            c_row_hover()
        } else {
            Color32::TRANSPARENT
        };
        ui.painter()
            .rect_filled(rect, CornerRadius::same(crate::theme::RADIUS_ROW), fill);

        fixed_row_ui(
            ui,
            rect.shrink2(egui::vec2(6.0, 0.0)),
            Layout::left_to_right(Align::Center),
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
                    // Fixed-width status slot: proportional letters ("M" vs "?") used to
                    // start each file name at a slightly different x.
                    let (status_rect, _) =
                        ui.allocate_exact_size(egui::vec2(12.0, 16.0), Sense::hover());
                    ui.painter().text(
                        status_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        entry.status,
                        FontId::monospace(FS_SMALL),
                        crate::app::file_explorer::git_status_color(entry.status),
                    );
                    ui.add_space(6.0);
                    let (icon, icon_color) =
                        crate::app::file_explorer::file_icon(std::path::Path::new(&entry.path));
                    ui.label(
                        RichText::new(icon)
                            .font(FontId::new(FS_SMALL, icon_font()))
                            .color(icon_color),
                    );
                    ui.add_space(5.0);
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
                    let action_w = 2.0 * (18.0 + ui.spacing().item_spacing.x);
                    let label_w = (ui.available_width() - action_w).max(0.0);
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
                        if row_hot
                            && entry.status != 'D'
                            && crate::ui::chrome::icon_button_inline(
                                ui,
                                ICON_FILE,
                                FS_TINY,
                                c_text_faint(),
                            )
                            .on_hover_text("Open file")
                            .clicked()
                        {
                            open_file = true;
                        }
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

        // Like VS Code: a click shows the change as a diff in the file's editor tab (editable
        // for working tree changes); the hover action opens the plain file.
        if open_file {
            self.open_changed_file(&entry.path);
        } else if response.clicked() {
            let source = if staged {
                crate::app::file_explorer::DiffSource::Staged
            } else {
                crate::app::file_explorer::DiffSource::WorkTree
            };
            // A deleted or binary file has no editable text: show its patch instead.
            if !self.open_diff_editor(&entry.path, source, None) {
                self.request(GitOp::ShowDiff {
                    path: entry.path.clone(),
                    staged,
                });
                self.conv.diff_view.open = true;
                self.conv.editor.diff_tab_active = true;
            }
        }
        if hovered {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }

    /// Open a changed file in the editor at its first change, with git changes highlighted.
    pub(crate) fn open_changed_file(&mut self, relative: &str) {
        let path = std::path::PathBuf::from(&self.active_workspace().root_path).join(relative);
        if !path.is_file() {
            return;
        }
        let first_changed_line = self
            .git_gutter_line_changes(relative)
            .and_then(|changes| changes.iter().map(|change| change.line).min());
        self.close_editor_git_diff();
        self.apply_diff_action(crate::ui::diff_view::DiffAction::OpenFile {
            path: relative.to_owned(),
            line: first_changed_line.map(|line| line + 1),
        });
        if let Some(index) = self.conv.editor.active.filter(|&index| {
            self.conv.editor.git_full_highlight_path.as_ref()
                == Some(&self.conv.editor.documents[index].path)
        }) {
            self.close_document_diff(index);
        }
    }
}

/// Rows have fixed geometry: overflowing child content must not widen the scroll
/// area's subsequent rows and push their right-aligned actions beyond the panel.
fn fixed_row_ui<R>(
    ui: &mut Ui,
    rect: egui::Rect,
    layout: Layout,
    contents: impl FnOnce(&mut Ui) -> R,
) -> R {
    let mut row_ui = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(layout));
    row_ui.shrink_clip_rect(rect);
    contents(&mut row_ui)
}

#[cfg(test)]
mod layout_tests {
    use super::*;

    #[test]
    fn git_rows_keep_bulk_actions_inside_narrow_panels() {
        for width in [224.0, 284.0, 624.0] {
            for scale in [1.0, 1.25, 1.5, 2.0] {
                let mut fonts_ready = false;
                let mut harness = egui_kittest::Harness::builder()
                    .with_size(egui::vec2(width + 16.0, 240.0))
                    .with_pixels_per_point(scale)
                    .with_os(egui::os::OperatingSystem::Windows)
                    .build_ui(move |ui| {
                        if !fonts_ready {
                            crate::theme::setup_style(ui.ctx());
                            fonts_ready = true;
                            return;
                        }
                        ui.set_width(width);
                        ScrollArea::vertical().show(ui, |ui| {
                            let right = ui.max_rect().right();
                            // An overflowing file row used to widen the following
                            // section's available area, putting its actions offscreen.
                            let (rect, _) = ui.allocate_exact_size(
                                egui::vec2(ui.available_width(), 22.0),
                                Sense::hover(),
                            );
                            fixed_row_ui(ui, rect, Layout::left_to_right(Align::Center), |ui| {
                                ui.add(
                                    egui::Label::new("very_long_directory/".repeat(40))
                                        .wrap_mode(egui::TextWrapMode::Extend),
                                );
                            });
                            assert!(ui.max_rect().right() <= right + 0.5);
                            let (rect, _) = ui.allocate_exact_size(
                                egui::vec2(ui.available_width(), 20.0),
                                Sense::hover(),
                            );
                            fixed_row_ui(ui, rect, Layout::right_to_left(Align::Center), |ui| {
                                for icon in [ICON_TRASH, ICON_PLUS] {
                                    let response = crate::ui::chrome::icon_button_inline(
                                        ui,
                                        icon,
                                        FS_TINY,
                                        c_text_faint(),
                                    );
                                    assert!(ui.clip_rect().contains_rect(response.rect));
                                    assert!(response.rect.right() <= right + 0.5);
                                }
                                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                    ui.add(egui::Label::new("CHANGES").truncate());
                                });
                            });
                        });
                    });
                harness.run_steps(3);
            }
        }
    }
}
