//! VS Code–style diff editor for unified patches (git file/commit diffs, unsaved changes):
//! side-by-side or inline panes with line numbers, syntax colors, word-level change highlights,
//! collapsible unchanged regions, change navigation (F7 / Shift+F7) and an overview ruler.
//!
//! Rows have fixed heights and only the visible ones are laid out, so whole-file diffs stay
//! cheap. Text selection works like the editor's: it stays inside one pane, is painted as one
//! continuous shape and supports double/triple click, Shift+click, select all and copy.

mod paint;
mod parse;
mod rows;
mod selection;
#[cfg(test)]
mod tests;

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

use paint::{commit_height, line_job};
pub(crate) use paint::{segment, toolbar_icon};
use parse::parse;
pub(crate) use parse::word_diff;
use rows::{build_rows, row_file};

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
}
