//! Main text editor viewport, layout caching, gutter, and in-memory Git markers.

use std::{path::PathBuf, sync::Arc};

use eframe::egui::scroll_area::ScrollBarVisibility;
use eframe::egui::{self, FontId, Margin, ScrollArea, TextEdit, Ui};

use crate::theme::*;

use super::super::OxiApp;
use super::editor_logic::{char_index_to_byte, live_git_line_changes};
use super::editor_paint::{
    byte_range_rects, caret_logical_line, editor_selection_rects, paint_caret, paint_indent_guides,
    paint_selected_whitespace, paint_selection, selected_logical_lines,
};
use super::support::{
    apply_definition_underline, apply_search_highlights, find_match_ranges, language_for_path,
};
use super::{EditorLayoutCache, minimap};

pub(super) type EditorScrollOutput = egui::scroll_area::ScrollAreaOutput<(
    Vec<(usize, f32)>,
    bool,
    Option<(usize, usize)>,
    Option<usize>,
    Option<usize>,
    Option<usize>,
    (usize, Option<f32>),
)>;

impl OxiApp {
    pub(super) fn render_editor_body(&mut self, ui: &mut Ui) {
        let Some(index) = self.conv.editor.active else {
            return;
        };
        let extension = language_for_path(&self.conv.editor.documents[index].path).to_owned();
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
        let find_ranges = if self.conv.editor.find_open
            || self.conv.editor.find_select_pending
            || self.conv.editor.find_focus_editor_pending
        {
            find_match_ranges(
                &self.conv.editor.documents[index].content,
                &self.conv.editor.find_query,
                self.conv.editor.find_case_sensitive,
            )
        } else {
            Vec::new()
        };
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
        let disk_git_line_changes = self
            .conv
            .git
            .line_changes
            .get(&relative_path)
            .cloned()
            .unwrap_or_default();
        // Git itself only sees the saved file. When editing changes the number of lines,
        // project those disk-based markers onto the in-memory buffer so the gutter follows
        // inserted/deleted newlines without doing Git work on every keystroke.
        let git_line_changes = if self.conv.editor.documents[index].is_dirty() {
            live_git_line_changes(
                &disk_git_line_changes,
                &self.conv.editor.documents[index].saved_content,
                &self.conv.editor.documents[index].content,
            )
        } else {
            disk_git_line_changes
        };
        const MINIMAP_WIDTH: f32 = 96.0;
        let editor_view_size = ui.available_size();
        const MINIMAP_SCROLLBAR_WIDTH: f32 = 10.0;
        let prospective_editor_width =
            (editor_view_size.x - gutter_width - MINIMAP_WIDTH - MINIMAP_SCROLLBAR_WIDTH).max(80.0);
        let prospective_width_bits = prospective_editor_width.round().to_bits();
        let resize_anchor = self.conv.editor.documents[index]
            .viewport_width_bits
            .filter(|width| *width != prospective_width_bits)
            .map(|_| self.conv.editor.documents[index].viewport_anchor_line);
        self.conv.editor.documents[index].viewport_width_bits = Some(prospective_width_bits);
        let mut goto_definition_byte = None;
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
                    ScrollArea::both()
                        .id_salt("text_editor_scroll")
                        // A shared scrollbar is painted to the right of the minimap below.
                        .scroll_bar_visibility(ScrollBarVisibility::AlwaysHidden)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            let editor_size = egui::vec2(
                                editor_view_width.max(80.0),
                                editor_view_size.y.max(24.0),
                            );
                            let document = &mut self.conv.editor.documents[index];
                            let revision = document.content_revision;
                            let pixels_per_point_bits = ui.ctx().pixels_per_point().to_bits();
                            let allow_layout_cache = !has_mutating_text_input(ui);
                            let layout_cache = &mut document.layout_cache;
                            let mut layouter =
                                |ui: &Ui, text: &dyn egui::TextBuffer, wrap_width: f32| {
                                    let wrap_width_bits = wrap_width.round().to_bits();
                                    if allow_layout_cache
                                        && layout_cache.revision == revision
                                        && layout_cache.wrap_width_bits == wrap_width_bits
                                        && layout_cache.pixels_per_point_bits
                                            == pixels_per_point_bits
                                        && let Some(galley) = &layout_cache.geometry
                                    {
                                        return Arc::clone(galley);
                                    }

                                    // TextEdit only needs glyph geometry here; cache it before
                                    // egui's whole-LayoutJob hashing so selection-only frames are O(1).
                                    let mut job = egui::text::LayoutJob::simple(
                                        text.as_str().to_owned(),
                                        FontId::monospace(FS_SMALL),
                                        egui::Color32::TRANSPARENT,
                                        wrap_width,
                                    );
                                    job.wrap.max_width = wrap_width;
                                    let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
                                    if allow_layout_cache {
                                        layout_cache.revision = revision;
                                        layout_cache.wrap_width_bits = wrap_width_bits;
                                        layout_cache.pixels_per_point_bits = pixels_per_point_bits;
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
                                    TextEdit::multiline(&mut document.content)
                                        .id_salt(("workspace_text_editor", index))
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
                            if output.response.changed() {
                                document.content_revision =
                                    document.content_revision.wrapping_add(1);
                                document.dirty = document.content != document.saved_content;
                                document.layout_cache = EditorLayoutCache::default();
                                document.minimap_cache = None;
                            }
                            // TextEdit reports `text_clip_rect` as the full text rect — the whole
                            // document laid out inside the ScrollArea — not the visible viewport.
                            // Every "visible only" cull below must use the real viewport, or it
                            // silently degrades to whole-file work per frame (a select-all in a
                            // few-thousand-line file drops to ~12 fps otherwise).
                            let viewport_clip = ui.clip_rect().intersect(output.text_clip_rect);
                            let selection_target = navigation_range.as_ref();
                            let find_caret_target = select_find_match
                                .then(|| &find_ranges[active_find_match.unwrap_or(0)]);
                            if let Some(byte_range) = selection_target.or(find_caret_target) {
                                let start = document.content[..byte_range.start].chars().count();
                                let end =
                                    start + document.content[byte_range.clone()].chars().count();
                                let cursor_range = if selection_target.is_some() {
                                    egui::text::CCursorRange::two(
                                        egui::text::CCursor::new(start),
                                        egui::text::CCursor::new(end),
                                    )
                                } else {
                                    // Find navigation places an insertion caret immediately after
                                    // the match, ready to continue editing the document.
                                    egui::text::CCursorRange::one(egui::text::CCursor::new(end))
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
                                reveal_find_match
                                    .then(|| &find_ranges[active_find_match.unwrap_or(0)])
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
                            let hovered_definition = if extension == "rs"
                                && definition_modifier
                                && output.response.hovered()
                            {
                                ui.input(|input| input.pointer.hover_pos())
                                    .filter(|position| viewport_clip.contains(*position))
                                    .and_then(|position| {
                                        let cursor = output
                                            .galley
                                            .cursor_from_pos(position - output.galley_pos);
                                        let byte =
                                            char_index_to_byte(&document.content, cursor.index.0);
                                        crate::rust_goto::identifier_at(&document.content, byte)
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
                            output.response.context_menu(|ui| {
                                if extension == "rs"
                                    && ui.button("Go to definition    F12").clicked()
                                {
                                    context_goto = true;
                                    ui.close();
                                }
                            });
                            let navigation_request = if extension == "rs" {
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
                            let selection = output.cursor_range.filter(|range| !range.is_empty());
                            let wrap_width_bits =
                                output.galley.job.wrap.max_width.round().to_bits();
                            let pixels_per_point_bits = ui.ctx().pixels_per_point().to_bits();
                            let can_reuse_syntax = find_ranges.is_empty()
                                && hovered_definition.is_none()
                                && document.layout_cache.revision == document.content_revision
                                && document.layout_cache.wrap_width_bits == wrap_width_bits
                                && document.layout_cache.pixels_per_point_bits
                                    == pixels_per_point_bits;
                            let mut syntax_job = None;
                            let visible_galley = if can_reuse_syntax {
                                document.layout_cache.syntax.as_ref().map(Arc::clone)
                            } else {
                                None
                            }
                            .unwrap_or_else(|| {
                                let mut job = crate::theme::highlight_editor_code_with_revision(
                                    &mut document.syntax_state,
                                    &document.content,
                                    &extension,
                                    FontId::monospace(FS_SMALL),
                                    Some(document.content_revision),
                                )
                                .unwrap_or_else(|| {
                                    themed_highlight(
                                        ui,
                                        &document.content,
                                        &extension,
                                        FontId::monospace(FS_SMALL),
                                    )
                                });
                                if document.minimap_cache.is_none() {
                                    minimap::ensure_geometry(
                                        &document.content,
                                        &job,
                                        &mut document.minimap_cache,
                                    );
                                }
                                apply_search_highlights(&mut job, &find_ranges, active_find_match);
                                if let Some(range) = hovered_definition.as_ref() {
                                    apply_definition_underline(&mut job, range);
                                }
                                job.wrap.max_width = output.galley.job.wrap.max_width;
                                let galley = ui.fonts_mut(|fonts| fonts.layout_job(job.clone()));
                                syntax_job = Some(job);
                                // Store only when the cache keys already describe this exact
                                // layout (they are written by the geometry layouter). Overwriting
                                // the keys here could relabel a geometry galley from an older
                                // wrap width as current and corrupt both caches.
                                if find_ranges.is_empty()
                                    && hovered_definition.is_none()
                                    && document.layout_cache.revision == document.content_revision
                                    && document.layout_cache.wrap_width_bits == wrap_width_bits
                                    && document.layout_cache.pixels_per_point_bits
                                        == pixels_per_point_bits
                                {
                                    document.layout_cache.syntax = Some(Arc::clone(&galley));
                                }
                                galley
                            });
                            if document.minimap_cache.is_none()
                                && let Some(job) = syntax_job.as_ref()
                            {
                                minimap::ensure_geometry(
                                    &document.content,
                                    job,
                                    &mut document.minimap_cache,
                                );
                            }
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
                            if full_git_highlight {
                                let change_painter = ui.painter().with_clip_rect(viewport_clip);
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
                                        egui::pos2(viewport_clip.left(), row_rect.top()),
                                        egui::pos2(viewport_clip.right(), row_rect.bottom()),
                                    );
                                    let color = match change.kind {
                                        crate::git::GitLineKind::Added => c_diff_add_bg(),
                                        crate::git::GitLineKind::Modified => c_warning_bg(),
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
                                paint_selection(
                                    &selection_painter,
                                    &selection_rects,
                                    selection_color,
                                );
                            }

                            // TextEdit's geometry is transparent; paint the cached syntax galley.
                            ui.painter().with_clip_rect(viewport_clip).galley(
                                output.galley_pos,
                                visible_galley,
                                c_text(),
                            );

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

                            let selected_lines = selection
                                .map(|range| selected_logical_lines(&output.galley, range));

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
                                if line_rect.top() <= viewport_clip.top() {
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

            goto_definition_byte = scroll_output.inner.3;
            if let Some(caret_char) = scroll_output.inner.5 {
                self.conv.editor.navigation_cursor_char = caret_char;
            }

            // The minimap is outside the editor ScrollArea. Its own narrow scroll strip is painted
            // after it, so the visual order is source → minimap → scrollbar.
            if let Some(fraction) = minimap::paint(
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
        });
        if let Some(byte) = goto_definition_byte {
            self.go_to_rust_definition(byte);
        }
    }
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

fn themed_highlight(
    _ui: &Ui,
    content: &str,
    language: &str,
    font_id: FontId,
) -> egui::text::LayoutJob {
    crate::theme::highlight_code(content, language, font_id)
}
