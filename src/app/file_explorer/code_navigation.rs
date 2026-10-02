//! Tree-sitter go to definition and Goto Symbol in Project, off the UI thread.
//!
//! The first lookup in a workspace parses its source files once on a worker; later lookups
//! reuse [`SymbolIndex`]'s per-file cache and only stat the tree, so F12 / Cmd+click stays
//! cheap even in large projects.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError};

use eframe::egui;
use walkdir::WalkDir;

use crate::code_nav::{DefinitionLocation, DefinitionRequest, Symbol, SymbolIndex};

use super::super::OxiApp;
use super::support::{load_gitignore_patterns, should_ignore};

/// Upper bound on files a workspace index walks, so a huge monorepo cannot stall a lookup.
const MAX_WORKSPACE_FILES: usize = 30_000;

/// Directories that never hold the user's own definitions, whether or not they are ignored.
const SKIPPED_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    ".venv",
    "venv",
    "__pycache__",
    "dist",
    "build",
    ".next",
];

/// A definition in the workspace, for Goto Symbol in Project.
#[derive(Clone)]
pub(crate) struct ProjectSymbol {
    pub path: PathBuf,
    pub symbol: Symbol,
}

struct DefinitionJob {
    /// Where the lookup started, recorded in the back history when it lands.
    origin: (PathBuf, usize),
    result: Receiver<Option<DefinitionLocation>>,
}

/// Workspace symbol index plus the lookups running on worker threads.
#[derive(Default)]
pub(crate) struct CodeNavState {
    index: Arc<SymbolIndex>,
    definition: Option<DefinitionJob>,
    /// `(workspace root, language family)` pairs already indexed in the background.
    warmed: Vec<(String, &'static str)>,
    project_symbols_job: Option<Receiver<Vec<ProjectSymbol>>>,
    /// Outline symbols of the whole workspace, once Goto Symbol in Project has loaded them.
    pub project_symbols: Option<Arc<Vec<ProjectSymbol>>>,
}

/// Source files under `root` that navigation understands, skipping ignored and vendored trees.
fn workspace_source_files(root: &Path) -> Vec<PathBuf> {
    let ignored = load_gitignore_patterns(root);
    WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            if entry.path() == root {
                return true;
            }
            let directory = entry.file_type().is_dir();
            let name = entry.file_name().to_string_lossy();
            !(directory && SKIPPED_DIRS.contains(&name.as_ref()))
                && !should_ignore(root, entry.path(), directory, &ignored)
        })
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .filter(|entry| crate::code_nav::language_for_path(entry.path()).is_some())
        .take(MAX_WORKSPACE_FILES)
        .map(|entry| entry.into_path())
        .collect()
}

fn canonical_root(root: &str) -> PathBuf {
    std::fs::canonicalize(root).unwrap_or_else(|_| PathBuf::from(root))
}

fn spawn(name: &str, work: impl FnOnce() + Send + 'static) {
    let _ = std::thread::Builder::new().name(name.into()).spawn(work);
}

impl OxiApp {
    /// Index the active document's language family in the background the first time it is
    /// opened, so the first F12 does not wait for a whole-workspace parse.
    pub(super) fn prewarm_code_navigation(&mut self, language: &str) {
        let Some(family) = crate::code_nav::family_of(language) else {
            return;
        };
        // Called every editor frame: compare the raw root, canonicalize only on the worker.
        let root = &self.conv.workspaces[self.conv.active_workspace].root_path;
        let state = &mut self.conv.editor.code_nav;
        if state
            .warmed
            .iter()
            .any(|(warmed_root, warmed)| warmed_root == root && *warmed == family)
        {
            return;
        }
        state.warmed.push((root.clone(), family));
        let root = root.clone();
        let index = Arc::clone(&state.index);
        spawn("oxi-symbol-index", move || {
            let files: Vec<_> = workspace_source_files(&canonical_root(&root))
                .into_iter()
                .filter(|path| {
                    crate::code_nav::language_for_path(path).and_then(crate::code_nav::family_of)
                        == Some(family)
                })
                .collect();
            index.symbols_for(&files, &HashMap::new());
        });
    }

    /// Look up the definition of the identifier at `cursor_byte` on a worker; the jump happens
    /// in [`Self::poll_code_navigation`] when it answers.
    pub(super) fn go_to_definition(&mut self, cursor_byte: usize, ctx: &egui::Context) {
        let Some(document) = self.conv.editor.active_document() else {
            return;
        };
        let Some(language) = crate::code_nav::language_for_path(&document.path) else {
            return;
        };
        let request = DefinitionRequest {
            current_path: document.path.clone(),
            current_source: document.content.clone(),
            cursor_byte,
            workspace_files: Vec::new(),
            open_buffers: self
                .conv
                .editor
                .documents
                .iter()
                .filter(|other| other.media.is_none() && other.path != document.path)
                .filter(|other| {
                    crate::code_nav::language_for_path(&other.path)
                        .is_some_and(|other| crate::code_nav::same_family(language, other))
                })
                .map(|other| (other.path.clone(), other.content.clone()))
                .collect(),
        };
        let origin = (document.path.clone(), cursor_byte);
        let root = canonical_root(&self.active_workspace().root_path);
        let index = Arc::clone(&self.conv.editor.code_nav.index);
        let (sender, receiver) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        spawn("oxi-goto-definition", move || {
            let mut request = request;
            request.workspace_files = workspace_source_files(&root);
            let _ = sender.send(crate::code_nav::find_definition(&index, &request));
            ctx.request_repaint();
        });
        // A newer request supersedes one still running: its answer is dropped with the receiver.
        self.conv.editor.code_nav.definition = Some(DefinitionJob {
            origin,
            result: receiver,
        });
    }

    /// Start loading workspace symbols for Goto Symbol in Project (Cmd+Shift+R).
    pub(super) fn load_project_symbols(&mut self, ctx: &egui::Context) {
        let root = canonical_root(&self.active_workspace().root_path);
        let open_buffers: Vec<(PathBuf, String)> = self
            .conv
            .editor
            .documents
            .iter()
            .filter(|document| document.media.is_none() && document.is_dirty())
            .map(|document| (document.path.clone(), document.content.clone()))
            .collect();
        let index = Arc::clone(&self.conv.editor.code_nav.index);
        let (sender, receiver) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        spawn("oxi-project-symbols", move || {
            let files = workspace_source_files(&root);
            let overrides: HashMap<PathBuf, &str> = open_buffers
                .iter()
                .map(|(path, text)| (path.clone(), text.as_str()))
                .collect();
            let symbols = index
                .symbols_for(&files, &overrides)
                .into_iter()
                .flat_map(|(path, symbols)| {
                    symbols
                        .iter()
                        .filter(|symbol| symbol.kind.is_outline())
                        .map(|symbol| ProjectSymbol {
                            path: path.clone(),
                            symbol: symbol.clone(),
                        })
                        .collect::<Vec<_>>()
                })
                .collect();
            let _ = sender.send(symbols);
            ctx.request_repaint();
        });
        self.conv.editor.code_nav.project_symbols_job = Some(receiver);
    }

    /// Adopt finished worker results: jump to a resolved definition, store project symbols.
    pub(crate) fn poll_code_navigation(&mut self) {
        let state = &mut self.conv.editor.code_nav;
        if let Some(job) = &state.project_symbols_job {
            match job.try_recv() {
                Ok(symbols) => {
                    state.project_symbols = Some(Arc::new(symbols));
                    state.project_symbols_job = None;
                }
                Err(TryRecvError::Disconnected) => state.project_symbols_job = None,
                Err(TryRecvError::Empty) => {}
            }
        }
        let Some(job) = &state.definition else {
            return;
        };
        let answer = match job.result.try_recv() {
            Ok(answer) => answer,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => None,
        };
        let Some(DefinitionJob { origin, .. }) = state.definition.take() else {
            return;
        };
        match answer {
            Some(location) => {
                let (path, byte) = origin;
                self.conv.editor.navigation_back.push((path, byte..byte));
                self.conv.editor.navigation_forward.clear();
                self.open_editor_file(location.path.clone());
                self.conv.editor.navigation_target = Some((location.path, location.byte_range));
                // Jumping to a definition opens/reuses a tab without focus: hand focus back
                // so the caret is live at the target selection, ready to keep editing.
                self.conv.editor.focus_editor_next_frame = true;
                self.conv.editor.error = None;
            }
            None => {
                self.conv.editor.error = Some("Definition not found.".into());
            }
        }
    }
}
