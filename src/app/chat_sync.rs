//! Chats changed on disk by another oxi process, mainly the `oxi headless` daemon that editors
//! share. A background thread stats each workspace's session folder and the open chat files
//! once a second and wakes the UI when one changed; the UI then reads idle chats again in place
//! (the composer draft stays), lists new chat files and drops removed ones. Without this a turn
//! run from an editor only showed after a restart, and continuing that chat here overwrote the
//! newer file. Headless does the same for files the app writes (`reload_if_changed`).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant, SystemTime};

use eframe::egui;

use super::{OxiApp, SessionKey};
use crate::model::Session;
use crate::session_store;

const CHECK_EVERY: Duration = Duration::from_secs(1);

#[derive(Default)]
pub(crate) struct ChatDiskWatch {
    /// What the watcher thread stats. It holds a weak handle and exits once the app is gone.
    paths: Arc<Mutex<Vec<PathBuf>>>,
    /// Set by the watcher when a watched path changed since its previous look.
    changed: Arc<AtomicBool>,
    started: bool,
    /// Session folder per workspace root; resolving one reads settings files.
    dirs: HashMap<String, PathBuf>,
    last_paths_update: Option<Instant>,
    /// Something could not be applied yet (a dialog was open), or the watch list grew and
    /// the new paths were never compared; checked again at most once per `CHECK_EVERY`.
    pending: bool,
    last_sync: Option<Instant>,
    /// Files this app deleted: their trashing runs in the background, so they may still be
    /// listed for a moment and must not come back as new chats.
    deleted: HashSet<String>,
}

impl ChatDiskWatch {
    pub(crate) fn forget_deleted(&mut self, path: String) {
        self.deleted.insert(path);
    }

    fn start(&mut self, ctx: egui::Context) {
        self.started = true;
        let paths = Arc::downgrade(&self.paths);
        let changed = Arc::clone(&self.changed);
        let spawned = std::thread::Builder::new()
            .name("oxi-chat-watch".into())
            .spawn(move || watch_loop(&paths, &changed, &ctx));
        if let Err(e) = spawned {
            log::warn!("chat watch: could not start: {e}");
        }
    }
}

fn file_mtime(path: impl AsRef<Path>) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn watch_loop(paths: &Weak<Mutex<Vec<PathBuf>>>, changed: &AtomicBool, ctx: &egui::Context) {
    let mut seen = HashMap::new();
    loop {
        std::thread::sleep(CHECK_EVERY);
        let Some(paths) = paths.upgrade() else {
            return;
        };
        let list = paths.lock().map(|p| p.clone()).unwrap_or_default();
        drop(paths);
        let now: HashMap<PathBuf, Option<SystemTime>> = list
            .into_iter()
            .map(|path| {
                let mtime = file_mtime(&path);
                (path, mtime)
            })
            .collect();
        if stamps_changed(&seen, &now) {
            changed.store(true, Ordering::Relaxed);
            ctx.request_repaint();
        }
        seen = now;
    }
}

/// Whether a path watched in both looks has a different mtime (or appeared/vanished) now.
/// Paths new to the list are only a baseline: the UI compares those itself.
fn stamps_changed(
    before: &HashMap<PathBuf, Option<SystemTime>>,
    now: &HashMap<PathBuf, Option<SystemTime>>,
) -> bool {
    now.iter()
        .any(|(path, mtime)| before.get(path).is_some_and(|old| old != mtime))
}

/// What a workspace's session folder says about the chats listed for it.
#[derive(Debug, Default, PartialEq)]
struct FolderDiff {
    /// Listed chats whose file has another mtime than when it was last read or written.
    changed: Vec<usize>,
    /// Listed chats whose file is gone.
    removed: Vec<usize>,
    /// Files no listed chat points at.
    added: Vec<String>,
}

/// Compare a workspace's chats with its session folder. A chat whose file lies outside the
/// folder (the configured folder changed meanwhile) is checked on its own.
fn diff_folder(
    dir: &Path,
    sessions: &[Session],
    on_disk: &HashMap<String, SystemTime>,
    deleted: &HashSet<String>,
) -> FolderDiff {
    let mut diff = FolderDiff::default();
    for (si, session) in sessions.iter().enumerate() {
        let Some(file) = session.session_file.as_deref() else {
            continue;
        };
        let mtime = match on_disk.get(file) {
            Some(mtime) => Some(*mtime),
            None if Path::new(file).parent() == Some(dir) => None,
            None => file_mtime(file),
        };
        match mtime {
            None => diff.removed.push(si),
            Some(mtime) if Some(mtime) != session.disk_mtime => diff.changed.push(si),
            Some(_) => {}
        }
    }
    let listed: HashSet<&str> = sessions
        .iter()
        .filter_map(|s| s.session_file.as_deref())
        .collect();
    let mut added: Vec<(&String, &SystemTime)> = on_disk
        .iter()
        .filter(|(path, _)| !listed.contains(path.as_str()) && !deleted.contains(*path))
        .collect();
    // Oldest first: each is inserted at the top, so the newest ends up first.
    added.sort_by_key(|(path, mtime)| (**mtime, (*path).clone()));
    diff.added = added.into_iter().map(|(path, _)| path.clone()).collect();
    diff
}

impl OxiApp {
    /// Read chats again that another oxi process changed on disk. Runs every frame; does
    /// real work only when the watcher saw a change.
    pub(crate) fn sync_chats_from_disk(&mut self, ctx: &egui::Context) {
        if self.conn.no_session {
            return;
        }
        if !self.chat_watch.started {
            self.chat_watch.start(ctx.clone());
        }
        if self
            .chat_watch
            .last_paths_update
            .is_none_or(|at| at.elapsed() >= CHECK_EVERY)
        {
            self.update_watched_paths();
        }
        let changed = self.chat_watch.changed.swap(false, Ordering::Relaxed);
        let retry_due = self.chat_watch.pending
            && self
                .chat_watch
                .last_sync
                .is_none_or(|at| at.elapsed() >= CHECK_EVERY);
        if !changed && !retry_due {
            return;
        }
        self.chat_watch.last_sync = Some(Instant::now());
        let mut done = true;
        for wi in 0..self.conv.workspaces.len() {
            done &= self.sync_workspace_from_disk(wi);
        }
        self.chat_watch.pending = !done;
        if !done {
            ctx.request_repaint_after(CHECK_EVERY);
        }
    }

    /// Point the watcher at every workspace's session folder and every chat read in full.
    fn update_watched_paths(&mut self) {
        self.chat_watch.last_paths_update = Some(Instant::now());
        let roots: HashSet<&str> = self
            .conv
            .workspaces
            .iter()
            .map(|w| w.root_path.as_str())
            .collect();
        self.chat_watch
            .dirs
            .retain(|root, _| roots.contains(root.as_str()));
        let mut paths = Vec::new();
        for workspace in &self.conv.workspaces {
            let dir = self
                .chat_watch
                .dirs
                .entry(workspace.root_path.clone())
                .or_insert_with(|| session_store::workspace_session_dir(&workspace.root_path));
            paths.push(dir.clone());
            paths.extend(
                workspace
                    .sessions
                    .iter()
                    .filter(|s| s.messages_loaded)
                    .filter_map(|s| s.session_file.as_ref().map(PathBuf::from)),
            );
        }
        if let Ok(mut watched) = self.chat_watch.paths.lock()
            && *watched != paths
        {
            // New paths are only a baseline for the watcher; compare them here once.
            self.chat_watch.pending |= paths.iter().any(|p| !watched.contains(p));
            *watched = paths;
        }
    }

    /// Bring workspace `wi`'s chats in line with its session folder. Returns false when part
    /// of it has to wait.
    fn sync_workspace_from_disk(&mut self, wi: usize) -> bool {
        let root = self.conv.workspaces[wi].root_path.clone();
        let Some(dir) = self.chat_watch.dirs.get(&root).cloned() else {
            return true;
        };
        let Some(files) = session_store::list_session_files(&dir) else {
            return true;
        };
        let on_disk: HashMap<String, SystemTime> = files.into_iter().collect();
        self.chat_watch
            .deleted
            .retain(|path| on_disk.contains_key(path));
        let diff = diff_folder(
            &dir,
            &self.conv.workspaces[wi].sessions,
            &on_disk,
            &self.chat_watch.deleted,
        );
        for &si in &diff.changed {
            let key = self.session_key(wi, si);
            if !self.chat_is_busy(key) {
                self.reload_session_from_disk(key);
            }
        }
        let active = self.conv.workspaces[wi].active;
        let removed: Vec<usize> = diff
            .removed
            .into_iter()
            .filter(|&si| si != active && !self.chat_is_busy(self.session_key(wi, si)))
            .collect();
        if removed.is_empty() && diff.added.is_empty() {
            return true;
        }
        // Rows are addressed by index in dialogs and menus; wait until none is open.
        if !self.session_rows_can_shift(wi) {
            return false;
        }
        for &si in removed.iter().rev() {
            log::info!(
                "chat sync: {:?} removed on disk",
                self.conv.workspaces[wi].sessions[si].session_file
            );
            self.remove_session_row(wi, si);
        }
        for path in diff.added {
            if let Some(session) = session_store::load_session_summary(&path) {
                log::info!("chat sync: {path} added on disk");
                self.insert_session_row(wi, session);
            }
        }
        // A workspace left with only blank placeholders still shows a chat to type into.
        true
    }

    /// The chat is in use here: a turn is streaming or about to, it is being compacted, or its
    /// last prompt is being edited. Its file is this app's to write.
    fn chat_is_busy(&self, key: SessionKey) -> bool {
        self.run_state(key)
            .is_some_and(|run| run.waiting_response || run.agent_rx.is_some())
            || self.compaction_active_for(key)
            || self
                .session_by_key(key)
                .messages
                .last()
                .is_some_and(|m| m.streaming)
            || (key == self.active_session_key()
                && self.conv.composer.editing_last_prompt.is_some())
    }

    /// Read `key`'s chat again when its file changed since this app last read or wrote it.
    /// Called before a prompt goes out, so a turn run elsewhere a moment ago is not lost.
    pub(crate) fn reload_session_if_changed(&mut self, key: SessionKey) {
        let session = self.session_by_key(key);
        let Some(file) = session.session_file.as_deref() else {
            return;
        };
        if file_mtime(file).is_some_and(|m| Some(m) != session.disk_mtime)
            && !self.chat_is_busy(key)
        {
            self.reload_session_from_disk(key);
        }
    }

    /// Replace `key`'s transcript, title and config with its file's, keeping the draft.
    fn reload_session_from_disk(&mut self, key: SessionKey) {
        let Some(file) = self.session_by_key(key).session_file.clone() else {
            return;
        };
        let loaded = self.session_by_key(key).messages_loaded;
        // An unopened chat only needs its sidebar entry; the transcript is read on open.
        let fresh = if loaded {
            session_store::load_session(&file)
        } else {
            session_store::load_session_summary(&file)
        };
        let Some(fresh) = fresh else {
            return;
        };
        log::info!("chat sync: {file} changed on disk; reading it again");
        let session = self.session_mut_by_key(key);
        session.title = fresh.title;
        session.config = fresh.config;
        session.modified = fresh.modified;
        session.disk_mtime = fresh.disk_mtime;
        if !loaded {
            return;
        }
        session.messages = fresh.messages;
        session.wire_cache = fresh.wire_cache;
        session.agent_context = None;
        // The agent subprocess this app keeps for the chat did not see the new turns; the
        // next prompt resumes its session from the agent's own store, which did.
        self.acp.stop(&file);
        let (wi, si) = (key.workspace_idx, key.session_idx);
        self.conv
            .transcript
            .heights
            .retain(|&(w, s, _), _| (w, s) != (wi, si));
        self.conv.sidebar.search_cache.remove(&(wi, si));
        if let Some(run) = self.flow.sessions.get_mut(&key) {
            run.stream_error = None;
        }
        if key == self.active_session_key() {
            // The chat may now use another model; show it, or the next config capture would
            // write the old one back.
            self.restore_active_session_config();
            self.conv.transcript.scroll_to_bottom_once = true;
        }
    }

    /// Whether rows of workspace `wi` may move: nothing on screen holds one by index.
    fn session_rows_can_shift(&self, wi: usize) -> bool {
        self.conv.confirm_prompt.is_none()
            && self
                .conv
                .sidebar
                .renaming_session
                .is_none_or(|(w, _)| w != wi)
            && self
                .conv
                .transcript
                .message_menu
                .as_ref()
                .is_none_or(|m| m.workspace_idx != wi)
    }

    /// List a chat another process created at the top of workspace `wi`.
    fn insert_session_row(&mut self, wi: usize, session: Session) {
        self.conv.workspaces[wi].sessions.insert(0, session);
        self.conv.workspaces[wi].active += 1;
        self.shift_session_rows(wi, |si| Some(si + 1));
    }

    /// Drop a chat whose file another process deleted. Never the open one.
    fn remove_session_row(&mut self, wi: usize, idx: usize) {
        self.conv.workspaces[wi].sessions.remove(idx);
        if idx < self.conv.workspaces[wi].active {
            self.conv.workspaces[wi].active -= 1;
        }
        self.shift_session_rows(wi, |si| match si.cmp(&idx) {
            std::cmp::Ordering::Less => Some(si),
            std::cmp::Ordering::Equal => None,
            std::cmp::Ordering::Greater => Some(si - 1),
        });
    }

    /// Re-key per-chat state of workspace `wi` after its rows moved; `None` drops it.
    fn shift_session_rows(&mut self, wi: usize, map: impl Fn(usize) -> Option<usize>) {
        self.flow.sessions = std::mem::take(&mut self.flow.sessions)
            .into_iter()
            .filter_map(|(mut key, state)| {
                if key.workspace_idx == wi {
                    key.session_idx = map(key.session_idx)?;
                }
                Some((key, state))
            })
            .collect();
        if wi == self.conv.active_workspace {
            let pending = self.flow.pending_session_idx.and_then(&map);
            self.flow.pending_session_idx = pending;
            self.flow.pending_load_session_idx = pending;
        }
        if let Some(c) = self.conv.compaction.as_mut()
            && c.key.workspace_idx == wi
        {
            match map(c.key.session_idx) {
                Some(si) => c.key.session_idx = si,
                None => self.conv.compaction = None,
            }
        }
        self.conv.transcript.heights.retain(|&(w, _, _), _| w != wi);
        self.conv.sidebar.search_cache.retain(|&(w, _), _| w != wi);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(file: Option<&str>, disk_mtime: Option<SystemTime>) -> Session {
        let mut session = OxiApp::blank_session("Chat");
        session.session_file = file.map(str::to_string);
        session.disk_mtime = disk_mtime;
        session
    }

    #[test]
    fn diff_folder_finds_changed_removed_and_added_chats() {
        let dir = Path::new("/sessions");
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        let t1 = t0 + Duration::from_secs(1);
        let sessions = vec![
            session(None, None),
            session(Some("/sessions/same.jsonl"), Some(t0)),
            session(Some("/sessions/edited.jsonl"), Some(t0)),
            session(Some("/sessions/gone.jsonl"), Some(t0)),
        ];
        let on_disk: HashMap<String, SystemTime> = [
            ("/sessions/same.jsonl", t0),
            ("/sessions/edited.jsonl", t1),
            ("/sessions/newer.jsonl", t1),
            ("/sessions/older.jsonl", t0),
            ("/sessions/trashing.jsonl", t1),
        ]
        .into_iter()
        .map(|(p, t)| (p.to_string(), t))
        .collect();
        let deleted = HashSet::from(["/sessions/trashing.jsonl".to_string()]);

        let diff = diff_folder(dir, &sessions, &on_disk, &deleted);

        assert_eq!(
            diff,
            FolderDiff {
                changed: vec![2],
                removed: vec![3],
                added: vec![
                    "/sessions/older.jsonl".to_string(),
                    "/sessions/newer.jsonl".to_string(),
                ],
            }
        );
    }

    #[test]
    fn diff_folder_after_own_save_is_empty() {
        let root = std::env::temp_dir().join(format!(
            "oxi-chat-sync-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let dir = root.join("sessions");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("chat.jsonl");
        let mut chat = session(Some(file.to_str().unwrap()), None);
        chat.messages
            .push(crate::model::ChatMessage::user("hello", &[]));
        session_store::save_session_messages(root.to_str().unwrap(), &mut chat).unwrap();
        let on_disk: HashMap<String, SystemTime> = session_store::list_session_files(&dir)
            .unwrap()
            .into_iter()
            .collect();
        let sessions = vec![chat];

        assert_eq!(
            diff_folder(&dir, &sessions, &on_disk, &HashSet::new()),
            FolderDiff::default()
        );

        // Another process writes the chat: its mtime moves on.
        let later = SystemTime::now() + Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(later)
            .unwrap();
        let on_disk: HashMap<String, SystemTime> = session_store::list_session_files(&dir)
            .unwrap()
            .into_iter()
            .collect();
        assert_eq!(
            diff_folder(&dir, &sessions, &on_disk, &HashSet::new()).changed,
            vec![0]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn watcher_ignores_new_paths_and_flags_changed_ones() {
        let t0 = Some(SystemTime::UNIX_EPOCH);
        let t1 = Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1));
        let before = HashMap::from([(PathBuf::from("a"), t0)]);
        let added = HashMap::from([(PathBuf::from("a"), t0), (PathBuf::from("b"), t1)]);
        assert!(!stamps_changed(&before, &added));
        let edited = HashMap::from([(PathBuf::from("a"), t1)]);
        assert!(stamps_changed(&before, &edited));
        let removed = HashMap::from([(PathBuf::from("a"), None)]);
        assert!(stamps_changed(&before, &removed));
    }
}
