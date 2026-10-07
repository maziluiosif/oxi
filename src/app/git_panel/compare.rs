//! Compare tab: the current branch against a base branch, like GitLens "Compare with" or a
//! GitHub pull request. Commits the base lacks, then every changed file (committed or not)
//! as a tree. A file opens as a diff against the merge base; its hover action opens the file
//! itself, whose gutter then marks every line changed since the base, so it can be edited
//! in place.

use std::collections::{BTreeMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::mpsc::{self, Receiver};

use eframe::egui::{
    self, Align, Color32, CornerRadius, FontId, Layout, RichText, ScrollArea, Sense, Ui,
};

use crate::git::{CompareFile, GitCompare, GitLineChange, GitOp, GitState};
use crate::theme::*;

use super::super::OxiApp;
use super::GitTab;

const ROW_H: f32 = 22.0;
const INDENT: f32 = 12.0;
const CHEVRON_W: f32 = 14.0;

#[derive(Default)]
pub struct CompareView {
    pub data: Option<GitCompare>,
    /// Base picked by the user; empty follows the repository's default branch.
    base: String,
    rx: Option<(String, Receiver<GitCompare>)>,
    /// The work tree changed while a comparison ran: run it again when it lands.
    rerun: bool,
    /// [`worktree_fingerprint`] of the snapshot the comparison reflects.
    fingerprint: Option<u64>,
    tree: CompareTree,
    collapsed: HashSet<String>,
    commits_collapsed: bool,
    files_collapsed: bool,
    flat: bool,
}

#[derive(Default)]
struct CompareTree {
    dirs: Vec<CompareDir>,
    /// Indices into [`GitCompare::files`].
    files: Vec<usize>,
}

struct CompareDir {
    /// Single-child folder chains are merged into one row, e.g. `src/app/git_panel`.
    label: String,
    path: String,
    tree: CompareTree,
}

impl CompareTree {
    fn build(files: &[CompareFile]) -> Self {
        #[derive(Default)]
        struct Node {
            dirs: BTreeMap<String, Node>,
            files: Vec<usize>,
        }
        fn convert(node: Node, prefix: &str) -> CompareTree {
            let dirs = node
                .dirs
                .into_iter()
                .map(|(name, mut child)| {
                    let mut label = name;
                    let mut path = if prefix.is_empty() {
                        label.clone()
                    } else {
                        format!("{prefix}/{label}")
                    };
                    while child.files.is_empty() && child.dirs.len() == 1 {
                        let Some((name, grandchild)) = child.dirs.pop_first() else {
                            break;
                        };
                        label = format!("{label}/{name}");
                        path = format!("{path}/{name}");
                        child = grandchild;
                    }
                    let tree = convert(child, &path);
                    CompareDir { label, path, tree }
                })
                .collect();
            CompareTree {
                dirs,
                files: node.files,
            }
        }
        let mut root = Node::default();
        for (index, file) in files.iter().enumerate() {
            let mut node = &mut root;
            let mut parts = file.path.split('/').collect::<Vec<_>>();
            parts.pop();
            for part in parts {
                node = node.dirs.entry(part.to_owned()).or_default();
            }
            node.files.push(index);
        }
        convert(root, "")
    }
}

/// Changes whenever the branch, its commits or the work tree change, so the comparison is
/// recomputed only then and not on every two-second auto refresh.
fn worktree_fingerprint(git: &GitState) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    git.branch.hash(&mut hasher);
    git.log.first().map(|commit| &commit.hash).hash(&mut hasher);
    for entry in git.staged.iter().chain(&git.unstaged) {
        (&entry.path, entry.status).hash(&mut hasher);
    }
    // Map order is arbitrary: combine the entries order-independently.
    let mut lines = 0u64;
    for (path, changes) in &git.line_changes {
        let mut entry = std::collections::hash_map::DefaultHasher::new();
        path.hash(&mut entry);
        for change in changes {
            (change.line, change.kind == crate::git::GitLineKind::Added).hash(&mut entry);
        }
        lines = lines.wrapping_add(entry.finish());
    }
    lines.hash(&mut hasher);
    hasher.finish()
}

impl OxiApp {
    /// Line markers for the editor gutter: changes since the compare base while the Compare
    /// tab is showing, otherwise the uncommitted changes.
    pub(crate) fn git_gutter_line_changes(&self, relative: &str) -> Option<&Vec<GitLineChange>> {
        if self.conv.git_ui.open
            && self.conv.git_ui.tab == GitTab::Compare
            && let Some(data) = &self.conv.git_ui.compare.data
            && data.error.is_none()
        {
            return data.line_changes.get(relative);
        }
        self.conv.git.line_changes.get(relative)
    }

    fn request_git_compare(&mut self) {
        let fingerprint = worktree_fingerprint(&self.conv.git);
        let cwd = self.active_workspace().root_path.clone();
        let ctx = self.conv.git_ctx.clone();
        let view = &mut self.conv.git_ui.compare;
        view.fingerprint = Some(fingerprint);
        if view.rx.is_some() {
            view.rerun = true;
            return;
        }
        let base = view.base.clone();
        let (tx, rx) = mpsc::channel();
        let worker_cwd = cwd.clone();
        let spawned = std::thread::Builder::new()
            .name("oxi-git-compare".into())
            .spawn(move || {
                let _ = tx.send(crate::git::compare(&worker_cwd, &base));
                ctx.request_repaint();
            });
        if spawned.is_ok() {
            view.rx = Some((cwd, rx));
        }
    }

    pub(super) fn drain_git_compare(&mut self) {
        let Some((cwd, rx)) = &self.conv.git_ui.compare.rx else {
            return;
        };
        let result = match rx.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => None,
        };
        let current = cwd == &self.active_workspace().root_path;
        let view = &mut self.conv.git_ui.compare;
        view.rx = None;
        if let Some(result) = result.filter(|_| current) {
            view.tree = CompareTree::build(&result.files);
            view.data = Some(result);
        }
        if std::mem::take(&mut view.rerun) {
            self.request_git_compare();
        }
    }

    pub(super) fn render_git_compare(&mut self, ui: &mut Ui) {
        let loading = self.conv.git_ui.compare.rx.is_some();
        if !loading && !self.conv.git.busy {
            let fingerprint = worktree_fingerprint(&self.conv.git);
            if self.conv.git_ui.compare.data.is_none()
                || self.conv.git_ui.compare.fingerprint != Some(fingerprint)
            {
                self.request_git_compare();
            }
        }

        self.render_compare_header(ui, loading);
        ui.add_space(6.0);
        crate::ui::chrome::hairline(ui);
        ui.add_space(4.0);

        let Some(data) = self.conv.git_ui.compare.data.take() else {
            return;
        };
        if let Some(error) = &data.error {
            ui.label(RichText::new(error).size(FS_SMALL).color(c_text_muted()));
            self.conv.git_ui.compare.data = Some(data);
            return;
        }
        let tree = std::mem::take(&mut self.conv.git_ui.compare.tree);
        ScrollArea::vertical()
            .id_salt("git_compare_scroll")
            .max_height(ui.available_height())
            .auto_shrink([false, true])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded)
            .show(ui, |ui| {
                if data.commits.is_empty() && data.files.is_empty() {
                    ui.add_space(10.0);
                    ui.label(
                        RichText::new(format!("{} is up to date with {}", data.branch, data.base))
                            .size(FS_SMALL)
                            .color(c_text_muted()),
                    );
                    return;
                }
                self.render_compare_commits(ui, &data);
                ui.add_space(6.0);
                self.render_compare_files(ui, &data, &tree);
            });
        self.conv.git_ui.compare.tree = tree;
        // A new comparison may have landed meanwhile only via `drain_git_compare`, which
        // runs outside rendering; putting the old one back cannot overwrite it.
        self.conv.git_ui.compare.data = Some(data);
    }

    fn render_compare_header(&mut self, ui: &mut Ui, loading: bool) {
        let data = self.conv.git_ui.compare.data.as_ref();
        let shown_base = data.map(|data| data.base.clone()).unwrap_or_default();
        let bases = data.map(|data| data.bases.clone()).unwrap_or_default();
        let mut picked = None;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.label(
                RichText::new("Base")
                    .size(FS_TINY)
                    .color(c_text_muted())
                    .strong(),
            );
            let refresh_w = 22.0 + ui.spacing().item_spacing.x;
            egui::ComboBox::from_id_salt("git_compare_base")
                .selected_text(
                    RichText::new(if shown_base.is_empty() {
                        "…"
                    } else {
                        &shown_base
                    })
                    .size(FS_SMALL)
                    .monospace(),
                )
                .width((ui.available_width() - refresh_w).max(80.0))
                .height(320.0)
                .show_ui(ui, |ui| {
                    for base in &bases {
                        if ui
                            .selectable_label(
                                *base == shown_base,
                                RichText::new(base).size(FS_SMALL).monospace(),
                            )
                            .clicked()
                        {
                            picked = Some(base.clone());
                        }
                    }
                });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if loading {
                    ui.add(egui::Spinner::new().size(12.0).color(c_text_muted()));
                } else if crate::ui::chrome::icon_button_plain(ui, ICON_REFRESH, 22.0, false)
                    .on_hover_text("Compare again")
                    .clicked()
                {
                    self.request_git_compare();
                }
            });
        });
        if let Some(base) = picked
            && base != shown_base
        {
            self.conv.git_ui.compare.base = base;
            self.request_git_compare();
        }

        let Some(data) = self
            .conv
            .git_ui
            .compare
            .data
            .as_ref()
            .filter(|d| d.error.is_none())
        else {
            return;
        };
        ui.add_space(2.0);
        let (added, deleted) = data
            .files
            .iter()
            .fold((0, 0), |(a, d), f| (a + f.added, d + f.deleted));
        let merge_base = &data.merge_base[..data.merge_base.len().min(7)];
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(4.0, 2.0);
            ui.label(
                RichText::new(format!("{} → {}", data.branch, data.base))
                    .size(FS_TINY)
                    .color(c_text_muted())
                    .monospace(),
            )
            .on_hover_text(format!(
                "Changes on {} since it forked from {} (merge base {merge_base}), \
                 including uncommitted work",
                data.branch, data.base
            ));
            ui.label(
                RichText::new(format!("+{added}"))
                    .size(FS_TINY)
                    .color(c_success())
                    .monospace(),
            );
            ui.label(
                RichText::new(format!("−{deleted}"))
                    .size(FS_TINY)
                    .color(c_danger())
                    .monospace(),
            );
        });
    }

    fn render_compare_commits(&mut self, ui: &mut Ui, data: &GitCompare) {
        let count = if data.commits_truncated {
            format!("{}+", data.commits.len())
        } else {
            data.commits.len().to_string()
        };
        let collapsed = self.conv.git_ui.compare.commits_collapsed;
        if compare_section_header(ui, &format!("Commits {count}"), collapsed, |_| {}) {
            self.conv.git_ui.compare.commits_collapsed = !collapsed;
        }
        if collapsed {
            return;
        }
        if data.commits.is_empty() {
            ui.label(
                RichText::new("No commits yet — only uncommitted changes")
                    .size(FS_TINY)
                    .color(c_text_faint()),
            );
        }
        for (i, commit) in data.commits.iter().enumerate() {
            ui.push_id(("compare_commit", i), |ui| {
                self.render_commit_row(ui, commit)
            });
        }
    }

    fn render_compare_files(&mut self, ui: &mut Ui, data: &GitCompare, tree: &CompareTree) {
        let collapsed = self.conv.git_ui.compare.files_collapsed;
        let mut flat = self.conv.git_ui.compare.flat;
        let title = format!("Files changed {}", data.files.len());
        if compare_section_header(ui, &title, collapsed, |ui| {
            let (icon, hover) = if flat {
                (ICON_FOLDER, "View as tree")
            } else {
                (ICON_MENU, "View as list")
            };
            if crate::ui::chrome::icon_button_inline(ui, icon, FS_TINY, c_text_faint())
                .on_hover_text(hover)
                .clicked()
            {
                flat = !flat;
            }
        }) {
            self.conv.git_ui.compare.files_collapsed = !collapsed;
        }
        self.conv.git_ui.compare.flat = flat;
        if collapsed {
            return;
        }
        if flat {
            for index in 0..data.files.len() {
                self.render_compare_file(ui, data, index, 0, true);
            }
        } else {
            self.render_compare_tree(ui, data, tree, 0);
        }
    }

    fn render_compare_tree(
        &mut self,
        ui: &mut Ui,
        data: &GitCompare,
        tree: &CompareTree,
        depth: usize,
    ) {
        for dir in &tree.dirs {
            let collapsed = self.conv.git_ui.compare.collapsed.contains(&dir.path);
            let (rect, response) =
                ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H), Sense::click());
            if ui.is_rect_visible(rect) {
                paint_row_fill(ui, rect, response.hovered(), false);
                let painter = ui.painter_at(rect);
                let x = rect.left() + 4.0 + depth as f32 * INDENT;
                let y = rect.center().y;
                painter.text(
                    egui::pos2(x + CHEVRON_W * 0.5, y),
                    egui::Align2::CENTER_CENTER,
                    if collapsed {
                        ICON_CHEVRON_RIGHT
                    } else {
                        ICON_ANGLE_DOWN
                    },
                    FontId::new(FS_TINY, icon_font()),
                    c_text_faint(),
                );
                let icon = painter.text(
                    egui::pos2(x + CHEVRON_W + 2.0, y),
                    egui::Align2::LEFT_CENTER,
                    if collapsed {
                        ICON_FOLDER
                    } else {
                        ICON_FOLDER_OPEN
                    },
                    FontId::new(FS_SMALL, icon_font()),
                    c_text_muted(),
                );
                paint_truncated(
                    &painter,
                    &dir.label,
                    egui::pos2(icon.right() + 5.0, y),
                    rect.right() - 14.0,
                    c_text(),
                );
            }
            if response.clicked() {
                if collapsed {
                    self.conv.git_ui.compare.collapsed.remove(&dir.path);
                } else {
                    self.conv.git_ui.compare.collapsed.insert(dir.path.clone());
                }
            }
            if !collapsed {
                self.render_compare_tree(ui, data, &dir.tree, depth + 1);
            }
        }
        for &index in &tree.files {
            self.render_compare_file(ui, data, index, depth, false);
        }
    }

    fn render_compare_file(
        &mut self,
        ui: &mut Ui,
        data: &GitCompare,
        index: usize,
        depth: usize,
        flat: bool,
    ) {
        let file = &data.files[index];
        let (rect, response) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H), Sense::click());
        let selected = match self.active_diff_target() {
            Some((path, crate::app::file_explorer::DiffSource::Compare { base, .. })) => {
                path == file.path && *base == data.base
            }
            Some(_) => false,
            None => {
                self.conv.diff_view.open
                    && self.conv.editor.diff_tab_active
                    && self.conv.git.current_diff_path.as_deref() == Some(file.path.as_str())
                    && self.conv.git.diff.as_ref().is_some_and(|(title, _)| {
                        title.starts_with(crate::git::COMPARE_TITLE_PREFIX)
                    })
            }
        };
        let uncommitted = self
            .conv
            .git
            .staged
            .iter()
            .chain(&self.conv.git.unstaged)
            .any(|entry| entry.path == file.path);
        let row_hot = ui
            .input(|i| i.pointer.hover_pos())
            .is_some_and(|p| rect.contains(p));
        let can_open = file.status != 'D';
        let open_rect = egui::Rect::from_center_size(
            egui::pos2(rect.right() - 12.0, rect.center().y),
            egui::vec2(18.0, 18.0),
        );
        let mut open_clicked = false;
        if row_hot && can_open {
            let open = ui
                .interact(
                    open_rect,
                    ui.id().with(("compare_open", index)),
                    Sense::click(),
                )
                .on_hover_text("Open file to edit");
            open_clicked = open.clicked();
        }

        if ui.is_rect_visible(rect) {
            paint_row_fill(ui, rect, response.hovered(), selected);
            let painter = ui.painter_at(rect);
            let y = rect.center().y;
            let mut right = rect.right() - 10.0;
            if row_hot && can_open {
                painter.text(
                    open_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    ICON_FILE,
                    FontId::new(FS_TINY, icon_font()),
                    c_text_muted(),
                );
                right = open_rect.left() - 4.0;
            } else {
                let status = painter.text(
                    egui::pos2(right, y),
                    egui::Align2::RIGHT_CENTER,
                    file.status,
                    FontId::monospace(FS_TINY),
                    crate::app::file_explorer::git_status_color(file.status),
                );
                right = status.left() - 6.0;
            }
            for (sign, count, color) in [
                ('−', file.deleted, c_danger()),
                ('+', file.added, c_success()),
            ] {
                if count > 0 {
                    let r = painter.text(
                        egui::pos2(right, y),
                        egui::Align2::RIGHT_CENTER,
                        format!("{sign}{count}"),
                        FontId::monospace(FS_TINY),
                        color,
                    );
                    right = r.left() - 4.0;
                }
            }
            if uncommitted {
                let r = painter.text(
                    egui::pos2(right, y),
                    egui::Align2::RIGHT_CENTER,
                    "●",
                    FontId::proportional(FS_TINY * 0.8),
                    c_warning_fg(),
                );
                right = r.left() - 4.0;
            }

            let x = rect.left() + 4.0 + depth as f32 * INDENT + CHEVRON_W + 2.0;
            let (icon, icon_color) =
                crate::app::file_explorer::file_icon(std::path::Path::new(&file.path));
            let icon = painter.text(
                egui::pos2(x, y),
                egui::Align2::LEFT_CENTER,
                icon,
                FontId::new(FS_SMALL, icon_font()),
                icon_color,
            );
            let (dir, name) = match file.path.rsplit_once('/') {
                Some((dir, name)) => (Some(dir), name),
                None => (None, file.path.as_str()),
            };
            let name_color = if file.status == 'D' {
                c_text_muted()
            } else if selected {
                c_text_strong()
            } else {
                c_text()
            };
            let mut job = egui::text::LayoutJob::default();
            job.append(
                name,
                0.0,
                egui::TextFormat {
                    strikethrough: if file.status == 'D' {
                        egui::Stroke::new(1.0, c_text_muted())
                    } else {
                        egui::Stroke::NONE
                    },
                    ..egui::TextFormat::simple(FontId::proportional(FS_SMALL), name_color)
                },
            );
            if flat && let Some(dir) = dir {
                job.append(
                    dir,
                    8.0,
                    egui::TextFormat::simple(FontId::proportional(FS_TINY), c_text_faint()),
                );
            }
            let x = icon.right() + 5.0;
            job.wrap = egui::text::TextWrapping::truncate_at_width((right - 6.0 - x).max(0.0));
            let galley = painter.layout_job(job);
            painter.galley(egui::pos2(x, y - galley.size().y * 0.5), galley, name_color);
        }

        let response = response.on_hover_ui(|ui| {
            let mut text = file.path.clone();
            if let Some(old) = &file.old_path {
                text = format!("{old} → {text}");
            }
            text.push_str(&format!(
                "\n+{} −{} since {}",
                file.added, file.deleted, data.base
            ));
            if uncommitted {
                text.push_str("\nIncludes uncommitted changes");
            }
            text.push_str("\n\nClick: diff · Double-click: open file to edit");
            ui.label(RichText::new(text).size(FS_SMALL).color(c_text()));
        });
        if open_clicked || (response.double_clicked() && can_open) {
            self.open_changed_file(&file.path);
        } else if response.clicked() {
            let source = crate::app::file_explorer::DiffSource::Compare {
                base: data.base.clone(),
                old_path: file.old_path.clone(),
            };
            if !self.open_diff_editor(&file.path, source, None) {
                self.request(GitOp::ShowCompareDiff {
                    base: data.base.clone(),
                    path: file.path.clone(),
                    old_path: file.old_path.clone(),
                });
                self.conv.diff_view.open = true;
                self.conv.editor.diff_tab_active = true;
            }
        }
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }
}

/// A clickable section title with a chevron; `actions` adds buttons on the right.
/// Returns whether the title was clicked.
fn compare_section_header(
    ui: &mut Ui,
    title: &str,
    collapsed: bool,
    actions: impl FnOnce(&mut Ui),
) -> bool {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 20.0), Sense::click());
    let painter = ui.painter_at(rect);
    painter.text(
        egui::pos2(rect.left() + 4.0 + CHEVRON_W * 0.5, rect.center().y),
        egui::Align2::CENTER_CENTER,
        if collapsed {
            ICON_CHEVRON_RIGHT
        } else {
            ICON_ANGLE_DOWN
        },
        FontId::new(FS_TINY, icon_font()),
        c_text_faint(),
    );
    painter.text(
        egui::pos2(rect.left() + 4.0 + CHEVRON_W + 2.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        title.to_uppercase(),
        FontId::proportional(FS_TINY),
        if response.hovered() {
            c_text_muted()
        } else {
            c_text_faint()
        },
    );
    let mut actions_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink2(egui::vec2(4.0, 0.0)))
            .layout(Layout::right_to_left(Align::Center)),
    );
    actions(&mut actions_ui);
    response.clicked()
}

fn paint_row_fill(ui: &Ui, rect: egui::Rect, hovered: bool, selected: bool) {
    let fill = if selected {
        c_row_active()
    } else if hovered {
        c_row_hover()
    } else {
        Color32::TRANSPARENT
    };
    ui.painter()
        .rect_filled(rect, CornerRadius::same(RADIUS_ROW), fill);
}

fn paint_truncated(
    painter: &egui::Painter,
    text: &str,
    left_center: egui::Pos2,
    right: f32,
    color: Color32,
) {
    let mut job = egui::text::LayoutJob::single_section(
        text.to_owned(),
        egui::TextFormat::simple(FontId::proportional(FS_SMALL), color),
    );
    job.wrap = egui::text::TextWrapping::truncate_at_width((right - left_center.x).max(0.0));
    let galley = painter.layout_job(job);
    painter.galley(
        egui::pos2(left_center.x, left_center.y - galley.size().y * 0.5),
        galley,
        color,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str) -> CompareFile {
        CompareFile {
            path: path.into(),
            old_path: None,
            status: 'M',
            added: 1,
            deleted: 0,
        }
    }

    #[test]
    fn tree_merges_single_child_folders() {
        let files = [
            file("README.md"),
            file("src/app/git_panel/compare.rs"),
            file("src/app/git_panel/refs.rs"),
            file("src/git.rs"),
        ];
        let tree = CompareTree::build(&files);
        assert_eq!(tree.files, vec![0]);
        assert_eq!(tree.dirs.len(), 1);
        let src = &tree.dirs[0];
        assert_eq!((src.label.as_str(), src.files_len()), ("src", 1));
        assert_eq!(src.tree.dirs[0].label, "app/git_panel");
        assert_eq!(src.tree.dirs[0].path, "src/app/git_panel");
        assert_eq!(src.tree.dirs[0].tree.files, vec![1, 2]);
    }

    impl CompareDir {
        fn files_len(&self) -> usize {
            self.tree.files.len()
        }
    }
}
