//! Hot exit for the composer, like Sublime's: unsent drafts are written to `drafts.json` shortly
//! after they change and put back on the next start, so a restart, update or crash loses nothing.
//!
//! Drafts are keyed by chat file. A chat that has not been sent yet has no file; its draft is
//! keyed by workspace (`new:<root>`) and comes back as a fresh chat at the top of that workspace.
//! Only text is kept: pasted images can be megabytes and are cheap to attach again.

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use super::OxiApp;

/// Typing bursts are written once they pause for this long.
const WRITE_AFTER: Duration = Duration::from_millis(800);

/// Draft text by chat key, as stored.
type Drafts = BTreeMap<String, String>;

fn drafts_path() -> PathBuf {
    crate::app_dirs::data_dir().join("drafts.json")
}

fn new_chat_key(root: &str) -> String {
    format!("new:{root}")
}

/// Change tracking and the writer thread. The write (with its fsync) stays off the UI thread.
#[derive(Default)]
pub(crate) struct DraftStore {
    written_hash: Option<u64>,
    changed_at: Option<Instant>,
    writer: Option<Sender<Vec<u8>>>,
}

impl DraftStore {
    fn write(&mut self, content: Vec<u8>) {
        let writer = self.writer.get_or_insert_with(|| {
            let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
            std::thread::Builder::new()
                .name("oxi-drafts".into())
                .spawn(move || {
                    while let Ok(mut content) = rx.recv() {
                        // Only the newest state matters.
                        while let Ok(newer) = rx.try_recv() {
                            content = newer;
                        }
                        if let Err(e) =
                            crate::fsutil::write_atomic_private(&drafts_path(), &content)
                        {
                            log::warn!("drafts: could not save: {e}");
                        }
                    }
                })
                .expect("spawn drafts writer");
            tx
        });
        let _ = writer.send(content);
    }
}

impl OxiApp {
    /// Every non-blank draft: the active chat's lives in the composer, the others on their session.
    fn collect_drafts(&self) -> Drafts {
        let active = self.active_session_key();
        let mut drafts = Drafts::new();
        for (wi, workspace) in self.conv.workspaces.iter().enumerate() {
            for (si, session) in workspace.sessions.iter().enumerate() {
                let text = if (wi, si) == (active.workspace_idx, active.session_idx) {
                    &self.conv.composer.input
                } else {
                    &session.input_text
                };
                if text.trim().is_empty() {
                    continue;
                }
                let key = session
                    .session_file
                    .clone()
                    .unwrap_or_else(|| new_chat_key(&workspace.root_path));
                drafts.insert(key, text.clone());
            }
        }
        drafts
    }

    fn drafts_hash(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        let active = self.active_session_key();
        for (wi, workspace) in self.conv.workspaces.iter().enumerate() {
            for (si, session) in workspace.sessions.iter().enumerate() {
                let text = if (wi, si) == (active.workspace_idx, active.session_idx) {
                    &self.conv.composer.input
                } else {
                    &session.input_text
                };
                if !text.trim().is_empty() {
                    (&session.session_file, &workspace.root_path, text).hash(&mut hasher);
                }
            }
        }
        hasher.finish()
    }

    /// Once per frame: notice changed drafts and write them after a pause in typing.
    pub(crate) fn persist_drafts(&mut self, ctx: &eframe::egui::Context) {
        let hash = self.drafts_hash();
        let store = &mut self.drafts;
        let Some(written) = store.written_hash else {
            // The first frame shows what was loaded; nothing to write yet.
            store.written_hash = Some(hash);
            return;
        };
        if hash == written {
            store.changed_at = None;
            return;
        }
        let changed_at = *store.changed_at.get_or_insert_with(Instant::now);
        let due = changed_at + WRITE_AFTER;
        if Instant::now() < due {
            ctx.request_repaint_after(due - Instant::now());
            return;
        }
        self.flush_drafts();
    }

    /// Write the drafts once typing pauses, on the writer thread.
    fn flush_drafts(&mut self) {
        let hash = self.drafts_hash();
        let content = serde_json::to_vec_pretty(&self.collect_drafts()).unwrap_or_default();
        let store = &mut self.drafts;
        store.written_hash = Some(hash);
        store.changed_at = None;
        store.write(content);
    }

    /// On exit: write right away, as the writer thread may not get to run.
    pub(crate) fn flush_drafts_now(&mut self) {
        if self.drafts.written_hash == Some(self.drafts_hash()) {
            return;
        }
        let content = serde_json::to_vec_pretty(&self.collect_drafts()).unwrap_or_default();
        if let Err(e) = crate::fsutil::write_atomic_private(&drafts_path(), &content) {
            log::warn!("drafts: could not save on exit: {e}");
        }
    }

    /// Put saved drafts back on their chats. Called once at startup.
    pub(crate) fn restore_drafts(&mut self) {
        let Ok(content) = std::fs::read(drafts_path()) else {
            return;
        };
        let Ok(mut drafts) = serde_json::from_slice::<Drafts>(&content) else {
            log::warn!("drafts: unreadable {}", drafts_path().display());
            return;
        };
        self.apply_drafts(&mut drafts);
    }

    fn apply_drafts(&mut self, drafts: &mut Drafts) {
        let active = self.active_session_key();
        for wi in 0..self.conv.workspaces.len() {
            let root = self.conv.workspaces[wi].root_path.clone();
            if let Some(text) = drafts.remove(&new_chat_key(&root)) {
                let workspace = &mut self.conv.workspaces[wi];
                match workspace
                    .sessions
                    .iter()
                    .position(|s| s.session_file.is_none())
                {
                    Some(si) => workspace.sessions[si].input_text = text,
                    None => {
                        let mut blank = Self::blank_session("New chat");
                        blank.input_text = text;
                        workspace.sessions.insert(0, blank);
                        workspace.active += 1;
                    }
                }
            }
            for session in &mut self.conv.workspaces[wi].sessions {
                if let Some(file) = &session.session_file
                    && let Some(text) = drafts.remove(file)
                {
                    session.input_text = text;
                }
            }
        }
        // The active chat's draft belongs in the composer.
        let wi = active.workspace_idx;
        if let Some(workspace) = self.conv.workspaces.get_mut(wi) {
            let si = workspace.active;
            if self.conv.composer.input.is_empty() {
                self.conv.composer.input = std::mem::take(&mut workspace.sessions[si].input_text);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drafts_come_back_on_their_chats_and_new_chats_are_recreated() {
        crate::secrets::use_mock_store();
        let mut app = OxiApp::new();
        let mut first = OxiApp::blank_session("First");
        first.session_file = Some("/chats/first.jsonl".into());
        let mut second = OxiApp::blank_session("Second");
        second.session_file = Some("/chats/second.jsonl".into());
        app.conv.workspaces = vec![crate::app::Workspace {
            root_path: "/work".into(),
            sessions: vec![first, second],
            active: 1,
            sidebar_folded: false,
            pinned: vec![],
            archived: Vec::new(),
            folded_groups: vec![],
            worktree: None,
        }];
        app.conv.active_workspace = 0;
        app.conv.composer.input.clear();

        let mut drafts = Drafts::from([
            (
                "/chats/first.jsonl".to_string(),
                "half a thought".to_string(),
            ),
            (
                "/chats/second.jsonl".to_string(),
                "active draft".to_string(),
            ),
            (new_chat_key("/work"), "a new idea".to_string()),
            ("/chats/gone.jsonl".to_string(), "orphan".to_string()),
        ]);
        app.apply_drafts(&mut drafts);

        let ws = &app.conv.workspaces[0];
        assert_eq!(ws.sessions.len(), 3, "the unsent chat is recreated");
        assert_eq!(ws.sessions[0].input_text, "a new idea");
        assert_eq!(ws.sessions[1].input_text, "half a thought");
        assert_eq!(ws.active, 2, "the active chat stays the same chat");
        assert_eq!(app.conv.composer.input, "active draft");
        assert_eq!(drafts.len(), 1, "drafts of deleted chats are dropped");

        // Round trip: what is collected is what was restored.
        let collected = app.collect_drafts();
        assert_eq!(
            collected.get("/chats/second.jsonl").unwrap(),
            "active draft"
        );
        assert_eq!(collected.get(&new_chat_key("/work")).unwrap(), "a new idea");
        assert_eq!(collected.len(), 3);
    }
}
