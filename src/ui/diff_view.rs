//! VS Code–style diff editor for unified patches (git file/commit diffs, unsaved changes):
//! side-by-side or inline panes with line numbers, syntax colors, word-level change highlights,
//! collapsible unchanged regions, change navigation (F7 / Shift+F7) and an overview ruler.
//!
//! Rows have fixed heights and only the visible ones are laid out, so whole-file diffs stay
//! cheap. Text selection works like the editor's: it stays inside one pane, is painted as one
//! continuous shape and supports double/triple click, Shift+click, select all and copy.

use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::sync::Arc;

use eframe::egui::text::{CCursor, LayoutJob, TextWrapping};
use eframe::egui::{
    self, Align, Align2, Color32, FontId, Layout, Rect, Sense, Shape, Stroke, TextFormat, Ui,
    UiBuilder, pos2, vec2,
};

use crate::theme::*;
use crate::ui::chrome::icon_glyph_rich;

/// Unchanged lines kept visible around each change while unchanged regions are collapsed.
const CONTEXT: usize = 3;
/// Shorter unchanged runs are never folded: the fold row would hide almost nothing.
const MIN_FOLD: usize = 4;
/// Below this width two panes are too cramped to read; fall back to the inline layout.
const MIN_SPLIT_WIDTH: f32 = 640.0;
const TOOLBAR_H: f32 = 32.0;
const FILE_HEADER_H: f32 = 30.0;
const BAND_H: f32 = 24.0;
const RULER_W: f32 = 12.0;
const SIGN_W: f32 = 16.0;
const GUTTER_PAD: f32 = 10.0;
/// Word-level highlights only help when most of the line survived; past this share of
/// changed text the line tint alone reads better.
const MAX_WORD_CHANGE: f32 = 0.6;

fn line_height() -> f32 {
    FS_SMALL * 1.35
}

fn code_font() -> FontId {
    FontId::monospace(FS_SMALL)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Context,
    Removed,
    Added,
}

#[derive(Debug)]
struct Line {
    kind: Kind,
    old_no: Option<usize>,
    new_no: Option<usize>,
    text: String,
    /// Byte ranges that differ from the paired line on the other side.
    words: Vec<Range<usize>>,
    /// Byte offset of this line in each side's highlight text (`[old, new]`).
    offset: [usize; 2],
}

#[derive(Debug)]
enum Item {
    Line(Line),
    /// Hunk boundary: the lines in between aren't part of the patch.
    Gap {
        heading: String,
        hidden: Option<usize>,
    },
}

#[derive(Debug, Default)]
struct DiffFile {
    old_path: Option<String>,
    new_path: Option<String>,
    items: Vec<Item>,
    added: usize,
    removed: usize,
    binary: bool,
    truncated: bool,
    /// Each side's lines (old, new) joined by `\n`, highlighted as one text so multi-line
    /// constructs (strings, comments) keep their colors.
    side_text: [String; 2],
    side_job: [Option<LayoutJob>; 2],
    max_chars: usize,
    /// Item ranges of each contiguous run of changed lines.
    blocks: Vec<Range<usize>>,
    /// Index into `blocks` for each item.
    block_of: Vec<Option<usize>>,
}

impl DiffFile {
    fn path(&self) -> &str {
        self.new_path
            .as_deref()
            .or(self.old_path.as_deref())
            .unwrap_or_default()
    }

    fn status(&self) -> char {
        match (&self.old_path, &self.new_path) {
            (None, Some(_)) => 'A',
            (Some(_), None) => 'D',
            (Some(old), Some(new)) if old != new => 'R',
            _ => 'M',
        }
    }

    fn line(&self, item: usize) -> Option<&Line> {
        match self.items.get(item) {
            Some(Item::Line(line)) => Some(line),
            _ => None,
        }
    }

    fn kind(&self, item: usize) -> Option<Kind> {
        self.line(item).map(|line| line.kind)
    }

    fn block(&self, block: usize) -> DiffBlock {
        let range = self.blocks[block].clone();
        let lines = || range.clone().filter_map(|item| self.line(item));
        let side_lines = |kind| {
            lines()
                .filter(|line| line.kind == kind)
                .map(|line| line.text.clone())
                .collect::<Vec<_>>()
        };
        // An empty side starts right after the line before the block (or right before the
        // line after it), within the same hunk.
        let neighbor = |number: fn(&Line) -> Option<usize>| {
            lines().find_map(number).or_else(|| {
                self.items[..range.start]
                    .iter()
                    .rev()
                    .map_while(|item| match item {
                        Item::Line(line) => Some(line),
                        Item::Gap { .. } => None,
                    })
                    .find_map(|line| number(line).map(|n| n + 1))
                    .or_else(|| {
                        self.items[range.end..]
                            .iter()
                            .map_while(|item| match item {
                                Item::Line(line) => Some(line),
                                Item::Gap { .. } => None,
                            })
                            .find_map(number)
                    })
                    .or(Some(1))
            })
        };
        DiffBlock {
            path: self.path().to_owned(),
            old_start: neighbor(|line| line.old_no).unwrap_or(1),
            old_lines: side_lines(Kind::Removed),
            new_start: neighbor(|line| line.new_no).unwrap_or(1),
            new_lines: side_lines(Kind::Added),
        }
    }
}

#[derive(Debug, Default)]
struct CommitInfo {
    hash: String,
    author: String,
    date: String,
    message: String,
}

#[derive(Debug, Default)]
struct DiffModel {
    commit: Option<CommitInfo>,
    files: Vec<DiffFile>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Row {
    Commit,
    File(usize),
    Note(usize),
    Gap {
        file: usize,
        item: usize,
    },
    Fold {
        file: usize,
        start: usize,
        end: usize,
    },
    Split {
        file: usize,
        left: Option<usize>,
        right: Option<usize>,
    },
    Inline {
        file: usize,
        item: usize,
    },
}

/// Text selected in one pane: 0 = old (split left), 1 = new (split right), 2 = inline.
/// Positions are `(row, char)`; the char is clamped to the line when used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TextSelection {
    pane: usize,
    anchor: (usize, usize),
    cursor: (usize, usize),
}

impl TextSelection {
    fn sorted(&self) -> ((usize, usize), (usize, usize)) {
        (self.anchor.min(self.cursor), self.anchor.max(self.cursor))
    }
}

/// One contiguous block of changed rows, for navigation and the overview ruler.
#[derive(Clone, Copy, Debug)]
struct Change {
    y: f32,
    h: f32,
    removed: bool,
    added: bool,
}

#[derive(Default)]
struct RowLayout {
    rows: Vec<Row>,
    ys: Vec<f32>,
    total: f32,
    changes: Vec<Change>,
    /// Some unchanged run is long enough to fold (whether or not it currently is).
    foldable: bool,
}

/// What the host should do after a frame of the diff view.
pub enum DiffAction {
    /// Open the file (patch path, e.g. `src/main.rs`) in the editor, optionally at a 1-based line.
    OpenFile { path: String, line: Option<usize> },
    /// Apply `action` to one block of changed lines.
    Block {
        action: BlockAction,
        block: DiffBlock,
    },
}

/// Per-block buttons the host supports for the diff it shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockAction {
    /// Put the old lines back in place of the new ones.
    Revert,
    /// Write the new lines into the old side (the index).
    Stage,
    /// Put the old lines back in the new side (the index).
    Unstage,
}

impl BlockAction {
    fn label(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Self::Revert => (ICON_UNDO, "Revert", "Revert this change"),
            Self::Stage => (ICON_PLUS, "Stage", "Stage this change"),
            Self::Unstage => (ICON_CLOSE, "Unstage", "Unstage this change"),
        }
    }
}

/// One block of changed lines: both sides' lines and where they start (1-based; where they
/// would be inserted when a side is empty).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffBlock {
    pub path: String,
    pub old_start: usize,
    pub old_lines: Vec<String>,
    pub new_start: usize,
    pub new_lines: Vec<String>,
}

/// A parsed diff plus its view state (layout mode, folds, scroll), kept across frames.
pub struct DiffView {
    hash: u64,
    model: DiffModel,
    inline: bool,
    collapse_unchanged: bool,
    expanded: HashSet<(usize, usize)>,
    collapsed_files: HashSet<usize>,
    layout: RowLayout,
    layout_key: Option<(bool, bool, u64)>,
    digits: usize,
    scroll_x: f32,
    scroll_y: f32,
    viewport_h: f32,
    pending_scroll: Option<f32>,
    reveal_first_change: bool,
    selection: Option<TextSelection>,
}

impl DiffView {
    /// Keep `slot` in sync with `text`. An unchanged patch keeps all view state; a refreshed
    /// patch for the same view keeps the layout preferences and scroll position.
    pub fn sync(slot: &mut Option<DiffView>, text: &str) {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut hasher);
        let hash = hasher.finish();
        if slot.as_ref().is_some_and(|view| view.hash == hash) {
            return;
        }
        let model = parse(text);
        let previous = slot.take();
        let same_target = previous.as_ref().is_some_and(|view| {
            view.model.files.len() == model.files.len()
                && view
                    .model
                    .files
                    .iter()
                    .zip(&model.files)
                    .all(|(a, b)| a.path() == b.path())
                && view.model.commit.as_ref().map(|c| &c.hash)
                    == model.commit.as_ref().map(|c| &c.hash)
        });
        let digits = model
            .files
            .iter()
            .flat_map(|file| &file.items)
            .filter_map(|item| match item {
                Item::Line(line) => line.old_no.max(line.new_no),
                Item::Gap { .. } => None,
            })
            .max()
            .unwrap_or(1)
            .to_string()
            .len()
            .max(2);
        let (inline, collapse_unchanged) = previous
            .as_ref()
            .map_or((false, true), |view| (view.inline, view.collapse_unchanged));
        let (scroll_x, scroll_y) = previous
            .as_ref()
            .filter(|_| same_target)
            .map_or((0.0, 0.0), |view| (view.scroll_x, view.scroll_y));
        *slot = Some(DiffView {
            hash,
            model,
            inline,
            collapse_unchanged,
            expanded: HashSet::new(),
            collapsed_files: HashSet::new(),
            layout: RowLayout::default(),
            layout_key: None,
            digits,
            scroll_x,
            scroll_y,
            viewport_h: 0.0,
            pending_scroll: same_target.then_some(scroll_y),
            reveal_first_change: !same_target,
            selection: None,
        });
    }

    #[cfg(test)]
    pub fn set_inline(&mut self, inline: bool) {
        self.inline = inline;
    }

    /// Path of the only file in this diff, if it shows exactly one.
    pub fn single_path(&self) -> Option<&str> {
        match self.model.files.as_slice() {
            [file] => Some(file.path()),
            _ => None,
        }
    }

    /// Render toolbar + body into the remaining space. `source` labels where the diff comes
    /// from ("Working Tree", "Staged", …); `can_open` says whether a patch path exists on disk.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        source: &str,
        can_open: &dyn Fn(&str) -> bool,
        block_actions: &[BlockAction],
    ) -> Option<DiffAction> {
        let mut action = None;
        let full = ui.available_rect_before_wrap();
        let body_w = full.width() - RULER_W;
        let split_possible = body_w >= MIN_SPLIT_WIDTH;
        let split = split_possible && !self.inline;

        self.ensure_layout(split);
        let toolbar_rect = Rect::from_min_size(full.min, vec2(full.width(), TOOLBAR_H));
        let mut toolbar = ui.new_child(
            UiBuilder::new()
                .id_salt("diff_toolbar")
                .max_rect(toolbar_rect.shrink2(vec2(10.0, 0.0)))
                .layout(Layout::left_to_right(Align::Center)),
        );
        toolbar.set_clip_rect(toolbar_rect.intersect(ui.clip_rect()));
        if let Some(a) = self.toolbar(&mut toolbar, source, split_possible, can_open) {
            action = Some(a);
        }
        // Toolbar toggles apply this frame, not one input event later.
        let split = split_possible && !self.inline;
        self.ensure_layout(split);
        ui.painter().hline(
            full.x_range(),
            toolbar_rect.bottom() - 0.5,
            Stroke::new(1.0, c_border_subtle()),
        );

        let body = Rect::from_min_max(pos2(full.left(), toolbar_rect.bottom()), full.max);
        let content = Rect::from_min_max(body.min, pos2(body.right() - RULER_W, body.bottom()));
        let ruler = Rect::from_min_max(pos2(content.right(), body.top()), body.max);
        ui.allocate_rect(full, Sense::hover());

        if std::mem::take(&mut self.reveal_first_change)
            && let Some(first) = self.layout.changes.first()
            && first.y + first.h > content.height() * 0.8
        {
            self.pending_scroll = Some((first.y - content.height() / 3.0).max(0.0));
        }

        let (f7, shift) = ui.input(|i| (i.key_pressed(egui::Key::F7), i.modifiers.shift));
        if f7 {
            self.jump(!shift);
        }

        // Horizontal scrolling is shared by both panes so aligned rows stay aligned.
        let char_w = ui.fonts_mut(|f| f.glyph_width(&code_font(), '0'));
        let pane_text_w = if split {
            content.width() / 2.0 - self.gutter_w(char_w, false)
        } else {
            content.width() - self.gutter_w(char_w, true)
        };
        let widest = self
            .model
            .files
            .iter()
            .map(|f| f.max_chars)
            .max()
            .unwrap_or(0) as f32
            * char_w;
        let max_x = (widest + 24.0 - pane_text_w).max(0.0);
        if ui.rect_contains_pointer(content) {
            let delta = ui.input(|i| i.smooth_scroll_delta.x);
            self.scroll_x -= delta;
        }
        self.scroll_x = self.scroll_x.clamp(0.0, max_x);

        let mut content_ui = ui.new_child(
            UiBuilder::new()
                .id_salt("diff_body")
                .max_rect(content)
                .layout(Layout::top_down(Align::Min)),
        );
        content_ui.set_clip_rect(content.intersect(ui.clip_rect()));
        let mut scroll = egui::ScrollArea::vertical()
            .id_salt("diff_scroll")
            .auto_shrink([false, false])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden);
        if let Some(y) = self.pending_scroll.take() {
            scroll = scroll.vertical_scroll_offset(y);
        }
        let output = scroll.show_viewport(&mut content_ui, |ui, viewport| {
            ui.set_min_size(vec2(content.width(), self.layout.total));
            let origin = ui.max_rect().min;
            if let Some(a) = self.paint_rows(ui, origin, viewport, char_w, can_open, block_actions)
            {
                action = Some(a);
            }
        });
        self.scroll_y = output.state.offset.y;
        self.viewport_h = output.inner_rect.height();

        self.ruler(ui, ruler);
        action
    }

    fn ensure_layout(&mut self, split: bool) {
        let key = (split, self.collapse_unchanged, self.fold_state_hash());
        if self.layout_key != Some(key) {
            // Selections are row positions; they don't survive the rows changing.
            self.selection = None;
            self.layout = build_rows(
                &self.model,
                split,
                self.collapse_unchanged,
                &self.expanded,
                &self.collapsed_files,
            );
            self.layout_key = Some(key);
        }
    }

    fn fold_state_hash(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        let mut expanded: Vec<_> = self.expanded.iter().collect();
        expanded.sort();
        expanded.hash(&mut hasher);
        let mut collapsed: Vec<_> = self.collapsed_files.iter().collect();
        collapsed.sort();
        collapsed.hash(&mut hasher);
        hasher.finish()
    }

    fn gutter_w(&self, char_w: f32, inline: bool) -> f32 {
        let numbers = self.digits as f32 * char_w;
        let columns = if inline { 2.0 * numbers + 8.0 } else { numbers };
        GUTTER_PAD + columns + GUTTER_PAD + SIGN_W
    }

    fn jump(&mut self, forward: bool) {
        let anchor = self.scroll_y + self.viewport_h / 3.0;
        let changes = &self.layout.changes;
        let target = if forward {
            changes
                .iter()
                .find(|c| c.y > anchor + 1.0)
                .or(changes.first())
        } else {
            changes
                .iter()
                .rev()
                .find(|c| c.y < anchor - 1.0)
                .or(changes.last())
        };
        if let Some(change) = target {
            self.pending_scroll = Some((change.y - self.viewport_h / 3.0).max(0.0));
        }
    }

    fn toolbar(
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

    fn paint_rows(
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
    fn place_line_text(
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

    /// Horizontal extent of a pane's text column, in the rows' coordinates.
    fn pane_text_column(&self, pane: usize, left: f32, width: f32, numbers_w: f32) -> egui::Rangef {
        let half = (width / 2.0).floor();
        let (pane_left, pane_right, numbers) = match pane {
            0 => (left, left + half, numbers_w),
            1 => (left + half + 1.0, left + width, numbers_w),
            _ => (left, left + width, 2.0 * numbers_w + 8.0),
        };
        egui::Rangef::new(pane_left + numbers + 2.0 * GUTTER_PAD + SIGN_W, pane_right)
    }

    /// The line `pane` shows on `row`, with the side its highlighting comes from.
    fn pane_line(&self, row: usize, pane: usize) -> Option<(&DiffFile, &Line, usize)> {
        match (*self.layout.rows.get(row)?, pane) {
            (Row::Split { file, left, .. }, 0) => {
                let f = &self.model.files[file];
                Some((f, f.line(left?)?, 0))
            }
            (Row::Split { file, right, .. }, 1) => {
                let f = &self.model.files[file];
                Some((f, f.line(right?)?, 1))
            }
            (Row::Inline { file, item }, 2) => {
                let f = &self.model.files[file];
                let line = f.line(item)?;
                Some((f, line, usize::from(line.kind != Kind::Removed)))
            }
            _ => None,
        }
    }

    /// `(row, char)` under `pos` in `pane`; above or below the rows snaps to the ends.
    fn text_pos_at(
        &self,
        ui: &Ui,
        origin: egui::Pos2,
        width: f32,
        numbers_w: f32,
        pane: usize,
        pos: egui::Pos2,
    ) -> (usize, usize) {
        let rows = self.layout.rows.len();
        let y = pos.y - origin.y;
        if rows == 0 || y < 0.0 {
            return (0, 0);
        }
        if y >= self.layout.total {
            return (rows - 1, usize::MAX);
        }
        let row = self
            .layout
            .ys
            .partition_point(|&top| top <= y)
            .saturating_sub(1);
        let Some((f, line, side)) = self.pane_line(row, pane) else {
            return (row, 0);
        };
        let Some(job) = line_job(f, line, side) else {
            return (row, 0);
        };
        let galley = ui.painter().layout_job(job);
        let text_x = self.pane_text_column(pane, origin.x, width, numbers_w).min - self.scroll_x;
        let col = galley
            .cursor_from_pos(vec2(pos.x - text_x, galley.size().y / 2.0))
            .index;
        (row, col.into())
    }

    fn handle_selection(
        &mut self,
        ui: &Ui,
        response: &egui::Response,
        origin: egui::Pos2,
        width: f32,
        numbers_w: f32,
        area: Rect,
    ) {
        let split = self.layout_key.is_some_and(|(split, _, _)| split);
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
        }
        let (pointer, pressed, shift) = ui.input(|i| {
            (
                i.pointer.interact_pos(),
                i.pointer.primary_pressed(),
                i.modifiers.shift,
            )
        });
        if pressed
            && response.is_pointer_button_down_on()
            && let Some(pos) = pointer
        {
            response.request_focus();
            let pane = if !split {
                2
            } else if pos.x < origin.x + (width / 2.0).floor() {
                0
            } else {
                1
            };
            let at = self.text_pos_at(ui, origin, width, numbers_w, pane, pos);
            self.selection = Some(match self.selection {
                Some(sel) if shift && sel.pane == pane => TextSelection { cursor: at, ..sel },
                _ => TextSelection {
                    pane,
                    anchor: at,
                    cursor: at,
                },
            });
        } else if response.dragged()
            && let (Some(pos), Some(sel)) = (pointer, self.selection)
        {
            let at = self.text_pos_at(ui, origin, width, numbers_w, sel.pane, pos);
            self.selection = Some(TextSelection { cursor: at, ..sel });
            // Dragging past the top/bottom edge keeps scrolling, like the editor.
            let dy = if pos.y < area.top() {
                area.top() - pos.y
            } else if pos.y > area.bottom() {
                area.bottom() - pos.y
            } else {
                0.0
            };
            if dy != 0.0 {
                ui.scroll_with_delta_animation(
                    vec2(0.0, dy.clamp(-40.0, 40.0)),
                    egui::style::ScrollAnimation::none(),
                );
                ui.ctx().request_repaint();
            }
        }

        if let Some(sel) = self.selection
            && (response.double_clicked() || response.triple_clicked())
            && let Some((_, line, _)) = self.pane_line(sel.cursor.0, sel.pane)
        {
            let row = sel.cursor.0;
            let (from, to) = if response.triple_clicked() {
                (0, line.text.chars().count())
            } else {
                word_at(&line.text, sel.cursor.1)
            };
            self.selection = Some(TextSelection {
                pane: sel.pane,
                anchor: (row, from),
                cursor: (row, to),
            });
        }

        if response.has_focus() {
            let (copy, select_all) = ui.input_mut(|i| {
                (
                    i.events.iter().any(|e| matches!(e, egui::Event::Copy)),
                    i.consume_key(egui::Modifiers::COMMAND, egui::Key::A),
                )
            });
            if select_all && !self.layout.rows.is_empty() {
                let pane = self
                    .selection
                    .map_or(if split { 1 } else { 2 }, |sel| sel.pane);
                self.selection = Some(TextSelection {
                    pane,
                    anchor: (0, 0),
                    cursor: (self.layout.rows.len() - 1, usize::MAX),
                });
            }
            if copy && let Some(text) = self.selected_text() {
                ui.ctx().copy_text(text);
            }
        }
    }

    /// The selected text, one line per row of the pane that has a line.
    fn selected_text(&self) -> Option<String> {
        let sel = self.selection?;
        let (start, end) = sel.sorted();
        if start == end {
            return None;
        }
        let mut lines = Vec::new();
        for row in start.0..=end.0.min(self.layout.rows.len().saturating_sub(1)) {
            let Some((_, line, _)) = self.pane_line(row, sel.pane) else {
                continue;
            };
            let from = if row == start.0 { start.1 } else { 0 };
            let to = if row == end.0 { end.1 } else { usize::MAX };
            lines.push(
                line.text
                    .chars()
                    .skip(from)
                    .take(to.saturating_sub(from))
                    .collect::<String>(),
            );
        }
        Some(lines.join("\n"))
    }

    /// The changed-lines block a row shows, if any.
    fn row_block(&self, row: Row) -> Option<(usize, usize)> {
        let (file, items) = match row {
            Row::Split { file, left, right } => (file, [left, right]),
            Row::Inline { file, item } => (file, [Some(item), None]),
            _ => return None,
        };
        let f = &self.model.files[file];
        items
            .into_iter()
            .flatten()
            .find_map(|item| f.block_of.get(item).copied().flatten())
            .map(|block| (file, block))
    }

    /// Buttons for the block under the pointer, over the top right of its first visible row.
    fn block_buttons(
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
    fn file_header(
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

    fn ruler(&mut self, ui: &mut Ui, rect: Rect) {
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

fn row_file(row: Row) -> Option<usize> {
    match row {
        Row::Commit => None,
        Row::File(file)
        | Row::Note(file)
        | Row::Gap { file, .. }
        | Row::Fold { file, .. }
        | Row::Split { file, .. }
        | Row::Inline { file, .. } => Some(file),
    }
}

/// Char range of the word (or run of spaces/punctuation) at char `col`, for double-click.
fn word_at(text: &str, col: usize) -> (usize, usize) {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return (0, 0);
    }
    let class = |c: char| {
        if c.is_alphanumeric() || c == '_' {
            0
        } else if c.is_whitespace() {
            1
        } else {
            2
        }
    };
    let at = col.min(chars.len() - 1);
    let kind = class(chars[at]);
    let from = chars[..at]
        .iter()
        .rposition(|&c| class(c) != kind)
        .map_or(0, |i| i + 1);
    let to = chars[at..]
        .iter()
        .position(|&c| class(c) != kind)
        .map_or(chars.len(), |i| at + i);
    (from, to)
}

/// Row tint, line numbers and the +/− sign for one pane of a line row.
fn paint_line_chrome(
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
fn gutter_click(
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
fn paint_hatch(painter: &egui::Painter, rect: Rect) {
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
fn paint_band(painter: &egui::Painter, rect: Rect, label: &str, detail: &str, hovered: bool) {
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

fn paint_commit(ui: &mut Ui, rect: Rect, commit: &CommitInfo) {
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

fn commit_height(commit: &CommitInfo) -> f32 {
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
fn line_job(file: &DiffFile, line: &Line, side: usize) -> Option<LayoutJob> {
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

fn segment(ui: &mut Ui, label: &str, selected: bool, enabled: bool, hint: &str) -> egui::Response {
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

fn toolbar_icon(ui: &mut Ui, icon: &str, hint: &str, enabled: bool) -> egui::Response {
    ui.add_enabled(
        enabled,
        egui::Button::new(icon_glyph_rich(icon, FS_SMALL, c_text_muted()))
            .frame(false)
            .min_size(vec2(24.0, 24.0)),
    )
    .on_hover_text(hint)
}

fn build_rows(
    model: &DiffModel,
    split: bool,
    collapse: bool,
    expanded: &HashSet<(usize, usize)>,
    collapsed_files: &HashSet<usize>,
) -> RowLayout {
    let line_h = line_height();
    let mut out = RowLayout::default();
    let push = |out: &mut RowLayout, row: Row, h: f32| {
        out.rows.push(row);
        out.ys.push(out.total);
        out.total += h;
    };
    if let Some(commit) = &model.commit {
        push(&mut out, Row::Commit, commit_height(commit));
    }
    let headers = model.files.len() > 1 || model.commit.is_some();
    for (f, file) in model.files.iter().enumerate() {
        if headers {
            push(&mut out, Row::File(f), FILE_HEADER_H);
            if collapsed_files.contains(&f) {
                continue;
            }
        }
        if file.binary || file.truncated && file.items.is_empty() || file.items.is_empty() {
            push(&mut out, Row::Note(f), BAND_H);
            continue;
        }
        // New and deleted files have nothing to compare against: one pane reads better.
        let split = split && !matches!(file.status(), 'A' | 'D');
        let line_row = |item: usize| {
            if split {
                Row::Split {
                    file: f,
                    left: Some(item),
                    right: Some(item),
                }
            } else {
                Row::Inline { file: f, item }
            }
        };
        let n = file.items.len();
        let is_gap = |i: usize| matches!(file.items.get(i), Some(Item::Gap { .. }));
        let mut i = 0;
        while i < n {
            match file.kind(i) {
                None => {
                    push(&mut out, Row::Gap { file: f, item: i }, BAND_H);
                    i += 1;
                }
                Some(Kind::Context) => {
                    let start = i;
                    while i < n && file.kind(i) == Some(Kind::Context) {
                        i += 1;
                    }
                    let lead = if start == 0 || is_gap(start - 1) {
                        0
                    } else {
                        CONTEXT
                    };
                    let trail = if i == n || is_gap(i) { 0 } else { CONTEXT };
                    let fold_start = (start + lead).min(i);
                    let fold_end = i.saturating_sub(trail).max(fold_start);
                    out.foldable |= fold_end - fold_start >= MIN_FOLD;
                    let fold = collapse
                        && fold_end - fold_start >= MIN_FOLD
                        && !expanded.contains(&(f, fold_start));
                    if fold {
                        for item in start..fold_start {
                            push(&mut out, line_row(item), line_h);
                        }
                        push(
                            &mut out,
                            Row::Fold {
                                file: f,
                                start: fold_start,
                                end: fold_end,
                            },
                            BAND_H,
                        );
                        for item in fold_end..i {
                            push(&mut out, line_row(item), line_h);
                        }
                    } else {
                        for item in start..i {
                            push(&mut out, line_row(item), line_h);
                        }
                    }
                }
                Some(_) => {
                    let start = i;
                    while i < n && file.kind(i) == Some(Kind::Removed) {
                        i += 1;
                    }
                    let mid = i;
                    while i < n && file.kind(i) == Some(Kind::Added) {
                        i += 1;
                    }
                    let change_y = out.total;
                    if split {
                        for k in 0..(mid - start).max(i - mid) {
                            push(
                                &mut out,
                                Row::Split {
                                    file: f,
                                    left: (start + k < mid).then_some(start + k),
                                    right: (mid + k < i).then_some(mid + k),
                                },
                                line_h,
                            );
                        }
                    } else {
                        for item in start..i {
                            push(&mut out, Row::Inline { file: f, item }, line_h);
                        }
                    }
                    out.changes.push(Change {
                        y: change_y,
                        h: out.total - change_y,
                        removed: mid > start,
                        added: i > mid,
                    });
                }
            }
        }
    }
    out
}

fn parse(text: &str) -> DiffModel {
    let mut model = DiffModel::default();
    let mut lines = text.lines().peekable();
    if text.starts_with("commit ") {
        let mut commit = CommitInfo::default();
        let mut message = Vec::new();
        while let Some(line) = lines.next_if(|l| !l.starts_with("diff --git ")) {
            if let Some(hash) = line.strip_prefix("commit ") {
                commit.hash = hash.trim().to_owned();
            } else if let Some(author) = line.strip_prefix("Author:") {
                commit.author = author.trim().to_owned();
            } else if let Some(date) = line.strip_prefix("Date:") {
                commit.date = date.trim().to_owned();
            } else {
                message.push(line.strip_prefix("    ").unwrap_or(line));
            }
        }
        commit.message = message.join("\n").trim().to_owned();
        model.commit = Some(commit);
    }

    let mut file: Option<DiffFile> = None;
    let (mut old_left, mut new_left) = (0usize, 0usize);
    let (mut old_no, mut new_no) = (0usize, 0usize);
    let mut old_end: Option<usize> = None;
    for line in lines {
        if old_left > 0 || new_left > 0 {
            let (kind, content) = match line.as_bytes().first() {
                Some(b'-') => (Kind::Removed, &line[1..]),
                Some(b'+') => (Kind::Added, &line[1..]),
                Some(b' ') => (Kind::Context, &line[1..]),
                None => (Kind::Context, ""),
                Some(b'\\') => continue,
                _ => {
                    // A truncated or malformed hunk: stop consuming lines as its body.
                    old_left = 0;
                    new_left = 0;
                    (Kind::Context, "")
                }
            };
            if old_left > 0 || new_left > 0 {
                let f = file.get_or_insert_with(DiffFile::default);
                let (o, n) = match kind {
                    Kind::Removed => (Some(old_no), None),
                    Kind::Added => (None, Some(new_no)),
                    Kind::Context => (Some(old_no), Some(new_no)),
                };
                if o.is_some() {
                    old_no += 1;
                    old_left = old_left.saturating_sub(1);
                }
                if n.is_some() {
                    new_no += 1;
                    new_left = new_left.saturating_sub(1);
                }
                f.items.push(Item::Line(Line {
                    kind,
                    old_no: o,
                    new_no: n,
                    text: content.trim_end_matches('\r').to_owned(),
                    words: Vec::new(),
                    offset: [0, 0],
                }));
                old_end = Some(old_no);
                continue;
            }
        }
        if let Some(paths) = line.strip_prefix("diff --git ") {
            model.files.extend(file.take());
            let mut f = DiffFile::default();
            if let Some((a, b)) = paths.split_once(" b/") {
                f.old_path = Some(a.strip_prefix("a/").unwrap_or(a).to_owned());
                f.new_path = Some(b.to_owned());
            }
            file = Some(f);
            old_end = None;
        } else if let Some(path) = line.strip_prefix("--- ") {
            if file.as_ref().is_none_or(|f| !f.items.is_empty()) {
                model.files.extend(file.take());
                old_end = None;
            }
            file.get_or_insert_with(DiffFile::default).old_path = patch_path(path, "a/");
        } else if let Some(path) = line.strip_prefix("+++ ") {
            file.get_or_insert_with(DiffFile::default).new_path = patch_path(path, "b/");
        } else if line.starts_with("@@") {
            let Some((old_start, old_len, new_start, new_len, heading)) = hunk_header(line) else {
                continue;
            };
            let f = file.get_or_insert_with(DiffFile::default);
            let hidden = match old_end {
                Some(end) => old_start.saturating_sub(end),
                None => old_start.saturating_sub(1),
            };
            if hidden > 0 {
                f.items.push(Item::Gap {
                    heading: heading.to_owned(),
                    hidden: Some(hidden),
                });
            }
            (old_no, new_no) = (old_start.max(1), new_start.max(1));
            (old_left, new_left) = (old_len, new_len);
        } else if line.starts_with("Binary files") || line.starts_with("GIT binary patch") {
            file.get_or_insert_with(DiffFile::default).binary = true;
        } else if line.starts_with("new file mode") {
            if let Some(f) = file.as_mut() {
                f.old_path = None;
            }
        } else if line.starts_with("deleted file mode") {
            if let Some(f) = file.as_mut() {
                f.new_path = None;
            }
        } else if let Some(path) = line.strip_prefix("rename from ") {
            file.get_or_insert_with(DiffFile::default).old_path = Some(path.to_owned());
        } else if let Some(path) = line.strip_prefix("rename to ") {
            file.get_or_insert_with(DiffFile::default).new_path = Some(path.to_owned());
        } else if line.starts_with("… [truncated]") {
            file.get_or_insert_with(DiffFile::default).truncated = true;
        }
    }
    model.files.extend(file);
    for file in &mut model.files {
        finish_file(file);
    }
    model
}

fn patch_path(path: &str, prefix: &str) -> Option<String> {
    let path = path.trim_end();
    (path != "/dev/null").then(|| path.strip_prefix(prefix).unwrap_or(path).to_owned())
}

/// `@@ -a,b +c,d @@ heading` → (a, b, c, d, heading); omitted lengths default to 1.
fn hunk_header(line: &str) -> Option<(usize, usize, usize, usize, &str)> {
    let rest = line.strip_prefix("@@ ")?;
    let (ranges, heading) = rest.split_once(" @@").unwrap_or((rest, ""));
    let mut parts = ranges.split_whitespace();
    let range = |part: &str| -> Option<(usize, usize)> {
        let (start, len) = part.split_once(',').unwrap_or((part, "1"));
        Some((start.parse().ok()?, len.parse().ok()?))
    };
    let (old_start, old_len) = range(parts.next()?.strip_prefix('-')?)?;
    let (new_start, new_len) = range(parts.next()?.strip_prefix('+')?)?;
    Some((old_start, old_len, new_start, new_len, heading.trim()))
}

/// Pair changed lines for word highlights, then build each side's highlight text.
fn finish_file(file: &mut DiffFile) {
    let n = file.items.len();
    file.block_of = vec![None; n];
    let mut i = 0;
    while i < n {
        if file.kind(i).is_none_or(|kind| kind == Kind::Context) {
            i += 1;
            continue;
        }
        let start = i;
        while file.kind(i).is_some_and(|kind| kind != Kind::Context) {
            file.block_of[i] = Some(file.blocks.len());
            i += 1;
        }
        file.blocks.push(start..i);
    }
    let mut i = 0;
    while i < n {
        if file.kind(i) != Some(Kind::Removed) {
            i += 1;
            continue;
        }
        let start = i;
        while i < n && file.kind(i) == Some(Kind::Removed) {
            i += 1;
        }
        let mid = i;
        while i < n && file.kind(i) == Some(Kind::Added) {
            i += 1;
        }
        for k in 0..(mid - start).min(i - mid) {
            let (old, new) = (start + k, mid + k);
            let (Some(a), Some(b)) = (file.line(old), file.line(new)) else {
                continue;
            };
            let (old_words, new_words) = word_diff(&a.text, &b.text);
            if let Item::Line(line) = &mut file.items[old] {
                line.words = old_words;
            }
            if let Item::Line(line) = &mut file.items[new] {
                line.words = new_words;
            }
        }
    }

    let mut texts = [String::new(), String::new()];
    for item in &mut file.items {
        let Item::Line(line) = item else { continue };
        file.max_chars = file.max_chars.max(line.text.chars().count());
        match line.kind {
            Kind::Removed => file.removed += 1,
            Kind::Added => file.added += 1,
            Kind::Context => {}
        }
        for (side, text) in texts.iter_mut().enumerate() {
            let present = match line.kind {
                Kind::Context => true,
                Kind::Removed => side == 0,
                Kind::Added => side == 1,
            };
            if present {
                line.offset[side] = text.len();
                text.push_str(&line.text);
                text.push('\n');
            }
        }
    }
    file.side_text = texts;
}

fn tokens(text: &str) -> Vec<Range<usize>> {
    fn class(c: char) -> u8 {
        if c.is_alphanumeric() || c == '_' {
            0
        } else if c.is_whitespace() {
            1
        } else {
            2
        }
    }
    let mut out: Vec<Range<usize>> = Vec::new();
    let mut previous = None;
    for (index, c) in text.char_indices() {
        let class = class(c);
        let end = index + c.len_utf8();
        match out.last_mut() {
            Some(last) if previous == Some(class) && class != 2 => last.end = end,
            _ => out.push(index..end),
        }
        previous = Some(class);
    }
    out
}

/// Changed byte ranges of two similar lines (token LCS). Empty when the lines are too different
/// for word highlights to help.
fn word_diff(old: &str, new: &str) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    let (a, b) = (tokens(old), tokens(new));
    let (n, m) = (a.len(), b.len());
    if n == 0 || m == 0 || n * m > 40_000 {
        return (Vec::new(), Vec::new());
    }
    let width = m + 1;
    let mut lcs = vec![0u16; (n + 1) * width];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i * width + j] = if old[a[i].clone()] == new[b[j].clone()] {
                lcs[(i + 1) * width + j + 1] + 1
            } else {
                lcs[(i + 1) * width + j].max(lcs[i * width + j + 1])
            };
        }
    }
    let (mut keep_a, mut keep_b) = (vec![false; n], vec![false; m]);
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if old[a[i].clone()] == new[b[j].clone()] {
            keep_a[i] = true;
            keep_b[j] = true;
            i += 1;
            j += 1;
        } else if lcs[(i + 1) * width + j] >= lcs[i * width + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    let old_ranges = changed_ranges(old, &a, &keep_a);
    let new_ranges = changed_ranges(new, &b, &keep_b);
    let changed: usize = old_ranges.iter().chain(&new_ranges).map(|r| r.len()).sum();
    let total = old.trim().len() + new.trim().len();
    if changed as f32 > total as f32 * MAX_WORD_CHANGE {
        return (Vec::new(), Vec::new());
    }
    (old_ranges, new_ranges)
}

/// Merge unkept tokens into ranges, bridging single whitespace gaps between changed words.
fn changed_ranges(text: &str, tokens: &[Range<usize>], keep: &[bool]) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    for (token, kept) in tokens.iter().zip(keep) {
        if *kept {
            continue;
        }
        match out.last_mut() {
            Some(last) if text[last.end..token.start].trim().is_empty() => last.end = token.end,
            _ => out.push(token.clone()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_know_both_sides_and_where_they_start() {
        let model = parse(
            "diff --git a/f.txt b/f.txt\n--- a/f.txt\n+++ b/f.txt\n@@ -1,6 +1,7 @@\n a\n-b\n+B\n+B2\n c\n-d\n e\n f\n+g\n",
        );
        let file = &model.files[0];
        assert_eq!(file.blocks.len(), 3);
        let modified = file.block(0);
        assert_eq!((modified.old_start, modified.new_start), (2, 2));
        assert_eq!(modified.old_lines, ["b"]);
        assert_eq!(modified.new_lines, ["B", "B2"]);
        // A deletion has no new lines: it would be reinserted after new line 4 ("c").
        let deleted = file.block(1);
        assert_eq!((deleted.old_start, deleted.new_start), (4, 5));
        assert_eq!(deleted.old_lines, ["d"]);
        assert!(deleted.new_lines.is_empty());
        let appended = file.block(2);
        assert_eq!((appended.old_start, appended.new_start), (7, 7));
        assert_eq!(appended.new_lines, ["g"]);
    }

    fn lines(file: &DiffFile) -> Vec<(Kind, Option<usize>, Option<usize>, &str)> {
        file.items
            .iter()
            .filter_map(|item| match item {
                Item::Line(l) => Some((l.kind, l.old_no, l.new_no, l.text.as_str())),
                Item::Gap { .. } => None,
            })
            .collect()
    }

    #[test]
    fn parses_git_patch_with_line_numbers_and_gaps() {
        let model = parse(
            "diff --git a/src/a.rs b/src/a.rs\nindex 1..2 100644\n--- a/src/a.rs\n+++ b/src/a.rs\n\
             @@ -10,3 +10,3 @@ fn main() {\n ctx\n-old\n+new\n tail\n@@ -40,2 +40,3 @@\n x\n+y\n z\n",
        );
        assert_eq!(model.files.len(), 1);
        let file = &model.files[0];
        assert_eq!(file.path(), "src/a.rs");
        assert_eq!(file.status(), 'M');
        assert_eq!((file.added, file.removed), (2, 1));
        assert!(matches!(
            &file.items[0],
            Item::Gap { hidden: Some(9), heading } if heading == "fn main() {"
        ));
        assert!(matches!(
            file.items[5],
            Item::Gap {
                hidden: Some(27),
                ..
            }
        ));
        let l = lines(file);
        assert_eq!(l[1], (Kind::Removed, Some(11), None, "old"));
        assert_eq!(l[2], (Kind::Added, None, Some(11), "new"));
        assert_eq!(l[5], (Kind::Added, None, Some(41), "y"));
    }

    #[test]
    fn removed_line_that_looks_like_a_file_header_stays_in_the_hunk() {
        let model = parse("--- a/q.sql\n+++ b/q.sql\n@@ -1,2 +1,1 @@\n--- comment\n keep\n");
        assert_eq!(model.files.len(), 1);
        assert_eq!(
            lines(&model.files[0])[0],
            (Kind::Removed, Some(1), None, "-- comment")
        );
    }

    #[test]
    fn parses_commit_header_and_new_and_deleted_files() {
        let model = parse(
            "commit abc123\nAuthor: Ana <a@b>\nDate:   Mon\n\n    Add things\n\n    Body\n\n\
             diff --git a/new.txt b/new.txt\nnew file mode 100644\n--- /dev/null\n+++ b/new.txt\n\
             @@ -0,0 +1 @@\n+hi\ndiff --git a/gone.txt b/gone.txt\ndeleted file mode 100644\n\
             --- a/gone.txt\n+++ /dev/null\n@@ -1 +0,0 @@\n-bye\n\
             diff --git a/img.png b/img.png\nBinary files a/img.png and b/img.png differ\n",
        );
        let commit = model.commit.as_ref().unwrap();
        assert_eq!(commit.hash, "abc123");
        assert_eq!(commit.message, "Add things\n\nBody");
        let statuses: Vec<char> = model.files.iter().map(DiffFile::status).collect();
        assert_eq!(statuses, ['A', 'D', 'M']);
        assert!(model.files[2].binary);
        assert_eq!(
            lines(&model.files[0])[0],
            (Kind::Added, None, Some(1), "hi")
        );
    }

    #[test]
    fn double_click_word_spans_one_character_class() {
        let text = "let foo_bar = x.len();";
        assert_eq!(word_at(text, 5), (4, 11));
        assert_eq!(word_at(text, 3), (3, 4));
        assert_eq!(word_at(text, 15), (15, 16));
        assert_eq!(word_at(text, 99), (19, 22));
        assert_eq!(word_at("", 0), (0, 0));
    }

    #[test]
    fn selected_text_stays_in_one_pane() {
        let mut slot = None;
        DiffView::sync(
            &mut slot,
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,3 +1,3 @@\n keep one\n-old two\n+new two\n keep three\n",
        );
        let view = slot.as_mut().unwrap();
        view.ensure_layout(true);
        let first = (0..view.layout.rows.len())
            .find(|&row| view.pane_line(row, 1).is_some())
            .unwrap();
        view.selection = Some(TextSelection {
            pane: 1,
            anchor: (first, 5),
            cursor: (first + 2, 4),
        });
        assert_eq!(view.selected_text().as_deref(), Some("one\nnew two\nkeep"));
        view.selection = Some(TextSelection {
            pane: 0,
            anchor: (first + 1, 0),
            cursor: (first + 1, usize::MAX),
        });
        assert_eq!(view.selected_text().as_deref(), Some("old two"));
    }

    #[test]
    fn word_diff_marks_only_the_changed_token() {
        let (old, new) = word_diff("let total = sum(values);", "let total = mean(values);");
        assert_eq!(old, vec![12..15]);
        assert_eq!(new, vec![12..16]);
        let (old, new) = word_diff("alpha beta", "gamma delta");
        assert!(old.is_empty() && new.is_empty());
    }

    #[test]
    fn split_rows_pair_changes_and_fold_long_unchanged_runs() {
        let mut text = String::from("--- a/f\n+++ b/f\n@@ -1,22 +1,22 @@\n");
        for i in 0..10 {
            text.push_str(&format!(" a{i}\n"));
        }
        text.push_str("-old1\n-old2\n+new1\n");
        for i in 0..10 {
            text.push_str(&format!(" b{i}\n"));
        }
        text.push_str("+tail\n");
        let model = parse(&text);
        let layout = build_rows(&model, true, true, &HashSet::new(), &HashSet::new());
        let folds: Vec<_> = layout
            .rows
            .iter()
            .filter_map(|r| match r {
                Row::Fold { start, end, .. } => Some((*start, *end)),
                _ => None,
            })
            .collect();
        // Leading run keeps only its trailing context; the middle run keeps both edges.
        assert_eq!(folds, [(0, 7), (16, 20)]);
        assert!(layout.rows.contains(&Row::Split {
            file: 0,
            left: Some(10),
            right: Some(12)
        }));
        assert!(layout.rows.contains(&Row::Split {
            file: 0,
            left: Some(11),
            right: None
        }));
        assert_eq!(layout.changes.len(), 2);
        let expanded = HashSet::from([(0, 0)]);
        let layout = build_rows(&model, true, true, &expanded, &HashSet::new());
        assert_eq!(
            layout
                .rows
                .iter()
                .filter(|r| matches!(r, Row::Fold { .. }))
                .count(),
            1
        );
    }
}
