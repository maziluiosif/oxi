//! Main text editor viewport, layout caching, gutter, and in-memory Git markers.

use std::{path::PathBuf, sync::Arc};

use eframe::egui::scroll_area::ScrollBarVisibility;
use eframe::egui::{self, FontId, Margin, ScrollArea, TextEdit, Ui};

use crate::theme::*;

use super::super::OxiApp;
use super::editor_logic::{char_index_to_byte, live_git_line_changes};
use super::editor_paint::{
    byte_range_rects, caret_logical_line, editor_selection_rects, paint_bracket_underlines,
    paint_caret, paint_indent_guides, paint_selected_whitespace, paint_selection,
    paint_selection_matches, selected_logical_lines,
};
use super::editor_text::EditorText;
use super::support::{apply_definition_underline, apply_search_highlights, language_for_path};
use super::{EditorLayoutCache, line_layout, minimap, syntax_window};

pub(super) type EditorScrollOutput = egui::scroll_area::ScrollAreaOutput<(
    Vec<(usize, f32)>,
    bool,
    Option<(usize, usize)>,
    Option<usize>,
    Option<usize>,
    Option<usize>,
    (usize, Option<f32>),
    bool,
    f32,
    egui::Id,
)>;

impl OxiApp {
    pub(super) fn render_editor_body(&mut self, ui: &mut Ui) {
        let Some(index) = self.conv.editor.active else {
            return;
        };
        let extension = language_for_path(&self.conv.editor.documents[index].path).to_owned();
        let navigation_supported = crate::code_nav::supports(&extension);
        if navigation_supported {
            self.prewarm_code_navigation(&extension);
        }
        let navigation_range = self
            .conv
            .editor
            .navigation_target
            .as_ref()
            .filter(|(path, _)| path == &self.conv.editor.documents[index].path)
            .map(|(_, range)| range.clone());
        if navigation_range.is_some() {
            self.conv.editor.navigation_target = None;
        }
        let goto_definition_requested =
            std::mem::take(&mut self.conv.editor.goto_definition_requested);
        // Keep match geometry for one extra frame while Find closes so Escape/X can
        // apply the current match caret before the panel disappears.
        let find_results = if self.conv.editor.find_open
            || self.conv.editor.find_select_pending
            || self.conv.editor.find_focus_editor_pending
        {
            self.conv.editor.find_results()
        } else {
            Arc::default()
        };
        let find_ranges: &[std::ops::Range<usize>] = &find_results.ranges;
        let active_find_match = (!find_ranges.is_empty()).then(|| {
            self.conv
                .editor
                .find_active_match
                .min(find_ranges.len() - 1)
        });
        let select_find_match = self.conv.editor.find_select_pending && active_find_match.is_some();
        let reveal_find_match = (select_find_match || self.conv.editor.find_reveal_pending)
            && active_find_match.is_some();
        let focus_editor_for_find = std::mem::take(&mut self.conv.editor.find_focus_editor_pending);
        let focus_editor_requested =
            focus_editor_for_find || std::mem::take(&mut self.conv.editor.focus_editor_next_frame);
        let clear_selection_requested =
            std::mem::take(&mut self.conv.editor.clear_editor_selection_next_frame);
        self.conv.editor.find_select_pending = false;
        self.conv.editor.find_reveal_pending = false;
        let logical_line_count = self.conv.editor.documents[index]
            .minimap_cache
            .as_ref()
            .map_or_else(
                || {
                    self.conv.editor.documents[index]
                        .content
                        .bytes()
                        .filter(|byte| *byte == b'\n')
                        .count()
                        + 1
                },
                |geometry| geometry.line_count,
            );
        let gutter_digits = logical_line_count.to_string().len().max(2) as f32;
        let digit_width = ui.fonts_mut(|fonts| {
            fonts
                .glyph_width(&FontId::monospace(FS_SMALL), '0')
                .max(FS_SMALL * 0.5)
        });
        const GIT_MARKER_WIDTH: f32 = 2.0;
        // Keep the line numbers at their original position while placing the slimmer Git
        // marker flush against the editor container's left boundary.
        const GUTTER_LEFT_PADDING: f32 = 10.0;
        const GUTTER_RIGHT_PADDING: f32 = 12.0;
        let gutter_width = gutter_digits * digit_width
            + GUTTER_LEFT_PADDING
            + GUTTER_RIGHT_PADDING
            + GIT_MARKER_WIDTH;
        let root = PathBuf::from(&self.active_workspace().root_path);
        let relative_path = self.conv.editor.documents[index]
            .path
            .strip_prefix(&root)
            .unwrap_or(&self.conv.editor.documents[index].path)
            .to_string_lossy()
            .replace('\\', "/");
        let full_git_highlight = self
            .conv
            .editor
            .git_full_highlight_path
            .as_ref()
            .is_some_and(|path| path == &self.conv.editor.documents[index].path);
        // Diff mode (see `diff_editor`): no soft wrap, rows inserted to align with the base,
        // and an overview ruler instead of the minimap.
        let diff_mode = {
            let document = &mut self.conv.editor.documents[index];
            let revision = document.content_revision;
            document
                .diff
                .as_mut()
                .filter(|diff| diff.source.editable() && diff.ready())
                .map(|diff| {
                    let decor = diff.decor_at(revision, &document.content);
                    (
                        diff.inline,
                        diff.layout_key(),
                        diff.scroll_request.take(),
                        decor,
                    )
                })
        };
        let diff_key = diff_mode.as_ref().map_or(0, |(_, key, _, _)| *key);
        let diff_inline = diff_mode.as_ref().is_some_and(|(inline, ..)| *inline);
        let diff_scroll_request = diff_mode.as_ref().and_then(|(_, _, request, _)| *request);
        let diff_decor = diff_mode.and_then(|(_, _, _, decor)| decor);
        let disk_git_line_changes = self
            .git_gutter_line_changes(&relative_path)
            .cloned()
            .unwrap_or_default();
        // Git itself only sees the saved file. When editing changes the number of lines,
        // project those disk-based markers onto the in-memory buffer so the gutter follows
        // inserted/deleted newlines without doing Git work on every keystroke.
        let git_line_changes = if let Some(decor) = &diff_decor {
            super::diff_editor::line_changes(decor, logical_line_count)
        } else if self.conv.editor.documents[index].is_dirty() {
            // Splits both texts into lines: once per edit (the cache is reset with the text),
            // not on every frame.
            let document = &mut self.conv.editor.documents[index];
            match &document.layout_cache.live_git_lines {
                Some((disk, live)) if *disk == disk_git_line_changes => live.clone(),
                _ => {
                    let live = live_git_line_changes(
                        &disk_git_line_changes,
                        &document.saved_content,
                        &document.content,
                    );
                    document.layout_cache.live_git_lines =
                        Some((disk_git_line_changes, live.clone()));
                    live
                }
            }
        } else {
            disk_git_line_changes
        };
        const MINIMAP_WIDTH: f32 = 96.0;
        let editor_view_size = ui.available_size();
        const MINIMAP_SCROLLBAR_WIDTH: f32 = 10.0;
        let side_strip_width = if diff_key != 0 {
            super::diff_editor::RULER_W
        } else {
            MINIMAP_WIDTH + MINIMAP_SCROLLBAR_WIDTH
        };
        let prospective_editor_width =
            (editor_view_size.x - gutter_width - side_strip_width).max(80.0);
        // Exact, not rounded: at fractional DPI (125%/150% on Windows) a sidebar drag moves the
        // width by sub-pixel steps that still re-wrap rows. Treating those frames as "no resize"
        // re-sampled the anchor from a shifted layout and the file crept away while dragging.
        let prospective_width_bits = prospective_editor_width.to_bits();
        let resize_anchor = self.conv.editor.documents[index]
            .viewport_width_bits
            .filter(|width| *width != prospective_width_bits)
            .map(|_| self.conv.editor.documents[index].viewport_anchor_line);
        self.conv.editor.documents[index].viewport_width_bits = Some(prospective_width_bits);
        let mut goto_definition_byte = None;
        let mut editor_selection = None;
        let mut selection_to_chat = false;
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;

            // The gutter is fixed, like Sublime's: horizontal scrolling only moves source text.
            let (_, gutter_rect) =
                ui.allocate_space(egui::vec2(gutter_width, editor_view_size.y.max(24.0)));
            ui.painter().vline(
                gutter_rect.right(),
                gutter_rect.y_range(),
                egui::Stroke::new(1.0, c_border_subtle()),
            );

            let editor_view_width = prospective_editor_width;
            let scroll_output = ui
                .vertical(|ui| {
                    ui.set_width(editor_view_width);
                    ui.set_height(editor_view_size.y.max(24.0));
                    let mut scroll_area = ScrollArea::both()
                        .id_salt("text_editor_scroll")
                        // A shared scrollbar is painted to the right of the minimap below.
                        .scroll_bar_visibility(ScrollBarVisibility::AlwaysHidden)
                        .auto_shrink([false, false]);
                    if let Some(offset) = diff_scroll_request {
                        scroll_area = scroll_area.vertical_scroll_offset(offset);
                    }
                    scroll_area.show(ui, |ui| {
                        let editor_size =
                            egui::vec2(editor_view_width.max(80.0), editor_view_size.y.max(24.0));
                        let document = &mut self.conv.editor.documents[index];
                        let editor_id = ui.make_persistent_id(("workspace_text_editor", index));
                        if self.conv.editor.text_edit_ids.get(&document.path) != Some(&editor_id) {
                            self.conv
                                .editor
                                .text_edit_ids
                                .insert(document.path.clone(), editor_id);
                        }
                        // Sublime's editing commands (Cmd+/, Cmd+D, Cmd+L, ...) run before
                        // TextEdit so it never sees their keys.
                        let command = super::editor_commands::handle_editor_commands(
                            ui,
                            editor_id,
                            &mut document.content,
                            &extension,
                        );
                        let command_edited = command.is_some_and(|(_, edited)| edited);
                        let mut previous_minimap = None;
                        if command_edited {
                            previous_minimap = mark_document_edited(document);
                        }
                        let revision = document.content_revision;
                        let pixels_per_point_bits = ui.ctx().pixels_per_point().to_bits();
                        let allow_layout_cache = !has_mutating_text_input(ui);
                        let layout_cache = &mut document.layout_cache;
                        let diff_slot = &mut document.diff;
                        // TextEdit moves the caret on any button press, so a right-click
                        // would drop the selection the context menu acts on. Keep it.
                        let selection_before_secondary_press = ui
                            .input(|i| i.pointer.secondary_pressed())
                            .then(|| TextEdit::load_state(ui.ctx(), editor_id))
                            .flatten()
                            .and_then(|state| state.cursor.char_range())
                            .filter(|range| !range.is_empty());
                        let mut layouter =
                            |ui: &Ui, text: &dyn egui::TextBuffer, wrap_width: f32| {
                                // Diff rows must stay aligned with the base's: no wrapping.
                                let wrap_width = if diff_key != 0 {
                                    f32::INFINITY
                                } else {
                                    wrap_width
                                };
                                let wrap_width_bits = wrap_width.round().to_bits();
                                // TextEdit only needs glyph geometry here; cache it before
                                // egui's whole-LayoutJob hashing so selection-only frames are O(1).
                                let geometry_job = |text: &str| {
                                    let mut job = egui::text::LayoutJob::simple(
                                        text.to_owned(),
                                        FontId::monospace(FS_SMALL),
                                        egui::Color32::TRANSPARENT,
                                        wrap_width,
                                    );
                                    job.wrap.max_width = wrap_width;
                                    job
                                };
                                // A keystroke frame first lays out the text as it was before
                                // the input; that is still the cached text, so check it by
                                // content (a memcmp) instead of skipping the cache outright.
                                if layout_cache.revision == revision
                                    && layout_cache.wrap_width_bits == wrap_width_bits
                                    && layout_cache.pixels_per_point_bits == pixels_per_point_bits
                                    && let Some(galley) = &layout_cache.geometry
                                    && (allow_layout_cache
                                        || galley.job.text.as_str() == text.as_str())
                                    && layout_cache.fonts_generation
                                        == crate::theme::fonts_generation()
                                    && layout_cache.diff_key == diff_key
                                {
                                    return Arc::clone(galley);
                                }

                                let pixels_per_point = ui.ctx().pixels_per_point();
                                let galley = ui.fonts_mut(|fonts| {
                                    line_layout::layout(
                                        &mut layout_cache.lines,
                                        geometry_job(text.as_str()),
                                        pixels_per_point,
                                        |job| fonts.layout_job(job),
                                    )
                                });
                                let galley = match diff_slot
                                    .as_mut()
                                    .filter(|_| diff_key != 0)
                                    .and_then(|diff| diff.decor_for(text.as_str()))
                                {
                                    Some(decor) => super::diff_editor::apply_gaps(
                                        &galley,
                                        0,
                                        &decor.gaps(super::diff_editor::Side::New, diff_inline),
                                        diff_row_height(&galley),
                                    ),
                                    None => galley,
                                };
                                if allow_layout_cache {
                                    layout_cache.revision = revision;
                                    layout_cache.wrap_width_bits = wrap_width_bits;
                                    layout_cache.pixels_per_point_bits = pixels_per_point_bits;
                                    layout_cache.fonts_generation =
                                        crate::theme::fonts_generation();
                                    layout_cache.diff_key = diff_key;
                                    layout_cache.geometry = Some(Arc::clone(&galley));
                                    // The cached syntax galley was laid out for the previous
                                    // wrap width / dpi. The keys now describe this new
                                    // geometry, so keeping it would repaint stale rows (text
                                    // visibly truncated after the editor is resized).
                                    layout_cache.syntax = None;
                                }
                                galley
                            };
                        let mut output = ui
                            .scope(|ui| {
                                ui.visuals_mut().extreme_bg_color = egui::Color32::TRANSPARENT;
                                // Paint the complete selection ourselves after TextEdit. Keeping
                                // egui's pass transparent avoids the moving edge being painted once
                                // natively and once from our syntax galley (the last-row flicker).
                                // Note: stock egui still clones the galley for transparent
                                // selection painting — that was the main reason for the fork.
                                ui.visuals_mut().selection.bg_fill = egui::Color32::TRANSPARENT;
                                ui.visuals_mut().selection.stroke = egui::Stroke::NONE;
                                // The native caret is hidden and repainted after syntax text
                                // below, giving it identical pixel width on empty and text rows.
                                ui.visuals_mut().text_cursor.stroke.color =
                                    egui::Color32::TRANSPARENT;
                                ui.visuals_mut().text_cursor.blink = false;
                                TextEdit::multiline(&mut EditorText(&mut document.content))
                                    .id(editor_id)
                                    .font(FontId::monospace(FS_SMALL))
                                    .code_editor()
                                    .frame(egui::Frame::NONE)
                                    .background_color(egui::Color32::TRANSPARENT)
                                    .desired_width(f32::INFINITY)
                                    .min_size(editor_size)
                                    .margin(Margin::same(8))
                                    .layouter(&mut layouter)
                                    .show(ui)
                            })
                            .inner;
                        if let Some(range) = selection_before_secondary_press
                            && output.response.hovered()
                        {
                            output.state.cursor.set_char_range(Some(range));
                            output.state.clone().store(ui.ctx(), output.response.id);
                            output.cursor_range = Some(range);
                        }
                        let scratchpad_changed =
                            (output.response.changed() || command_edited) && document.is_scratchpad;
                        if output.response.changed() {
                            previous_minimap = mark_document_edited(document);
                        }
                        if clear_selection_requested {
                            if let Some(range) = output.cursor_range
                                && !range.is_empty()
                            {
                                let caret = egui::text::CCursorRange::one(range.primary);
                                output.state.cursor.set_char_range(Some(caret));
                                output.state.clone().store(ui.ctx(), output.response.id);
                                output.cursor_range = Some(caret);
                            }
                            output.response.request_focus();
                        }
                        // TextEdit's large minimum height makes its hit area extend below the
                        // document. Explicitly map a click in that blank tail to EOF; otherwise
                        // egui can retain the previous caret instead of treating the area as the
                        // end of the last line.
                        let clicked_below_document = output.response.clicked()
                            && ui.input(|input| input.modifiers.is_none())
                            && output
                                .response
                                .interact_pointer_pos()
                                .is_some_and(|pointer| {
                                    pointer.y > output.galley_pos.y + output.galley.rect.bottom()
                                });
                        if clicked_below_document {
                            let end = egui::text::CCursor::new(document.content.chars().count());
                            let caret = egui::text::CCursorRange::one(end);
                            output.state.cursor.set_char_range(Some(caret));
                            output.state.clone().store(ui.ctx(), output.response.id);
                            output.cursor_range = Some(caret);
                            output.response.request_focus();
                        }
                        // TextEdit reports `text_clip_rect` as the full text rect — the whole
                        // document laid out inside the ScrollArea — not the visible viewport.
                        // Every "visible only" cull below must use the real viewport, or it
                        // silently degrades to whole-file work per frame (a select-all in a
                        // few-thousand-line file drops to ~12 fps otherwise).
                        // Horizontally it is only as wide as the galley — zero for an empty
                        // file, which clipped the caret away, and flush with column 0, which
                        // cut the caret there in half — so take the x range from the whole
                        // TextEdit (margins included) instead.
                        let viewport_clip = ui.clip_rect().intersect(egui::Rect::from_x_y_ranges(
                            output.response.rect.x_range(),
                            output.text_clip_rect.y_range(),
                        ));
                        let navigation_range = navigation_range.as_ref().map(|range| {
                            super::editor_logic::clamp_byte_range(&document.content, range)
                        });
                        let selection_target = navigation_range.as_ref();
                        let find_caret_target =
                            select_find_match.then(|| &find_ranges[active_find_match.unwrap_or(0)]);
                        if let Some(byte_range) = selection_target.or(find_caret_target) {
                            let start = document.content[..byte_range.start].chars().count();
                            let end = start + document.content[byte_range.clone()].chars().count();
                            let cursor_range = if selection_target.is_some() {
                                egui::text::CCursorRange::two(
                                    egui::text::CCursor::new(start),
                                    egui::text::CCursor::new(end),
                                )
                            } else {
                                // Find selects the match, like Sublime: typing replaces it and
                                // F3 / Cmd+G continue from its end.
                                egui::text::CCursorRange::two(
                                    egui::text::CCursor::new(start),
                                    egui::text::CCursor::new(end),
                                )
                            };
                            output.state.cursor.set_char_range(Some(cursor_range));
                            output.state.store(ui.ctx(), output.response.id);
                            if focus_editor_requested {
                                output.response.request_focus();
                            }
                        }

                        // Scrolling only needs match geometry. Keep it separate from cursor
                        // mutation so live query updates cannot disturb the focused Find field.
                        let reveal_target = selection_target.or_else(|| {
                            reveal_find_match.then(|| &find_ranges[active_find_match.unwrap_or(0)])
                        });
                        if let Some(byte_range) = reveal_target {
                            let end = document.content[..byte_range.end].chars().count();
                            let caret = output
                                .galley
                                .pos_from_cursor(egui::text::CCursor {
                                    index: egui::text::CharIndex(end),
                                    prefer_next_row: true,
                                })
                                .translate(output.galley_pos.to_vec2());
                            ui.scroll_to_rect(caret, Some(egui::Align::Center));
                        }

                        if focus_editor_requested && find_caret_target.is_none() {
                            output.response.request_focus();
                        }
                        if let Some((caret, _)) = command {
                            let caret = output
                                .galley
                                .pos_from_cursor(egui::text::CCursor::new(caret))
                                .translate(output.galley_pos.to_vec2());
                            ui.scroll_to_rect(caret, None);
                        }

                        // The primary caret's char index is free from the cursor range. The
                        // byte offset (needed only when a definition jump fires) and the logical
                        // line are derived from the layout, not by walking the document. The old
                        // char-by-char scans grew to the whole file whenever the caret sat near
                        // the end, e.g. right after Select All.
                        let caret_char = output.cursor_range.map(|range| range.primary.index.0);
                        let active_line = output
                            .cursor_range
                            .map(|range| caret_logical_line(&output.galley, range.primary));
                        let definition_modifier =
                            ui.input(|input| input.modifiers.command || input.modifiers.ctrl);
                        let hovered_definition = if navigation_supported
                            && definition_modifier
                            && output.response.hovered()
                        {
                            ui.input(|input| input.pointer.hover_pos())
                                .filter(|position| viewport_clip.contains(*position))
                                .and_then(|position| {
                                    let cursor =
                                        output.galley.cursor_from_pos(position - output.galley_pos);
                                    let byte =
                                        char_index_to_byte(&document.content, cursor.index.0);
                                    crate::code_nav::identifier_at(&document.content, byte)
                                        .map(|(_, range)| range)
                                        .filter(|range| {
                                            byte_range_rects(
                                                &output.galley,
                                                output.galley_pos,
                                                &document.content,
                                                range,
                                            )
                                            .iter()
                                            .any(|rect| rect.contains(position))
                                        })
                                })
                        } else {
                            None
                        };
                        if hovered_definition.is_some() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                        let click_byte = (output.response.clicked() && definition_modifier)
                            .then(|| hovered_definition.as_ref().map(|range| range.start))
                            .flatten();
                        let mut context_goto = false;
                        let has_selection =
                            output.cursor_range.is_some_and(|range| !range.is_empty());
                        output.response.context_menu(|ui| {
                            if navigation_supported
                                && ui.button("Go to Definition    F12").clicked()
                            {
                                context_goto = true;
                                ui.close();
                            }
                            if has_selection
                                && ui
                                    .button(if cfg!(target_os = "macos") {
                                        "Add Selection to Chat    ⌘⇧L"
                                    } else {
                                        "Add Selection to Chat    Ctrl+Shift+L"
                                    })
                                    .clicked()
                            {
                                selection_to_chat = true;
                                ui.close();
                            }
                        });
                        let navigation_request = if navigation_supported {
                            click_byte.or_else(|| {
                                (goto_definition_requested || context_goto)
                                    .then(|| {
                                        caret_char.map(|index| {
                                            char_index_to_byte(&document.content, index)
                                        })
                                    })
                                    .flatten()
                            })
                        } else {
                            None
                        };
                        editor_selection = output.cursor_range.map(|range| {
                            let sorted = range.as_sorted_char_range();
                            (sorted.start.0, sorted.end.0)
                        });
                        let selection = output.cursor_range.filter(|range| !range.is_empty());
                        let wrap_width_bits = output.galley.job.wrap.max_width.round().to_bits();
                        let pixels_per_point_bits = ui.ctx().pixels_per_point().to_bits();
                        let window = syntax_window::line_window(
                            &output.galley,
                            output.galley_pos,
                            viewport_clip,
                        );
                        let cached_lines = &document.layout_cache.syntax_lines;
                        let can_reuse_syntax = find_ranges.is_empty()
                            && hovered_definition.is_none()
                            && document.layout_cache.syntax_palette
                                == crate::theme::palette_generation()
                            && document.layout_cache.revision == document.content_revision
                            && document.layout_cache.wrap_width_bits == wrap_width_bits
                            && document.layout_cache.pixels_per_point_bits == pixels_per_point_bits
                            && cached_lines.start <= window.visible.start
                            && cached_lines.end >= window.visible.end;
                        let mut highlight_pending = false;
                        let mut syntax_lines = document.layout_cache.syntax_lines.clone();
                        // A whole-document colored job, when one is at hand for free (the
                        // syntect path); the minimap reuses it for its colors.
                        let mut full_job = None;
                        let visible_galley = if can_reuse_syntax {
                            document.layout_cache.syntax.as_ref().map(Arc::clone)
                        } else {
                            None
                        }
                        .unwrap_or_else(|| {
                            syntax_lines = window.padded.clone();
                            let window_bytes =
                                syntax_window::line_byte_range(&document.content, &syntax_lines);
                            // Tree-sitter colors just the window; other languages go through
                            // syntect on a worker thread, which colors the whole text.
                            let (mut job, job_start) =
                                match crate::theme::highlight_editor_code_with_revision(
                                    &mut document.syntax_state,
                                    &document.content,
                                    &extension,
                                    FontId::monospace(FS_SMALL),
                                    Some(document.content_revision),
                                    Some(window_bytes.clone()),
                                ) {
                                    Some(job) => {
                                        // Colors from a tree still being reparsed in the
                                        // background: show them, but don't cache them.
                                        if document
                                            .syntax_state
                                            .as_ref()
                                            .is_some_and(|state| state.parse_pending())
                                        {
                                            highlight_pending = true;
                                            ui.ctx().request_repaint_after(
                                                std::time::Duration::from_millis(16),
                                            );
                                        }
                                        (job, window_bytes.start)
                                    }
                                    None => match crate::theme::highlight_code_async(
                                        &document.content,
                                        &extension,
                                        FontId::monospace(FS_SMALL),
                                        ui.ctx(),
                                    ) {
                                        Some(job) => {
                                            full_job = Some(job.clone());
                                            (job, 0)
                                        }
                                        None => {
                                            // Highlighting runs in the background; show
                                            // plain text now and let a later frame pick the
                                            // colors up.
                                            highlight_pending = true;
                                            (
                                                egui::text::LayoutJob::simple(
                                                    document.content[window_bytes.clone()]
                                                        .to_owned(),
                                                    FontId::monospace(FS_SMALL),
                                                    c_text(),
                                                    f32::INFINITY,
                                                ),
                                                window_bytes.start,
                                            )
                                        }
                                    },
                                };
                            // Find matches and the go-to-definition underline are document
                            // byte ranges; move them into the job's own coordinates.
                            let job_end = job_start + job.text.len();
                            let to_job = |range: &std::ops::Range<usize>| {
                                let start = range.start.clamp(job_start, job_end);
                                start - job_start..range.end.clamp(start, job_end) - job_start
                            };
                            // Matches are sorted; only those inside the job need highlighting.
                            let first_find =
                                find_ranges.partition_point(|range| range.end <= job_start);
                            let local_find: Vec<_> = find_ranges[first_find..]
                                .iter()
                                .take_while(|range| range.start < job_end)
                                .map(to_job)
                                .collect();
                            let local_active = active_find_match
                                .and_then(|active| active.checked_sub(first_find))
                                .filter(|active| *active < local_find.len());
                            apply_search_highlights(&mut job, &local_find, local_active);
                            if let Some(range) = hovered_definition.as_ref()
                                && !to_job(range).is_empty()
                            {
                                apply_definition_underline(&mut job, &to_job(range));
                            }
                            job.wrap.max_width = output.galley.job.wrap.max_width;
                            let visible_job =
                                if job_start == 0 && job.text.len() == document.content.len() {
                                    syntax_window::slice_job(&job, &syntax_lines)
                                } else {
                                    job
                                };
                            let galley = ui.fonts_mut(|fonts| fonts.layout_job(visible_job));
                            // Store only when the cache keys already describe this exact
                            // layout (they are written by the geometry layouter). Overwriting
                            // the keys here could relabel a geometry galley from an older
                            // wrap width as current and corrupt both caches.
                            if find_ranges.is_empty()
                                && hovered_definition.is_none()
                                && !highlight_pending
                                && document.layout_cache.revision == document.content_revision
                                && document.layout_cache.wrap_width_bits == wrap_width_bits
                                && document.layout_cache.pixels_per_point_bits
                                    == pixels_per_point_bits
                            {
                                document.layout_cache.syntax = Some(Arc::clone(&galley));
                                document.layout_cache.syntax_lines = syntax_lines.clone();
                                document.layout_cache.syntax_palette =
                                    crate::theme::palette_generation();
                            }
                            galley
                        });
                        minimap::refresh(ui.ctx(), document, &extension, full_job.as_ref());
                        minimap::carry_layout_over_edit(
                            previous_minimap.take(),
                            &mut document.minimap_cache,
                        );
                        minimap::ensure_layout(
                            ui.ctx(),
                            document
                                .minimap_cache
                                .as_mut()
                                .expect("editor geometry was just prepared"),
                            &output.galley,
                            document.layout_cache.edited_at.is_some_and(|edited_at| {
                                edited_at.elapsed() < minimap::RECOLOR_AFTER_EDIT
                            }),
                        );
                        paint_indent_guides(
                            ui,
                            &output.galley,
                            output.galley_pos,
                            viewport_clip,
                            &document
                                .minimap_cache
                                .as_ref()
                                .expect("editor geometry was just prepared")
                                .indent_columns,
                        );

                        // Make Git changes readable where they are edited, not only as a thin
                        // gutter stripe. Every visual row belonging to a changed logical line
                        // gets a full-width tint; unchanged lines retain the normal editor
                        // background, so the boundary between the two is immediately visible.
                        if let Some(decor) = &diff_decor
                            && let Some(diff) = document.diff.as_mut()
                        {
                            let rows = super::diff_editor::PaneRows {
                                galley: &output.galley,
                                origin: output.galley_pos,
                                row_h: diff_row_height(&output.galley),
                            };
                            let clip = egui::Rect::from_x_y_ranges(
                                ui.clip_rect().x_range(),
                                viewport_clip.y_range(),
                            );
                            diff.base_job(&extension, ui.ctx());
                            super::diff_editor::Underlay {
                                decor,
                                side: super::diff_editor::Side::New,
                                inline: diff_inline,
                                clip,
                                zones: diff_inline.then(|| super::diff_editor::ZoneText {
                                    base: diff.base_text().unwrap_or_default(),
                                    base_job: diff.cached_base_job(),
                                    line_starts: diff.base_starts(),
                                }),
                            }
                            .paint(
                                ui.painter(),
                                &rows,
                                &FontId::monospace(FS_SMALL),
                            );
                            diff.frame.new_spans = decor
                                .changes
                                .iter()
                                .map(|change| {
                                    rows.span(change, super::diff_editor::Side::New, diff_inline)
                                })
                                .collect();
                            diff.frame.new_clip = clip;
                            diff.frame.zone_numbers = if diff_inline {
                                decor.zone_numbers(&rows, clip.y_range())
                            } else {
                                Vec::new()
                            };
                        } else if full_git_highlight {
                            // Up to the viewport's right edge, not just the text's: the
                            // TextEdit is only as wide as its longest line.
                            let tint_clip = egui::Rect::from_min_max(
                                viewport_clip.min,
                                egui::pos2(
                                    ui.clip_rect().right().max(viewport_clip.right()),
                                    viewport_clip.bottom(),
                                ),
                            );
                            let change_painter = ui.painter().with_clip_rect(tint_clip);
                            let mut logical_line = 0usize;
                            for (row, placed_row) in output.galley.rows.iter().enumerate() {
                                if row > 0 && output.galley.rows[row - 1].ends_with_newline {
                                    logical_line += 1;
                                }
                                let row_rect =
                                    placed_row.rect().translate(output.galley_pos.to_vec2());
                                if !row_rect.intersects(viewport_clip) {
                                    continue;
                                }
                                let Ok(change_index) = git_line_changes
                                    .binary_search_by_key(&logical_line, |change| change.line)
                                else {
                                    continue;
                                };
                                let change = &git_line_changes[change_index];
                                let highlight_rect = egui::Rect::from_min_max(
                                    egui::pos2(tint_clip.left(), row_rect.top()),
                                    egui::pos2(tint_clip.right(), row_rect.bottom()),
                                );
                                let color = match change.kind {
                                    crate::git::GitLineKind::Added => c_diff_add_bg(),
                                    crate::git::GitLineKind::Modified => c_warning_bg(),
                                    crate::git::GitLineKind::Deleted => continue,
                                };
                                change_painter.rect_filled(highlight_rect, 0.0, color);
                            }
                        }

                        // Paint selection behind the visible galley. The previous order put a
                        // translucent wash over the glyphs; depending on the backend it looked
                        // opaque and left only our whitespace markers visible.
                        if let Some(selection) = selection {
                            let selection_color = crate::theme::editor_selection_fill();
                            let selection_painter = ui.painter().with_clip_rect(viewport_clip);
                            let selection_rects = editor_selection_rects(
                                &output.galley,
                                output.galley_pos,
                                viewport_clip,
                                selection,
                            );
                            paint_selection(&selection_painter, &selection_rects, selection_color);
                        }

                        // TextEdit's geometry is transparent; paint the syntax galley, which
                        // covers only a window of lines, at that window's first row.
                        let syntax_origin = output.galley_pos
                            + egui::vec2(
                                0.0,
                                syntax_window::line_top(&output.galley, syntax_lines.start),
                            );
                        let visible_galley = match &diff_decor {
                            Some(decor) => super::diff_editor::apply_gaps(
                                &visible_galley,
                                syntax_lines.start,
                                &decor.gaps(super::diff_editor::Side::New, diff_inline),
                                diff_row_height(&output.galley),
                            ),
                            None => visible_galley,
                        };
                        ui.painter().with_clip_rect(viewport_clip).galley(
                            syntax_origin,
                            visible_galley,
                            c_text(),
                        );

                        paint_selection_matches(
                            ui,
                            &output.galley,
                            output.galley_pos,
                            viewport_clip,
                            &document.content,
                            selection,
                            &window.visible,
                        );
                        if let Some(cursor_range) = output.cursor_range
                            && cursor_range.is_empty()
                        {
                            let caret = cursor_range.primary.index.0;
                            let pair = match document.layout_cache.bracket_pair {
                                Some((cached, pair)) if cached == caret => pair,
                                _ => {
                                    let content = document.content.as_str();
                                    let pair = super::editor_commands::bracket_pair_at(
                                        content,
                                        super::editor_text::byte_index(content, caret),
                                    )
                                    .map(|(a, b)| {
                                        (
                                            super::editor_text::char_index(content, a),
                                            super::editor_text::char_index(content, b),
                                        )
                                    });
                                    document.layout_cache.bracket_pair = Some((caret, pair));
                                    pair
                                }
                            };
                            if let Some((a, b)) = pair {
                                paint_bracket_underlines(
                                    ui,
                                    &output.galley,
                                    output.galley_pos,
                                    viewport_clip,
                                    [a, b],
                                );
                            }
                        }
                        if let Some(selection) = selection {
                            paint_selected_whitespace(
                                ui,
                                &output.galley,
                                output.galley_pos,
                                viewport_clip,
                                selection,
                            );
                        }
                        if output.response.has_focus()
                            && let Some(cursor_range) = output.cursor_range
                        {
                            paint_caret(
                                ui,
                                &output.galley,
                                output.galley_pos,
                                viewport_clip,
                                cursor_range.primary,
                            );
                        }

                        let selected_lines =
                            selection.map(|range| selected_logical_lines(&output.galley, range));

                        // Return the screen-space positions needed to paint the fixed gutter,
                        // plus whether the pointer is extending a text selection. egui normally
                        // suppresses ScrollArea wheel input while a child is being dragged, so
                        // the latter is used below to keep editor scrolling responsive.
                        //
                        // Only the lines inside the viewport are emitted. The gutter lays out a
                        // number glyph per entry, so returning every logical line made a 3000-line
                        // file shape 3000 tiny galleys each frame; culling here keeps that O(visible).
                        let gutter_clip_range = viewport_clip.y_range();
                        let gutter_line_height = FS_SMALL * 1.35;
                        let mut logical_line = 0usize;
                        let mut viewport_anchor_line = 0usize;
                        let mut resize_anchor_top = None;
                        let mut line_positions: Vec<(usize, f32)> = Vec::new();
                        for (row, placed_row) in output.galley.rows.iter().enumerate() {
                            let starts_line =
                                row == 0 || output.galley.rows[row - 1].ends_with_newline;
                            if !starts_line {
                                continue;
                            }
                            let line_rect =
                                placed_row.rect().translate(output.galley_pos.to_vec2());
                            let y = line_rect.center().y;
                            // Tolerance: after a resize rebase the anchor line sits exactly at
                            // the viewport top, and float error must not hand the anchor to
                            // the line above (each resize frame would then creep upwards).
                            if line_rect.top() <= viewport_clip.top() + 0.5 {
                                viewport_anchor_line = logical_line;
                            }
                            if resize_anchor == Some(logical_line) {
                                resize_anchor_top = Some(line_rect.top());
                            }
                            if y >= gutter_clip_range.min - gutter_line_height
                                && y <= gutter_clip_range.max + gutter_line_height
                            {
                                line_positions.push((logical_line, y));
                            }
                            logical_line += 1;
                        }
                        (
                            line_positions,
                            output.response.dragged(),
                            selected_lines,
                            navigation_request,
                            active_line,
                            caret_char,
                            (viewport_anchor_line, resize_anchor_top),
                            scratchpad_changed,
                            output.galley_pos.y + output.galley.rect.bottom(),
                            output.response.id,
                        )
                    })
                })
                .inner;

            // ScrollArea preserves a raw pixel offset when its width changes. That makes
            // soft-wrapped rows above the viewport appear to push the document around. Rebase
            // that offset to the same first logical line instead, keeping its line number at the
            // top regardless of how many visual rows are added or removed by wrapping.
            if resize_anchor.is_some()
                && let Some(line_top) = scroll_output.inner.6.1
            {
                let mut state = scroll_output.state;
                let max_y =
                    (scroll_output.content_size.y - scroll_output.inner_rect.height()).max(0.0);
                state.offset.y =
                    (line_top - scroll_output.inner_rect.top() + state.offset.y).clamp(0.0, max_y);
                state.store(ui.ctx(), scroll_output.id);
                ui.ctx().request_repaint();
            }

            if resize_anchor.is_none() {
                self.conv.editor.documents[index].viewport_anchor_line = scroll_output.inner.6.0;
            }

            let gutter_clip = gutter_rect.intersect(ui.clip_rect());
            if let Some(diff) = self.conv.editor.documents[index]
                .diff
                .as_mut()
                .filter(|_| diff_decor.is_some())
            {
                diff.scroll_y = scroll_output.state.offset.y;
                for &(number, y) in &diff.frame.zone_numbers {
                    ui.painter().with_clip_rect(gutter_clip).text(
                        egui::pos2(gutter_rect.right() - GUTTER_RIGHT_PADDING, y),
                        egui::Align2::RIGHT_CENTER,
                        number,
                        FontId::monospace(FS_SMALL),
                        c_diff_del_fg().gamma_multiply(0.75),
                    );
                }
            }
            for (line, y) in scroll_output.inner.0.iter().copied() {
                let line_height = FS_SMALL * 1.35;
                if scroll_output.inner.4 == Some(line) {
                    ui.painter().with_clip_rect(gutter_clip).rect_filled(
                        egui::Rect::from_center_size(
                            egui::pos2(gutter_rect.center().x, y),
                            egui::vec2(gutter_rect.width(), line_height),
                        ),
                        0.0,
                        c_row_active(),
                    );
                }
                if let Some(change) = git_line_changes.iter().find(|change| change.line == line) {
                    let color = match change.kind {
                        crate::git::GitLineKind::Added => c_diff_add_fg(),
                        crate::git::GitLineKind::Modified => c_warning_fg(),
                        crate::git::GitLineKind::Deleted => {
                            // Removed lines sit between rows: a wedge on this row's top edge.
                            let top = y - line_height * 0.5;
                            let x = gutter_rect.left();
                            ui.painter().with_clip_rect(gutter_clip).add(
                                egui::Shape::convex_polygon(
                                    vec![
                                        egui::pos2(x, top - 4.0),
                                        egui::pos2(x + 5.0, top),
                                        egui::pos2(x, top + 4.0),
                                    ],
                                    c_diff_del_fg(),
                                    egui::Stroke::NONE,
                                ),
                            );
                            egui::Color32::TRANSPARENT
                        }
                    };
                    ui.painter().with_clip_rect(gutter_clip).rect_filled(
                        egui::Rect::from_center_size(
                            egui::pos2(gutter_rect.left() + GIT_MARKER_WIDTH * 0.5, y),
                            egui::vec2(GIT_MARKER_WIDTH, line_height),
                        ),
                        0.0,
                        color,
                    );
                }
                ui.painter().with_clip_rect(gutter_clip).text(
                    egui::pos2(gutter_rect.right() - GUTTER_RIGHT_PADDING, y),
                    egui::Align2::RIGHT_CENTER,
                    line + 1,
                    FontId::monospace(FS_SMALL),
                    if scroll_output.inner.4 == Some(line) {
                        c_text_muted()
                    } else {
                        c_text_faint()
                    },
                );
            }

            // ScrollArea deliberately ignores the wheel while TextEdit owns a selection drag.
            // Restore that expected editor behavior and also auto-scroll when the pointer approaches
            // the top/bottom edge while extending the selection.
            let selection_scroll = if scroll_output.inner.1 {
                let wheel_y = ui.input(|input| input.smooth_scroll_delta.y);
                let edge_y =
                    ui.input(|input| input.pointer.interact_pos())
                        .map_or(0.0, |pointer| {
                            const EDGE_ZONE: f32 = 28.0;
                            if pointer.y < scroll_output.inner_rect.top() + EDGE_ZONE {
                                ((scroll_output.inner_rect.top() + EDGE_ZONE - pointer.y)
                                    / EDGE_ZONE)
                                    .clamp(0.0, 2.5)
                                    * -12.0
                            } else if pointer.y > scroll_output.inner_rect.bottom() - EDGE_ZONE {
                                ((pointer.y - (scroll_output.inner_rect.bottom() - EDGE_ZONE))
                                    / EDGE_ZONE)
                                    .clamp(0.0, 2.5)
                                    * 12.0
                            } else {
                                0.0
                            }
                        });
                -wheel_y + edge_y
            } else {
                0.0
            };
            if selection_scroll != 0.0 {
                let mut state = scroll_output.state;
                let max_y =
                    (scroll_output.content_size.y - scroll_output.inner_rect.height()).max(0.0);
                state.offset.y = (state.offset.y + selection_scroll).clamp(0.0, max_y);
                state.store(ui.ctx(), scroll_output.id);
                ui.ctx().request_repaint();
            }

            // The ScrollArea can own the blank viewport below a short document instead of the
            // TextEdit response. Handle that outer area too, so clicking anywhere in the source
            // column below the final row reliably places the caret at EOF.
            let clicked_blank_tail = ui.input(|input| input.pointer.primary_clicked())
                && ui.input(|input| input.modifiers.is_none())
                && ui
                    .input(|input| input.pointer.interact_pos())
                    .is_some_and(|pointer| {
                        scroll_output.inner_rect.contains(pointer)
                            && pointer.y > scroll_output.inner.8
                    });
            if clicked_blank_tail {
                let id = scroll_output.inner.9;
                let end = egui::text::CCursor::new(
                    self.conv.editor.documents[index].content.chars().count(),
                );
                let caret = egui::text::CCursorRange::one(end);
                if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), id) {
                    state.cursor.set_char_range(Some(caret));
                    state.store(ui.ctx(), id);
                }
                ui.ctx().memory_mut(|memory| memory.request_focus(id));
                self.conv.editor.navigation_cursor_char = end.index.0;
                ui.ctx().request_repaint();
            }

            if scroll_output.inner.7 {
                self.autosave_scratchpad(index);
            }

            goto_definition_byte = scroll_output.inner.3;
            if let Some(caret_char) = scroll_output.inner.5 {
                self.conv.editor.navigation_cursor_char = caret_char;
            }

            // The minimap is outside the editor ScrollArea. Its own narrow scroll strip is painted
            // after it, so the visual order is source → minimap → scrollbar. A diff shows its
            // overview ruler there instead.
            if let Some(decor) = &diff_decor {
                let (ruler_rect, _) = ui.allocate_exact_size(
                    egui::vec2(super::diff_editor::RULER_W, editor_view_size.y.max(24.0)),
                    egui::Sense::hover(),
                );
                let content_top = scroll_output.inner_rect.top() - scroll_output.state.offset.y;
                let spans: Vec<_> = self.conv.editor.documents[index]
                    .diff
                    .as_ref()
                    .map(|diff| {
                        diff.frame
                            .new_spans
                            .iter()
                            .zip(&decor.changes)
                            .map(|(&(top, bottom), change)| {
                                (
                                    top - content_top,
                                    bottom - content_top,
                                    !change.old_lines.is_empty(),
                                    !change.new_lines.is_empty(),
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if let Some(offset) = super::diff_editor::ruler(
                    ui,
                    ruler_rect,
                    &spans,
                    scroll_output.content_size.y,
                    scroll_output.inner_rect.height(),
                    scroll_output.state.offset.y,
                ) {
                    let mut state = scroll_output.state;
                    state.offset.y = offset;
                    state.store(ui.ctx(), scroll_output.id);
                    ui.ctx().request_repaint();
                }
            } else if let Some(fraction) = minimap::paint(
                ui,
                egui::vec2(MINIMAP_WIDTH, editor_view_size.y.max(24.0)),
                &scroll_output,
                scroll_output.inner.2,
                self.conv.editor.documents[index]
                    .minimap_cache
                    .as_ref()
                    .expect("minimap geometry is prepared during editor rendering"),
            ) {
                let mut state = scroll_output.state;
                let max_y =
                    (scroll_output.content_size.y - scroll_output.inner_rect.height()).max(0.0);
                state.offset.y = max_y * fraction;
                state.store(ui.ctx(), scroll_output.id);
                ui.ctx().request_repaint();
            }

            // Last: opening the diff changes how the document is laid out.
            if diff_decor.is_none() {
                self.gutter_marker_click(
                    ui,
                    index,
                    gutter_rect,
                    &scroll_output.inner.0,
                    &git_line_changes,
                );
            }
        });
        self.conv.editor.editor_selection_chars = editor_selection;
        if selection_to_chat {
            self.add_editor_selection_to_chat();
        }
        if let Some(byte) = goto_definition_byte {
            self.go_to_definition(byte, ui.ctx());
        }
    }
}

/// Invalidate every per-revision cache after the text changed. Returns the old minimap, which
/// keeps being shown until the new one is laid out.
pub(super) fn mark_document_edited(
    document: &mut crate::app::state::EditorDocument,
) -> Option<super::MinimapGeometry> {
    document.content_revision = document.content_revision.wrapping_add(1);
    document.dirty = document.content != document.saved_content;
    document.layout_cache = EditorLayoutCache {
        edited_at: Some(std::time::Instant::now()),
        lines: document.layout_cache.lines.take(),
        ..Default::default()
    };
    document.minimap_cache.take()
}

/// Height of one row of a diff pane's galley (rows never wrap there).
fn diff_row_height(galley: &egui::Galley) -> f32 {
    galley
        .rows
        .first()
        .map_or(FS_SMALL * 1.35, |row| row.rect().height())
}

fn has_mutating_text_input(ui: &Ui) -> bool {
    ui.input(|input| {
        input.events.iter().any(|event| match event {
            egui::Event::Cut | egui::Event::Paste(_) | egui::Event::Text(_) => true,
            egui::Event::Ime(egui::ImeEvent::Preedit { .. } | egui::ImeEvent::Commit(_)) => true,
            egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } => {
                matches!(
                    key,
                    egui::Key::Backspace | egui::Key::Delete | egui::Key::Enter | egui::Key::Tab
                ) || (modifiers.command && matches!(key, egui::Key::Z | egui::Key::Y))
            }
            _ => false,
        })
    })
}
