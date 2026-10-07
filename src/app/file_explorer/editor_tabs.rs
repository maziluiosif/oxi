//! Editor tab strip and tab-level actions.

use std::path::PathBuf;

use eframe::egui::scroll_area::ScrollBarVisibility;
use eframe::egui::{self, Align, FontId, Frame, Layout, Margin, ScrollArea, Ui};

use crate::theme::*;
use crate::ui::chrome::icon_glyph_rich;

use super::super::{OxiApp, state::FileOperation};

impl OxiApp {
    pub(super) fn render_editor_tabs(&mut self, ui: &mut Ui) {
        let mut select = None;
        let mut close = None;
        let mut save = false;
        let mut reveal = false;
        let mut toggle_diff = false;
        let mut select_git_diff = false;
        let mut close_git_diff = false;
        let mut new_file = false;
        let mut navigate_back = false;
        let mut navigate_forward = false;
        let git_diff_tab = self.conv.diff_view.open && self.conv.git.diff.is_some();
        let git_diff_active = git_diff_tab && self.conv.editor.diff_tab_active;
        let can_go_back = !self.conv.editor.navigation_back.is_empty();
        let can_go_forward = !self.conv.editor.navigation_forward.is_empty();
        let sidebar_open = self.conv.sidebar.open;
        let workspace_root = PathBuf::from(&self.active_workspace().root_path);
        let tab_strip_width = (ui.available_width()
            - 126.0
            - if sidebar_open {
                0.0
            } else {
                crate::ui::window_chrome::TRAFFIC_LIGHTS_W
            })
        .max(80.0);

        Frame::new()
            .fill(c_bg_elevated_2())
            .inner_margin(Margin::symmetric(6, 0))
            .show(ui, |ui| {
                ui.set_height(34.0);
                // Not `ui.horizontal`: that caps the row at `interact_size.y`, so the tabs would
                // stop short of the strip's lower edge and a hovered tab would float above it.
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    if !sidebar_open {
                        // The macOS traffic lights sit over the window's top-left corner.
                        ui.add_space(crate::ui::window_chrome::TRAFFIC_LIGHTS_W);
                    }
                    let back = ui.add_enabled(
                        can_go_back,
                        egui::Button::new(icon_glyph_rich(
                            ICON_CHEVRON_LEFT,
                            FS_SMALL,
                            c_text_muted(),
                        ))
                        .frame(false)
                        .min_size(egui::vec2(24.0, 30.0)),
                    );
                    if back.clicked() {
                        navigate_back = true;
                    }
                    let forward = ui.add_enabled(
                        can_go_forward,
                        egui::Button::new(icon_glyph_rich(
                            ICON_CHEVRON_RIGHT,
                            FS_SMALL,
                            c_text_muted(),
                        ))
                        .frame(false)
                        .min_size(egui::vec2(24.0, 30.0)),
                    );
                    if forward.clicked() {
                        navigate_forward = true;
                    }

                    ScrollArea::horizontal()
                        .id_salt("editor_tabs")
                        .max_width(tab_strip_width)
                        .scroll_bar_visibility(ScrollBarVisibility::AlwaysHidden)
                        .show(ui, |ui| {
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                ui.spacing_mut().item_spacing.x = 0.0;
                                for (index, document) in
                                    self.conv.editor.documents.iter().enumerate()
                                {
                                    let name = if document.is_scratchpad {
                                        std::borrow::Cow::Borrowed("Scratchpad")
                                    } else {
                                        document
                                            .path
                                            .file_name()
                                            .unwrap_or_default()
                                            .to_string_lossy()
                                    };
                                    let active =
                                        !git_diff_active && self.conv.editor.active == Some(index);
                                    let tab = editor_tab(
                                        ui,
                                        ui.id().with(("editor_tab", index)),
                                        &name,
                                        active,
                                        document.is_dirty(),
                                    );
                                    let response = tab.response;
                                    if tab.close_clicked || response.middle_clicked() {
                                        close = Some(index);
                                    } else if response.clicked() {
                                        select = Some(index);
                                    }
                                    let response = if document.is_scratchpad {
                                        response.on_hover_text("Scratchpad")
                                    } else {
                                        response.on_hover_ui(|ui| {
                                            ui.label(super::support::display_path(
                                                &workspace_root,
                                                &document.path,
                                            ));
                                        })
                                    };
                                    response.context_menu(|ui| {
                                        if ui.button("Save").clicked() {
                                            select = Some(index);
                                            save = true;
                                            ui.close();
                                        }
                                        if !document.is_scratchpad
                                            && ui.button("Reveal in Explorer").clicked()
                                        {
                                            select = Some(index);
                                            reveal = true;
                                            ui.close();
                                        }
                                        if !document.is_scratchpad
                                            && ui.button("Unsaved changes diff").clicked()
                                        {
                                            select = Some(index);
                                            toggle_diff = true;
                                            ui.close();
                                        }
                                        if ui.button("Close").clicked() {
                                            close = Some(index);
                                            ui.close();
                                        }
                                    });
                                }

                                // Git diff pseudo-tab: keeps the diff one click away from the
                                // editable file tabs instead of replacing the whole chat area.
                                if git_diff_tab {
                                    let label = self.git_diff_tab_label();
                                    let tab = editor_tab(
                                        ui,
                                        ui.id().with("editor_git_diff_tab"),
                                        &label,
                                        git_diff_active,
                                        false,
                                    );
                                    let response = tab.response;
                                    if tab.close_clicked || response.middle_clicked() {
                                        close_git_diff = true;
                                    } else if response.clicked() {
                                        select_git_diff = true;
                                    }
                                    let hover = self.git_diff_tab_hover();
                                    response.on_hover_text(hover);
                                }
                            });
                        });

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        // Frameless like the neighbouring buttons: a boxed button here read
                        // as a stray rectangle at the end of the tab strip.
                        egui::containers::menu::MenuButton::from_button(
                            egui::Button::new(icon_glyph_rich(
                                ICON_ANGLE_DOWN,
                                FS_SMALL,
                                c_text_muted(),
                            ))
                            .frame(false)
                            .min_size(egui::vec2(24.0, 30.0)),
                        )
                        .ui(ui, |ui| {
                            ui.set_min_width(180.0);
                            for (index, document) in self.conv.editor.documents.iter().enumerate() {
                                let name = if document.is_scratchpad {
                                    std::borrow::Cow::Borrowed("Scratchpad")
                                } else {
                                    document
                                        .path
                                        .file_name()
                                        .unwrap_or_default()
                                        .to_string_lossy()
                                };
                                if ui
                                    .selectable_label(
                                        !git_diff_active && self.conv.editor.active == Some(index),
                                        name,
                                    )
                                    .clicked()
                                {
                                    select = Some(index);
                                    ui.close();
                                }
                            }
                        })
                        .0
                        .on_hover_text("Open tabs");
                        if ui
                            .add(
                                egui::Button::new(icon_glyph_rich(
                                    ICON_PLUS,
                                    FS_SMALL,
                                    c_text_muted(),
                                ))
                                .frame(false)
                                .min_size(egui::vec2(24.0, 30.0)),
                            )
                            .on_hover_text("New file")
                            .clicked()
                        {
                            new_file = true;
                        }
                    });
                });
            });
        if new_file {
            let root = PathBuf::from(&self.active_workspace().root_path);
            self.start_file_operation(FileOperation::NewFile(root));
        }
        if navigate_back {
            self.navigate_editor_history(false);
        } else if navigate_forward {
            self.navigate_editor_history(true);
        }
        if let Some(index) = select {
            self.conv.editor.git_full_highlight_path = None;
            self.conv.editor.active = Some(index);
            self.conv.editor.diff_tab_active = false;
            self.conv.editor.focus_editor_next_frame = true;
            if let Some(path) = self
                .conv
                .editor
                .documents
                .get(index)
                .filter(|document| !document.is_scratchpad)
                .map(|document| document.path.clone())
            {
                self.reveal_editor_file_in_explorer(&path);
            }
        }
        if select_git_diff {
            self.conv.editor.diff_tab_active = true;
        }
        if close_git_diff {
            self.close_editor_git_diff();
        }
        if save {
            self.save_editor_file();
        }
        if reveal {
            self.reveal_active_file();
        }
        if toggle_diff {
            self.conv.editor.show_diff = !self.conv.editor.show_diff;
        }
        if let Some(index) = close {
            self.request_close_editor_tab(index);
        }
    }

    /// VS Code-style diff tab title: `stats.py (Working Tree)`, `stats.py (Staged)` or
    /// `Commit 1a2b3c4`.
    fn git_diff_tab_label(&self) -> String {
        let title = self.conv.git.diff.as_ref().map_or("", |(title, _)| title);
        if let Some(hash) = title.strip_prefix("Commit ") {
            return format!("Commit {}", &hash[..hash.len().min(7)]);
        }
        let path = self.conv.git.current_diff_path.as_deref().unwrap_or("diff");
        let file = path.rsplit_once('/').map_or(path, |(_, file)| file);
        if let Some(base) = compare_base(title) {
            format!("{file} (vs {})", crate::git::checkpoint::base_label(base))
        } else if self.conv.git.current_diff_staged == Some(true) {
            format!("{file} (Staged)")
        } else {
            format!("{file} (Working Tree)")
        }
    }

    /// Diff tab tooltip: the full repo-relative path and side (the tab itself only fits the
    /// file name), or the full hash and subject for a commit.
    fn git_diff_tab_hover(&self) -> String {
        let title = self.conv.git.diff.as_ref().map_or("", |(title, _)| title);
        let path = self
            .conv
            .git
            .current_diff_path
            .as_deref()
            .unwrap_or_default();
        if title.starts_with("Commit ") {
            return title.to_owned();
        }
        if let Some(base) = compare_base(title) {
            format!(
                "{path} · Changes since {}",
                crate::git::checkpoint::base_label(base)
            )
        } else if self.conv.git.current_diff_staged == Some(true) {
            format!("{path} · Staged changes")
        } else {
            format!("{path} · Working tree changes")
        }
    }
}

/// The base branch of a branch-compare diff title.
pub(crate) fn compare_base(title: &str) -> Option<&str> {
    let rest = title.strip_prefix(crate::git::COMPARE_TITLE_PREFIX)?;
    rest.split_once(": ").map(|(base, _)| base)
}

struct EditorTab {
    response: egui::Response,
    close_clicked: bool,
}

/// One Sublime-style tab. The active tab takes the editor's background and merges into it;
/// a hovered tab lights up within its own outline only (no pill spilling into the editor).
/// The close button shows on the active and hovered tabs; an unsaved tab shows a dot there
/// instead until hovered.
fn editor_tab(ui: &mut Ui, id: egui::Id, label: &str, active: bool, dirty: bool) -> EditorTab {
    const PADDING_LEFT: f32 = 12.0;
    const CLOSE_SLOT: f32 = 26.0;
    const TOP_INSET: f32 = 8.0;
    let font = FontId::proportional(FS_SMALL);
    let galley = ui.fonts_mut(|fonts| fonts.layout_no_wrap(label.to_owned(), font, c_text()));
    let width = (galley.rect.width() + PADDING_LEFT + CLOSE_SLOT).max(72.0);
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(width, ui.available_height()),
        egui::Sense::click(),
    );
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::SelectableLabel,
            ui.is_enabled(),
            active,
            label,
        )
    });
    // The close target overlaps the tab; test the whole tab so both share one hover state.
    let hovered = ui.rect_contains_pointer(rect);
    let body = egui::Rect::from_min_max(egui::pos2(rect.left(), rect.top() + TOP_INSET), rect.max);
    let top_rounded = egui::CornerRadius {
        nw: RADIUS_ROW,
        ne: RADIUS_ROW,
        sw: 0,
        se: 0,
    };
    if active {
        // Reach past the strip's lower edge so the tab and the editor read as one surface.
        let mut fill = body;
        fill.max.y += 3.0;
        ui.painter().rect_filled(fill, top_rounded, c_bg_main());
    } else if hovered {
        // Well short of the active tab's color, and inset from the neighbours, so a hovered tab
        // next to the active one never reads as one merged shape.
        let fill = c_bg_elevated_2().lerp_to_gamma(c_bg_main(), 0.3);
        ui.painter()
            .rect_filled(body.shrink2(egui::vec2(2.0, 0.0)), top_rounded, fill);
    }
    let text_color = if active {
        c_text_strong()
    } else if hovered {
        c_text()
    } else {
        c_text_muted()
    };
    ui.painter().galley(
        egui::pos2(
            body.left() + PADDING_LEFT,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        text_color,
    );

    let close_rect = egui::Rect::from_center_size(
        egui::pos2(body.right() - CLOSE_SLOT / 2.0, rect.center().y),
        egui::vec2(18.0, 18.0),
    );
    let close = ui.interact(close_rect, id.with("close"), egui::Sense::click());
    close.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            ui.is_enabled(),
            format!("Close {label}"),
        )
    });
    if close.hovered() {
        ui.painter().rect_filled(
            close_rect,
            egui::CornerRadius::same(4),
            c_bg_elevated_2().lerp_to_gamma(c_text_faint(), 0.25),
        );
    }
    if hovered || active && !dirty {
        ui.painter().text(
            close_rect.center(),
            egui::Align2::CENTER_CENTER,
            ICON_CLOSE,
            FontId::new(FS_TINY, icon_font()),
            if close.hovered() {
                c_text_strong()
            } else {
                c_text_faint()
            },
        );
    } else if dirty {
        ui.painter().circle_filled(
            close_rect.center(),
            3.5,
            if active { c_text() } else { c_text_muted() },
        );
    }
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    EditorTab {
        response,
        close_clicked: close.clicked(),
    }
}
