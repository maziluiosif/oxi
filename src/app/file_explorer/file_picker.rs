//! Workspace file discovery, fuzzy ranking, and temporary editor previews. Like Sublime's Goto
//! Anything, `@query` lists the current file's symbols, `:42` goes to a line and `name:42` opens
//! a file at a line; Cmd/Ctrl+Shift+R lists the whole project's symbols.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use eframe::egui::{self, RichText, ScrollArea, TextEdit};
use walkdir::WalkDir;

use crate::theme::*;

use super::super::OxiApp;
use super::support::{
    fuzzy_match_positions, fuzzy_path_score, load_gitignore_patterns, should_ignore,
};

/// Where a picker row leads.
#[derive(Clone, PartialEq)]
enum PickerTarget {
    /// A workspace file, optionally at a 1-based line.
    File { path: PathBuf, line: Option<usize> },
    /// A byte range in a file (a symbol or a line).
    Location { path: PathBuf, range: Range<usize> },
}

/// Goto Symbol entries of one file: outline symbols with their 1-based line numbers.
pub(crate) type PickerOutline = Vec<(crate::code_nav::Symbol, usize)>;

struct PickerEntry {
    primary: String,
    primary_matches: Vec<usize>,
    secondary: String,
    secondary_matches: Vec<usize>,
    target: Option<PickerTarget>,
}

/// Rows shown when no query matches, by mode.
const NO_MATCHES: &[&str] = &["No matching files", "No matching symbols"];

impl OxiApp {
    /// Open the picker with a mode prefix already typed (`@` symbols, `:` line).
    pub(crate) fn open_file_picker_with(&mut self, query: &str) {
        self.open_file_picker();
        self.conv.editor.file_picker_query = query.to_owned();
    }

    /// Goto Symbol in Project: the picker lists every function/type in the workspace.
    pub(crate) fn open_project_symbol_picker(&mut self, ctx: &egui::Context) {
        self.open_file_picker();
        self.conv.editor.file_picker_project_symbols = true;
        self.load_project_symbols(ctx);
    }

    pub(crate) fn open_file_picker(&mut self) {
        let root = PathBuf::from(&self.active_workspace().root_path);
        let ignored = load_gitignore_patterns(&root);
        self.conv.editor.file_picker_files = WalkDir::new(&root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|entry| {
                entry.path() == root
                    || !should_ignore(&root, entry.path(), entry.file_type().is_dir(), &ignored)
            })
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .map(|entry| entry.into_path())
            .collect();
        self.conv
            .editor
            .file_picker_files
            .sort_by_cached_key(|path| {
                path.strip_prefix(&root)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .to_ascii_lowercase()
            });
        self.conv.editor.file_picker_query.clear();
        self.conv.editor.file_picker_last_query.clear();
        self.conv.editor.file_picker_selected = 0;
        self.conv.editor.file_picker_preview = None;
        self.conv.editor.file_picker_previous_active = self.conv.editor.active;
        self.conv.editor.file_picker_previous_diff_active = self.conv.editor.diff_tab_active;
        self.conv.editor.file_picker_preview_created = false;
        self.conv.editor.file_picker_project_symbols = false;
        self.conv.editor.file_picker_previewed = None;
        self.conv.editor.file_picker_origin = self.conv.editor.active_document().map(|document| {
            let caret = super::editor_text::byte_index(
                &document.content,
                self.conv.editor.navigation_cursor_char,
            );
            (document.path.clone(), caret)
        });
        self.conv.editor.file_picker_open = true;
    }

    fn preview_file_picker_path(&mut self, path: &Path) {
        self.conv.editor.git_full_highlight_path = None;
        if self
            .conv
            .editor
            .file_picker_preview
            .as_deref()
            .is_some_and(|preview| preview == path)
        {
            return;
        }

        if self.conv.editor.file_picker_preview_created
            && let Some(preview) = self.conv.editor.file_picker_preview.as_ref()
            && let Some(index) = self
                .conv
                .editor
                .documents
                .iter()
                .position(|document| &document.path == preview)
        {
            self.conv.editor.documents.remove(index);
        }

        let safe_path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let was_open = self
            .conv
            .editor
            .documents
            .iter()
            .any(|document| document.path == safe_path);
        self.conv.editor.file_picker_preview = None;
        self.conv.editor.file_picker_preview_created = false;
        self.open_editor_file_impl(path.to_path_buf(), false);
        if self
            .conv
            .editor
            .active_document()
            .is_some_and(|document| document.path == safe_path)
        {
            self.conv.editor.file_picker_preview = Some(safe_path);
            self.conv.editor.file_picker_preview_created = !was_open;
        }
    }

    fn clear_file_picker_preview(&mut self) {
        if self.conv.editor.file_picker_preview_created
            && let Some(preview) = self.conv.editor.file_picker_preview.as_ref()
            && let Some(index) = self
                .conv
                .editor
                .documents
                .iter()
                .position(|document| &document.path == preview)
        {
            self.conv.editor.documents.remove(index);
        }
        self.conv.editor.active = self
            .conv
            .editor
            .file_picker_previous_active
            .filter(|&index| index < self.conv.editor.documents.len());
        self.conv.editor.diff_tab_active = self.conv.editor.file_picker_previous_diff_active;
        self.conv.editor.file_picker_preview = None;
        self.conv.editor.file_picker_preview_created = false;
    }

    pub(crate) fn cancel_file_picker(&mut self) {
        self.clear_file_picker_preview();
        // A symbol/line preview moved the caret: put it back where it was, like Sublime.
        if self.conv.editor.file_picker_previewed.take().is_some()
            && let Some((path, caret)) = self.conv.editor.file_picker_origin.take()
        {
            self.conv.editor.navigation_target = Some((path, caret..caret));
        }
        self.conv.editor.file_picker_open = false;
    }

    /// The Goto Symbol outline of `path` (an open document), with 1-based line numbers.
    fn picker_outline(&mut self, path: &Path) -> Arc<PickerOutline> {
        let Some(document) = self
            .conv
            .editor
            .documents
            .iter()
            .find(|document| document.path == path)
        else {
            return Arc::default();
        };
        if let Some((cached_path, revision, outline)) = &self.conv.editor.file_picker_outline
            && cached_path == path
            && *revision == document.content_revision
        {
            return Arc::clone(outline);
        }
        let language = crate::code_nav::language_for_path(path).unwrap_or_default();
        let mut line = 1;
        let mut counted = 0;
        let outline: Vec<_> = crate::code_nav::file_symbols(language, &document.content)
            .into_iter()
            .filter(|symbol| symbol.kind.is_outline())
            .map(|symbol| {
                let start = symbol.name_range.start;
                line += memchr::memchr_iter(b'\n', &document.content.as_bytes()[counted..start])
                    .count();
                counted = start;
                (symbol, line)
            })
            .collect();
        let outline = Arc::new(outline);
        self.conv.editor.file_picker_outline = Some((
            path.to_path_buf(),
            document.content_revision,
            Arc::clone(&outline),
        ));
        outline
    }

    /// Rows for the current query, best first.
    fn picker_entries(&mut self, root: &Path, query: &str) -> Vec<PickerEntry> {
        let relative = |path: &Path| {
            path.strip_prefix(root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/")
        };
        if self.conv.editor.file_picker_project_symbols {
            let Some(symbols) = self.conv.editor.code_nav.project_symbols.clone() else {
                return vec![PickerEntry {
                    primary: "Indexing workspace symbols…".into(),
                    primary_matches: Vec::new(),
                    secondary: String::new(),
                    secondary_matches: Vec::new(),
                    target: None,
                }];
            };
            let mut ranked: Vec<_> = symbols
                .iter()
                .filter_map(|item| {
                    fuzzy_path_score(&item.symbol.name, query).map(|score| (score, item))
                })
                .collect();
            ranked.sort_by(|a, b| {
                b.0.cmp(&a.0)
                    .then_with(|| a.1.symbol.name.cmp(&b.1.symbol.name))
            });
            return ranked
                .into_iter()
                .take(100)
                .map(|(_, item)| PickerEntry {
                    primary: item.symbol.name.clone(),
                    primary_matches: fuzzy_match_positions(&item.symbol.name, query),
                    secondary: format!("{}   {}", item.symbol.kind.label(), relative(&item.path)),
                    secondary_matches: Vec::new(),
                    target: Some(PickerTarget::Location {
                        path: item.path.clone(),
                        range: item.symbol.name_range.clone(),
                    }),
                })
                .collect();
        }

        let origin = self
            .conv
            .editor
            .file_picker_origin
            .as_ref()
            .map(|(path, _)| path.clone());
        if let Some(symbol_query) = query.strip_prefix('@') {
            let Some(path) = origin else {
                return Vec::new();
            };
            let outline = self.picker_outline(&path);
            let mut ranked: Vec<_> = outline
                .iter()
                .enumerate()
                .filter_map(|(order, (symbol, line))| {
                    let score = if symbol_query.is_empty() {
                        // No query: file order, like Sublime's outline.
                        -(order as i64)
                    } else {
                        fuzzy_path_score(&symbol.name, symbol_query)?
                    };
                    Some((score, symbol, *line))
                })
                .collect();
            ranked.sort_by_key(|entry| std::cmp::Reverse(entry.0));
            return ranked
                .into_iter()
                .map(|(_, symbol, line)| PickerEntry {
                    primary: symbol.name.clone(),
                    primary_matches: fuzzy_match_positions(&symbol.name, symbol_query),
                    secondary: format!("{}   line {line}", symbol.kind.label()),
                    secondary_matches: Vec::new(),
                    target: Some(PickerTarget::Location {
                        path: path.clone(),
                        range: symbol.name_range.clone(),
                    }),
                })
                .collect();
        }
        if let Some(line_query) = query.strip_prefix(':') {
            let Some(path) = origin else {
                return Vec::new();
            };
            let Some(document) = self
                .conv
                .editor
                .documents
                .iter()
                .find(|document| document.path == path)
            else {
                return Vec::new();
            };
            let lines = memchr::memchr_iter(b'\n', document.content.as_bytes()).count() + 1;
            let target = line_query
                .trim()
                .parse::<usize>()
                .ok()
                .map(|line| line.clamp(1, lines))
                .map(|line| (line, line_start(&document.content, line)));
            return vec![PickerEntry {
                primary: target.map_or_else(
                    || "Type a line number".to_owned(),
                    |(line, _)| format!("Go to line {line}"),
                ),
                primary_matches: Vec::new(),
                secondary: format!("of {lines}"),
                secondary_matches: Vec::new(),
                target: target.map(|(_, start)| PickerTarget::Location {
                    path,
                    range: start..start,
                }),
            }];
        }

        // `name:42` opens a file at a line.
        let (file_query, line) = match query.rsplit_once(':') {
            Some((name, line)) if !line.is_empty() && line.bytes().all(|b| b.is_ascii_digit()) => {
                (name, line.parse::<usize>().ok())
            }
            Some((name, "")) => (name, None),
            _ => (query, None),
        };
        let mut ranked = self
            .conv
            .editor
            .file_picker_files
            .iter()
            .filter_map(|path| {
                let display = relative(path);
                fuzzy_path_score(&display, file_query).map(|score| (score, display, path))
            })
            .collect::<Vec<_>>();
        ranked.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        ranked
            .into_iter()
            .take(100)
            .map(|(_, display, path)| {
                let matched = fuzzy_match_positions(&display, file_query);
                let name_start = display.rfind('/').map_or(0, |i| i + 1);
                PickerEntry {
                    primary: display[name_start..].to_owned(),
                    primary_matches: matched
                        .iter()
                        .filter(|at| **at >= name_start)
                        .map(|at| at - name_start)
                        .collect(),
                    secondary: display[..name_start.saturating_sub(1)].to_owned(),
                    secondary_matches: matched
                        .into_iter()
                        .filter(|at| *at + 1 < name_start)
                        .collect(),
                    target: Some(PickerTarget::File {
                        path: path.clone(),
                        line,
                    }),
                }
            })
            .collect()
    }

    /// Show `target` in the editor behind the picker. Files open as a temporary preview tab;
    /// locations select their range, only when it changed so the view is not re-scrolled
    /// every frame.
    fn preview_picker_target(&mut self, target: &PickerTarget) {
        let (path, range) = match target {
            PickerTarget::File { path, line } => {
                self.preview_file_picker_path(path);
                let Some(line) = line else {
                    return;
                };
                let Some(document) = self.conv.editor.active_document() else {
                    return;
                };
                let start = line_start(&document.content, *line);
                (document.path.clone(), start..start)
            }
            PickerTarget::Location { path, range } => {
                let origin = self
                    .conv
                    .editor
                    .file_picker_origin
                    .as_ref()
                    .is_some_and(|(origin, _)| origin == path);
                if origin {
                    // Symbols/lines of the file the picker was opened from: drop any file
                    // preview so that file is the one shown.
                    if self.conv.editor.file_picker_preview.is_some() {
                        self.clear_file_picker_preview();
                    }
                } else {
                    self.preview_file_picker_path(path);
                }
                (path.clone(), range.clone())
            }
        };
        let location = (path, range);
        if self.conv.editor.file_picker_previewed.as_ref() != Some(&location) {
            self.conv.editor.navigation_target = Some(location.clone());
            self.conv.editor.file_picker_previewed = Some(location);
        }
    }

    /// Enter/click: keep the previewed file as a real tab and jump there.
    fn accept_picker_target(&mut self, target: PickerTarget) {
        let origin = self.conv.editor.file_picker_origin.take();
        self.conv.editor.file_picker_previewed = None;
        self.preview_picker_target(&target);
        let path = match &target {
            PickerTarget::File { path, .. } | PickerTarget::Location { path, .. } => path.clone(),
        };
        if let Some((target_path, range)) = self.conv.editor.file_picker_previewed.take() {
            self.conv.editor.navigation_target = Some((target_path, range));
            // A symbol or line jump is a navigation: Cmd+Ctrl+- style back history returns here.
            if let Some((origin_path, caret)) = origin {
                self.conv
                    .editor
                    .navigation_back
                    .push((origin_path, caret..caret));
                self.conv.editor.navigation_forward.clear();
            }
        }
        self.conv.editor.file_picker_preview = None;
        self.conv.editor.file_picker_preview_created = false;
        self.conv.editor.file_picker_open = false;
        if matches!(target, PickerTarget::File { .. }) {
            self.reveal_editor_file_in_explorer(&path);
        }
        self.conv.editor.focus_editor_next_frame = true;
    }

    pub(crate) fn render_file_picker(&mut self, ctx: &egui::Context) {
        if !self.conv.editor.file_picker_open {
            return;
        }
        self.poll_code_navigation();
        let root = PathBuf::from(&self.active_workspace().root_path);
        let query = self.conv.editor.file_picker_query.to_ascii_lowercase();
        if query != self.conv.editor.file_picker_last_query {
            self.conv.editor.file_picker_selected = 0;
            self.conv.editor.file_picker_last_query.clone_from(&query);
        }
        let symbol_mode = self.conv.editor.file_picker_project_symbols || query.starts_with('@');
        let matches = self.picker_entries(&root, &query);

        let (arrow_up, arrow_down, enter) = ctx.input_mut(|input| {
            (
                input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                input.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
            )
        });
        let keyboard_navigation = arrow_up || arrow_down;
        let pointer_moved = ctx.input(|input| input.pointer.delta() != egui::Vec2::ZERO);
        if !matches.is_empty() {
            if arrow_down {
                self.conv.editor.file_picker_selected =
                    (self.conv.editor.file_picker_selected + 1).min(matches.len() - 1);
            } else if arrow_up {
                self.conv.editor.file_picker_selected =
                    self.conv.editor.file_picker_selected.saturating_sub(1);
            }
            self.conv.editor.file_picker_selected =
                self.conv.editor.file_picker_selected.min(matches.len() - 1);
        } else {
            self.conv.editor.file_picker_selected = 0;
        }
        let mut selected = enter
            .then(|| {
                matches
                    .get(self.conv.editor.file_picker_selected)
                    .and_then(|entry| entry.target.clone())
            })
            .flatten();
        let mut open = true;
        // Shrink for short result lists, but cap the picker so longer lists remain scrollable.
        let available = ctx.content_rect().size();
        let max_picker_height = 440.0_f32.min((available.y - 104.0).max(120.0));
        // Measure rows from the real font instead of assuming 24 px: with the default text size
        // a row is taller than that, and a short list ended with its last row cut in half.
        let style = ctx.global_style();
        let text_h =
            ctx.fonts_mut(|fonts| fonts.row_height(&egui::TextStyle::Button.resolve(&style)));
        let (interact_h, pad_y, gap_y) = (
            style.spacing.interact_size.y,
            style.spacing.button_padding.y,
            style.spacing.item_spacing.y,
        );
        let row_height = interact_h.max(text_h + 2.0 * pad_y) + gap_y;
        let query_height = interact_h.max(text_h + 8.0);
        // Whatever the list still overflowed by last frame (window chrome we can't predict).
        let overflow_id = egui::Id::new("workspace_file_picker_overflow");
        let overflow = ctx.data(|d| d.get_temp::<f32>(overflow_id)).unwrap_or(0.0);
        let picker_height =
            (16.0 + query_height + 8.0 + matches.len() as f32 * row_height + overflow)
                .clamp(120.0, max_picker_height);
        // Over the editor column when it is on screen this frame, like Sublime/VS Code's
        // palettes; otherwise (chat view) over the whole window.
        let frame_nr = ctx.cumulative_frame_nr();
        let area = self
            .conv
            .editor
            .editor_area
            .filter(|(drawn, rect)| *drawn == frame_nr && rect.width() >= 320.0)
            .map_or_else(|| ctx.content_rect(), |(_, rect)| rect);
        let picker_size = egui::vec2(
            560.0_f32.min((area.width() - 32.0).max(280.0)),
            picker_height,
        );
        let picker_pos = egui::pos2(area.center().x, area.top() + 72.0);
        // A command-palette style popup: no title bar (the hint says what it does); Escape or a
        // click outside closes it.
        let window = egui::Window::new("Open file")
            .id(egui::Id::new("workspace_file_picker"))
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .pivot(egui::Align2::CENTER_TOP)
            .fixed_pos(picker_pos)
            .fixed_size(picker_size)
            .show(ctx, |ui| {
                let response = ui.add(
                    TextEdit::singleline(&mut self.conv.editor.file_picker_query)
                        .id_salt("workspace_file_picker_query")
                        .hint_text(if self.conv.editor.file_picker_project_symbols {
                            "Go to symbol in project…"
                        } else {
                            "Go to file…  (@ symbol, : line)"
                        })
                        .desired_width(f32::INFINITY),
                );
                if !response.has_focus() {
                    response.request_focus();
                }
                ui.add_space(6.0);
                // Fill the remaining window height even when every match fits. Otherwise the
                // scroll area's clip edge can land on the final row as selection repaints.
                let list = ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .animated(false)
                    .show(ui, |ui| {
                        if matches.is_empty() {
                            ui.label(
                                RichText::new(NO_MATCHES[usize::from(symbol_mode)])
                                    .color(c_text_muted()),
                            );
                        }
                        for (match_index, entry) in matches.iter().enumerate() {
                            let response = file_picker_row(
                                ui,
                                entry,
                                match_index == self.conv.editor.file_picker_selected,
                            );
                            // Scrolling a hover-selected row every frame moves a different row under
                            // the stationary pointer, which changes selection again and causes a
                            // scroll/hover feedback loop. Only keyboard navigation needs auto-scroll.
                            if keyboard_navigation
                                && match_index == self.conv.editor.file_picker_selected
                            {
                                response.scroll_to_me(None);
                            }
                            // Keyboard navigation may scroll the list under the pointer. Do not let
                            // that same frame's hover state override the arrow-selected row.
                            if pointer_moved && !keyboard_navigation && response.hovered() {
                                self.conv.editor.file_picker_selected = match_index;
                            }
                            if response.clicked() {
                                selected = entry.target.clone();
                            }
                        }
                        ui.add_space(2.0);
                    });
                // A list longer than the cap scrolls by design; only correct a short list.
                let missing = (list.content_size.y - list.inner_rect.height())
                    .min(max_picker_height - picker_height);
                ctx.data_mut(|d| {
                    let total = d.get_temp::<f32>(overflow_id).unwrap_or(0.0);
                    d.insert_temp(overflow_id, (total + missing).clamp(0.0, 200.0));
                });
            });
        if let Some(window) = window {
            let rect = window.response.rect;
            let clicked_outside = ctx.input(|i| {
                i.pointer.any_pressed()
                    && i.pointer
                        .interact_pos()
                        .is_some_and(|pos| !rect.contains(pos))
            });
            if clicked_outside {
                open = false;
            }
        }
        if let Some(target) = selected {
            // Enter/click promotes the temporary preview to a regular editor tab.
            self.accept_picker_target(target);
        } else if !open {
            self.cancel_file_picker();
        } else {
            self.conv.editor.file_picker_open = true;
            if !query.is_empty() || self.conv.editor.file_picker_project_symbols {
                if let Some(target) = matches
                    .get(self.conv.editor.file_picker_selected)
                    .and_then(|entry| entry.target.clone())
                {
                    self.preview_picker_target(&target);
                } else if !query.starts_with(':') {
                    self.clear_file_picker_preview();
                }
            } else {
                // Cmd/Ctrl+P initially lists files without changing the editor. Preview starts only
                // after the user types at least one character.
                self.clear_file_picker_preview();
            }
        }
    }
}

/// One result row: the name (file name or symbol) in the normal text color, its context after
/// it in a faint color (directory, or symbol kind and location), matched characters in the
/// accent color. Only the keyboard/hover selection is filled — there is no separate hover fill,
/// so exactly one row is ever highlighted.
fn file_picker_row(ui: &mut egui::Ui, entry: &PickerEntry, selected: bool) -> egui::Response {
    use egui::text::{LayoutJob, TextFormat};
    let height = ui.spacing().interact_size.y + 4.0;
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::click(),
    );
    if !ui.is_rect_visible(rect) {
        return response;
    }
    if selected {
        ui.painter()
            .rect_filled(rect, egui::CornerRadius::same(RADIUS_ROW), c_row_active());
    }
    let font = egui::FontId::proportional(FS_SMALL);
    let mut job = LayoutJob::default();
    let push = |job: &mut LayoutJob, text: &str, matched: &[usize], base: egui::Color32| {
        if matched.is_empty() {
            job.append(text, 0.0, TextFormat::simple(font.clone(), base));
            return;
        }
        for (at, ch) in text.char_indices() {
            let color = if matched.contains(&at) {
                c_accent()
            } else {
                base
            };
            let mut buf = [0u8; 4];
            job.append(
                ch.encode_utf8(&mut buf),
                0.0,
                TextFormat::simple(font.clone(), color),
            );
        }
    };
    push(
        &mut job,
        &entry.primary,
        &entry.primary_matches,
        if selected { c_text_strong() } else { c_text() },
    );
    if !entry.secondary.is_empty() {
        job.append("   ", 0.0, TextFormat::simple(font.clone(), c_text_faint()));
        push(
            &mut job,
            &entry.secondary,
            &entry.secondary_matches,
            c_text_faint(),
        );
    }
    job.wrap.max_width = rect.width() - 16.0;
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job.wrap.overflow_character = Some('…');
    let galley = ui.fonts_mut(|f| f.layout_job(job));
    ui.painter().galley(
        egui::pos2(rect.left() + 8.0, rect.center().y - galley.size().y / 2.0),
        galley,
        c_text(),
    );
    if response.hovered() && entry.target.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

/// Byte offset where 1-based `line` starts (end of text past the last line).
fn line_start(content: &str, line: usize) -> usize {
    if line <= 1 {
        return 0;
    }
    memchr::memchr_iter(b'\n', content.as_bytes())
        .nth(line - 2)
        .map_or(content.len(), |newline| newline + 1)
}
