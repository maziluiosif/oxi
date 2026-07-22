//! Editor tab strip and tab-level actions.

use std::path::PathBuf;

use eframe::egui::scroll_area::ScrollBarVisibility;
use eframe::egui::{self, Align, FontId, Frame, Layout, Margin, RichText, ScrollArea, Ui};

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
        let git_diff_tab = self.conv.diff_view_open && self.conv.git.diff.is_some();
        let git_diff_active = git_diff_tab && self.conv.editor.diff_tab_active;
        let can_go_back = !self.conv.editor.navigation_back.is_empty();
        let can_go_forward = !self.conv.editor.navigation_forward.is_empty();
        let tab_strip_width = (ui.available_width() - 126.0).max(80.0);

        Frame::new()
            .fill(c_bg_elevated_2())
            .inner_margin(Margin::symmetric(6, 0))
            .show(ui, |ui| {
                ui.set_height(34.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
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
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 0.0;
                                for (index, document) in
                                    self.conv.editor.documents.iter().enumerate()
                                {
                                    let name = document
                                        .path
                                        .file_name()
                                        .unwrap_or_default()
                                        .to_string_lossy();
                                    let label = if document.is_dirty() {
                                        format!("{name}  ●")
                                    } else {
                                        name.into_owned()
                                    };
                                    let active =
                                        !git_diff_active && self.conv.editor.active == Some(index);
                                    let font = FontId::proportional(FS_SMALL);
                                    let label_width = ui.fonts_mut(|fonts| {
                                        fonts
                                            .layout_no_wrap(label.clone(), font.clone(), c_text())
                                            .rect
                                            .width()
                                    });
                                    let tab_width = label_width + 42.0;
                                    let (rect, response) = ui.allocate_exact_size(
                                        egui::vec2(tab_width, 28.0),
                                        egui::Sense::click(),
                                    );
                                    // The close hit target overlaps the tab response. Test the full rectangle
                                    // so the name and close icon still share one hover surface.
                                    let hovered = ui.rect_contains_pointer(rect);
                                    let fill = if active {
                                        c_bg_main()
                                    } else if hovered {
                                        c_row_hover()
                                    } else {
                                        egui::Color32::TRANSPARENT
                                    };
                                    if fill != egui::Color32::TRANSPARENT {
                                        let mut fill_rect = rect;
                                        if active || hovered {
                                            // Active and hovered tabs share the same silhouette, extending through
                                            // the header's lower edge instead of looking like floating pills.
                                            fill_rect.max.y += RADIUS_ROW as f32 + 4.0;
                                        }
                                        ui.painter().rect_filled(
                                            fill_rect,
                                            egui::CornerRadius::same(RADIUS_ROW),
                                            fill,
                                        );
                                    }
                                    ui.painter().text(
                                        egui::pos2(rect.left() + 10.0, rect.center().y),
                                        egui::Align2::LEFT_CENTER,
                                        label,
                                        font,
                                        if active {
                                            c_text_strong()
                                        } else {
                                            c_text_muted()
                                        },
                                    );
                                    let close_rect = egui::Rect::from_center_size(
                                        egui::pos2(rect.right() - 13.0, rect.center().y),
                                        egui::vec2(22.0, rect.height()),
                                    );
                                    let close_response = ui.interact(
                                        close_rect,
                                        ui.id().with(("editor_tab_close", index)),
                                        egui::Sense::click(),
                                    );
                                    ui.painter().text(
                                        close_rect.center(),
                                        egui::Align2::CENTER_CENTER,
                                        ICON_CLOSE,
                                        FontId::new(FS_TINY, icon_font()),
                                        if close_response.hovered() {
                                            c_accent()
                                        } else {
                                            c_text_faint()
                                        },
                                    );
                                    if close_response.hovered() || hovered {
                                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                                    }
                                    if close_response.clicked() || response.middle_clicked() {
                                        close = Some(index);
                                    } else if response.clicked() {
                                        select = Some(index);
                                    }
                                    let response =
                                        response.on_hover_text(document.path.display().to_string());
                                    response.context_menu(|ui| {
                                        if ui.button("Save").clicked() {
                                            select = Some(index);
                                            save = true;
                                            ui.close();
                                        }
                                        if ui.button("Reveal in Explorer").clicked() {
                                            select = Some(index);
                                            reveal = true;
                                            ui.close();
                                        }
                                        if ui.button("Unsaved changes diff").clicked() {
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
                                    let title = self
                                        .conv
                                        .git
                                        .current_diff_path
                                        .as_deref()
                                        .map(|path| {
                                            path.rsplit_once('/').map_or(path, |(_, file)| file)
                                        })
                                        .unwrap_or("diff")
                                        .to_owned();
                                    let label = format!("Diff: {title}");
                                    let font = FontId::proportional(FS_SMALL);
                                    let label_width = ui.fonts_mut(|fonts| {
                                        fonts
                                            .layout_no_wrap(label.clone(), font.clone(), c_text())
                                            .rect
                                            .width()
                                    });
                                    let tab_width = label_width + 42.0;
                                    let (rect, response) = ui.allocate_exact_size(
                                        egui::vec2(tab_width, 28.0),
                                        egui::Sense::click(),
                                    );
                                    let hovered = ui.rect_contains_pointer(rect);
                                    let fill = if git_diff_active {
                                        c_bg_main()
                                    } else if hovered {
                                        c_row_hover()
                                    } else {
                                        egui::Color32::TRANSPARENT
                                    };
                                    if fill != egui::Color32::TRANSPARENT {
                                        let mut fill_rect = rect;
                                        if git_diff_active || hovered {
                                            fill_rect.max.y += RADIUS_ROW as f32 + 4.0;
                                        }
                                        ui.painter().rect_filled(
                                            fill_rect,
                                            egui::CornerRadius::same(RADIUS_ROW),
                                            fill,
                                        );
                                    }
                                    ui.painter().text(
                                        egui::pos2(rect.left() + 10.0, rect.center().y),
                                        egui::Align2::LEFT_CENTER,
                                        label,
                                        font,
                                        if git_diff_active {
                                            c_text_strong()
                                        } else {
                                            c_text_muted()
                                        },
                                    );
                                    let close_rect = egui::Rect::from_center_size(
                                        egui::pos2(rect.right() - 13.0, rect.center().y),
                                        egui::vec2(22.0, rect.height()),
                                    );
                                    let close_response = ui.interact(
                                        close_rect,
                                        ui.id().with("editor_git_diff_tab_close"),
                                        egui::Sense::click(),
                                    );
                                    ui.painter().text(
                                        close_rect.center(),
                                        egui::Align2::CENTER_CENTER,
                                        ICON_CLOSE,
                                        FontId::new(FS_TINY, icon_font()),
                                        if close_response.hovered() {
                                            c_accent()
                                        } else {
                                            c_text_faint()
                                        },
                                    );
                                    if close_response.hovered() || hovered {
                                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                                    }
                                    if close_response.clicked() || response.middle_clicked() {
                                        close_git_diff = true;
                                    } else if response.clicked() {
                                        select_git_diff = true;
                                    }
                                    if let Some(path) = self.conv.git.current_diff_path.as_deref() {
                                        response.on_hover_text(path);
                                    }
                                }
                            });
                        });

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.menu_button(
                            RichText::new("▾").size(FS_SMALL).color(c_text_muted()),
                            |ui| {
                                ui.set_min_width(180.0);
                                for (index, document) in
                                    self.conv.editor.documents.iter().enumerate()
                                {
                                    let name = document
                                        .path
                                        .file_name()
                                        .unwrap_or_default()
                                        .to_string_lossy();
                                    if ui
                                        .selectable_label(
                                            !git_diff_active
                                                && self.conv.editor.active == Some(index),
                                            name,
                                        )
                                        .clicked()
                                    {
                                        select = Some(index);
                                        ui.close();
                                    }
                                }
                            },
                        )
                        .response
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
            self.conv.editor.active = Some(index);
            self.conv.editor.diff_tab_active = false;
            self.conv.editor.focus_editor_next_frame = true;
            if let Some(path) = self
                .conv
                .editor
                .documents
                .get(index)
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
            if self.conv.editor.documents[index].is_dirty() {
                self.conv.editor.error = Some("Save the file before closing its tab.".into());
            } else {
                self.conv.editor.documents.remove(index);
                self.conv.editor.active = if self.conv.editor.documents.is_empty() {
                    None
                } else {
                    Some(index.min(self.conv.editor.documents.len() - 1))
                };
                if self.conv.editor.active.is_some() {
                    self.conv.editor.focus_editor_next_frame = true;
                }
            }
        }
    }
}
