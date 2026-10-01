//! Sublime-style find and replace panel for the text editor: option toggles on the left, the
//! query field with a live match count in the middle, actions on the right.

use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui::{self, Align2, FontId, Margin, Rect, Response, Sense, TextEdit, Ui};

use crate::app::state::EditorState;
use crate::theme::*;

use super::super::OxiApp;
use super::editor_logic::char_index_to_byte;
use super::{EditorLayoutCache, FindOptions, FindResults, find_matches};

pub(crate) const FIND_FIELD_ID: &str = "workspace_editor_find";
const REPLACE_FIELD_ID: &str = "workspace_editor_replace";

const ROW_HEIGHT: f32 = 26.0;
const ROW_GAP: f32 = 6.0;
const PANEL_MARGIN: f32 = 7.0;
const TOGGLE_SIZE: f32 = 26.0;
const ACTION_WIDTH: f32 = 88.0;
const GAP: f32 = 6.0;

/// Matches of the Find query in one document revision.
pub(crate) struct FindCache {
    path: PathBuf,
    revision: u64,
    content_len: usize,
    query: String,
    options: FindOptions,
    results: Arc<FindResults>,
}

impl EditorState {
    /// Matches of the Find query in the active document. Cached per document revision, so the
    /// panel and the editor share one search per edit instead of rescanning every frame.
    pub(crate) fn find_results(&mut self) -> Arc<FindResults> {
        let Some(document) = self.active.and_then(|index| self.documents.get(index)) else {
            return Arc::default();
        };
        if let Some(cache) = &self.find_cache
            && cache.revision == document.content_revision
            && cache.content_len == document.content.len()
            && cache.path == document.path
            && cache.options == self.find_options
            && cache.query == self.find_query
        {
            return Arc::clone(&cache.results);
        }
        let results = Arc::new(find_matches(
            &document.content,
            &self.find_query,
            self.find_options,
        ));
        self.find_cache = Some(FindCache {
            path: document.path.clone(),
            revision: document.content_revision,
            content_len: document.content.len(),
            query: self.find_query.clone(),
            options: self.find_options,
            results: Arc::clone(&results),
        });
        results
    }

    /// Byte range of the editor selection as of the last editor frame.
    fn selection_bytes(&self) -> Option<(usize, usize)> {
        let document = self.active_document()?;
        let (start, end) = self.editor_selection_chars?;
        let start_byte = char_index_to_byte(&document.content, start);
        let end_byte =
            start_byte + char_index_to_byte(&document.content[start_byte..], end - start);
        Some((start_byte, end_byte))
    }

    /// Open the panel (`replace` also shows the Replace row). A short single-line selection
    /// becomes the query, and the whole query is selected so typing replaces it.
    pub(crate) fn open_find(&mut self, replace: bool) {
        let selection = self.selection_bytes();
        if let (Some((start, end)), Some(document)) = (selection, self.active_document())
            && start < end
            && end - start <= 200
        {
            let selected = &document.content[start..end];
            if !selected.contains('\n') {
                self.find_query = selected.to_owned();
            }
        }
        self.find_origin_byte = selection.map_or(0, |(start, _)| start);
        self.find_open = true;
        self.find_replace_open = replace;
        // Re-run the incremental search from the new origin even when the query is unchanged.
        self.find_last_query.clear();
        self.find_select_query_next_frame = true;
        self.focus_find_next_frame = true;
        self.find_select_pending = false;
        self.find_focus_editor_pending = false;
    }

    /// Move to the next/previous match, wrapping around the document. While the panel is open
    /// this steps through the results; otherwise (F3 / Cmd+G) it continues from the editor
    /// selection and selects the match in the editor. Returns whether anything matched.
    pub(crate) fn find_step(&mut self, forward: bool) -> bool {
        let results = self.find_results();
        let count = results.ranges.len();
        if count == 0 {
            return false;
        }
        let active = self.find_active_match.min(count - 1);
        self.find_active_match = if self.find_open && self.find_has_navigated {
            if forward {
                (active + 1) % count
            } else {
                active.checked_sub(1).unwrap_or(count - 1)
            }
        } else {
            let (start, end) = if self.find_open {
                (self.find_origin_byte, self.find_origin_byte)
            } else {
                self.selection_bytes().unwrap_or((0, 0))
            };
            if forward {
                results
                    .ranges
                    .iter()
                    .position(|range| range.start >= end)
                    .unwrap_or(0)
            } else {
                results
                    .ranges
                    .iter()
                    .rposition(|range| range.end <= start)
                    .unwrap_or(count - 1)
            }
        };
        self.find_has_navigated = true;
        self.find_reveal_pending = true;
        if !self.find_open {
            self.find_select_pending = true;
            self.find_focus_editor_pending = true;
        }
        true
    }

    /// Close the panel, selecting the current match in the editor and returning focus to it.
    pub(crate) fn close_find(&mut self) {
        self.find_open = false;
        self.find_select_pending = self.find_has_navigated;
        self.find_focus_editor_pending = true;
    }

    /// Replace the current match and move to the following one. Before any navigation this only
    /// finds the first match, so the user sees what will be replaced (Sublime behaviour).
    fn replace_current(&mut self) {
        let results = self.find_results();
        if results.ranges.is_empty() {
            return;
        }
        if !self.find_has_navigated {
            self.find_step(true);
            return;
        }
        let range = results.ranges[self.find_active_match.min(results.ranges.len() - 1)].clone();
        let replacement = self.replace_query.clone();
        let Some(document) = self.active_document_mut() else {
            return;
        };
        let text = results.replacement_for(&document.content, &range, &replacement);
        document.content.replace_range(range.clone(), &text);
        mark_edited(document);
        // Continue after the inserted text, so a replacement containing the query is not
        // matched again.
        let resume = range.start + text.len();
        let results = self.find_results();
        self.find_active_match = results
            .ranges
            .iter()
            .position(|next| next.start >= resume)
            .unwrap_or(0);
        self.find_has_navigated = !results.ranges.is_empty();
        self.find_reveal_pending = self.find_has_navigated;
    }

    /// Replace every match in one pass.
    fn replace_all(&mut self) {
        let results = self.find_results();
        if results.ranges.is_empty() {
            return;
        }
        let replacement = self.replace_query.clone();
        let Some(document) = self.active_document_mut() else {
            return;
        };
        let mut replaced = String::with_capacity(document.content.len());
        let mut copied = 0;
        for range in &results.ranges {
            replaced.push_str(&document.content[copied..range.start]);
            replaced.push_str(&results.replacement_for(&document.content, range, &replacement));
            copied = range.end;
        }
        replaced.push_str(&document.content[copied..]);
        document.content = replaced;
        mark_edited(document);
        self.find_active_match = 0;
        self.find_has_navigated = false;
    }
}

fn mark_edited(document: &mut crate::app::state::EditorDocument) {
    document.content_revision = document.content_revision.wrapping_add(1);
    document.dirty = document.content != document.saved_content;
    document.layout_cache = EditorLayoutCache::default();
    document.minimap_cache = None;
}

impl OxiApp {
    /// Height of the floating panel for its current mode.
    pub(super) fn find_panel_height(&self) -> f32 {
        let rows = if self.conv.editor.find_replace_open {
            2.0
        } else {
            1.0
        };
        PANEL_MARGIN * 2.0 + rows * ROW_HEIGHT + (rows - 1.0) * ROW_GAP + 1.0
    }

    pub(super) fn render_find_replace(&mut self, ui: &mut Ui) {
        let editor = &mut self.conv.editor;
        let query_changed = editor.find_query != editor.find_last_query
            || editor.find_options != editor.find_last_options;
        if query_changed {
            editor.find_last_query.clone_from(&editor.find_query);
            editor.find_last_options = editor.find_options;
            // Incremental search: preview the first match after the origin while typing. The
            // editor caret is left alone so the Find field keeps keyboard focus.
            let results = editor.find_results();
            let origin = editor.find_origin_byte;
            editor.find_active_match = results
                .ranges
                .iter()
                .position(|range| range.start >= origin)
                .unwrap_or(0);
            editor.find_has_navigated = !results.ranges.is_empty();
            editor.find_reveal_pending = editor.find_has_navigated;
            editor.find_select_pending = false;
            editor.find_focus_editor_pending = false;
        }
        let results = editor.find_results();
        let count = results.ranges.len();
        if count > 0 {
            editor.find_active_match = editor.find_active_match.min(count - 1);
        }
        let status = if editor.find_query.is_empty() {
            None
        } else if let Some(error) = &results.error {
            Some((error.clone(), true))
        } else if count == 0 {
            Some(("No results".to_owned(), true))
        } else if editor.find_has_navigated {
            Some((
                format!("{} of {count}", editor.find_active_match + 1),
                false,
            ))
        } else {
            Some((format!("{count} found"), false))
        };

        let mut replace_one = false;
        let mut replace_all = false;
        let replace_all_modifiers = if cfg!(target_os = "macos") {
            egui::Modifiers::MAC_CMD.plus(egui::Modifiers::ALT)
        } else {
            egui::Modifiers::CTRL.plus(egui::Modifiers::ALT)
        };
        let find_id = egui::Id::new(FIND_FIELD_ID);
        let replace_id = egui::Id::new(REPLACE_FIELD_ID);
        if std::mem::take(&mut editor.find_select_query_next_frame) {
            select_all_text(ui.ctx(), find_id, &editor.find_query);
        }

        let panel_rect = ui.max_rect();
        ui.painter().rect_filled(panel_rect, 0.0, c_bg_elevated_2());
        ui.painter().hline(
            panel_rect.x_range(),
            panel_rect.top() + 0.5,
            egui::Stroke::new(1.0, c_border_subtle()),
        );
        let inner = panel_rect.shrink2(egui::vec2(10.0, PANEL_MARGIN));
        // The replace switch is set slightly apart from the three search options.
        let toggles_width = TOGGLE_SIZE * 4.0 + 3.0 * 2.0 + 4.0;
        let field_left = inner.left() + toggles_width + GAP * 2.0;
        let field_right = inner.right() - TOGGLE_SIZE - GAP - (ACTION_WIDTH + GAP) * 2.0;
        let field_width = (field_right - field_left).max(80.0);
        let row_rect = |row: f32| {
            Rect::from_min_size(
                egui::pos2(inner.left(), inner.top() + row * (ROW_HEIGHT + ROW_GAP)),
                egui::vec2(inner.width(), ROW_HEIGHT),
            )
        };
        let find_row = row_rect(0.0);

        // Option toggles: [replace] [.*] [Aa] [ab], Sublime's order with the replace switch first.
        let toggle_rect = |slot: usize| {
            let x = find_row.left()
                + slot as f32 * (TOGGLE_SIZE + 2.0)
                + if slot > 0 { 4.0 } else { 0.0 };
            Rect::from_min_size(
                egui::pos2(x, find_row.center().y - TOGGLE_SIZE / 2.0),
                egui::vec2(TOGGLE_SIZE, TOGGLE_SIZE),
            )
        };
        let (toggle_modifier, modifier_label) = if cfg!(target_os = "macos") {
            (egui::Modifiers::MAC_CMD.plus(egui::Modifiers::ALT), "⌥⌘")
        } else {
            (egui::Modifiers::ALT, "Alt+")
        };
        let replace_icon = if editor.find_replace_open {
            ICON_ANGLE_DOWN
        } else {
            ICON_CHEVRON_RIGHT
        };
        let replace_toggle = toggle_button(
            ui,
            toggle_rect(0),
            "find_toggle_replace",
            false,
            ToggleLabel::Icon(replace_icon),
        )
        .on_hover_text(if editor.find_replace_open {
            "Hide Replace"
        } else {
            "Show Replace"
        });
        if replace_toggle.clicked() {
            editor.find_replace_open = !editor.find_replace_open;
            if editor.find_replace_open {
                ui.memory_mut(|memory| memory.request_focus(replace_id));
            }
        }
        let toggles = [
            (
                "find_toggle_regex",
                ToggleLabel::Text(".*"),
                "Regular expression",
                egui::Key::R,
            ),
            (
                "find_toggle_case",
                ToggleLabel::Text("Aa"),
                "Case sensitive",
                egui::Key::C,
            ),
            (
                "find_toggle_word",
                ToggleLabel::Word,
                "Whole word",
                egui::Key::W,
            ),
        ];
        for (index, (id, label, tip, key)) in toggles.into_iter().enumerate() {
            let flag = match index {
                0 => &mut editor.find_options.regex,
                1 => &mut editor.find_options.case_sensitive,
                _ => &mut editor.find_options.whole_word,
            };
            let clicked = toggle_button(ui, toggle_rect(index + 1), id, *flag, label)
                .on_hover_text(format!("{tip}  ({modifier_label}{key:?})"))
                .clicked();
            let shortcut = ui.input_mut(|input| input.consume_key(toggle_modifier, key));
            if clicked || shortcut {
                *flag = !*flag;
            }
        }

        // Query field with the match count drawn inside its right edge.
        let field_rect = Rect::from_min_size(
            egui::pos2(field_left, find_row.top()),
            egui::vec2(field_width, ROW_HEIGHT),
        );
        let status_width = status.as_ref().map_or(0.0, |(text, _)| {
            ui.fonts_mut(|fonts| {
                fonts
                    .layout_no_wrap(text.clone(), FontId::proportional(FS_SMALL), c_text_muted())
                    .size()
                    .x
            }) + 12.0
        });
        let find_response = query_field(
            ui,
            field_rect,
            find_id,
            &mut editor.find_query,
            "Find",
            status_width,
            status.as_ref().is_some_and(|(_, error)| *error),
        );
        if let Some((text, error)) = &status {
            ui.painter().text(
                egui::pos2(field_rect.right() - 8.0, field_rect.center().y),
                Align2::RIGHT_CENTER,
                text,
                FontId::proportional(FS_SMALL),
                if *error { c_error_fg() } else { c_text_faint() },
            );
        }
        if std::mem::take(&mut editor.focus_find_next_frame) {
            find_response.request_focus();
        }
        let mut action_x = field_rect.right() + GAP;
        let mut next_action = |ui: &mut Ui, label: &str, enabled: bool| {
            let rect = Rect::from_min_size(
                egui::pos2(action_x, find_row.top()),
                egui::vec2(ACTION_WIDTH, ROW_HEIGHT),
            );
            action_x += ACTION_WIDTH + GAP;
            ui.put(rect, action_button(label, rect)).clicked() && enabled
        };
        let mut next = next_action(ui, "Find", count > 0);
        let mut previous = next_action(ui, "Find Prev", count > 0);
        let close_rect = Rect::from_min_size(
            egui::pos2(action_x, find_row.center().y - TOGGLE_SIZE / 2.0),
            egui::vec2(TOGGLE_SIZE, TOGGLE_SIZE),
        );
        let close = toggle_button(
            ui,
            close_rect,
            "find_close",
            false,
            ToggleLabel::Icon(ICON_CLOSE),
        )
        .on_hover_text("Close  (Esc)")
        .clicked();

        // A single-line TextEdit gives up focus when Enter is pressed, so `lost_focus` must be
        // checked as well; `has_focus` alone misses the frame carrying Enter.
        let enter_pressed = ui.input(|input| input.key_pressed(egui::Key::Enter));
        let enter_in =
            |response: &Response| (response.has_focus() || response.lost_focus()) && enter_pressed;
        if enter_in(&find_response) {
            if ui.input(|input| input.modifiers.shift) {
                previous = true;
            } else {
                next = true;
            }
            editor.focus_find_next_frame = true;
        }
        let tab_pressed = ui.input(|input| input.key_pressed(egui::Key::Tab));

        if editor.find_replace_open {
            let replace_row = row_rect(1.0);
            let field_rect = Rect::from_min_size(
                egui::pos2(field_left, replace_row.top()),
                egui::vec2(field_width, ROW_HEIGHT),
            );
            let replace_response = query_field(
                ui,
                field_rect,
                replace_id,
                &mut editor.replace_query,
                "Replace",
                0.0,
                false,
            );
            let mut action_x = field_rect.right() + GAP;
            for (label, action) in [
                ("Replace", &mut replace_one),
                ("Replace All", &mut replace_all),
            ] {
                let rect = Rect::from_min_size(
                    egui::pos2(action_x, replace_row.top()),
                    egui::vec2(ACTION_WIDTH, ROW_HEIGHT),
                );
                action_x += ACTION_WIDTH + GAP;
                let tip = if label == "Replace" {
                    "Replace the current match and find the next  (Enter)"
                } else if cfg!(target_os = "macos") {
                    "Replace every match  (⌥⌘Enter)"
                } else {
                    "Replace every match  (Ctrl+Alt+Enter)"
                };
                *action = ui
                    .put(rect, action_button(label, rect))
                    .on_hover_text(tip)
                    .clicked();
            }
            if enter_in(&replace_response) {
                if ui.input(|input| input.modifiers.matches_exact(replace_all_modifiers)) {
                    replace_all = true;
                } else {
                    replace_one = true;
                }
                replace_response.request_focus();
            }
            // Tab / Shift+Tab hop between the two fields instead of walking every button.
            if tab_pressed && find_response.lost_focus() {
                replace_response.request_focus();
            } else if tab_pressed && replace_response.lost_focus() {
                find_response.request_focus();
            }
        } else if tab_pressed && find_response.lost_focus() {
            find_response.request_focus();
        }

        if next || previous {
            editor.find_step(next);
        }
        if replace_all {
            editor.replace_all();
        } else if replace_one {
            editor.replace_current();
        }
        if close {
            editor.close_find();
        }
    }
}

enum ToggleLabel {
    Text(&'static str),
    Icon(&'static str),
    /// "ab" with a bracket underneath, the usual whole-word glyph.
    Word,
}

/// Small square toggle: accent-tinted when on, a hover fill otherwise.
fn toggle_button(ui: &mut Ui, rect: Rect, id: &str, on: bool, label: ToggleLabel) -> Response {
    let response = ui.interact(rect, ui.id().with(id), Sense::click());
    let painter = ui.painter();
    if on {
        painter.rect(
            rect,
            4.0,
            c_pill_selected_bg(),
            egui::Stroke::new(1.0, c_pill_selected_border()),
            egui::StrokeKind::Inside,
        );
    } else if response.hovered() {
        painter.rect_filled(rect, 4.0, c_row_hover());
    }
    let color = if on || response.hovered() {
        c_text()
    } else {
        c_text_muted()
    };
    let center = rect.center();
    match label {
        ToggleLabel::Text(text) => {
            painter.text(
                center,
                Align2::CENTER_CENTER,
                text,
                FontId::monospace(FS_SMALL),
                color,
            );
        }
        ToggleLabel::Icon(icon) => {
            painter.text(
                center,
                Align2::CENTER_CENTER,
                icon,
                FontId::new(FS_TINY, icon_font()),
                color,
            );
        }
        ToggleLabel::Word => {
            let text_center = center - egui::vec2(0.0, 3.0);
            painter.text(
                text_center,
                Align2::CENTER_CENTER,
                "ab",
                FontId::monospace(FS_SMALL),
                color,
            );
            let y = center.y + 7.0;
            let stroke = egui::Stroke::new(1.0, color);
            painter.line_segment(
                [egui::pos2(center.x - 7.0, y), egui::pos2(center.x + 7.0, y)],
                stroke,
            );
            for x in [center.x - 7.0, center.x + 7.0] {
                painter.line_segment([egui::pos2(x, y - 2.5), egui::pos2(x, y)], stroke);
            }
        }
    }
    response
}

/// Single-line field at `rect`; `reserve_right` keeps typed text clear of the match count.
fn query_field(
    ui: &mut Ui,
    rect: Rect,
    id: egui::Id,
    text: &mut String,
    hint: &str,
    reserve_right: f32,
    error: bool,
) -> Response {
    let response = ui.put(
        rect,
        TextEdit::singleline(text)
            .id(id)
            .font(FontId::monospace(FS_SMALL))
            .vertical_align(egui::Align::Center)
            .margin(Margin {
                left: 8,
                right: (8.0 + reserve_right) as i8,
                top: 2,
                bottom: 2,
            })
            .hint_text(hint)
            .min_size(rect.size()),
    );
    if error {
        ui.painter().rect_stroke(
            rect,
            ui.visuals().widgets.inactive.corner_radius,
            egui::Stroke::new(1.0, c_error_stroke()),
            egui::StrokeKind::Inside,
        );
    }
    response
}

fn select_all_text(ctx: &egui::Context, id: egui::Id, text: &str) {
    let mut state = egui::text_edit::TextEditState::load(ctx, id).unwrap_or_default();
    state
        .cursor
        .set_char_range(Some(egui::text::CCursorRange::two(
            egui::text::CCursor::new(0),
            egui::text::CCursor::new(text.chars().count()),
        )));
    state.store(ctx, id);
}

/// Fixed-size action button; the label never wraps, even when the window is narrow.
fn action_button(label: &str, rect: Rect) -> egui::Button<'_> {
    egui::Button::new(egui::RichText::new(label).size(FS_SMALL))
        .wrap_mode(egui::TextWrapMode::Truncate)
        .min_size(rect.size())
}
