//! Find in Files (Cmd/Ctrl+Shift+F) and Find References (Shift+F12), shown in the sidebar.
//!
//! Searches run on a worker that walks the workspace like ripgrep (honouring `.gitignore`) and
//! streams each file's hits back, so the first results appear while a large tree is still being
//! read. Unsaved editor buffers are searched instead of their files on disk. A newer search
//! cancels the one still running.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use eframe::egui::{self, Align, FontId, Layout, Rect, RichText, ScrollArea, Sense, Ui};

use crate::theme::*;

use super::super::OxiApp;
use super::editor_logic::char_index_to_byte;
use super::find_replace::{ToggleLabel, query_field, toggle_button};
use super::{FindOptions, file_icon, find_matches};

pub(crate) const SEARCH_FIELD_ID: &str = "project_search_query";
/// egui temp-data key the editor's context menu sets to run Find References next frame.
pub(crate) const FIND_REFERENCES_REQUEST: &str = "find_references_request";
const REPLACE_FIELD_ID: &str = "project_search_replace";
/// Pause after the last keystroke before searching, so typing a word runs one search, not five.
const DEBOUNCE: Duration = Duration::from_millis(180);
/// Hits kept across the workspace; a one-letter query would otherwise list the whole tree.
const MAX_HITS: usize = 10_000;
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// Characters of a long line shown before the match in a result row.
const PREVIEW_LEAD: usize = 40;
const PREVIEW_MAX: usize = 240;
const ROW_H: f32 = 22.0;

/// One hit: where it is in the file and how its row shows it.
pub(crate) struct LineHit {
    pub range: Range<usize>,
    /// 1-based.
    pub line: usize,
    pub preview: String,
    /// The match within `preview`.
    pub highlight: Range<usize>,
}

pub(crate) struct FileHits {
    pub path: PathBuf,
    pub display: String,
    pub hits: Vec<LineHit>,
}

enum SearchMsg {
    File(FileHits),
    Done { truncated: bool },
}

/// What the result list currently shows.
#[derive(Clone, PartialEq)]
enum ResultsKind {
    Text,
    /// Find References for this identifier.
    References(String),
}

struct RunningSearch {
    receiver: Receiver<SearchMsg>,
    cancel: Arc<AtomicBool>,
}

impl Drop for RunningSearch {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Inputs a text search depends on; a change re-runs the search.
#[derive(Clone, PartialEq)]
struct SearchKey {
    root: String,
    query: String,
    options: FindOptions,
    include: String,
}

#[derive(Default)]
pub(crate) struct ProjectSearchState {
    pub query: String,
    pub replacement: String,
    /// Comma-separated globs (`*.rs, src/**`) limiting which files are searched.
    pub include: String,
    pub options: FindOptions,
    pub replace_open: bool,
    pub focus_query: bool,
    /// Set when the query or options changed; the search starts once typing pauses.
    edited_at: Option<Instant>,
    searched: Option<SearchKey>,
    running: Option<RunningSearch>,
    kind: Option<ResultsKind>,
    files: Vec<FileHits>,
    total: usize,
    truncated: bool,
    error: Option<String>,
    collapsed: HashSet<PathBuf>,
    /// Outcome of the last Replace All, shown under the fields.
    notice: Option<String>,
}

impl ProjectSearchState {
    fn clear_results(&mut self) {
        self.running = None;
        self.files.clear();
        self.total = 0;
        self.truncated = false;
        self.error = None;
        self.collapsed.clear();
    }

    pub(crate) fn total_hits(&self) -> (usize, usize) {
        (self.total, self.files.len())
    }

    /// Adopt hits streamed by the worker since the last frame.
    fn drain(&mut self) -> bool {
        let Some(running) = &self.running else {
            return false;
        };
        let mut changed = false;
        loop {
            match running.receiver.try_recv() {
                Ok(SearchMsg::File(file)) => {
                    self.total += file.hits.len();
                    self.files.push(file);
                    changed = true;
                }
                Ok(SearchMsg::Done { truncated }) => {
                    self.truncated = truncated;
                    self.running = None;
                    return true;
                }
                Err(TryRecvError::Empty) => return changed,
                Err(TryRecvError::Disconnected) => {
                    self.running = None;
                    return true;
                }
            }
        }
    }
}

/// Files under `root` worth searching: `.gitignore`d trees and `.git` are skipped, hidden
/// project folders such as `.github` are kept.
fn walk_files(root: &Path) -> impl Iterator<Item = PathBuf> {
    ignore::WalkBuilder::new(root)
        .hidden(false)
        .require_git(false)
        .sort_by_file_name(|a, b| a.cmp(b))
        .filter_entry(|entry| entry.file_name() != ".git")
        .build()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .map(ignore::DirEntry::into_path)
}

/// UTF-8 text of `path`, unless it is large or binary.
fn read_text(path: &Path) -> Option<String> {
    if crate::vfs::metadata(path).ok()?.len > MAX_FILE_BYTES {
        return None;
    }
    let bytes = crate::vfs::read(path).ok()?;
    if memchr::memchr(0, &bytes).is_some() {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// `include` globs; a pattern without a glob character matches as a path substring.
fn include_filter(include: &str) -> impl Fn(&str) -> bool + use<> {
    let patterns: Vec<Result<glob::Pattern, String>> = include
        .split(',')
        .map(str::trim)
        .filter(|pattern| !pattern.is_empty())
        .map(|pattern| {
            if pattern.contains(['*', '?', '[']) {
                glob::Pattern::new(pattern).map_err(|_| pattern.to_owned())
            } else {
                Err(pattern.to_owned())
            }
        })
        .collect();
    move |display: &str| {
        patterns.is_empty()
            || patterns.iter().any(|pattern| match pattern {
                Ok(glob) => {
                    glob.matches(display)
                        || display
                            .rsplit('/')
                            .next()
                            .is_some_and(|name| glob.matches(name))
                }
                Err(text) => display.contains(text.as_str()),
            })
    }
}

/// Rows for `ranges` (sorted) in `content`: line numbers and a preview cropped around the match.
pub(crate) fn line_hits(content: &str, ranges: &[Range<usize>]) -> Vec<LineHit> {
    let mut hits = Vec::with_capacity(ranges.len());
    let mut line = 1;
    let mut counted_to = 0;
    for range in ranges {
        line += memchr::memchr_iter(b'\n', &content.as_bytes()[counted_to..range.start]).count();
        counted_to = range.start;
        let line_start = content[..range.start].rfind('\n').map_or(0, |at| at + 1);
        let line_end = content[range.start..]
            .find('\n')
            .map_or(content.len(), |at| range.start + at);
        let line_text = content[line_start..line_end].trim_end_matches('\r');
        let match_end = range.end.min(line_start + line_text.len());
        // Leading indentation is noise in a narrow sidebar; long lines start near the match.
        let mut start = line_start + (line_text.len() - line_text.trim_start().len());
        start = start.min(range.start);
        let mut prefix = "";
        if range.start - start > PREVIEW_LEAD {
            start = range.start - PREVIEW_LEAD;
            while !content.is_char_boundary(start) {
                start += 1;
            }
            prefix = "…";
        }
        let mut end = (line_start + line_text.len()).max(match_end);
        if end - start > PREVIEW_MAX {
            end = (start + PREVIEW_MAX).max(match_end);
            while !content.is_char_boundary(end) {
                end += 1;
            }
        }
        let preview = format!("{prefix}{}", &content[start..end]);
        let highlight_start = prefix.len() + range.start - start;
        hits.push(LineHit {
            range: range.clone(),
            line,
            highlight: highlight_start..highlight_start + (match_end - range.start),
            preview,
        });
    }
    hits
}

fn display_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Search every file under `root` (open buffers in `overrides` win over disk) on this thread.
fn run_text_search(
    root: &Path,
    key: &SearchKey,
    overrides: &HashMap<PathBuf, String>,
    cancel: &AtomicBool,
    sender: &Sender<SearchMsg>,
) {
    if crate::vfs::is_remote(root) {
        remote_text_search(root, key, cancel, sender);
        return;
    }
    let included = include_filter(&key.include);
    let mut found = 0;
    for path in walk_files(root) {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let display = display_path(root, &path);
        if !included(&display) {
            continue;
        }
        let content = match overrides.get(&path) {
            Some(text) => text.clone(),
            None => match read_text(&path) {
                Some(text) => text,
                None => continue,
            },
        };
        let mut ranges = find_matches(&content, &key.query, key.options).ranges;
        if ranges.is_empty() {
            continue;
        }
        let truncated = found + ranges.len() >= MAX_HITS;
        ranges.truncate(MAX_HITS - found);
        found += ranges.len();
        let hits = line_hits(&content, &ranges);
        if sender
            .send(SearchMsg::File(FileHits {
                path,
                display,
                hits,
            }))
            .is_err()
        {
            return;
        }
        if truncated {
            let _ = sender.send(SearchMsg::Done { truncated: true });
            return;
        }
    }
    let _ = sender.send(SearchMsg::Done { truncated: false });
}

/// `fs.search` on a server: run the search on its files and return every hit at once.
pub(crate) fn search_json(root: &Path, params: &serde_json::Value) -> serde_json::Value {
    let key = SearchKey {
        root: root.to_string_lossy().to_string(),
        query: params["query"].as_str().unwrap_or_default().to_string(),
        options: FindOptions {
            regex: params["regex"].as_bool().unwrap_or(false),
            case_sensitive: params["case_sensitive"].as_bool().unwrap_or(false),
            whole_word: params["whole_word"].as_bool().unwrap_or(false),
        },
        include: params["include"].as_str().unwrap_or_default().to_string(),
    };
    let (sender, receiver) = std::sync::mpsc::channel();
    run_text_search(
        root,
        &key,
        &HashMap::new(),
        &AtomicBool::new(false),
        &sender,
    );
    drop(sender);
    let mut files = Vec::new();
    let mut truncated = false;
    for message in receiver {
        match message {
            SearchMsg::File(file) => files.push(serde_json::json!({
                "display": file.display,
                "hits": file.hits.iter().map(|hit| serde_json::json!([
                    hit.range.start, hit.range.end, hit.line,
                    hit.highlight.start, hit.highlight.end, hit.preview,
                ])).collect::<Vec<_>>(),
            })),
            SearchMsg::Done { truncated: t } => truncated = t,
        }
    }
    serde_json::json!({"files": files, "truncated": truncated})
}

/// A search of a remote workspace, run by its server.
fn remote_text_search(
    root: &Path,
    key: &SearchKey,
    cancel: &AtomicBool,
    sender: &Sender<SearchMsg>,
) {
    let Some((host, path)) = crate::remote::split(&key.root) else {
        return;
    };
    let params = serde_json::json!({
        "root": path,
        "query": key.query,
        "regex": key.options.regex,
        "case_sensitive": key.options.case_sensitive,
        "whole_word": key.options.whole_word,
        "include": key.include,
    });
    let result = crate::remote::client::call(host, "fs.search", params, Duration::from_secs(60));
    if cancel.load(Ordering::Relaxed) {
        return;
    }
    let Ok(result) = result else {
        let _ = sender.send(SearchMsg::Done { truncated: false });
        return;
    };
    let number = |v: &serde_json::Value| v.as_u64().unwrap_or(0) as usize;
    for file in result["files"].as_array().into_iter().flatten() {
        let display = file["display"].as_str().unwrap_or_default().to_string();
        let hits = file["hits"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|hit| LineHit {
                range: number(&hit[0])..number(&hit[1]),
                line: number(&hit[2]),
                highlight: number(&hit[3])..number(&hit[4]),
                preview: hit[5].as_str().unwrap_or_default().to_string(),
            })
            .collect();
        let path = root.join(&display);
        if sender
            .send(SearchMsg::File(FileHits {
                path,
                display,
                hits,
            }))
            .is_err()
        {
            return;
        }
    }
    let _ = sender.send(SearchMsg::Done {
        truncated: result["truncated"].as_bool() == Some(true),
    });
}

fn spawn_search(
    ctx: &egui::Context,
    work: impl FnOnce(&AtomicBool, &Sender<SearchMsg>) + Send + 'static,
) -> RunningSearch {
    let (sender, receiver) = std::sync::mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancel);
    let ctx = ctx.clone();
    let _ = std::thread::Builder::new()
        .name("oxi-project-search".into())
        .spawn(move || {
            work(&worker_cancel, &sender);
            ctx.request_repaint();
        });
    RunningSearch { receiver, cancel }
}

impl OxiApp {
    fn canonical_workspace_root(&self) -> PathBuf {
        let root = &self.active_workspace().root_path;
        crate::vfs::canonicalize(Path::new(root)).unwrap_or_else(|_| PathBuf::from(root))
    }

    /// Unsaved editor buffers by path, searched instead of their files on disk.
    fn dirty_buffers(&self) -> HashMap<PathBuf, String> {
        self.conv
            .editor
            .documents
            .iter()
            .filter(|document| document.media.is_none() && document.is_dirty())
            .map(|document| (document.path.clone(), document.content.clone()))
            .collect()
    }

    /// Show the search sidebar with the query focused; a short single-line editor selection
    /// becomes the query.
    pub(crate) fn show_project_search(&mut self) {
        if let Some(selected) = self.conv.editor.single_line_selection() {
            self.conv.project_search.query = selected;
            self.conv.project_search.edited_at = Some(Instant::now() - DEBOUNCE);
        }
        self.conv.sidebar.open = true;
        self.conv.sidebar.mode = super::super::state::SidebarMode::Search;
        self.conv.project_search.focus_query = true;
    }

    /// Find References (Shift+F12) for the identifier at the editor caret.
    pub(crate) fn find_references_at_caret(&mut self, ctx: &egui::Context) {
        let Some(document) = self.conv.editor.active_document() else {
            return;
        };
        let Some(language) = crate::code_nav::language_for_path(&document.path) else {
            return;
        };
        let cursor = char_index_to_byte(&document.content, self.conv.editor.navigation_cursor_char);
        let Some(target) = crate::code_nav::reference_target(language, &document.content, cursor)
        else {
            self.conv.editor.error = Some("No identifier at the caret.".into());
            return;
        };
        let root = self.canonical_workspace_root();
        let current_path = document.path.clone();
        let current_source = document.content.clone();
        let mut overrides = self.dirty_buffers();
        overrides.insert(current_path.clone(), current_source);

        let search = &mut self.conv.project_search;
        search.clear_results();
        search.searched = None;
        search.edited_at = None;
        search.notice = None;
        search.kind = Some(ResultsKind::References(target.name.clone()));
        search.running = Some(spawn_search(ctx, move |cancel, sender| {
            let emit = |path: PathBuf, content: &str, ranges: Vec<Range<usize>>| {
                if ranges.is_empty() {
                    return true;
                }
                let hits = line_hits(content, &ranges);
                let display = display_path(&root, &path);
                sender
                    .send(SearchMsg::File(FileHits {
                        path,
                        display,
                        hits,
                    }))
                    .is_ok()
            };
            let current = &overrides[&current_path];
            if let Some(scope) = &target.local_scope {
                let ranges = crate::code_nav::reference_ranges(language, current, &target.name)
                    .into_iter()
                    .filter(|range| scope.contains(&range.start))
                    .collect();
                emit(current_path, current, ranges);
                let _ = sender.send(SearchMsg::Done { truncated: false });
                return;
            }
            // The current file first, then the rest of the language family's files.
            let ranges = crate::code_nav::reference_ranges(language, current, &target.name);
            if !emit(current_path.clone(), current, ranges) {
                return;
            }
            for path in walk_files(&root) {
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                if path == current_path {
                    continue;
                }
                let Some(other) = crate::code_nav::language_for_path(&path) else {
                    continue;
                };
                if !crate::code_nav::same_family(language, other) {
                    continue;
                }
                let content = match overrides.get(&path) {
                    Some(text) => text.clone(),
                    None => match read_text(&path) {
                        Some(text) => text,
                        None => continue,
                    },
                };
                if !content.contains(target.name.as_str()) {
                    continue;
                }
                let ranges = crate::code_nav::reference_ranges(other, &content, &target.name);
                if !emit(path, &content, ranges) {
                    return;
                }
            }
            let _ = sender.send(SearchMsg::Done { truncated: false });
        }));
        self.conv.sidebar.open = true;
        self.conv.sidebar.mode = super::super::state::SidebarMode::Search;
    }

    /// Start the text search once typing pauses, and pull in streamed results.
    fn update_project_search(&mut self, ctx: &egui::Context) {
        if self.conv.project_search.drain() {
            ctx.request_repaint();
        }
        if self.conv.project_search.running.is_some() {
            // Show streamed hits as they arrive; the worker wakes the UI once more when done.
            ctx.request_repaint_after(Duration::from_millis(60));
        }
        let root = self.active_workspace().root_path.clone();
        let search = &self.conv.project_search;
        if search
            .kind
            .as_ref()
            .is_some_and(|kind| *kind != ResultsKind::Text)
        {
            return;
        }
        let key = SearchKey {
            root,
            query: search.query.clone(),
            options: search.options,
            include: search.include.clone(),
        };
        if search.searched.as_ref() == Some(&key) {
            return;
        }
        if let Some(edited_at) = search.edited_at
            && edited_at.elapsed() < DEBOUNCE
        {
            ctx.request_repaint_after(DEBOUNCE - edited_at.elapsed());
            return;
        }
        let overrides = self.dirty_buffers();
        let root = self.canonical_workspace_root();
        let search = &mut self.conv.project_search;
        search.clear_results();
        search.searched = Some(key.clone());
        search.edited_at = None;
        if key.query.is_empty() {
            search.kind = None;
            return;
        }
        let validity = find_matches("", &key.query, key.options);
        if let Some(error) = validity.error {
            search.error = Some(error);
            search.kind = None;
            return;
        }
        search.kind = Some(ResultsKind::Text);
        search.running = Some(spawn_search(ctx, move |cancel, sender| {
            run_text_search(&root, &key, &overrides, cancel, sender);
        }));
    }

    /// Open `path` at `hit`, recording the jump in the back history like Go to Definition.
    fn open_search_hit(&mut self, path: PathBuf, range: Range<usize>, line: usize) {
        if let Some(current) = self.conv.editor.active_document() {
            let byte =
                char_index_to_byte(&current.content, self.conv.editor.navigation_cursor_char);
            self.conv
                .editor
                .navigation_back
                .push((current.path.clone(), byte..byte));
            self.conv.editor.navigation_forward.clear();
        }
        self.open_editor_file_only(path.clone());
        let Some(document) = self.conv.editor.active_document() else {
            return;
        };
        if document.path != path {
            return;
        }
        // The file may have changed since the search: fall back to the hit's line.
        let range = if document.content.get(range.clone()).is_some() {
            range
        } else {
            let start = super::file_picker::line_start(&document.content, line);
            start..start
        };
        self.conv.editor.navigation_target = Some((path, range));
        self.conv.editor.focus_editor_next_frame = true;
    }

    /// Replace every match of the current query in the listed files: open documents are edited
    /// in place (and left unsaved), other files are rewritten on disk.
    pub(crate) fn replace_in_files(&mut self) {
        let search = &self.conv.project_search;
        let Some(key) = search.searched.clone() else {
            return;
        };
        let replacement = search.replacement.clone();
        let paths: Vec<PathBuf> = search.files.iter().map(|file| file.path.clone()).collect();
        let mut replaced = 0;
        let mut files = 0;
        let mut failures = Vec::new();
        for path in paths {
            let substitute = |content: &str| {
                let results = find_matches(content, &key.query, key.options);
                if results.ranges.is_empty() {
                    return None;
                }
                let mut out = String::with_capacity(content.len());
                let mut copied = 0;
                for range in &results.ranges {
                    out.push_str(&content[copied..range.start]);
                    out.push_str(&results.replacement_for(content, range, &replacement));
                    copied = range.end;
                }
                out.push_str(&content[copied..]);
                Some((out, results.ranges.len()))
            };
            if let Some(document) = self
                .conv
                .editor
                .documents
                .iter_mut()
                .find(|document| document.path == path && document.media.is_none())
            {
                if let Some((content, count)) = substitute(&document.content) {
                    document.content = content;
                    super::editor_body::mark_document_edited(document);
                    replaced += count;
                    files += 1;
                }
                continue;
            }
            let Some(content) = read_text(&path) else {
                failures.push(display_path(Path::new(&key.root), &path));
                continue;
            };
            if let Some((content, count)) = substitute(&content) {
                match crate::vfs::write(&path, content.as_bytes()) {
                    Ok(()) => {
                        replaced += count;
                        files += 1;
                    }
                    Err(_) => failures.push(display_path(Path::new(&key.root), &path)),
                }
            }
        }
        let search = &mut self.conv.project_search;
        let mut notice = format!(
            "Replaced {replaced} {} in {files} {}.",
            if replaced == 1 { "match" } else { "matches" },
            if files == 1 { "file" } else { "files" },
        );
        if !failures.is_empty() {
            notice.push_str(&format!(" Could not write: {}.", failures.join(", ")));
        }
        search.notice = Some(notice);
        // Search again: what is left is what the replacement did not cover.
        search.searched = None;
        search.edited_at = None;
    }

    pub(crate) fn render_project_search(&mut self, ui: &mut Ui) {
        ui.set_min_width(ui.max_rect().width());
        self.update_project_search(ui.ctx());

        ui.horizontal(|ui| {
            // Same 24 px toolbar as the Explorer and the chat list, so modes line up.
            ui.set_height(24.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let has_results = !self.conv.project_search.files.is_empty();
                if has_results
                    && crate::ui::chrome::icon_button_plain(ui, ICON_CLOSE, 20.0, false)
                        .on_hover_text("Clear results")
                        .clicked()
                {
                    let search = &mut self.conv.project_search;
                    search.query.clear();
                    search.kind = None;
                    search.notice = None;
                    search.clear_results();
                    search.searched = None;
                    search.focus_query = true;
                }
                if has_results {
                    let all_collapsed = self.conv.project_search.collapsed.len()
                        == self.conv.project_search.files.len();
                    if crate::ui::chrome::icon_button_plain(
                        ui,
                        if all_collapsed {
                            ICON_ANGLE_DOWN
                        } else {
                            ICON_ANGLE_UP
                        },
                        20.0,
                        false,
                    )
                    .on_hover_text(if all_collapsed {
                        "Expand all"
                    } else {
                        "Collapse all"
                    })
                    .clicked()
                    {
                        let search = &mut self.conv.project_search;
                        if all_collapsed {
                            search.collapsed.clear();
                        } else {
                            search.collapsed =
                                search.files.iter().map(|file| file.path.clone()).collect();
                        }
                    }
                }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.add(
                        egui::Label::new(
                            RichText::new("SEARCH")
                                .size(FS_TINY)
                                .strong()
                                .color(c_text_muted()),
                        )
                        .truncate(),
                    );
                });
            });
        });
        ui.add_space(8.0);

        let references = match &self.conv.project_search.kind {
            Some(ResultsKind::References(name)) => Some(name.clone()),
            _ => None,
        };
        if let Some(name) = &references {
            self.render_references_header(ui, name);
        } else {
            self.render_search_fields(ui);
        }
        ui.add_space(6.0);

        let search = &self.conv.project_search;
        let (total, files) = search.total_hits();
        let searching = search.running.is_some();
        let summary = if let Some(error) = &search.error {
            Some((error.clone(), true))
        } else if search.kind.is_none() {
            search.notice.clone().map(|notice| (notice, false))
        } else if total == 0 {
            Some((
                if searching {
                    "Searching…".to_owned()
                } else if references.is_some() {
                    "No references found.".to_owned()
                } else {
                    "No results.".to_owned()
                },
                false,
            ))
        } else {
            let mut text = format!(
                "{total} result{} in {files} file{}{}",
                if total == 1 { "" } else { "s" },
                if files == 1 { "" } else { "s" },
                if search.truncated {
                    " (limit reached)"
                } else {
                    ""
                },
            );
            if searching {
                text.push_str(" …");
            }
            if let Some(notice) = &search.notice {
                text = format!("{notice} {text}");
            }
            Some((text, false))
        };
        if let Some((text, error)) = summary {
            ui.add(
                egui::Label::new(RichText::new(text).size(FS_TINY).color(if error {
                    c_error_fg()
                } else {
                    c_text_faint()
                }))
                .wrap(),
            );
            ui.add_space(4.0);
        }

        let mut open = None;
        let mut toggle = None;
        ScrollArea::vertical()
            .id_salt("project_search_results")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let search = &self.conv.project_search;
                let width = ui.available_width();
                for file in &search.files {
                    let collapsed = search.collapsed.contains(&file.path);
                    let (rect, response) =
                        ui.allocate_exact_size(egui::vec2(width, ROW_H), Sense::click());
                    if ui.is_rect_visible(rect) {
                        paint_file_row(ui, rect, response.hovered(), file, collapsed);
                    }
                    if response.on_hover_text(&file.display).clicked() {
                        toggle = Some(file.path.clone());
                    }
                    if collapsed {
                        continue;
                    }
                    for hit in &file.hits {
                        let (rect, response) =
                            ui.allocate_exact_size(egui::vec2(width, ROW_H), Sense::click());
                        if ui.is_rect_visible(rect) {
                            paint_hit_row(ui, rect, response.hovered(), hit);
                        }
                        if response.clicked() {
                            open = Some((file.path.clone(), hit.range.clone(), hit.line));
                        }
                    }
                }
            });
        if let Some(path) = toggle {
            let collapsed = &mut self.conv.project_search.collapsed;
            if !collapsed.remove(&path) {
                collapsed.insert(path);
            }
        }
        if let Some((path, range, line)) = open {
            self.open_search_hit(path, range, line);
        }
    }

    fn render_references_header(&mut self, ui: &mut Ui, name: &str) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.label(
                RichText::new("References to")
                    .size(FS_SMALL)
                    .color(c_text_muted()),
            );
            ui.label(
                RichText::new(name)
                    .monospace()
                    .size(FS_SMALL)
                    .color(c_text()),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if crate::ui::chrome::mini_button(ui, "Text search")
                    .on_hover_text("Back to Find in Files")
                    .clicked()
                {
                    let search = &mut self.conv.project_search;
                    search.kind = None;
                    search.clear_results();
                    search.searched = None;
                    search.focus_query = true;
                }
            });
        });
    }

    fn render_search_fields(&mut self, ui: &mut Ui) {
        const FIELD_H: f32 = 26.0;
        const TOGGLE: f32 = 22.0;
        let search = &mut self.conv.project_search;
        let width = ui.available_width();
        let (row, _) = ui.allocate_exact_size(egui::vec2(width, FIELD_H), Sense::hover());
        let expander = Rect::from_min_size(row.min, egui::vec2(16.0, FIELD_H));
        let chevron = toggle_button(
            ui,
            expander,
            "project_search_toggle_replace",
            false,
            ToggleLabel::Icon(if search.replace_open {
                ICON_ANGLE_DOWN
            } else {
                ICON_CHEVRON_RIGHT
            }),
        )
        .on_hover_text(if search.replace_open {
            "Hide Replace"
        } else {
            "Show Replace"
        });
        if chevron.clicked() {
            search.replace_open = !search.replace_open;
        }
        let field_left = expander.right() + 4.0;
        let toggles_w = TOGGLE * 3.0 + 4.0;
        let field = Rect::from_min_max(
            egui::pos2(field_left, row.top()),
            egui::pos2(row.right(), row.bottom()),
        );
        let query_id = egui::Id::new(SEARCH_FIELD_ID);
        let response = query_field(
            ui,
            field,
            query_id,
            &mut search.query,
            "Search",
            toggles_w,
            search.error.is_some(),
        );
        if std::mem::take(&mut search.focus_query) {
            response.request_focus();
            select_all(ui.ctx(), query_id, &search.query);
        }
        let mut changed = response.changed();
        let toggles = [
            (".*", "Regular expression", 0),
            ("Aa", "Case sensitive", 1),
            ("ab", "Whole word", 2),
        ];
        for (index, (label, tip, slot)) in toggles.into_iter().enumerate() {
            let rect = Rect::from_min_size(
                egui::pos2(
                    field.right() - toggles_w + 2.0 + slot as f32 * TOGGLE,
                    field.center().y - TOGGLE / 2.0,
                ),
                egui::vec2(TOGGLE, TOGGLE),
            );
            let flag = match index {
                0 => &mut search.options.regex,
                1 => &mut search.options.case_sensitive,
                _ => &mut search.options.whole_word,
            };
            let label = if index == 2 {
                ToggleLabel::Word
            } else {
                ToggleLabel::Text(label)
            };
            if toggle_button(
                ui,
                rect,
                &format!("project_search_opt_{index}"),
                *flag,
                label,
            )
            .on_hover_text(tip)
            .clicked()
            {
                *flag = !*flag;
                changed = true;
            }
        }
        let enter = (response.has_focus() || response.lost_focus())
            && ui.input(|input| input.key_pressed(egui::Key::Enter));
        if enter {
            // Enter searches right away instead of waiting out the debounce.
            search.edited_at = Some(Instant::now() - DEBOUNCE);
            search.searched = None;
            response.request_focus();
        }

        if search.replace_open {
            ui.add_space(4.0);
            let (row, _) = ui.allocate_exact_size(egui::vec2(width, FIELD_H), Sense::hover());
            const BUTTON_W: f32 = 30.0;
            let field = Rect::from_min_max(
                egui::pos2(field_left, row.top()),
                egui::pos2(row.right() - BUTTON_W - 4.0, row.bottom()),
            );
            query_field(
                ui,
                field,
                egui::Id::new(REPLACE_FIELD_ID),
                &mut search.replacement,
                "Replace",
                0.0,
                false,
            );
            let button = Rect::from_min_max(
                egui::pos2(field.right() + 4.0, row.top()),
                row.right_bottom(),
            );
            let can_replace =
                search.running.is_none() && search.edited_at.is_none() && search.total > 0;
            let clicked = ui
                .put(
                    button,
                    egui::Button::new(icon_glyph(ICON_REFRESH, FS_SMALL)).min_size(button.size()),
                )
                .on_hover_text("Replace All")
                .clicked();
            if clicked && can_replace {
                let (total, files) = search.total_hits();
                self.request_confirm(crate::app::state::ConfirmAction::ReplaceInFiles {
                    matches: total,
                    files,
                });
                return;
            }
        }

        ui.add_space(4.0);
        let (row, _) = ui.allocate_exact_size(egui::vec2(width, FIELD_H), Sense::hover());
        let field = Rect::from_min_max(egui::pos2(field_left, row.top()), row.right_bottom());
        let include = query_field(
            ui,
            field,
            egui::Id::new("project_search_include"),
            &mut search.include,
            "Files to include (*.rs, src/)",
            0.0,
            false,
        );
        changed |= include.changed();
        if changed {
            search.edited_at = Some(Instant::now());
            search.notice = None;
            ui.ctx().request_repaint_after(DEBOUNCE);
        }
    }
}

fn icon_glyph(icon: &str, size: f32) -> RichText {
    RichText::new(icon).font(FontId::new(size, icon_font()))
}

fn select_all(ctx: &egui::Context, id: egui::Id, text: &str) {
    let mut state = egui::text_edit::TextEditState::load(ctx, id).unwrap_or_default();
    state
        .cursor
        .set_char_range(Some(egui::text::CCursorRange::two(
            egui::text::CCursor::new(0),
            egui::text::CCursor::new(text.chars().count()),
        )));
    state.store(ctx, id);
}

fn paint_row_background(ui: &Ui, rect: Rect, hovered: bool) {
    if hovered {
        ui.painter()
            .rect_filled(rect, egui::CornerRadius::same(RADIUS_ROW), c_row_hover());
    }
}

fn paint_file_row(ui: &Ui, rect: Rect, hovered: bool, file: &FileHits, collapsed: bool) {
    paint_row_background(ui, rect, hovered);
    let painter = ui.painter_at(rect);
    let center_y = rect.center().y;
    let mut x = rect.left() + 4.0;
    painter.text(
        egui::pos2(x + 7.0, center_y),
        egui::Align2::CENTER_CENTER,
        if collapsed {
            ICON_CHEVRON_RIGHT
        } else {
            ICON_ANGLE_DOWN
        },
        FontId::new(FS_TINY, icon_font()),
        c_text_faint(),
    );
    x += 16.0;
    let (icon, color) = file_icon(&file.path);
    let icon_rect = painter.text(
        egui::pos2(x, center_y),
        egui::Align2::LEFT_CENTER,
        icon,
        FontId::new(FS_SMALL, icon_font()),
        color,
    );
    x = icon_rect.right() + 5.0;
    let count_rect = painter.text(
        egui::pos2(rect.right() - 12.0, center_y),
        egui::Align2::RIGHT_CENTER,
        file.hits.len().to_string(),
        FontId::proportional(FS_TINY),
        c_text_faint(),
    );
    // File name first, its directory after in a fainter color, like VS Code.
    let (directory, name) = match file.display.rsplit_once('/') {
        Some((directory, name)) => (directory, name),
        None => ("", file.display.as_str()),
    };
    let mut job = egui::text::LayoutJob::default();
    job.append(
        name,
        0.0,
        egui::TextFormat::simple(FontId::proportional(FS_SMALL), c_text()),
    );
    if !directory.is_empty() {
        job.append(
            directory,
            6.0,
            egui::TextFormat::simple(FontId::proportional(FS_TINY), c_text_faint()),
        );
    }
    job.wrap = egui::text::TextWrapping::truncate_at_width((count_rect.left() - 6.0 - x).max(0.0));
    let galley = painter.layout_job(job);
    painter.galley(
        egui::pos2(x, center_y - galley.size().y * 0.5),
        galley,
        c_text(),
    );
}

fn paint_hit_row(ui: &Ui, rect: Rect, hovered: bool, hit: &LineHit) {
    paint_row_background(ui, rect, hovered);
    let painter = ui.painter_at(rect);
    let x = rect.left() + 26.0;
    let font = FontId::monospace(FS_TINY);
    let plain = egui::TextFormat::simple(font.clone(), c_text_muted());
    let mut marked = egui::TextFormat::simple(font, c_text());
    marked.background = crate::theme::blend_color(c_bg_sidebar(), c_warning_fg(), 0.38);
    let mut job = egui::text::LayoutJob::default();
    let preview = hit.preview.as_str();
    let highlight = hit.highlight.start.min(preview.len())..hit.highlight.end.min(preview.len());
    job.append(&preview[..highlight.start], 0.0, plain.clone());
    job.append(&preview[highlight.clone()], 0.0, marked);
    job.append(&preview[highlight.end..], 0.0, plain);
    job.wrap = egui::text::TextWrapping::truncate_at_width((rect.right() - 8.0 - x).max(0.0));
    let galley = painter.layout_job(job);
    painter.galley(
        egui::pos2(x, rect.center().y - galley.size().y * 0.5),
        galley,
        c_text(),
    );
    let _ = ui
        .interact(
            rect,
            ui.id().with(("search_hit_tip", hit.range.start)),
            Sense::hover(),
        )
        .on_hover_text(format!("Line {}", hit.line));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_hits_number_lines_and_crop_previews() {
        let long = format!("{}needle tail", "x".repeat(100));
        let content = format!("first\n    indented needle\n{long}\n");
        let ranges: Vec<_> = content
            .match_indices("needle")
            .map(|(at, found)| at..at + found.len())
            .collect();
        let hits = line_hits(&content, &ranges);
        assert_eq!(hits[0].line, 2);
        assert_eq!(hits[0].preview, "indented needle");
        assert_eq!(&hits[0].preview[hits[0].highlight.clone()], "needle");
        assert_eq!(hits[1].line, 3);
        assert!(hits[1].preview.starts_with('…'));
        assert_eq!(&hits[1].preview[hits[1].highlight.clone()], "needle");
    }

    #[test]
    fn include_filter_matches_globs_names_and_substrings() {
        let filter = include_filter("*.rs, docs/");
        assert!(filter("src/app/mod.rs"));
        assert!(filter("docs/index.html"));
        assert!(!filter("src/app/style.css"));
        assert!(include_filter("")("anything"));
    }

    #[test]
    fn text_search_respects_gitignore_and_open_buffers() {
        let root = std::env::temp_dir().join(format!(
            "oxi-project-search-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("dist")).unwrap();
        std::fs::write(root.join(".gitignore"), "dist/\n").unwrap();
        std::fs::write(root.join("dist/out.js"), "needle").unwrap();
        std::fs::write(root.join("a.txt"), "needle\nneedle").unwrap();
        std::fs::write(root.join("b.txt"), "nothing").unwrap();
        let overrides = HashMap::from([(root.join("b.txt"), "unsaved needle".to_owned())]);
        let key = SearchKey {
            root: String::new(),
            query: "needle".into(),
            options: FindOptions::default(),
            include: String::new(),
        };
        let (sender, receiver) = std::sync::mpsc::channel();
        run_text_search(&root, &key, &overrides, &AtomicBool::new(false), &sender);
        let files: Vec<(String, usize)> = receiver
            .try_iter()
            .filter_map(|message| match message {
                SearchMsg::File(file) => Some((file.display, file.hits.len())),
                SearchMsg::Done { .. } => None,
            })
            .collect();
        assert_eq!(files, [("a.txt".to_owned(), 2), ("b.txt".to_owned(), 1)]);
        let _ = std::fs::remove_dir_all(root);
    }
}
