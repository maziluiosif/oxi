//! Painting: toolbar, rows, file headers, block buttons and the overview ruler.

use super::*;

impl DiffView {
    pub(super) fn toolbar(
        &mut self,
        ui: &mut Ui,
        source: &str,
        split_possible: bool,
        can_open: &dyn Fn(&str) -> bool,
    ) -> Option<DiffAction> {
        let mut action = None;
        ui.spacing_mut().item_spacing.x = 6.0;
        if let Some(commit) = &self.model.commit {
            let short = &commit.hash[..commit.hash.len().min(7)];
            ui.label(
                egui::RichText::new(short)
                    .monospace()
                    .size(FS_SMALL)
                    .color(c_accent()),
            );
            let summary = commit.message.lines().next().unwrap_or_default();
            ui.add(
                egui::Label::new(
                    egui::RichText::new(summary)
                        .size(FS_SMALL)
                        .color(c_text_strong()),
                )
                .truncate(),
            );
        } else if let [file] = self.model.files.as_slice() {
            let path = file.path();
            let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
            ui.label(
                egui::RichText::new(name)
                    .size(FS_SMALL)
                    .color(c_text_strong()),
            );
            if !dir.is_empty() {
                ui.add(
                    egui::Label::new(egui::RichText::new(dir).size(FS_TINY).color(c_text_faint()))
                        .truncate(),
                );
            }
        } else {
            ui.label(
                egui::RichText::new(format!("{} files", self.model.files.len()))
                    .size(FS_SMALL)
                    .color(c_text_strong()),
            );
        }

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            if let Some(path) = self.single_path().filter(|p| can_open(p))
                && toolbar_icon(ui, ICON_FILE, "Open file", true).clicked()
            {
                action = Some(DiffAction::OpenFile {
                    path: path.to_owned(),
                    line: None,
                });
            }
            ui.add_space(6.0);
            let inline = self.inline || !split_possible;
            if segment(ui, "Inline", inline, true, "Show changes in one column").clicked() {
                self.inline = true;
            }
            let split_hint = if split_possible {
                "Show old and new side by side"
            } else {
                "Too narrow for side by side"
            };
            if segment(ui, "Split", !inline, split_possible, split_hint).clicked() {
                self.inline = false;
            }
            if self.layout.foldable {
                ui.add_space(6.0);
                let fold_hint = if self.collapse_unchanged {
                    "Show all unchanged lines"
                } else {
                    "Collapse unchanged regions"
                };
                if segment(ui, "Full file", !self.collapse_unchanged, true, fold_hint).clicked() {
                    self.collapse_unchanged = !self.collapse_unchanged;
                    self.expanded.clear();
                }
            }
            ui.add_space(6.0);
            let has_changes = !self.layout.changes.is_empty();
            if toolbar_icon(ui, ICON_ANGLE_DOWN, "Next change (F7)", has_changes).clicked() {
                self.jump(true);
            }
            if toolbar_icon(ui, ICON_ANGLE_UP, "Previous change (Shift+F7)", has_changes).clicked()
            {
                self.jump(false);
            }
            ui.add_space(4.0);
            let (added, removed) = self
                .model
                .files
                .iter()
                .fold((0, 0), |(a, r), f| (a + f.added, r + f.removed));
            for (count, sign, color) in [
                (removed, '−', c_diff_del_fg()),
                (added, '+', c_diff_add_fg()),
            ] {
                if count > 0 {
                    ui.label(
                        egui::RichText::new(format!("{sign}{count}"))
                            .monospace()
                            .size(FS_TINY)
                            .color(color),
                    );
                }
            }
            if !source.is_empty() {
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(source)
                        .size(FS_TINY)
                        .color(c_text_faint()),
                );
            }
        });
        action
    }

    pub(super) fn paint_rows(
        &mut self,
        ui: &mut Ui,
        origin: egui::Pos2,
        viewport: Rect,
        char_w: f32,
        can_open: &dyn Fn(&str) -> bool,
        block_actions: &[BlockAction],
    ) -> Option<DiffAction> {
        let mut action = None;
        let width = ui.max_rect().width();
        let line_h = line_height();
        let first = self
            .layout
            .ys
            .partition_point(|&y| y <= viewport.min.y)
            .saturating_sub(1);
        let visible: Vec<(usize, Row, Rect)> = (first..self.layout.rows.len())
            .map(|i| {
                let y = self.layout.ys[i];
                let h = self
                    .layout
                    .ys
                    .get(i + 1)
                    .copied()
                    .unwrap_or(self.layout.total)
                    - y;
                (
                    i,
                    self.layout.rows[i],
                    Rect::from_min_size(pos2(origin.x, origin.y + y), vec2(width, h)),
                )
            })
            .take_while(|(_, _, rect)| rect.top() < origin.y + viewport.max.y)
            .collect();

        // Highlight each visible file's sides once (off the UI thread; plain text meanwhile).
        let mut files: Vec<usize> = visible
            .iter()
            .filter_map(|(_, row, _)| row_file(*row))
            .collect();
        files.dedup();
        for file in files {
            let f = &mut self.model.files[file];
            let ext = f
                .path()
                .rsplit_once('.')
                .map(|(_, ext)| ext.to_owned())
                .unwrap_or_default();
            if ext.is_empty() {
                continue;
            }
            for side in 0..2 {
                if f.side_job[side].is_none() && !f.side_text[side].is_empty() {
                    f.side_job[side] =
                        highlight_code_async(&f.side_text[side], &ext, code_font(), ui.ctx());
                }
            }
        }

        let numbers_w = self.digits as f32 * char_w;
        // Added before the rows' own widgets (gutters, folds, headers), so those stay on top.
        let text_area = Rect::from_min_max(
            pos2(origin.x, origin.y + viewport.min.y),
            pos2(origin.x + width, origin.y + viewport.max.y),
        );
        let select = ui.interact(
            text_area,
            ui.id().with("diff_text_selection"),
            Sense::click_and_drag(),
        );
        self.handle_selection(ui, &select, origin, width, numbers_w, text_area);

        let painter = ui.painter().clone();
        let mut texts: Vec<(Arc<egui::Galley>, egui::Pos2, Rect)> = Vec::new();
        let mut selection_runs: Vec<Vec<Rect>> = vec![Vec::new()];
        for &(index, row, rect) in &visible {
            match row {
                Row::Commit => {
                    if let Some(commit) = &self.model.commit {
                        paint_commit(ui, rect, commit);
                    }
                }
                Row::File(file) => {
                    if self.file_header(ui, rect, file, can_open, &mut action) {
                        if !self.collapsed_files.remove(&file) {
                            self.collapsed_files.insert(file);
                        }
                        ui.ctx().request_repaint();
                    }
                }
                Row::Note(file) => {
                    let f = &self.model.files[file];
                    let text = if f.binary {
                        "Binary file — no text diff"
                    } else if f.truncated {
                        "Diff truncated"
                    } else {
                        "No content changes"
                    };
                    painter.text(
                        pos2(rect.left() + GUTTER_PAD + 4.0, rect.center().y),
                        Align2::LEFT_CENTER,
                        text,
                        FontId::proportional(FS_SMALL),
                        c_text_faint(),
                    );
                }
                Row::Gap { file, item } => {
                    if let Some(Item::Gap { heading, hidden }) =
                        self.model.files[file].items.get(item)
                    {
                        let label = match hidden {
                            Some(n) => format!("{n} lines not shown"),
                            None => "⋯".to_owned(),
                        };
                        paint_band(&painter, rect, &label, heading, false);
                    }
                }
                Row::Fold { file, start, end } => {
                    let response = ui.interact(
                        rect,
                        ui.id().with(("diff_fold", file, start)),
                        Sense::click(),
                    );
                    let label = format!("{} unchanged lines", end - start);
                    paint_band(
                        &painter,
                        rect,
                        &label,
                        "click to expand",
                        response.hovered(),
                    );
                    if response.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if response.clicked() {
                        self.expanded.insert((file, start));
                        ui.ctx().request_repaint();
                    }
                }
                Row::Split { file, left, right } => {
                    let half = (width / 2.0).floor();
                    let left_rect = Rect::from_min_size(rect.min, vec2(half, line_h));
                    let right_rect = Rect::from_min_max(
                        pos2(rect.left() + half + 1.0, rect.top()),
                        rect.right_bottom(),
                    );
                    for (side, item, pane) in [(0, left, left_rect), (1, right, right_rect)] {
                        let f = &self.model.files[file];
                        let Some(line) = item.and_then(|i| f.line(i)) else {
                            paint_hatch(&painter, pane);
                            if self.selection.is_some_and(|sel| sel.pane == side) {
                                selection_runs.push(Vec::new());
                            }
                            continue;
                        };
                        let number = if side == 0 { line.old_no } else { line.new_no };
                        paint_line_chrome(&painter, pane, line.kind, &[number], numbers_w);
                        let open = (side == 1 && f.new_path.as_deref().is_some_and(can_open))
                            .then(|| (f.path(), line.new_no));
                        if let Some(a) = gutter_click(ui, pane, numbers_w, open, (index, side)) {
                            action = Some(a);
                        }
                        let text_left = pane.left() + numbers_w + 2.0 * GUTTER_PAD + SIGN_W;
                        self.place_line_text(
                            ui,
                            (index, side),
                            line_job(f, line, side),
                            line.text.chars().count(),
                            pane,
                            text_left,
                            &mut texts,
                            &mut selection_runs,
                        );
                    }
                    painter.vline(
                        rect.left() + half + 0.5,
                        rect.y_range(),
                        Stroke::new(1.0, c_border()),
                    );
                }
                Row::Inline { file, item } => {
                    let f = &self.model.files[file];
                    let Some(line) = f.line(item) else { continue };
                    let pane = Rect::from_min_size(rect.min, vec2(width, line_h));
                    let inline_numbers = 2.0 * numbers_w + 8.0;
                    paint_line_chrome(
                        &painter,
                        pane,
                        line.kind,
                        &[line.old_no, line.new_no],
                        numbers_w,
                    );
                    let open = f
                        .new_path
                        .as_deref()
                        .is_some_and(can_open)
                        .then(|| (f.path(), line.new_no));
                    if let Some(a) = gutter_click(ui, pane, inline_numbers, open, (index, 0)) {
                        action = Some(a);
                    }
                    let side = usize::from(line.kind != Kind::Removed);
                    let text_left = pane.left() + inline_numbers + 2.0 * GUTTER_PAD + SIGN_W;
                    self.place_line_text(
                        ui,
                        (index, 2),
                        line_job(f, line, side),
                        line.text.chars().count(),
                        pane,
                        text_left,
                        &mut texts,
                        &mut selection_runs,
                    );
                }
            }
            // Non-line rows (headers, folds, gaps) break the selection shape.
            if !matches!(row, Row::Split { .. } | Row::Inline { .. }) {
                selection_runs.push(Vec::new());
            }
        }
        // Selection under the text, clipped to the selected pane's text column.
        if let Some(sel) = self.selection {
            let column = self.pane_text_column(sel.pane, origin.x, width, numbers_w);
            let clip = Rect::from_x_y_ranges(column, text_area.y_range());
            let selection_painter = painter.with_clip_rect(clip.intersect(painter.clip_rect()));
            for run in selection_runs.iter().filter(|run| !run.is_empty()) {
                selection_painter.add(crate::ui::text_selection::selection_shape(
                    run,
                    editor_selection_fill(),
                ));
            }
        }
        for (galley, pos, clip) in texts {
            painter
                .with_clip_rect(clip.intersect(painter.clip_rect()))
                .galley(pos, galley, c_text());
        }
        if !block_actions.is_empty()
            && let Some(a) = self.block_buttons(ui, &visible, block_actions)
        {
            action = Some(a);
        }

        // Sticky header: the file being read keeps its name visible while scrolled into it.
        if let Some(&(_, row, rect)) = visible.first()
            && let Some(file) = row_file(row)
            && let Some(header) = self.layout.rows.iter().position(|r| *r == Row::File(file))
            && origin.y + self.layout.ys[header] < origin.y + viewport.min.y
            && !matches!(row, Row::File(_))
        {
            let top = origin.y + viewport.min.y;
            let sticky = Rect::from_min_size(pos2(rect.left(), top), vec2(width, FILE_HEADER_H));
            if self.file_header(ui, sticky, file, can_open, &mut action) {
                self.collapsed_files.insert(file);
                self.pending_scroll = Some(self.layout.ys[header]);
                ui.ctx().request_repaint();
            }
        }
        action
    }

    /// Queue one line's text for painting and add its selected span to the selection shape.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn place_line_text(
        &self,
        ui: &Ui,
        (row, pane_index): (usize, usize),
        job: Option<LayoutJob>,
        chars: usize,
        pane: Rect,
        text_left: f32,
        texts: &mut Vec<(Arc<egui::Galley>, egui::Pos2, Rect)>,
        selection_runs: &mut Vec<Vec<Rect>>,
    ) {
        let galley = job.map(|job| ui.painter().layout_job(job));
        let x = text_left - self.scroll_x;
        if let Some(sel) = self.selection.filter(|sel| sel.pane == pane_index) {
            let (start, end) = sel.sorted();
            if start != end && (start.0..=end.0).contains(&row) {
                let x_of = |col: usize| {
                    galley
                        .as_ref()
                        .map_or(0.0, |g| g.pos_from_cursor(CCursor::new(col)).min.x)
                };
                let from = if row == start.0 {
                    start.1.min(chars)
                } else {
                    0
                };
                let left = x_of(from);
                let right = if row == end.0 {
                    x_of(end.1.min(chars))
                } else {
                    // Past the end, like the editor's newline marker.
                    galley.as_ref().map_or(0.0, |g| g.size().x) + pane.height() * 0.5
                };
                if right > left {
                    selection_runs
                        .last_mut()
                        .expect("never empty")
                        .push(Rect::from_min_max(
                            pos2(x + left, pane.top()),
                            pos2(x + right, pane.bottom()),
                        ));
                } else {
                    selection_runs.push(Vec::new());
                }
            }
        }
        if let Some(galley) = galley {
            let pos = pos2(x, pane.center().y - galley.size().y / 2.0);
            let clip = Rect::from_min_max(pos2(text_left, pane.top()), pane.max);
            texts.push((galley, pos, clip));
        }
    }

    /// Buttons for the block under the pointer, over the top right of its first visible row.
    pub(super) fn block_buttons(
        &self,
        ui: &mut Ui,
        visible: &[(usize, Row, Rect)],
        block_actions: &[BlockAction],
    ) -> Option<DiffAction> {
        let pointer = ui.input(|i| i.pointer.hover_pos())?;
        if !ui.clip_rect().contains(pointer) {
            return None;
        }
        let hovered = visible
            .iter()
            .find(|(_, _, rect)| rect.contains(pointer))
            .and_then(|&(_, row, _)| self.row_block(row))?;
        let (file, block) = hovered;
        if self.model.files[file].truncated {
            return None;
        }
        let &(_, _, first) = visible
            .iter()
            .find(|&&(_, row, _)| self.row_block(row) == Some(hovered))?;
        // Keep the buttons inside the viewport when the block starts above it.
        let top = first.top().max(ui.clip_rect().top());
        let mut right = first.right() - 8.0;
        let mut action = None;
        for &block_action in block_actions.iter().rev() {
            let (icon, label, hover) = block_action.label();
            let text = format!("{icon} {label}");
            let mut job = LayoutJob::default();
            job.append(
                icon,
                0.0,
                TextFormat::simple(FontId::new(FS_TINY, icon_font()), c_text()),
            );
            job.append(
                label,
                4.0,
                TextFormat::simple(FontId::proportional(FS_TINY), c_text()),
            );
            let galley = ui.painter().layout_job(job);
            let rect = Rect::from_min_size(
                pos2(right - galley.size().x - 12.0, top + 1.0),
                vec2(galley.size().x + 12.0, line_height() - 2.0),
            );
            right = rect.left() - 4.0;
            let response = ui
                .interact(
                    rect,
                    ui.id().with(("diff_block", file, block, text)),
                    Sense::click(),
                )
                .on_hover_text(hover);
            let painter = ui.painter();
            painter.rect(
                rect,
                egui::CornerRadius::same(4),
                if response.hovered() {
                    c_row_hover()
                } else {
                    c_bg_elevated()
                },
                Stroke::new(1.0, c_border()),
                egui::StrokeKind::Inside,
            );
            painter.galley(
                pos2(rect.left() + 6.0, rect.center().y - galley.size().y / 2.0),
                galley,
                c_text(),
            );
            if response.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if response.clicked() {
                action = Some(DiffAction::Block {
                    action: block_action,
                    block: self.model.files[file].block(block),
                });
            }
        }
        action
    }

    /// Paint a file header; returns whether it was clicked (toggle collapse).
    pub(super) fn file_header(
        &self,
        ui: &mut Ui,
        rect: Rect,
        file: usize,
        can_open: &dyn Fn(&str) -> bool,
        action: &mut Option<DiffAction>,
    ) -> bool {
        let f = &self.model.files[file];
        let collapsed = self.collapsed_files.contains(&file);
        let response = ui.interact(rect, ui.id().with(("diff_file", file)), Sense::click());
        // `hovered()` turns false while the pointer is on the open button drawn over the header,
        // which would hide that button again; track the pointer over the whole header instead.
        let hot = ui.rect_contains_pointer(rect);
        let painter = ui.painter();
        painter.rect_filled(rect, 0.0, if hot { c_row_hover() } else { c_bg_elevated() });
        painter.hline(
            rect.x_range(),
            rect.bottom() - 0.5,
            Stroke::new(1.0, c_border_subtle()),
        );
        let mut x = rect.left() + GUTTER_PAD;
        painter.text(
            pos2(x + 6.0, rect.center().y),
            Align2::CENTER_CENTER,
            if collapsed {
                ICON_CHEVRON_RIGHT
            } else {
                ICON_ANGLE_DOWN
            },
            FontId::new(FS_TINY, icon_font()),
            c_text_muted(),
        );
        x += 20.0;
        let status = f.status();
        let status_color = match status {
            'A' => c_diff_add_fg(),
            'D' => c_diff_del_fg(),
            _ => c_accent(),
        };
        let galley = painter.layout_no_wrap(
            status.to_string(),
            FontId::monospace(FS_SMALL),
            status_color,
        );
        painter.galley(
            pos2(x, rect.center().y - galley.size().y / 2.0),
            galley,
            status_color,
        );
        x += 18.0;
        let path = match (&f.old_path, &f.new_path) {
            (Some(old), Some(new)) if old != new => format!("{old} → {new}"),
            _ => f.path().to_owned(),
        };
        let (dir, name) = path.rsplit_once('/').unwrap_or(("", path.as_str()));
        let mut job = LayoutJob::default();
        job.append(
            name,
            0.0,
            TextFormat::simple(FontId::proportional(FS_SMALL), c_text_strong()),
        );
        if !dir.is_empty() {
            job.append(
                dir,
                8.0,
                TextFormat::simple(FontId::proportional(FS_TINY), c_text_faint()),
            );
        }
        let galley = painter.layout_job(job);
        painter.galley(
            pos2(x, rect.center().y - galley.size().y / 2.0),
            galley,
            c_text(),
        );
        let mut right = rect.right() - GUTTER_PAD;
        if hot && f.new_path.as_deref().is_some_and(can_open) {
            let open_rect =
                Rect::from_center_size(pos2(right - 10.0, rect.center().y), vec2(22.0, 22.0));
            let open = ui
                .interact(
                    open_rect,
                    ui.id().with(("diff_file_open", file)),
                    Sense::click(),
                )
                .on_hover_text("Open file");
            ui.painter().text(
                open_rect.center(),
                Align2::CENTER_CENTER,
                ICON_FILE,
                FontId::new(FS_TINY, icon_font()),
                if open.hovered() {
                    c_accent()
                } else {
                    c_text_muted()
                },
            );
            if open.clicked() {
                *action = Some(DiffAction::OpenFile {
                    path: f.path().to_owned(),
                    line: None,
                });
                return false;
            }
            right -= 28.0;
        }
        for (count, sign, color) in [
            (f.added, '+', c_diff_add_fg()),
            (f.removed, '−', c_diff_del_fg()),
        ] {
            if count == 0 {
                continue;
            }
            let text = format!("{sign}{count}");
            let galley = ui
                .painter()
                .layout_no_wrap(text, FontId::monospace(FS_TINY), color);
            right -= galley.size().x;
            ui.painter().galley(
                pos2(right, rect.center().y - galley.size().y / 2.0),
                galley,
                color,
            );
            right -= 8.0;
        }
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        response.clicked()
    }

    pub(super) fn ruler(&mut self, ui: &mut Ui, rect: Rect) {
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, c_bg_main());
        painter.vline(
            rect.left() + 0.5,
            rect.y_range(),
            Stroke::new(1.0, c_border_subtle()),
        );
        let total = self.layout.total.max(1.0);
        // Short diffs map 1:1 instead of stretching their few changes over the whole ruler.
        let scale = rect.height() / total.max(self.viewport_h);
        let lane = (rect.width() - 4.0) / 2.0;
        for change in &self.layout.changes {
            let y = rect.top() + change.y * scale;
            let h = (change.h * scale).max(2.0);
            if change.removed {
                painter.rect_filled(
                    Rect::from_min_size(pos2(rect.left() + 2.0, y), vec2(lane, h)),
                    0.0,
                    c_diff_del_fg().gamma_multiply(0.8),
                );
            }
            if change.added {
                painter.rect_filled(
                    Rect::from_min_size(pos2(rect.left() + 2.0 + lane, y), vec2(lane, h)),
                    0.0,
                    c_diff_add_fg().gamma_multiply(0.8),
                );
            }
        }
        if total <= self.viewport_h {
            return;
        }
        let thumb = Rect::from_min_size(
            pos2(rect.left() + 1.0, rect.top() + self.scroll_y * scale),
            vec2(rect.width() - 1.0, (self.viewport_h * scale).max(8.0)),
        );
        let response = ui.interact(rect, ui.id().with("diff_ruler"), Sense::click_and_drag());
        painter.rect_filled(
            thumb,
            0.0,
            c_text().gamma_multiply(if response.hovered() || response.dragged() {
                0.16
            } else {
                0.08
            }),
        );
        if (response.clicked() || response.dragged())
            && let Some(pointer) = response.interact_pointer_pos()
        {
            let y = (pointer.y - rect.top()) / scale - self.viewport_h / 2.0;
            self.pending_scroll = Some(y.clamp(0.0, total - self.viewport_h));
        }
    }
}

/// Row tint, line numbers and the +/− sign for one pane of a line row.
pub(super) fn paint_line_chrome(
    painter: &egui::Painter,
    pane: Rect,
    kind: Kind,
    numbers: &[Option<usize>],
    numbers_w: f32,
) {
    let (fill, fg) = match kind {
        Kind::Removed => (c_diff_del_bg(), c_diff_del_fg()),
        Kind::Added => (c_diff_add_bg(), c_diff_add_fg()),
        Kind::Context => (Color32::TRANSPARENT, c_text_faint()),
    };
    if fill != Color32::TRANSPARENT {
        painter.rect_filled(pane, 0.0, fill);
    }
    let number_color = if kind == Kind::Context {
        c_text_faint()
    } else {
        fg.gamma_multiply(0.75)
    };
    let mut x = pane.left() + GUTTER_PAD;
    for number in numbers {
        x += numbers_w;
        if let Some(n) = number {
            painter.text(
                pos2(x, pane.center().y),
                Align2::RIGHT_CENTER,
                n.to_string(),
                code_font(),
                number_color,
            );
        }
        x += 8.0;
    }
    let sign = match kind {
        Kind::Removed => "−",
        Kind::Added => "+",
        Kind::Context => return,
    };
    painter.text(
        pos2(x - 8.0 + GUTTER_PAD + SIGN_W / 2.0, pane.center().y),
        Align2::CENTER_CENTER,
        sign,
        code_font(),
        fg,
    );
}

/// Clicking a new-side line number opens the file at that line.
pub(super) fn gutter_click(
    ui: &mut Ui,
    pane: Rect,
    numbers_w: f32,
    open: Option<(&str, Option<usize>)>,
    id: (usize, usize),
) -> Option<DiffAction> {
    let (path, Some(line)) = open? else {
        return None;
    };
    let gutter = Rect::from_min_size(
        pane.min,
        vec2(GUTTER_PAD + numbers_w + GUTTER_PAD, pane.height()),
    );
    let response = ui.interact(gutter, ui.id().with(("diff_gutter", id)), Sense::click());
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response.clicked().then(|| DiffAction::OpenFile {
        path: path.to_owned(),
        line: Some(line),
    })
}

/// Diagonal hatching for the side of a row that has no line, like VS Code's filler.
pub(super) fn paint_hatch(painter: &egui::Painter, rect: Rect) {
    const STEP: f32 = 8.0;
    let painter = painter.with_clip_rect(rect.intersect(painter.clip_rect()));
    let stroke = Stroke::new(1.0, c_text_faint().gamma_multiply(0.25));
    // Lines x + y = k·STEP in absolute coordinates, so the pattern continues across rows.
    let first = ((rect.left() + rect.top()) / STEP).floor() as i64;
    let last = ((rect.right() + rect.bottom()) / STEP).ceil() as i64;
    let shapes = (first..=last)
        .map(|k| {
            let c = k as f32 * STEP;
            Shape::line_segment(
                [
                    pos2(c - rect.top(), rect.top()),
                    pos2(c - rect.bottom(), rect.bottom()),
                ],
                stroke,
            )
        })
        .collect::<Vec<_>>();
    painter.extend(shapes);
}

/// A full-width band row: hunk boundaries and collapsed unchanged regions.
pub(super) fn paint_band(
    painter: &egui::Painter,
    rect: Rect,
    label: &str,
    detail: &str,
    hovered: bool,
) {
    let band = rect.shrink2(vec2(0.0, 2.0));
    painter.rect_filled(
        band,
        0.0,
        if hovered {
            c_row_hover()
        } else {
            c_bg_elevated()
        },
    );
    let mut job = LayoutJob::default();
    job.append(
        "⋯  ",
        0.0,
        TextFormat::simple(FontId::proportional(FS_TINY), c_text_faint()),
    );
    job.append(
        label,
        0.0,
        TextFormat::simple(
            FontId::proportional(FS_TINY),
            if hovered { c_accent() } else { c_text_muted() },
        ),
    );
    if !detail.is_empty() {
        job.append(
            detail,
            12.0,
            TextFormat::simple(FontId::monospace(FS_TINY), c_text_faint()),
        );
    }
    let galley = painter.layout_job(job);
    painter.galley(
        pos2(
            band.left() + GUTTER_PAD + 4.0,
            band.center().y - galley.size().y / 2.0,
        ),
        galley,
        c_text_muted(),
    );
}

pub(super) fn paint_commit(ui: &mut Ui, rect: Rect, commit: &CommitInfo) {
    let inner = rect.shrink2(vec2(GUTTER_PAD + 4.0, 10.0));
    let mut child = ui.new_child(
        UiBuilder::new()
            .id_salt("diff_commit")
            .max_rect(inner)
            .layout(Layout::top_down(Align::Min)),
    );
    child.spacing_mut().item_spacing.y = 4.0;
    let mut lines = commit.message.lines();
    child.add(
        egui::Label::new(
            egui::RichText::new(lines.next().unwrap_or_default())
                .size(FS_BODY)
                .strong()
                .color(c_text_strong()),
        )
        .selectable(true),
    );
    let body = lines.collect::<Vec<_>>().join("\n");
    if !body.trim().is_empty() {
        child.add(
            egui::Label::new(
                egui::RichText::new(body.trim())
                    .size(FS_SMALL)
                    .color(c_text_muted()),
            )
            .selectable(true),
        );
    }
    // The author's email adds little here and pushes the line into a wrap.
    let author = commit
        .author
        .split_once(" <")
        .map_or(commit.author.as_str(), |(name, _)| name);
    child.add(
        egui::Label::new(
            egui::RichText::new(format!("{}  ·  {author}  ·  {}", commit.hash, commit.date))
                .size(FS_TINY)
                .monospace()
                .color(c_text_faint()),
        )
        .truncate(),
    );
    ui.painter().hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        Stroke::new(1.0, c_border_subtle()),
    );
}

pub(super) fn commit_height(commit: &CommitInfo) -> f32 {
    let body_lines = commit
        .message
        .trim()
        .lines()
        .skip(1)
        .skip_while(|l| l.trim().is_empty())
        .count();
    let body = if body_lines > 0 {
        body_lines as f32 * FS_SMALL * 1.3 + 4.0
    } else {
        0.0
    };
    20.0 + FS_BODY * 1.4 + 4.0 + body + FS_TINY * 1.4 + 4.0
}

/// Syntax-colored layout of one line with word-level change backgrounds.
pub(super) fn line_job(file: &DiffFile, line: &Line, side: usize) -> Option<LayoutJob> {
    if line.text.is_empty() {
        return None;
    }
    let text = line.text.as_str();
    let len = text.len();
    let font = code_font();
    let base = file.side_job[side].as_ref();
    let offset = line.offset[side];
    let sections = base.map_or(&[][..], |job| {
        let first = job
            .sections
            .partition_point(|s| s.byte_range.end.0 <= offset);
        let end =
            job.sections[first..].partition_point(|s| s.byte_range.start.0 < offset + len) + first;
        &job.sections[first..end]
    });
    let mut cuts = vec![0, len];
    for word in &line.words {
        cuts.extend([word.start.min(len), word.end.min(len)]);
    }
    for section in sections {
        cuts.push(section.byte_range.start.0.saturating_sub(offset).min(len));
        cuts.push(section.byte_range.end.0.saturating_sub(offset).min(len));
    }
    cuts.sort_unstable();
    cuts.dedup();
    let word_bg = match line.kind {
        Kind::Removed => c_diff_del_fg().gamma_multiply(0.3),
        Kind::Added => c_diff_add_fg().gamma_multiply(0.3),
        Kind::Context => Color32::TRANSPARENT,
    };
    let fallback = c_text();
    let mut job = LayoutJob {
        wrap: TextWrapping {
            max_width: f32::INFINITY,
            ..Default::default()
        },
        ..Default::default()
    };
    for pair in cuts.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if a >= b || !text.is_char_boundary(a) || !text.is_char_boundary(b) {
            continue;
        }
        let color = sections
            .iter()
            .find(|s| s.byte_range.start.0 <= offset + a && offset + a < s.byte_range.end.0)
            .map_or(fallback, |s| s.format.color);
        let background = if line.words.iter().any(|w| w.start <= a && b <= w.end) {
            word_bg
        } else {
            Color32::TRANSPARENT
        };
        job.append(
            &text[a..b],
            0.0,
            TextFormat {
                font_id: font.clone(),
                color,
                background,
                ..Default::default()
            },
        );
    }
    Some(job)
}

pub(crate) fn segment(
    ui: &mut Ui,
    label: &str,
    selected: bool,
    enabled: bool,
    hint: &str,
) -> egui::Response {
    let text = egui::RichText::new(label).size(FS_TINY).color(if selected {
        c_text_strong()
    } else {
        c_text_muted()
    });
    let button = egui::Button::new(text)
        .fill(if selected {
            c_row_active()
        } else {
            Color32::TRANSPARENT
        })
        .stroke(Stroke::NONE)
        .corner_radius(RADIUS_ROW)
        .min_size(vec2(0.0, 22.0));
    let response = ui.add_enabled(enabled, button);
    if enabled {
        response.on_hover_text(hint)
    } else {
        response.on_disabled_hover_text(hint)
    }
}

pub(crate) fn toolbar_icon(ui: &mut Ui, icon: &str, hint: &str, enabled: bool) -> egui::Response {
    ui.add_enabled(
        enabled,
        egui::Button::new(icon_glyph_rich(icon, FS_SMALL, c_text_muted()))
            .frame(false)
            .min_size(vec2(24.0, 24.0)),
    )
    .on_hover_text(hint)
}
