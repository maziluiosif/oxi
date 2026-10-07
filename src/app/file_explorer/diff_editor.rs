//! The editor's diff mode, VS Code style: a document tab shows its changes against a base
//! (the index, `HEAD`, a branch or agent turn, or the saved file) side by side or inline. The
//! document itself stays the live, editable editor (syntax colors, find, navigation, undo); the
//! base is a read-only pane beside it, aligned by blank rows. Hovering a change offers its
//! Stage / Discard (or Unstage) actions.
//!
//! The base is read from Git off the UI thread and re-read whenever Git reports a change, so
//! staging a change makes it disappear; the changes themselves are recomputed from the live
//! buffer on every edit.

mod decor;
mod pane;
#[cfg(test)]
mod tests;

use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError};

use eframe::egui::text::LayoutJob;
use eframe::egui::{
    self, Align, FontId, Layout, Rect, Sense, Stroke, TextFormat, Ui, UiBuilder, pos2, vec2,
};

use crate::git::{GitLineChange, GitLineKind};
use crate::theme::*;
use crate::ui::diff_view::{segment, toolbar_icon};

use super::super::OxiApp;
pub(crate) use decor::{DiffDecor, PaneRows, Side, Underlay, ZoneText, apply_gaps, ruler};
pub(crate) use pane::PaneCache;

const TOOLBAR_H: f32 = 32.0;
/// Width of the overview ruler that replaces the minimap in diff mode.
pub(super) const RULER_W: f32 = 12.0;
/// Below this width two panes are too cramped to read; fall back to the inline layout.
const MIN_SPLIT_WIDTH: f32 = 640.0;

/// What a document's diff mode compares the document with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DiffSource {
    /// Unstaged changes: the index against the editable buffer. Stage / Discard per change.
    WorkTree,
    /// Staged changes: `HEAD` against the index, read-only. Unstage per change.
    Staged,
    /// Changes since a branch's merge base or an agent turn (`turn:<tree>`), against the
    /// editable buffer. Discard per change.
    Compare {
        base: String,
        old_path: Option<String>,
    },
    /// Unsaved edits: the saved file against the buffer. Discard per change.
    Unsaved,
}

impl DiffSource {
    /// Shown after the file name in the tab: `stats.py (Working Tree)`.
    pub(crate) fn tab_label(&self) -> String {
        match self {
            Self::WorkTree => "Working Tree".to_owned(),
            Self::Staged => "Staged".to_owned(),
            Self::Compare { base, .. } => {
                format!("vs {}", crate::git::checkpoint::base_label(base))
            }
            Self::Unsaved => "Unsaved Changes".to_owned(),
        }
    }

    /// Where the left side comes from, for the toolbar.
    fn base_label(&self) -> String {
        match self {
            Self::WorkTree => "Index vs Working Tree".to_owned(),
            Self::Staged => "HEAD vs Index (read-only)".to_owned(),
            Self::Compare { base, .. } => {
                format!("Since {}", crate::git::checkpoint::base_label(base))
            }
            Self::Unsaved => "Saved vs Unsaved".to_owned(),
        }
    }

    pub(crate) fn editable(&self) -> bool {
        !matches!(self, Self::Staged)
    }

    fn git_base(&self) -> Option<crate::git::DiffBase> {
        match self {
            Self::WorkTree => Some(crate::git::DiffBase::Index),
            Self::Staged => Some(crate::git::DiffBase::Staged),
            Self::Compare { base, old_path } => Some(crate::git::DiffBase::Compare {
                base: base.clone(),
                old_path: old_path.clone(),
            }),
            Self::Unsaved => None,
        }
    }

    fn actions(&self) -> &'static [BlockAction] {
        match self {
            Self::WorkTree => &[BlockAction::Stage, BlockAction::Discard],
            Self::Staged => &[BlockAction::Unstage],
            Self::Compare { .. } | Self::Unsaved => &[BlockAction::Discard],
        }
    }
}

/// Per-change buttons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BlockAction {
    /// Write the change into the index.
    Stage,
    /// Take the change back out of the index.
    Unstage,
    /// Put the base's lines back in the buffer (one undoable edit).
    Discard,
}

impl BlockAction {
    fn label(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Self::Stage => (ICON_PLUS, "Stage", "Stage this change"),
            Self::Unstage => (ICON_CLOSE, "Unstage", "Unstage this change"),
            Self::Discard => (
                ICON_UNDO,
                "Discard",
                "Discard this change (undo with Cmd/Ctrl+Z)",
            ),
        }
    }
}

type Texts = Result<(String, Option<String>), String>;

/// A document's diff mode: the base text, the changes against the buffer and the view state.
pub(crate) struct DocumentDiff {
    pub(crate) source: DiffSource,
    /// Repository-relative path, for Git.
    pub(crate) relative: String,
    /// The base (left) text with `\n` line endings, once read.
    base: Option<Arc<String>>,
    /// The index text of a staged diff (its right side).
    staged: Option<Arc<String>>,
    base_starts: Vec<usize>,
    /// Bumped whenever `base` or `staged` change.
    version: u64,
    loading: Option<Receiver<Texts>>,
    /// `git_ui.epoch` the base was last requested at: a newer Git snapshot re-reads it.
    loaded_epoch: Option<u64>,
    /// The saved content an unsaved-changes base was taken from.
    saved_snapshot: Option<String>,
    error: Option<String>,
    /// Changes of the text with this (hash, version), and the content revision they match.
    decor: Option<((u64, u64), Arc<DiffDecor>)>,
    decor_revision: Option<(u64, u64)>,
    /// Whole-text syntax colors of the base and staged texts.
    base_job: Option<(u64, LayoutJob)>,
    staged_job: Option<(u64, LayoutJob)>,
    /// Tree-sitter parses of the base and staged texts, for the editor's own colors.
    base_syntax: Option<crate::theme::EditorSyntaxState>,
    staged_syntax: Option<crate::theme::EditorSyntaxState>,
    pub(super) left: PaneCache,
    pub(super) right: PaneCache,
    /// Effective layout of the last frame (narrow editors fall back to inline).
    pub(super) inline: bool,
    /// Vertical scroll shared by the panes: the editor's offset as of this frame, or the
    /// read-only panes' own offset.
    pub(super) scroll_y: f32,
    /// A scroll offset for the editor to take on its next frame (wheel over the base pane).
    pub(super) scroll_request: Option<f32>,
    /// New-side geometry of the last frame, for hover actions and the ruler.
    pub(super) frame: DiffFrame,
    /// Bring the change at this 0-based line (or the first change) into view once loaded.
    reveal: Option<Option<usize>>,
}

pub(super) struct DiffFrame {
    /// Screen spans of the changes in the new-side pane, and its text column.
    pub new_spans: Vec<(f32, f32)>,
    pub new_clip: Rect,
    pub old_spans: Vec<(f32, f32)>,
    pub old_clip: Rect,
    /// Removed lines' old numbers and row centers (inline), for the gutter.
    pub zone_numbers: Vec<(usize, f32)>,
}

impl Default for DiffFrame {
    fn default() -> Self {
        Self {
            new_spans: Vec::new(),
            new_clip: Rect::NOTHING,
            old_spans: Vec::new(),
            old_clip: Rect::NOTHING,
            zone_numbers: Vec::new(),
        }
    }
}

impl DocumentDiff {
    pub(crate) fn new(source: DiffSource, relative: String, reveal: Option<usize>) -> Self {
        Self {
            source,
            relative,
            base: None,
            staged: None,
            base_starts: Vec::new(),
            version: 0,
            loading: None,
            loaded_epoch: None,
            saved_snapshot: None,
            error: None,
            decor: None,
            decor_revision: None,
            base_job: None,
            staged_job: None,
            base_syntax: None,
            staged_syntax: None,
            left: PaneCache::default(),
            right: PaneCache::default(),
            inline: false,
            scroll_y: 0.0,
            scroll_request: None,
            frame: DiffFrame::default(),
            reveal: Some(reveal),
        }
    }

    pub(crate) fn ready(&self) -> bool {
        self.base.is_some() && (self.source.editable() || self.staged.is_some())
    }

    /// Replace the texts; a no-op when nothing changed (Git refreshes re-read them often).
    fn set_texts(&mut self, base: String, staged: Option<String>) {
        let base = base.replace("\r\n", "\n");
        let staged = staged.map(|text| text.replace("\r\n", "\n"));
        if self.base.as_deref() == Some(&base) && self.staged.as_deref() == staged.as_ref() {
            return;
        }
        self.base_starts = decor::line_starts(&base);
        self.base = Some(Arc::new(base));
        self.staged = staged.map(Arc::new);
        self.version += 1;
        self.decor = None;
        self.decor_revision = None;
        self.base_job = None;
        self.staged_job = None;
    }

    /// The changes from the base to `text`, cached by the text's hash.
    pub(crate) fn decor_for(&mut self, text: &str) -> Option<Arc<DiffDecor>> {
        let base = self.base.as_ref()?;
        let key = (text_hash(text), self.version);
        if let Some((cached, decor)) = &self.decor
            && *cached == key
        {
            return Some(Arc::clone(decor));
        }
        let decor = Arc::new(DiffDecor::compute(base, text));
        self.decor = Some((key, Arc::clone(&decor)));
        self.decor_revision = None;
        Some(decor)
    }

    /// [`Self::decor_for`] the document's content at `revision`, hashing it only once.
    pub(crate) fn decor_at(&mut self, revision: u64, text: &str) -> Option<Arc<DiffDecor>> {
        if self.decor_revision == Some((revision, self.version))
            && let Some((_, decor)) = &self.decor
        {
            return Some(Arc::clone(decor));
        }
        let decor = self.decor_for(text)?;
        self.decor_revision = Some((revision, self.version));
        Some(decor)
    }

    /// Changes whenever the editor's gaps could change without its text changing.
    pub(crate) fn layout_key(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (self.version, self.inline, self.base.is_some()).hash(&mut hasher);
        hasher.finish() | 1
    }

    pub(crate) fn base_text(&self) -> Option<&str> {
        self.base.as_deref().map(String::as_str)
    }

    pub(crate) fn base_starts(&self) -> &[usize] {
        &self.base_starts
    }

    /// The base's syntax colors, highlighted in the background on first use.
    pub(crate) fn base_job(&mut self, extension: &str, ctx: &egui::Context) -> Option<&LayoutJob> {
        let base = self.base.as_ref()?;
        cached_job(
            &mut self.base_job,
            &mut self.base_syntax,
            self.version,
            base,
            extension,
            ctx,
        )
    }

    #[cfg(test)]
    pub(crate) fn frame_new_span(&self, index: usize) -> Option<(f32, f32)> {
        self.frame.new_spans.get(index).copied()
    }

    #[cfg(test)]
    pub(crate) fn frame_new_clip(&self) -> Rect {
        self.frame.new_clip
    }

    pub(crate) fn cached_base_job(&self) -> Option<&LayoutJob> {
        self.base_job.as_ref().map(|(_, job)| job)
    }

    fn staged_job(&mut self, extension: &str, ctx: &egui::Context) -> Option<&LayoutJob> {
        let staged = self.staged.as_ref()?;
        cached_job(
            &mut self.staged_job,
            &mut self.staged_syntax,
            self.version,
            staged,
            extension,
            ctx,
        )
    }
}

/// The whole text's colors: Tree-sitter like the editor where it knows the language, syntect
/// (in the background) otherwise.
fn cached_job<'a>(
    slot: &'a mut Option<(u64, LayoutJob)>,
    syntax: &mut Option<crate::theme::EditorSyntaxState>,
    version: u64,
    text: &str,
    extension: &str,
    ctx: &egui::Context,
) -> Option<&'a LayoutJob> {
    if slot.as_ref().is_none_or(|(cached, _)| *cached != version) {
        if extension.is_empty() {
            return None;
        }
        let job = match crate::theme::highlight_editor_code_with_revision(
            syntax,
            text,
            extension,
            code_font(),
            Some(version),
            None,
        ) {
            Some(_) if syntax.as_ref().is_some_and(|state| state.parse_pending()) => {
                // Still parsing in the background: plain text until it is done.
                ctx.request_repaint_after(std::time::Duration::from_millis(16));
                return None;
            }
            Some(job) => job,
            None => crate::theme::highlight_code_async(text, extension, code_font(), ctx)?,
        };
        *slot = Some((version, job));
    }
    slot.as_ref().map(|(_, job)| job)
}

fn text_hash(text: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

fn code_font() -> FontId {
    FontId::monospace(FS_SMALL)
}

/// Gutter markers for the new side, like Git's (added, modified, removed-above).
pub(crate) fn line_changes(decor: &DiffDecor, line_count: usize) -> Vec<GitLineChange> {
    let last_line = line_count.saturating_sub(1);
    let mut markers = Vec::new();
    for change in &decor.changes {
        if change.new_lines.is_empty() {
            markers.push(GitLineChange {
                line: change.new_start.min(last_line),
                kind: GitLineKind::Deleted,
            });
            continue;
        }
        let kind = if change.old_lines.is_empty() {
            GitLineKind::Added
        } else {
            GitLineKind::Modified
        };
        markers.extend(
            (change.new_start..change.new_start + change.new_lines.len())
                .map(|line| GitLineChange { line, kind }),
        );
    }
    markers.sort_by_key(|marker| marker.line);
    markers.dedup_by_key(|marker| marker.line);
    markers
}

impl OxiApp {
    /// Show `relative` (a workspace-relative path) as a diff in its editor tab, at the change
    /// on 0-based `line` (or the first change). Returns false when the file can't be edited as
    /// text (deleted, binary): callers fall back to the patch view.
    pub(crate) fn open_diff_editor(
        &mut self,
        relative: &str,
        source: DiffSource,
        line: Option<usize>,
    ) -> bool {
        let path = PathBuf::from(&self.active_workspace().root_path).join(relative);
        if !path.is_file() || super::MediaKind::from_path(&path).is_some() {
            return false;
        }
        let Ok(wanted) = std::fs::canonicalize(&path) else {
            return false;
        };
        self.open_editor_file_only(path);
        let Some(index) = self.conv.editor.active.filter(|&index| {
            self.conv.editor.documents[index].path == wanted
                && self.conv.editor.documents[index].media.is_none()
        }) else {
            return false;
        };
        let document = &mut self.conv.editor.documents[index];
        match document.diff.as_mut() {
            Some(diff) if diff.source == source => diff.reveal = Some(line),
            _ => document.diff = Some(DocumentDiff::new(source, relative.to_owned(), line)),
        }
        self.conv.editor.git_full_highlight_path = None;
        self.conv.editor.markdown_preview_active = false;
        self.conv.editor.focus_editor_next_frame = true;
        true
    }

    /// The file and source of the diff the editor shows, for highlighting it in Git lists.
    pub(crate) fn active_diff_target(&self) -> Option<(&str, &DiffSource)> {
        if self.conv.editor.diff_tab_active || self.conv.editor.markdown_preview_active {
            return None;
        }
        self.conv
            .editor
            .active_document()
            .and_then(|document| document.diff.as_ref())
            .map(|diff| (diff.relative.as_str(), &diff.source))
    }

    /// Diff mode for the open document `index` (tab menu, gutter markers).
    pub(crate) fn open_document_diff(&mut self, index: usize, source: DiffSource) {
        let root = PathBuf::from(&self.active_workspace().root_path);
        let Some(document) = self.conv.editor.documents.get(index) else {
            return;
        };
        let relative = document
            .path
            .strip_prefix(std::fs::canonicalize(&root).unwrap_or(root.clone()))
            .or_else(|_| document.path.strip_prefix(&root))
            .unwrap_or(&document.path)
            .to_string_lossy()
            .replace('\\', "/");
        let line = self
            .conv
            .editor
            .active
            .filter(|active| *active == index)
            .map(|_| {
                let document = &self.conv.editor.documents[index];
                let byte = super::editor_logic::char_index_to_byte(
                    &document.content,
                    self.conv.editor.navigation_cursor_char,
                );
                document.content[..byte].matches('\n').count()
            });
        let document = &mut self.conv.editor.documents[index];
        document.diff = Some(DocumentDiff::new(source, relative, line));
        self.conv.editor.active = Some(index);
        self.conv.editor.diff_tab_active = false;
        self.conv.editor.markdown_preview_active = false;
        self.conv.editor.focus_editor_next_frame = true;
    }

    pub(crate) fn close_document_diff(&mut self, index: usize) {
        if let Some(document) = self.conv.editor.documents.get_mut(index) {
            document.diff = None;
            self.conv.editor.focus_editor_next_frame = true;
        }
    }

    /// Where an editor marker click should compare: the Compare tab's base while it drives
    /// the gutter, otherwise the unstaged changes.
    pub(crate) fn gutter_diff_source(&self) -> DiffSource {
        if self.conv.git_ui.open
            && self.conv.git_ui.tab == crate::app::git_panel::GitTab::Compare
            && let Some(data) = self
                .conv
                .git_ui
                .compare
                .data
                .as_ref()
                .filter(|data| data.error.is_none())
        {
            return DiffSource::Compare {
                base: data.base.clone(),
                old_path: None,
            };
        }
        DiffSource::WorkTree
    }

    /// Read (or re-read after a Git change) the base of document `index`'s diff.
    fn poll_document_diff(&mut self, index: usize) {
        let epoch = self.conv.git_ui.epoch;
        let root = self.active_workspace().root_path.clone();
        let ctx = self.conv.git_ctx.clone();
        let Some(document) = self.conv.editor.documents.get_mut(index) else {
            return;
        };
        let saved = &document.saved_content;
        let Some(diff) = document.diff.as_mut() else {
            return;
        };
        let Some(git_base) = diff.source.git_base() else {
            if diff.saved_snapshot.as_ref() != Some(saved) {
                diff.saved_snapshot = Some(saved.clone());
                diff.set_texts(saved.clone(), None);
            }
            return;
        };
        if let Some(rx) = &diff.loading {
            match rx.try_recv() {
                Ok(Ok((base, staged))) => {
                    diff.set_texts(base, staged);
                    diff.error = None;
                }
                Ok(Err(error)) => diff.error = Some(error),
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    diff.error = Some("the Git reader stopped".to_owned());
                }
            }
            diff.loading = None;
        }
        if diff.loaded_epoch == Some(epoch) {
            return;
        }
        diff.loaded_epoch = Some(epoch);
        let relative = diff.relative.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(crate::git::diff_texts(&root, &relative, &git_base));
            ctx.request_repaint();
        });
        diff.loading = Some(rx);
    }

    /// The active document in diff mode: toolbar, then the panes.
    pub(super) fn render_diff_editor(&mut self, ui: &mut Ui) {
        let Some(index) = self.conv.editor.active else {
            return;
        };
        self.poll_document_diff(index);
        let full = ui.available_rect_before_wrap();
        let body = Rect::from_min_max(pos2(full.left(), full.top() + TOOLBAR_H), full.max);
        let split_possible = body.width() - RULER_W >= MIN_SPLIT_WIDTH;
        let inline = self.conv.editor.diff_inline || !split_possible;
        let extension =
            super::support::language_for_path(&self.conv.editor.documents[index].path).to_owned();
        let ctx = ui.ctx().clone();

        // Changes of what the new side shows.
        let decor = {
            let document = &mut self.conv.editor.documents[index];
            let revision = document.content_revision;
            let Some(diff) = document.diff.as_mut() else {
                return;
            };
            if diff.inline != inline {
                diff.inline = inline;
            }
            if !diff.ready() {
                None
            } else if diff.source.editable() {
                diff.decor_at(revision, &document.content)
            } else {
                let staged = diff.staged.clone().unwrap_or_default();
                diff.decor_for(&staged)
            }
        };

        let toolbar_rect = Rect::from_min_size(full.min, vec2(full.width(), TOOLBAR_H));
        let mut toolbar = ui.new_child(
            UiBuilder::new()
                .id_salt("diff_editor_toolbar")
                .max_rect(toolbar_rect.shrink2(vec2(10.0, 0.0)))
                .layout(Layout::left_to_right(Align::Center)),
        );
        toolbar.set_clip_rect(toolbar_rect.intersect(ui.clip_rect()));
        let toolbar_action =
            self.diff_toolbar(&mut toolbar, index, decor.as_deref(), split_possible);
        ui.painter().hline(
            full.x_range(),
            toolbar_rect.bottom() - 0.5,
            Stroke::new(1.0, c_border_subtle()),
        );
        ui.allocate_rect(full, Sense::hover());
        match toolbar_action {
            Some(ToolbarAction::Close) => {
                self.close_document_diff(index);
                return;
            }
            Some(ToolbarAction::Jump(forward)) => self.jump_to_change(index, forward),
            None => {}
        }
        if ui.input(|i| i.key_pressed(egui::Key::F7)) {
            let forward = !ui.input(|i| i.modifiers.shift);
            self.jump_to_change(index, forward);
        }

        let Some(decor) = decor else {
            let document = &self.conv.editor.documents[index];
            let message = document
                .diff
                .as_ref()
                .and_then(|diff| diff.error.clone())
                .map_or_else(
                    || "Loading diff…".to_owned(),
                    |error| format!("Cannot compare with Git: {error}"),
                );
            ui.painter().text(
                body.min + vec2(16.0, 16.0),
                egui::Align2::LEFT_TOP,
                message,
                FontId::proportional(FS_SMALL),
                c_text_muted(),
            );
            return;
        };
        self.reveal_pending_change(index, &decor, body.height());

        let editable = self.conv.editor.documents[index]
            .diff
            .as_ref()
            .is_some_and(|diff| diff.source.editable());
        let (old_rect, new_rect) = if inline {
            (None, body)
        } else {
            let half = ((body.width() - RULER_W) / 2.0).floor();
            (
                Some(Rect::from_min_size(body.min, vec2(half, body.height()))),
                Rect::from_min_max(pos2(body.left() + half + 1.0, body.top()), body.max),
            )
        };

        if editable {
            ui.scope_builder(UiBuilder::new().max_rect(new_rect), |ui| {
                self.render_editor_body(ui);
            });
        } else {
            self.render_staged_pane(ui, index, new_rect, &decor, &extension, inline);
        }
        if let Some(old_rect) = old_rect {
            let document = &mut self.conv.editor.documents[index];
            let Some(diff) = document.diff.as_mut() else {
                return;
            };
            let base = diff.base.clone().unwrap_or_default();
            let version = diff.version;
            diff.base_job(&extension, &ctx);
            let scroll_y = diff.scroll_y;
            let output = pane::readonly_pane(
                ui,
                old_rect,
                pane::PaneInput {
                    id: ui.id().with(("diff_base_pane", index)),
                    text: &base,
                    text_version: version,
                    job: diff.base_job.as_ref().map(|(_, job)| job),
                    side: Side::Old,
                    inline: false,
                    decor: &decor,
                    zones: None,
                    scroll_y,
                },
                &mut diff.left,
            );
            diff.frame.old_spans = output.spans;
            diff.frame.old_clip = output.clip;
            if output.wheel_y != 0.0 {
                let max = (output.content_h - output.viewport_h).max(0.0);
                let target = (scroll_y - output.wheel_y).clamp(0.0, max);
                if editable {
                    diff.scroll_request = Some(target);
                } else {
                    diff.scroll_y = target;
                }
                ui.ctx().request_repaint();
            }
            ui.painter().vline(
                old_rect.right() + 0.5,
                body.y_range(),
                Stroke::new(1.0, c_border()),
            );
        } else if let Some(diff) = self.conv.editor.documents[index].diff.as_mut() {
            diff.frame.old_spans.clear();
        }

        if let Some(action) = self.diff_block_buttons(ui, index, &decor) {
            self.apply_diff_block(ui.ctx(), index, &decor, action);
        }
    }

    /// Both sides of a staged diff are Git's: the index text is shown read-only.
    fn render_staged_pane(
        &mut self,
        ui: &mut Ui,
        index: usize,
        rect: Rect,
        decor: &DiffDecor,
        extension: &str,
        inline: bool,
    ) {
        let ctx = ui.ctx().clone();
        let Some(diff) = self.conv.editor.documents[index].diff.as_mut() else {
            return;
        };
        let staged = diff.staged.clone().unwrap_or_default();
        let base = diff.base.clone().unwrap_or_default();
        diff.staged_job(extension, &ctx);
        diff.base_job(extension, &ctx);
        let pane_rect = Rect::from_min_max(rect.min, pos2(rect.right() - RULER_W, rect.bottom()));
        let output = pane::readonly_pane(
            ui,
            pane_rect,
            pane::PaneInput {
                id: ui.id().with(("diff_staged_pane", index)),
                text: &staged,
                text_version: diff.version,
                job: diff.staged_job.as_ref().map(|(_, job)| job),
                side: Side::New,
                inline,
                decor,
                zones: inline.then(|| ZoneText {
                    base: &base,
                    base_job: diff.base_job.as_ref().map(|(_, job)| job),
                    line_starts: &diff.base_starts,
                }),
                scroll_y: diff.scroll_y,
            },
            &mut diff.right,
        );
        let max = (output.content_h - output.viewport_h).max(0.0);
        if output.wheel_y != 0.0 {
            diff.scroll_y = (diff.scroll_y - output.wheel_y).clamp(0.0, max);
            ui.ctx().request_repaint();
        }
        diff.scroll_y = diff.scroll_y.min(max);
        let content_top = output.clip.top() - diff.scroll_y;
        let spans: Vec<_> = output
            .spans
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
            .collect();
        diff.frame.new_spans = output.spans;
        diff.frame.new_clip = output.clip;
        let ruler_rect = Rect::from_min_max(pos2(pane_rect.right(), rect.top()), rect.max);
        if let Some(y) = decor::ruler(
            ui,
            ruler_rect,
            &spans,
            output.content_h,
            output.viewport_h,
            diff.scroll_y,
        ) {
            diff.scroll_y = y;
            ui.ctx().request_repaint();
        }
    }

    /// Once the changes are known, bring the requested one into view.
    fn reveal_pending_change(&mut self, index: usize, decor: &DiffDecor, viewport_h: f32) {
        let document = &mut self.conv.editor.documents[index];
        let Some(diff) = document.diff.as_mut() else {
            return;
        };
        let Some(line) = diff.reveal.take() else {
            return;
        };
        let target = line
            .and_then(|line| decor.change_at_new_line(line))
            .or((!decor.changes.is_empty()).then_some(0));
        let Some(target) = target else {
            return;
        };
        if diff.source.editable() {
            let line = decor.changes[target].new_start;
            let byte = line_start_byte(&document.content, line);
            self.conv.editor.navigation_target = Some((document.path.clone(), byte..byte));
        } else {
            let font_h = FS_SMALL * 1.35;
            let top = decor.changes[target].new_start as f32 * font_h;
            diff.scroll_y = (top - viewport_h / 3.0).max(0.0);
        }
    }

    /// F7 / Shift+F7: the next or previous change from the caret (or the viewport).
    fn jump_to_change(&mut self, index: usize, forward: bool) {
        let caret_char = self.conv.editor.navigation_cursor_char;
        let document = &mut self.conv.editor.documents[index];
        let revision = document.content_revision;
        let Some(diff) = document.diff.as_mut() else {
            return;
        };
        if diff.source.editable() {
            let Some(decor) = diff.decor_at(revision, &document.content) else {
                return;
            };
            let byte = super::editor_logic::char_index_to_byte(&document.content, caret_char);
            let caret_line = document.content[..byte].matches('\n').count();
            let starts = decor.changes.iter().map(|change| change.new_start);
            let target = if forward {
                starts
                    .clone()
                    .find(|start| *start > caret_line)
                    .or(starts.clone().next())
            } else {
                starts
                    .clone()
                    .rev()
                    .find(|start| *start < caret_line)
                    .or(starts.clone().next_back())
            };
            if let Some(line) = target {
                let byte = line_start_byte(&document.content, line);
                self.conv.editor.navigation_target = Some((document.path.clone(), byte..byte));
                self.conv.editor.focus_editor_next_frame = true;
            }
        } else {
            let spans = &diff.frame.new_spans;
            let clip = diff.frame.new_clip;
            let anchor = clip.top() + clip.height() / 3.0;
            let target = if forward {
                spans
                    .iter()
                    .find(|(top, _)| *top > anchor + 1.0)
                    .or(spans.first())
            } else {
                spans
                    .iter()
                    .rev()
                    .find(|(top, _)| *top < anchor - 1.0)
                    .or(spans.last())
            };
            if let Some(&(top, _)) = target {
                diff.scroll_y = (diff.scroll_y + top - anchor).max(0.0);
            }
        }
    }

    fn diff_toolbar(
        &mut self,
        ui: &mut Ui,
        index: usize,
        decor: Option<&DiffDecor>,
        split_possible: bool,
    ) -> Option<ToolbarAction> {
        let mut action = None;
        let diff = self.conv.editor.documents[index].diff.as_ref()?;
        let path = diff.relative.clone();
        let base_label = diff.source.base_label();
        ui.spacing_mut().item_spacing.x = 6.0;
        let (dir, name) = path.rsplit_once('/').unwrap_or(("", path.as_str()));
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
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            if toolbar_icon(ui, ICON_CLOSE, "Close diff, keep editing the file", true).clicked() {
                action = Some(ToolbarAction::Close);
            }
            ui.add_space(6.0);
            let inline = self.conv.editor.diff_inline || !split_possible;
            if segment(ui, "Inline", inline, true, "Show changes in one column").clicked() {
                self.conv.editor.diff_inline = true;
            }
            let split_hint = if split_possible {
                "Show old and new side by side"
            } else {
                "Too narrow for side by side"
            };
            if segment(ui, "Split", !inline, split_possible, split_hint).clicked() {
                self.conv.editor.diff_inline = false;
            }
            ui.add_space(6.0);
            let has_changes = decor.is_some_and(|decor| !decor.changes.is_empty());
            if toolbar_icon(ui, ICON_ANGLE_DOWN, "Next change (F7)", has_changes).clicked() {
                action = Some(ToolbarAction::Jump(true));
            }
            if toolbar_icon(ui, ICON_ANGLE_UP, "Previous change (Shift+F7)", has_changes).clicked()
            {
                action = Some(ToolbarAction::Jump(false));
            }
            ui.add_space(4.0);
            if let Some(decor) = decor {
                for (count, sign, color) in [
                    (decor.removed, '−', c_diff_del_fg()),
                    (decor.added, '+', c_diff_add_fg()),
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
            }
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(base_label)
                    .size(FS_TINY)
                    .color(c_text_faint()),
            );
        });
        action
    }

    /// The hovered change's actions, over the top right of its new side.
    fn diff_block_buttons(
        &mut self,
        ui: &mut Ui,
        index: usize,
        decor: &DiffDecor,
    ) -> Option<(BlockAction, usize)> {
        let diff = self.conv.editor.documents[index].diff.as_ref()?;
        let actions = diff.source.actions();
        let pointer = ui.input(|i| i.pointer.hover_pos())?;
        let frame = &diff.frame;
        let in_spans = |spans: &[(f32, f32)], clip: Rect| {
            clip.contains(pointer)
                .then(|| {
                    spans.iter().position(|&(top, bottom)| {
                        top <= pointer.y && pointer.y < bottom.max(top + 4.0)
                    })
                })
                .flatten()
        };
        let change = in_spans(&frame.new_spans, frame.new_clip)
            .or_else(|| in_spans(&frame.old_spans, frame.old_clip))?;
        decor.changes.get(change)?;
        let &(top, bottom) = frame.new_spans.get(change)?;
        let clip = frame.new_clip;
        let line_h = FS_SMALL * 1.35;
        // Keep the buttons inside the viewport when the change starts above it.
        let top = top.max(clip.top()).min(bottom.max(top + line_h) - line_h);
        if top > clip.bottom() - line_h {
            return None;
        }
        let mut right = clip.right() - 8.0;
        let mut clicked = None;
        let painter = ui.painter().with_clip_rect(clip.intersect(ui.clip_rect()));
        for &block_action in actions.iter().rev() {
            let (icon, label, hover) = block_action.label();
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
            let galley = painter.layout_job(job);
            let rect = Rect::from_min_size(
                pos2(right - galley.size().x - 12.0, top + 1.0),
                vec2(galley.size().x + 12.0, line_h - 2.0),
            );
            right = rect.left() - 4.0;
            let response = ui
                .interact(
                    rect,
                    ui.id().with(("diff_editor_block", index, change, label)),
                    Sense::click(),
                )
                .on_hover_text(hover);
            response
                .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
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
                clicked = Some((block_action, change));
            }
        }
        clicked
    }

    fn apply_diff_block(
        &mut self,
        ctx: &egui::Context,
        index: usize,
        decor: &DiffDecor,
        (action, change): (BlockAction, usize),
    ) {
        let Some(change) = decor.changes.get(change) else {
            return;
        };
        let Some(diff) = self.conv.editor.documents[index].diff.as_ref() else {
            return;
        };
        let relative = diff.relative.clone();
        let source = diff.source.clone();
        match action {
            BlockAction::Stage => {
                self.request(crate::git::GitOp::ApplyBlock(crate::git::BlockEdit {
                    path: relative,
                    target: crate::git::BlockTarget::Index,
                    start: change.old_start + 1,
                    expected: change.old_lines.clone(),
                    replacement: change.new_lines.clone(),
                }))
            }
            BlockAction::Unstage => {
                self.request(crate::git::GitOp::ApplyBlock(crate::git::BlockEdit {
                    path: relative,
                    target: crate::git::BlockTarget::Index,
                    start: change.new_start + 1,
                    expected: change.new_lines.clone(),
                    replacement: change.old_lines.clone(),
                }));
            }
            BlockAction::Discard => {
                let was_clean = !self.conv.editor.documents[index].is_dirty();
                let edit = crate::git::BlockEdit {
                    path: relative,
                    target: crate::git::BlockTarget::WorkTree,
                    start: change.new_start + 1,
                    expected: change.new_lines.clone(),
                    replacement: change.old_lines.clone(),
                };
                if let Err(error) = self.edit_document_block(ctx, index, &edit) {
                    self.conv.editor.error = Some(error);
                    return;
                }
                // A clean file stays in step with the disk, like Git's discard; the edit is
                // still one undo step in the buffer.
                if was_clean
                    && source != DiffSource::Unsaved
                    && let Err(error) = self.save_editor_document(index, false)
                {
                    self.conv.editor.error = Some(error);
                }
            }
        }
    }

    /// In the plain editor, clicking a change marker in the gutter opens the diff at it.
    pub(super) fn gutter_marker_click(
        &mut self,
        ui: &mut Ui,
        index: usize,
        gutter_rect: Rect,
        visible: &[(usize, f32)],
        markers: &[GitLineChange],
    ) {
        const MARKER_HIT_W: f32 = 10.0;
        let line_h = FS_SMALL * 1.35;
        let strip = Rect::from_min_max(
            gutter_rect.left_top(),
            pos2(gutter_rect.left() + MARKER_HIT_W, gutter_rect.bottom()),
        )
        .intersect(ui.clip_rect());
        let response = ui.interact(strip, ui.id().with("diff_marker_strip"), Sense::click());
        let marked = response
            .hover_pos()
            .and_then(|pos| {
                visible
                    .iter()
                    .find(|(_, center)| (pos.y - center).abs() <= line_h * 0.5)
                    .map(|(line, _)| *line)
            })
            .filter(|line| markers.iter().any(|marker| marker.line == *line));
        let Some(line) = marked else {
            return;
        };
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        let response = response.on_hover_text("Show change");
        if response.clicked() {
            let source = self.gutter_diff_source();
            self.open_document_diff(index, source);
            if let Some(diff) = self.conv.editor.documents[index].diff.as_mut() {
                diff.reveal = Some(Some(line));
            }
        }
    }
}

enum ToolbarAction {
    Close,
    Jump(bool),
}

/// Byte offset where 0-based `line` starts (end of text past the last line).
fn line_start_byte(content: &str, line: usize) -> usize {
    content.split_inclusive('\n').take(line).map(str::len).sum()
}
