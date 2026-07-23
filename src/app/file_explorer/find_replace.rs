//! Find and replace panel behavior and rendering.

use eframe::egui::{self, Align, Frame, Layout, Margin, RichText, TextEdit, Ui};

use crate::theme::*;
use crate::ui::chrome::icon_glyph_rich;

use super::super::OxiApp;
use super::{EditorLayoutCache, find_match_ranges};

impl OxiApp {
    pub(super) fn render_find_replace(&mut self, ui: &mut Ui) {
        let query_changed = self.conv.editor.find_query != self.conv.editor.find_last_query
            || self.conv.editor.find_case_sensitive != self.conv.editor.find_last_case_sensitive;
        if query_changed {
            self.conv
                .editor
                .find_last_query
                .clone_from(&self.conv.editor.find_query);
            self.conv.editor.find_last_case_sensitive = self.conv.editor.find_case_sensitive;
            self.conv.editor.find_active_match = 0;
            // Typing previews and reveals the first result, but deliberately does not update
            // the editor TextEdit's cursor state: doing so can steal focus from the Find input.
            self.conv.editor.find_has_navigated = !self.conv.editor.find_query.is_empty();
            self.conv.editor.find_select_pending = false;
            self.conv.editor.find_reveal_pending = !self.conv.editor.find_query.is_empty();
            self.conv.editor.find_focus_editor_pending = false;
        }
        let ranges = self
            .conv
            .editor
            .active_document()
            .map(|document| {
                find_match_ranges(
                    &document.content,
                    &self.conv.editor.find_query,
                    self.conv.editor.find_case_sensitive,
                )
            })
            .unwrap_or_default();
        if ranges.is_empty() {
            self.conv.editor.find_active_match = 0;
        } else {
            self.conv.editor.find_active_match %= ranges.len();
        }

        let mut previous = false;
        let mut next = false;
        let mut replace_one = false;
        let mut replace_all = false;
        let mut close = false;
        Frame::new()
            .fill(c_bg_elevated_2())
            .stroke(egui::Stroke::new(1.0, c_border_subtle()))
            .inner_margin(Margin {
                left: 12,
                right: 12,
                top: 2,
                bottom: 2,
            })
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing = egui::vec2(8.0, 7.0);
                const TOGGLE_WIDTH: f32 = 56.0;
                const LABEL_WIDTH: f32 = 64.0;
                const ACTION_WIDTH: f32 = 92.0;
                const CLOSE_WIDTH: f32 = 28.0;
                const ROW_HEIGHT: f32 = 28.0;
                const FIELD_HEIGHT: f32 = 24.0;
                let input_width = (ui.available_width()
                    - TOGGLE_WIDTH
                    - 32.0
                    - LABEL_WIDTH
                    - ACTION_WIDTH * 2.0
                    - CLOSE_WIDTH
                    - 7.0 * 8.0)
                    .max(80.0);

                ui.horizontal(|ui| {
                    let replace_toggle = ui
                        .add_sized(
                            [TOGGLE_WIDTH, 28.0],
                            egui::Button::selectable(self.conv.editor.find_replace_open, "AB"),
                        )
                        .on_hover_text("Show replace field");
                    if replace_toggle.clicked() {
                        self.conv.editor.find_replace_open = !self.conv.editor.find_replace_open;
                    }
                    let case_toggle = ui
                        .add_sized(
                            [32.0, ROW_HEIGHT],
                            egui::Button::selectable(self.conv.editor.find_case_sensitive, "Aa"),
                        )
                        .on_hover_text(if self.conv.editor.find_case_sensitive {
                            "Case sensitive"
                        } else {
                            "Case insensitive"
                        });
                    if case_toggle.clicked() {
                        self.conv.editor.find_case_sensitive =
                            !self.conv.editor.find_case_sensitive;
                    }
                    ui.allocate_ui_with_layout(
                        egui::vec2(LABEL_WIDTH, ROW_HEIGHT),
                        Layout::right_to_left(Align::Center),
                        |ui| {
                            ui.label(RichText::new("Find:").size(FS_SMALL).color(c_text_muted()));
                        },
                    );
                    let find_response = ui
                        .allocate_ui_with_layout(
                            egui::vec2(input_width, ROW_HEIGHT),
                            Layout::left_to_right(Align::Center),
                            |ui| {
                                ui.add_sized(
                                    [input_width, FIELD_HEIGHT],
                                    TextEdit::singleline(&mut self.conv.editor.find_query)
                                        .id_salt("workspace_editor_find")
                                        .margin(Margin::symmetric(6, 2))
                                        .hint_text("Find"),
                                )
                            },
                        )
                        .inner;
                    if query_changed || std::mem::take(&mut self.conv.editor.focus_find_next_frame)
                    {
                        find_response.request_focus();
                    }
                    next = ui
                        .add_sized([ACTION_WIDTH, 28.0], egui::Button::new("Find"))
                        .clicked();
                    previous = ui
                        .add_sized([ACTION_WIDTH, 28.0], egui::Button::new("Find Prev"))
                        .clicked();
                    close = ui
                        .add_sized(
                            [CLOSE_WIDTH, 28.0],
                            egui::Button::new(icon_glyph_rich(ICON_CLOSE, FS_TINY, c_text_muted()))
                                .frame(false),
                        )
                        .on_hover_text("Close find")
                        .clicked();

                    // A single-line TextEdit gives up focus when Enter is pressed. Checking
                    // `lost_focus` is therefore essential; `has_focus` alone misses the exact
                    // frame carrying Enter and neither navigation nor focus restoration runs.
                    let enter_while_editing = (find_response.has_focus()
                        || find_response.lost_focus())
                        && ui.input(|input| input.key_pressed(egui::Key::Enter));
                    if enter_while_editing {
                        if ui.input(|input| input.modifiers.shift) {
                            previous = true;
                        } else {
                            next = true;
                        }
                    }
                });

                if self.conv.editor.find_replace_open {
                    ui.horizontal(|ui| {
                        ui.allocate_exact_size(
                            egui::vec2(TOGGLE_WIDTH + 32.0 + 8.0, ROW_HEIGHT),
                            egui::Sense::hover(),
                        );
                        ui.allocate_ui_with_layout(
                            egui::vec2(LABEL_WIDTH, ROW_HEIGHT),
                            Layout::right_to_left(Align::Center),
                            |ui| {
                                ui.label(
                                    RichText::new("Replace:")
                                        .size(FS_SMALL)
                                        .color(c_text_muted()),
                                );
                            },
                        );
                        ui.allocate_ui_with_layout(
                            egui::vec2(input_width, ROW_HEIGHT),
                            Layout::left_to_right(Align::Center),
                            |ui| {
                                ui.add_sized(
                                    [input_width, FIELD_HEIGHT],
                                    TextEdit::singleline(&mut self.conv.editor.replace_query)
                                        .margin(Margin::symmetric(6, 2))
                                        .hint_text("Replace"),
                                );
                            },
                        );
                        replace_one = ui
                            .add_sized([ACTION_WIDTH, 28.0], egui::Button::new("Replace"))
                            .clicked();
                        replace_all = ui
                            .add_sized([ACTION_WIDTH, 28.0], egui::Button::new("Replace All"))
                            .clicked();
                    });
                }
            });

        if next || previous {
            // A single-line TextEdit releases focus on Enter, including when there are no matches.
            // Always return focus to Find after a navigation attempt so repeated searches and query
            // edits continue to work without requiring another click.
            self.conv.editor.focus_find_next_frame = true;
        }
        if !ranges.is_empty() && (next || previous) {
            self.conv.editor.find_active_match = if previous {
                if self.conv.editor.find_has_navigated {
                    self.conv
                        .editor
                        .find_active_match
                        .checked_sub(1)
                        .unwrap_or(ranges.len() - 1)
                } else {
                    ranges.len() - 1
                }
            } else if self.conv.editor.find_has_navigated {
                (self.conv.editor.find_active_match + 1) % ranges.len()
            } else {
                0
            };
            self.conv.editor.find_has_navigated = true;
            // While Find is open, navigation changes the active highlight and scroll only.
            // The editor caret is committed when Find closes, avoiding a competing TextEdit
            // cursor update that could take keyboard focus from the query field.
            self.conv.editor.find_select_pending = false;
            self.conv.editor.find_reveal_pending = true;
            self.conv.editor.find_focus_editor_pending = false;
        }
        if (replace_one || replace_all) && !ranges.is_empty() {
            let replacement = self.conv.editor.replace_query.clone();
            let active = self.conv.editor.find_active_match;
            if let Some(document) = self.conv.editor.active_document_mut() {
                if replace_all {
                    for range in ranges.iter().rev() {
                        document.content.replace_range(range.clone(), &replacement);
                    }
                } else {
                    document
                        .content
                        .replace_range(ranges[active].clone(), &replacement);
                }
                document.content_revision = document.content_revision.wrapping_add(1);
                document.dirty = document.content != document.saved_content;
                document.layout_cache = EditorLayoutCache::default();
                document.minimap_cache = None;
            }
            if replace_all {
                self.conv.editor.find_active_match = 0;
            }
            self.conv.editor.find_select_pending = false;
            self.conv.editor.find_reveal_pending = true;
        }
        if close {
            self.conv.editor.find_open = false;
            // Apply the current result on the following editor frame, then return focus.
            self.conv.editor.find_select_pending = self.conv.editor.find_has_navigated;
            self.conv.editor.find_focus_editor_pending = true;
        }
    }
}
