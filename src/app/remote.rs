//! Remote workspaces: folders on another machine whose chats an oxi server there runs (see
//! [`crate::remote`]). Their roots are `ssh://host/path`.
//!
//! The app keeps a copy of each remote chat like a local one, so the transcript, composer and
//! approvals work unchanged; what differs is where it comes from and goes to. The chat list and
//! transcripts are asked for over the connection, a prompt starts the turn on the server, and
//! the turn's events arrive on the same channel a local turn feeds. The server saves the chats;
//! the app only sends its changes (a rename, a rewound transcript) and its history with a prompt
//! when the server's differs.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use eframe::egui;
use serde_json::{Value, json};

use super::{OxiApp, PendingApproval, SessionKey};
use crate::agent::AgentEvent;
use crate::model::{MsgRole, Session, UserAttachment};
use crate::remote::client::{Client, ConnState, Note, Pending};
use crate::remote::protocol;

/// What an answer is for.
enum Asked {
    List {
        root: String,
    },
    /// Opening a chat: its snapshot, and the events after it if a turn runs.
    Load {
        file: String,
        rx: Receiver<AgentEvent>,
    },
    /// `prompt.send` of the run whose cancel flag this is.
    Turn {
        cancel: Arc<AtomicBool>,
        host: String,
    },
    /// The finished turn's messages as the server stored them, from `from` on.
    Reconcile {
        file: String,
        from: usize,
    },
    Browse,
    /// Shown as a sidebar notice when it fails.
    Action(&'static str),
}

pub(crate) struct RemoteState {
    clients: HashMap<String, Client>,
    notes_tx: Sender<Note>,
    notes_rx: Receiver<Note>,
    /// Set on the first frame; the connection threads wake the UI through it.
    ctx: Arc<Mutex<Option<egui::Context>>>,
    asked: Vec<(Asked, Pending)>,
    /// Chat files being loaded, and those whose load failed (tried again on reconnect).
    loading: HashSet<String>,
    load_failed: HashSet<String>,
    /// Roots being listed, and those to list again once that answer is in.
    listing: HashSet<String>,
    relist: HashSet<String>,
    /// Hosts connected at least once: connecting again is a reconnect.
    was_connected: HashSet<String>,
    pub(crate) dialog: Option<ConnectDialog>,
}

impl Default for RemoteState {
    fn default() -> Self {
        let (notes_tx, notes_rx) = std::sync::mpsc::channel();
        Self {
            clients: HashMap::new(),
            notes_tx,
            notes_rx,
            ctx: Arc::new(Mutex::new(None)),
            asked: Vec::new(),
            loading: HashSet::new(),
            load_failed: HashSet::new(),
            listing: HashSet::new(),
            relist: HashSet::new(),
            was_connected: HashSet::new(),
            dialog: None,
        }
    }
}

/// The "Connect to host" dialog: pick a host, then a folder on it.
#[derive(Default)]
pub(crate) struct ConnectDialog {
    pub host: String,
    /// Hosts from `~/.ssh/config`.
    pub suggestions: Vec<String>,
    /// The host the folder browser is on, once connecting started.
    pub connected_host: Option<String>,
    pub path: String,
    pub parent: Option<String>,
    pub entries: Vec<(String, bool)>,
    pub error: Option<String>,
    pub browsing: bool,
    pub focus_host: bool,
}

impl RemoteState {
    fn client(&mut self, host: &str) -> &Client {
        if !self.clients.contains_key(host) {
            let ctx = Arc::clone(&self.ctx);
            let waker: crate::remote::client::Waker = Arc::new(move || {
                if let Some(ctx) = ctx.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                    ctx.request_repaint();
                }
            });
            let client = Client::connect(host, None, self.notes_tx.clone(), waker);
            self.clients.insert(host.to_string(), client);
        }
        &self.clients[host]
    }

    fn connected(&self, host: &str) -> Option<&Client> {
        self.clients
            .get(host)
            .filter(|c| c.state() == ConnState::Connected)
    }

    pub(crate) fn state(&self, host: &str) -> Option<ConnState> {
        self.clients.get(host).map(Client::state)
    }
}

fn str_of(v: &Value, key: &str) -> String {
    v[key].as_str().unwrap_or_default().to_string()
}

fn modified_from(v: &Value) -> SystemTime {
    v["modified"]
        .as_u64()
        .map(|secs| SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
        .unwrap_or_else(SystemTime::now)
}

fn attachment_json(attachment: &UserAttachment) -> Value {
    use base64::Engine as _;
    match attachment {
        UserAttachment::Image { mime, data } => json!({
            "mime": mime,
            "data": base64::engine::general_purpose::STANDARD.encode(data),
        }),
        UserAttachment::Text { name, text } => json!({"name": name, "text": text}),
    }
}

impl OxiApp {
    pub(crate) fn is_remote_workspace(&self, wi: usize) -> bool {
        self.conv
            .workspaces
            .get(wi)
            .is_some_and(|w| crate::remote::is_remote(&w.root_path))
    }

    /// Where a turn's snapshot repository is from here: a remote chat's repository path is
    /// one on its host.
    pub(crate) fn turn_repo(&self, repo: &str) -> std::path::PathBuf {
        match crate::remote::host_of(&self.active_workspace().root_path) {
            Some(host) if !crate::remote::is_remote(repo) => {
                std::path::PathBuf::from(crate::remote::location(host, repo))
            }
            _ => std::path::PathBuf::from(repo),
        }
    }

    pub(crate) fn is_remote_key(&self, key: SessionKey) -> bool {
        self.is_remote_workspace(key.workspace_idx)
    }

    /// Connect to the hosts of the remote workspaces restored at start.
    pub(crate) fn connect_remote_workspaces(&mut self) {
        let hosts: HashSet<String> = self
            .conv
            .workspaces
            .iter()
            .filter_map(|w| crate::remote::host_of(&w.root_path).map(str::to_string))
            .collect();
        for host in hosts {
            self.remote.client(&host);
        }
    }

    /// The row of the chat whose session file is `file`.
    fn key_for_file(&self, file: &str) -> Option<SessionKey> {
        self.conv.workspaces.iter().enumerate().find_map(|(wi, w)| {
            w.sessions
                .iter()
                .position(|s| s.session_file.as_deref() == Some(file))
                .map(|si| SessionKey {
                    workspace_idx: wi,
                    session_idx: si,
                })
        })
    }

    /// Workspaces showing folder `path` of `host`.
    fn workspaces_at(&self, host: &str, path: &str) -> Vec<usize> {
        (0..self.conv.workspaces.len())
            .filter(|&wi| {
                crate::remote::split(&self.conv.workspaces[wi].root_path)
                    .is_some_and(|(h, p)| h == host && p == path)
            })
            .collect()
    }

    /// Handle what the connections delivered. Runs every frame, before the agent events.
    pub(crate) fn drain_remote(&mut self, ctx: &egui::Context) {
        self.drop_unused_remote_clients();
        {
            let mut slot = self.remote.ctx.lock().unwrap_or_else(|e| e.into_inner());
            if slot.is_none() {
                *slot = Some(ctx.clone());
            }
        }
        while let Ok(note) = self.remote.notes_rx.try_recv() {
            match note {
                Note::State { host, state } => self.on_remote_state(&host, state),
                Note::Server {
                    host,
                    method,
                    params,
                } => self.on_remote_notification(&host, &method, &params),
                Note::Resync { host, chat, result } => {
                    let file = crate::remote::location(&host, &chat);
                    if let Some(key) = self.key_for_file(&file) {
                        self.apply_remote_snapshot(key, &result);
                    }
                }
            }
            ctx.request_repaint();
        }
        let asked = std::mem::take(&mut self.remote.asked);
        for (what, pending) in asked {
            match pending.try_recv() {
                Ok(result) => {
                    self.on_remote_answer(what, result);
                    ctx.request_repaint();
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    self.remote.asked.push((what, pending))
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {}
            }
        }
    }

    fn on_remote_state(&mut self, host: &str, state: ConnState) {
        if let Some(dialog) = self.remote.dialog.as_mut()
            && dialog.connected_host.as_deref() == Some(host)
        {
            match &state {
                ConnState::Connected => {
                    if dialog.entries.is_empty() && !dialog.browsing {
                        let start = if dialog.path.is_empty() {
                            "~".to_string()
                        } else {
                            dialog.path.clone()
                        };
                        self.remote_browse(&start);
                    }
                }
                ConnState::Failed(e) => {
                    dialog.error = Some(e.clone());
                }
                ConnState::Connecting => {}
            }
        }
        if state != ConnState::Connected {
            return;
        }
        self.remote
            .load_failed
            .retain(|f| crate::remote::host_of(f) != Some(host));
        if !self.remote.was_connected.insert(host.to_string()) {
            // Back after a drop: what was read before may be out of date. Idle chats are read
            // again when shown (the open one right away); followed turns re-attach themselves.
            for wi in 0..self.conv.workspaces.len() {
                if crate::remote::host_of(&self.conv.workspaces[wi].root_path) != Some(host) {
                    continue;
                }
                for si in 0..self.conv.workspaces[wi].sessions.len() {
                    let key = self.session_key(wi, si);
                    let session = self.session_by_key(key);
                    if session.session_file.is_some()
                        && session.messages_loaded
                        && !self.chat_is_busy(key)
                    {
                        self.session_mut_by_key(key).messages_loaded = false;
                    }
                }
            }
            self.ensure_active_session_loaded();
        }
        for wi in 0..self.conv.workspaces.len() {
            if crate::remote::host_of(&self.conv.workspaces[wi].root_path) == Some(host) {
                self.remote_list_chats(wi);
            }
        }
    }

    fn on_remote_notification(&mut self, host: &str, method: &str, params: &Value) {
        let chat = params["chat"].as_str().unwrap_or_default();
        let file = crate::remote::location(host, chat);
        match method {
            "chats_changed" => {
                let root = str_of(params, "root");
                for wi in self.workspaces_at(host, &root) {
                    self.remote_list_chats(wi);
                }
            }
            "title" => {
                if let Some(key) = self.key_for_file(&file)
                    && let Some(title) = params["title"].as_str()
                {
                    self.session_mut_by_key(key).title = title.to_string();
                }
            }
            "chat_removed" => {
                if let Some(key) = self.key_for_file(&file)
                    && key.session_idx != self.conv.workspaces[key.workspace_idx].active
                    && !self.chat_is_busy(key)
                    && self.session_rows_can_shift(key.workspace_idx)
                {
                    self.remove_session_row(key.workspace_idx, key.session_idx);
                }
            }
            // Another client changed or runs the chat: read it again, following its turn.
            "chat_reloaded" | "turn" | "agent_event" => {
                let running = method != "chat_reloaded"
                    && (method == "agent_event" || params["running"].as_bool() == Some(true));
                if let Some(key) = self.key_for_file(&file)
                    && self.session_by_key(key).messages_loaded
                    && !self.chat_is_busy(key)
                    && (running || method == "chat_reloaded")
                    && !self.remote.loading.contains(&file)
                {
                    let follows = self
                        .remote
                        .clients
                        .get(host)
                        .is_some_and(|c| c.follows(chat));
                    if !follows {
                        self.remote_load_session(key);
                    }
                }
            }
            _ => {}
        }
    }

    fn on_remote_answer(&mut self, asked: Asked, result: Result<Value, String>) {
        match asked {
            Asked::List { root } => {
                self.remote.listing.remove(&root);
                match result {
                    Ok(chats) => self.apply_remote_list(&root, &chats),
                    Err(e) => log::warn!("remote: listing chats of {root}: {e}"),
                }
                if self.remote.relist.remove(&root)
                    && let Some(wi) = self
                        .conv
                        .workspaces
                        .iter()
                        .position(|w| w.root_path == root)
                {
                    self.remote_list_chats(wi);
                }
            }
            Asked::Load { file, rx } => {
                self.remote.loading.remove(&file);
                let Some(key) = self.key_for_file(&file) else {
                    return;
                };
                match result {
                    Ok(snapshot) => {
                        // A prompt sent meanwhile owns the chat now.
                        if self.run_state(key).is_some_and(|r| r.agent_rx.is_some()) {
                            return;
                        }
                        self.apply_remote_snapshot(key, &snapshot);
                        if snapshot["running"].as_bool() == Some(true) {
                            self.follow_remote_turn(key, &snapshot, rx);
                        }
                    }
                    Err(e) => {
                        self.remote.load_failed.insert(file);
                        self.run_state_mut(key).stream_error =
                            Some(format!("Could not open the chat: {e}"));
                    }
                }
            }
            Asked::Turn { cancel, host } => {
                let Ok(value) = result else {
                    // The run got a failed `Finished` from the client already.
                    return;
                };
                let Some(key) = self.flow.sessions.iter().find_map(|(key, run)| {
                    run.cancel_agent
                        .as_ref()
                        .is_some_and(|c| Arc::ptr_eq(c, &cancel))
                        .then_some(*key)
                }) else {
                    return;
                };
                let file = crate::remote::location(&host, &str_of(&value, "chat"));
                let new_chat = self.session_by_key(key).session_file.is_none();
                let session = self.session_mut_by_key(key);
                session.session_file = Some(file);
                if let Some(title) = value["title"].as_str().filter(|t| !t.is_empty()) {
                    session.title = title.to_string();
                }
                // The prompt as the server stored it (its @mentions expanded).
                if let Some(index) = value["user_index"].as_u64().map(|i| i as usize)
                    && let Some(stored) = protocol::messages_from_json(&value["user"]).pop()
                    && session
                        .messages
                        .get(index)
                        .is_some_and(|m| m.role == MsgRole::User)
                {
                    session.messages[index] = stored;
                }
                if new_chat && key == self.active_session_key() {
                    self.persist_active_session_selection();
                }
            }
            Asked::Reconcile { file, from } => {
                let (Some(key), Ok(value)) = (self.key_for_file(&file), result) else {
                    return;
                };
                if self.chat_is_busy(key) {
                    return;
                }
                let count = value["count"].as_u64().unwrap_or(0) as usize;
                let session = self.session_mut_by_key(key);
                session.delegated =
                    serde_json::from_value(value["delegated"].clone()).unwrap_or_default();
                if let Some(title) = value["title"].as_str().filter(|t| !t.is_empty()) {
                    session.title = title.to_string();
                }
                if count == session.messages.len() && from <= count {
                    let tail = protocol::messages_from_json(&value["messages"]);
                    if from + tail.len() == count {
                        session.messages.truncate(from);
                        session.messages.extend(tail);
                    }
                } else {
                    // Something else happened to the chat too; read all of it again.
                    self.remote_load_session(key);
                }
            }
            Asked::Browse => {
                let Some(dialog) = self.remote.dialog.as_mut() else {
                    return;
                };
                dialog.browsing = false;
                match result {
                    Ok(listing) => {
                        dialog.error = None;
                        dialog.path = str_of(&listing, "path");
                        dialog.parent = listing["parent"].as_str().map(str::to_string);
                        dialog.entries = listing["entries"]
                            .as_array()
                            .map(|entries| {
                                entries
                                    .iter()
                                    .map(|e| (str_of(e, "name"), e["dir"].as_bool() == Some(true)))
                                    .collect()
                            })
                            .unwrap_or_default();
                    }
                    Err(e) => dialog.error = Some(e),
                }
            }
            Asked::Action(what) => {
                if let Err(e) = result {
                    self.conv.sidebar.notice = Some(format!("{what}: {e}"));
                }
            }
        }
    }

    /// Hang up on hosts no workspace (and not the connect dialog) uses any more.
    fn drop_unused_remote_clients(&mut self) {
        if self.remote.clients.is_empty() {
            return;
        }
        let in_use: HashSet<&str> = self
            .conv
            .workspaces
            .iter()
            .filter_map(|w| crate::remote::host_of(&w.root_path))
            .chain(
                self.remote
                    .dialog
                    .as_ref()
                    .and_then(|d| d.connected_host.as_deref()),
            )
            .collect();
        if self
            .remote
            .clients
            .keys()
            .all(|h| in_use.contains(h.as_str()))
        {
            return;
        }
        let unused: Vec<String> = self
            .remote
            .clients
            .keys()
            .filter(|h| !in_use.contains(h.as_str()))
            .cloned()
            .collect();
        for host in unused {
            self.remote.clients.remove(&host);
        }
    }

    /// Ask for workspace `wi`'s chats (once at a time; asked again after if more changed).
    pub(crate) fn remote_list_chats(&mut self, wi: usize) {
        let root = self.conv.workspaces[wi].root_path.clone();
        let Some((host, path)) = crate::remote::split(&root) else {
            return;
        };
        if self.remote.listing.contains(&root) {
            self.remote.relist.insert(root);
            return;
        }
        let Some(client) = self.remote.connected(host) else {
            return;
        };
        let pending = client.request("chats.list", json!({"root": path}));
        self.remote.listing.insert(root.clone());
        self.remote.asked.push((Asked::List { root }, pending));
    }

    /// Bring workspace `root`'s rows in line with the server's chat list.
    fn apply_remote_list(&mut self, root: &str, chats: &Value) {
        let Some(wi) = self
            .conv
            .workspaces
            .iter()
            .position(|w| w.root_path == root)
        else {
            return;
        };
        let Some(host) = crate::remote::host_of(root).map(str::to_string) else {
            return;
        };
        let mut listed: Vec<(String, Value)> = chats
            .as_array()
            .map(|chats| {
                chats
                    .iter()
                    .map(|c| (crate::remote::location(&host, &str_of(c, "id")), c.clone()))
                    .collect()
            })
            .unwrap_or_default();
        listed.sort_by_key(|(_, c)| std::cmp::Reverse(c["modified"].as_u64().unwrap_or(0)));
        let session_from = |file: &str, chat: &Value| {
            let mut session = Self::blank_session(str_of(chat, "title"));
            session.session_file = Some(file.to_string());
            session.messages_loaded = false;
            session.modified = modified_from(chat);
            session
        };

        // First answer: the workspace shows only the placeholder chat it was opened with.
        let only_placeholder = {
            let sessions = &self.conv.workspaces[wi].sessions;
            sessions
                .iter()
                .all(|s| s.session_file.is_none() && s.messages.is_empty())
                && sessions.len() <= 1
        };
        if only_placeholder {
            if listed.is_empty() {
                return;
            }
            if !self.session_rows_can_shift(wi) {
                self.remote.relist.insert(root.to_string());
                return;
            }
            let sessions: Vec<Session> = listed
                .iter()
                .map(|(file, chat)| session_from(file, chat))
                .collect();
            let remembered = self.conv.settings.last_active_session_file.clone();
            let workspace = &mut self.conv.workspaces[wi];
            workspace.sessions = sessions;
            workspace.active = remembered
                .and_then(|f| {
                    workspace
                        .sessions
                        .iter()
                        .position(|s| s.session_file.as_deref() == Some(f.as_str()))
                })
                .unwrap_or(0);
            self.shift_session_rows(wi, |_| None);
            if wi == self.conv.active_workspace {
                self.ensure_active_session_loaded();
                self.restore_active_session_config();
            }
            return;
        }

        let on_server: HashSet<&str> = listed.iter().map(|(f, _)| f.as_str()).collect();
        let active = self.conv.workspaces[wi].active;
        let removed: Vec<usize> = self.conv.workspaces[wi]
            .sessions
            .iter()
            .enumerate()
            .filter(|(si, s)| {
                *si != active
                    && s.session_file
                        .as_deref()
                        .is_some_and(|f| !on_server.contains(f))
            })
            .map(|(si, _)| si)
            .filter(|&si| !self.chat_is_busy(self.session_key(wi, si)))
            .collect();
        let known: HashSet<String> = self.conv.workspaces[wi]
            .sessions
            .iter()
            .filter_map(|s| s.session_file.clone())
            .collect();
        let added: Vec<&(String, Value)> = listed
            .iter()
            .rev()
            .filter(|(f, _)| !known.contains(f))
            .collect();
        if (!removed.is_empty() || !added.is_empty()) && !self.session_rows_can_shift(wi) {
            self.remote.relist.insert(root.to_string());
            return;
        }
        for &si in removed.iter().rev() {
            self.remove_session_row(wi, si);
        }
        for (file, chat) in added {
            self.insert_session_row(wi, session_from(file, chat));
        }
        // Titles and ages of the chats both sides know.
        for (file, chat) in &listed {
            if let Some(key) = self.key_for_file(file) {
                let renaming = self.conv.sidebar.renaming_session
                    == Some((key.workspace_idx, key.session_idx));
                let busy = self.chat_is_busy(key);
                let session = self.session_mut_by_key(key);
                if !renaming && !busy {
                    session.title = str_of(chat, "title");
                }
                session.modified = modified_from(chat);
            }
        }
    }

    /// Ask for a remote chat's transcript (and follow its turn if one runs).
    pub(crate) fn remote_load_session(&mut self, key: SessionKey) {
        let Some(file) = self.session_by_key(key).session_file.clone() else {
            return;
        };
        let Some((host, chat)) = crate::remote::split(&file) else {
            return;
        };
        if self.remote.loading.contains(&file) || self.remote.load_failed.contains(&file) {
            return;
        }
        let root = self.conv.workspaces[key.workspace_idx].root_path.clone();
        let root_path = crate::remote::path_of(&root).to_string();
        let Some(client) = self.remote.connected(host) else {
            return;
        };
        let (pending, rx) = client.attach(&root_path, &chat);
        self.remote.loading.insert(file.clone());
        self.remote.asked.push((Asked::Load { file, rx }, pending));
    }

    /// Replace `key`'s copy with the server's snapshot (`chat.get` from message 0).
    fn apply_remote_snapshot(&mut self, key: SessionKey, snapshot: &Value) {
        let mut messages = protocol::messages_from_json(&snapshot["messages"]);
        let streaming = snapshot["streaming"].as_bool() == Some(true);
        if streaming && let Some(last) = messages.last_mut() {
            if last.role == MsgRole::Assistant {
                last.streaming = true;
                last.started_at = Some(std::time::Instant::now());
                last.worked_duration = None;
            } else {
                messages.push(crate::model::ChatMessage::streaming_assistant(None));
            }
        } else if streaming {
            messages.push(crate::model::ChatMessage::streaming_assistant(None));
        }
        let config: Option<crate::model::SessionConfig> =
            serde_json::from_value(snapshot["config"].clone()).ok();
        let context = snapshot["context"].as_object().map(|c| {
            (
                c["used"].as_u64().unwrap_or(0),
                c["size"].as_u64().unwrap_or(0),
            )
        });
        let (wi, si) = (key.workspace_idx, key.session_idx);
        let session = self.session_mut_by_key(key);
        session.delegated =
            serde_json::from_value(snapshot["delegated"].clone()).unwrap_or_default();
        session.messages = messages;
        session.messages_loaded = true;
        if let Some(title) = snapshot["title"].as_str().filter(|t| !t.is_empty()) {
            session.title = title.to_string();
        }
        if snapshot.get("modified").is_some() {
            session.modified = modified_from(snapshot);
        }
        session.agent_context = match (&config, context) {
            (Some(config), Some((used, size))) => Some((config.provider, used, size)),
            _ => None,
        };
        if config.is_some() {
            session.config = config;
        }
        session.wire_cache = None;
        self.conv
            .transcript
            .heights
            .retain(|&(w, s, _), _| (w, s) != (wi, si));
        self.conv.sidebar.search_cache.remove(&(wi, si));
        if key == self.active_session_key() {
            self.restore_active_session_config();
        }
    }

    /// Show a turn the server runs for `key` (started elsewhere) as if it ran here.
    fn follow_remote_turn(&mut self, key: SessionKey, snapshot: &Value, rx: Receiver<AgentEvent>) {
        let approval = snapshot["approval"].as_object().map(|a| PendingApproval {
            name: a
                .get("tool")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            summary: a
                .get("summary")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            allow_prefix: a
                .get("allow_prefix")
                .and_then(Value::as_str)
                .map(str::to_string),
        });
        let run = self.run_state_mut(key);
        run.begin_waiting_response();
        run.stream_error = None;
        run.agent_rx = Some(rx);
        run.approval_tx = None;
        run.cancel_agent = Some(Arc::new(AtomicBool::new(false)));
        run.pending_approval = approval;
        run.plan_mode = snapshot["plan"].as_bool().unwrap_or(run.plan_mode);
        run.evidence_mode
            .set(snapshot["evidence"].as_bool().unwrap_or(false));
    }

    /// `send_prompt_payload` for a remote chat: start the turn on the server. The chat ends
    /// with the prompt and the empty reply the events stream into.
    pub(crate) fn start_remote_turn(&mut self, key: SessionKey) -> Result<(), String> {
        let root = self.conv.workspaces[key.workspace_idx].root_path.clone();
        let (host, root_path) =
            crate::remote::split(&root).ok_or("internal: not a remote workspace")?;
        let host = host.to_string();
        let root_path = root_path.to_string();
        if self.remote.connected(&host).is_none() {
            self.remote.client(&host).retry_now();
            return Err(format!(
                "Not connected to {host}. Reconnecting; try again in a moment."
            ));
        }
        let config = self.ensure_session_config(key);
        let (plan, evidence) = self
            .run_state(key)
            .map(|r| (r.plan_mode, r.evidence_mode.enabled()))
            .unwrap_or_default();
        let session = self.session_by_key(key);
        let n = session.messages.len();
        if n < 2 || session.messages[n - 2].role != MsgRole::User {
            return Err("internal: expected user and assistant messages".into());
        }
        let user = &session.messages[n - 2];
        let history = &session.messages[..n - 2];
        let chat = session
            .session_file
            .as_deref()
            .map(|f| crate::remote::path_of(f).to_string());
        let history_json = protocol::messages_to_json(history);
        let mut params = json!({
            "root": root_path,
            "chat": chat,
            "text": user.text,
            "attachments": user.attachments.iter().map(attachment_json).collect::<Vec<_>>(),
            "plan": plan,
            "evidence": evidence,
            "config": config,
            "title": session.title,
            "history_hash": protocol::messages_hash(history),
        });
        if chat.is_none() {
            params.as_object_mut().expect("object").remove("chat");
            // A new chat may start from a transcript (a fork); the server has none to compare.
            if !history.is_empty() {
                params["history"] = history_json.clone();
            }
        }
        let client = self.remote.client(&host);
        let (pending, rx) = client.start_turn(params, history_json);
        let cancel = Arc::new(AtomicBool::new(false));
        let run = self.run_state_mut(key);
        run.agent_rx = Some(rx);
        run.approval_tx = None;
        run.pending_approval = None;
        run.cancel_agent = Some(Arc::clone(&cancel));
        run.undo_journal = None;
        self.remote
            .asked
            .push((Asked::Turn { cancel, host }, pending));
        Ok(())
    }

    /// The chat's host and id on it, when it is a saved remote chat.
    fn remote_chat(&self, key: SessionKey) -> Option<(String, String, String)> {
        let file = self.session_by_key(key).session_file.as_deref()?;
        let (host, chat) = crate::remote::split(file)?;
        let root = crate::remote::path_of(&self.conv.workspaces[key.workspace_idx].root_path);
        Some((host.to_string(), chat.to_string(), root.to_string()))
    }

    pub(super) fn remote_stop_subagent(
        &mut self,
        key: SessionKey,
        root: String,
        child: String,
        task: bool,
    ) {
        let Some((host, chat, path)) = self.remote_chat(key) else {
            return;
        };
        if let Some(client) = self.remote.connected(&host) {
            let pending = client.request(
                "subagent.cancel",
                json!({"root":path,"chat":chat,"sessionId":root,"child":child,"task":task}),
            );
            self.remote
                .asked
                .push((Asked::Action("Could not stop the subagent"), pending));
        }
    }

    /// Pass an approval decision for a remote chat's tool call to its server.
    pub(crate) fn remote_respond_to_approval(
        &mut self,
        key: SessionKey,
        decision: &crate::agent::ApprovalDecision,
    ) {
        let Some((host, chat, _)) = self.remote_chat(key) else {
            return;
        };
        if let Some(client) = self.remote.connected(&host) {
            let mut params = protocol::decision_to_json(decision);
            params["chat"] = json!(chat);
            let pending = client.request("approval.respond", params);
            self.remote
                .asked
                .push((Asked::Action("Could not answer the approval"), pending));
        }
    }

    /// Ask the server to stop `key`'s turn. False when that isn't possible (no connection, a
    /// prompt whose chat isn't known yet): the caller stops the run here instead.
    pub(crate) fn remote_cancel(&mut self, key: SessionKey) -> bool {
        let Some((host, chat, _)) = self.remote_chat(key) else {
            return false;
        };
        let Some(client) = self.remote.connected(&host) else {
            return false;
        };
        if !client.follows(&chat) {
            return false;
        }
        client.notify("cancel", json!({"chat": chat}));
        self.run_state_mut(key).stream_retrying = Some("Stopping…".into());
        true
    }

    /// After a remote turn ended here, take the server's version of its reply.
    pub(crate) fn remote_reconcile(&mut self, key: SessionKey) {
        let Some((host, chat, root)) = self.remote_chat(key) else {
            return;
        };
        let Some(from) = self
            .session_by_key(key)
            .messages
            .iter()
            .rposition(|m| m.role == MsgRole::Assistant)
        else {
            return;
        };
        let Some(client) = self.remote.connected(&host) else {
            return;
        };
        let file = self
            .session_by_key(key)
            .session_file
            .clone()
            .unwrap_or_default();
        let pending = client.request(
            "chat.get",
            json!({"root": root, "chat": chat, "from": from}),
        );
        self.remote
            .asked
            .push((Asked::Reconcile { file, from }, pending));
    }

    /// Send the app's copy of an idle remote chat (after a rename, rewind, compaction, model
    /// change) to its server. Chats not saved there yet go along with their first prompt.
    pub(crate) fn remote_save_session(&mut self, key: SessionKey) {
        let Some((host, chat, root)) = self.remote_chat(key) else {
            return;
        };
        if self.chat_is_busy(key) {
            return;
        }
        let session = self.session_by_key(key);
        let params = json!({
            "root": root,
            "chat": chat,
            "messages": protocol::messages_to_json(&session.messages),
            "title": session.title,
            "config": session.config,
        });
        if let Some(client) = self.remote.connected(&host) {
            let pending = client.request("chat.save", params);
            self.remote.asked.push((
                Asked::Action("Could not save the chat on the server"),
                pending,
            ));
        }
    }

    pub(crate) fn remote_delete_chat(&mut self, root: &str, file: &str) {
        let Some((host, chat)) = crate::remote::split(file) else {
            return;
        };
        if let Some(client) = self.remote.connected(host) {
            let pending = client.request(
                "chat.delete",
                json!({"root": crate::remote::path_of(root), "chat": chat}),
            );
            self.remote
                .asked
                .push((Asked::Action("Could not delete the chat"), pending));
        } else {
            self.conv.sidebar.notice =
                Some(format!("Not connected to {host}; the chat stays there."));
        }
    }

    /// Connection status of a remote workspace, for its sidebar header: `None` when connected.
    pub(crate) fn remote_status(&self, wi: usize) -> Option<String> {
        let host = crate::remote::host_of(&self.conv.workspaces[wi].root_path)?;
        match self.remote.state(host) {
            Some(ConnState::Connected) => None,
            Some(ConnState::Connecting) | None => Some(format!("Connecting to {host}…")),
            Some(ConnState::Failed(e)) => Some(format!("{host}: {e}")),
        }
    }

    pub(crate) fn retry_remote(&mut self, wi: usize) {
        if let Some(host) = crate::remote::host_of(&self.conv.workspaces[wi].root_path) {
            let host = host.to_string();
            self.remote.client(&host).retry_now();
        }
    }

    // ---- connect dialog ----------------------------------------------------------------

    pub(crate) fn open_remote_dialog(&mut self) {
        let suggestions = crate::remote::ssh_config_hosts();
        let host = self
            .conv
            .workspaces
            .iter()
            .find_map(|w| crate::remote::host_of(&w.root_path).map(str::to_string))
            .or_else(|| suggestions.first().cloned())
            .unwrap_or_default();
        self.remote.dialog = Some(ConnectDialog {
            host,
            suggestions,
            focus_host: true,
            ..Default::default()
        });
    }

    fn remote_dialog_connect(&mut self) {
        let Some(dialog) = self.remote.dialog.as_mut() else {
            return;
        };
        let host = dialog.host.trim().to_string();
        if host.is_empty() {
            return;
        }
        dialog.connected_host = Some(host.clone());
        dialog.entries.clear();
        dialog.error = None;
        dialog.path.clear();
        let client = self.remote.client(&host);
        match client.state() {
            ConnState::Connected => self.remote_browse("~"),
            ConnState::Failed(_) => client.retry_now(),
            ConnState::Connecting => {}
        }
    }

    fn remote_browse(&mut self, path: &str) {
        let Some(host) = self
            .remote
            .dialog
            .as_ref()
            .and_then(|d| d.connected_host.clone())
        else {
            return;
        };
        let Some(client) = self.remote.connected(&host) else {
            return;
        };
        let pending = client.request("fs.list", json!({"path": path}));
        if let Some(dialog) = self.remote.dialog.as_mut() {
            dialog.browsing = true;
        }
        self.remote.asked.push((Asked::Browse, pending));
    }

    /// Add folder `path` of `host` as a workspace and switch to it.
    fn open_remote_folder(&mut self, host: &str, path: &str) {
        self.remote.dialog = None;
        let root = crate::remote::location(host, path);
        self.open_workspace_path(root);
        if let Some(wi) = self.conv.workspaces.iter().position(|w| {
            crate::remote::split(&w.root_path).is_some_and(|(h, p)| h == host && p == path)
        }) {
            self.remote_list_chats(wi);
        }
    }

    pub(crate) fn render_remote_dialog(&mut self, ctx: &egui::Context) {
        use crate::theme::*;
        use crate::ui::chrome::{ghost_button, modal_backdrop, primary_button};
        if self.remote.dialog.is_none() {
            return;
        }
        let mut close = modal_backdrop(ctx, "remote_dialog_backdrop");
        let mut connect = false;
        let mut browse: Option<String> = None;
        let mut open: Option<(String, String)> = None;
        let mut open_general: Option<(String, String)> = None;
        let connected = self
            .remote
            .dialog
            .as_ref()
            .and_then(|d| d.connected_host.clone())
            .map(|h| (self.remote.state(&h), h));
        let general_dir = connected
            .as_ref()
            .and_then(|(_, host)| self.remote.clients.get(host))
            .and_then(|c| c.info()["general"].as_str().map(str::to_string));
        let dialog = self.remote.dialog.as_mut().expect("checked above");
        egui::Area::new(egui::Id::new("remote_dialog"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(c_bg_elevated())
                    .stroke(egui::Stroke::new(1.0, c_border()))
                    .corner_radius(RADIUS_CHIP)
                    .inner_margin(egui::Margin::same(16))
                    .show(ui, |ui| {
                        ui.set_width(460.0);
                        ui.label(
                            egui::RichText::new("Connect to a remote host")
                                .size(FS_BODY)
                                .color(c_text())
                                .strong(),
                        );
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new(
                                "oxi connects over SSH (keys or ssh-agent) and runs the agents on \
                                 the host. The host needs oxi; run `oxi server install` there once.",
                            )
                            .size(FS_SMALL)
                            .color(c_text_muted()),
                        );
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            let field = ui.add(
                                egui::TextEdit::singleline(&mut dialog.host)
                                    .hint_text("SSH host, e.g. my-server or user@host")
                                    .desired_width(330.0),
                            );
                            if dialog.focus_host {
                                field.request_focus();
                                dialog.focus_host = false;
                            }
                            if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
                            {
                                connect = true;
                            }
                            if primary_button(ui, "Connect").clicked() {
                                connect = true;
                            }
                        });
                        if !dialog.suggestions.is_empty() {
                            ui.add_space(4.0);
                            ui.horizontal_wrapped(|ui| {
                                ui.label(
                                    egui::RichText::new("From ~/.ssh/config:")
                                        .size(FS_TINY)
                                        .color(c_text_faint()),
                                );
                                for host in dialog.suggestions.clone() {
                                    if ui
                                        .small_button(egui::RichText::new(&host).size(FS_TINY))
                                        .clicked()
                                    {
                                        dialog.host = host;
                                        connect = true;
                                    }
                                }
                            });
                        }
                        if let Some((state, host)) = &connected {
                            ui.add_space(10.0);
                            match state {
                                Some(ConnState::Connected) => {
                                    ui.horizontal(|ui| {
                                        if ui
                                            .add_enabled(
                                                dialog.parent.is_some(),
                                                egui::Button::new(".."),
                                            )
                                            .on_hover_text("Parent folder")
                                            .clicked()
                                        {
                                            browse = dialog.parent.clone();
                                        }
                                        ui.label(
                                            egui::RichText::new(format!("{host}:{}", dialog.path))
                                                .size(FS_SMALL)
                                                .color(c_text())
                                                .monospace(),
                                        );
                                    });
                                    ui.add_space(4.0);
                                    egui::Frame::new()
                                        .stroke(egui::Stroke::new(1.0, c_border()))
                                        .corner_radius(RADIUS_CHIP)
                                        .inner_margin(egui::Margin::same(4))
                                        .show(ui, |ui| {
                                            egui::ScrollArea::vertical()
                                                .max_height(260.0)
                                                .auto_shrink([false, false])
                                                .show(ui, |ui| {
                                                    if dialog.browsing && dialog.entries.is_empty() {
                                                        ui.label("Loading…");
                                                    }
                                                    for (name, dir) in &dialog.entries {
                                                        if !dir {
                                                            continue;
                                                        }
                                                        let row = ui.selectable_label(
                                                            false,
                                                            format!("{name}/"),
                                                        );
                                                        if row.double_clicked() || row.clicked() {
                                                            let base = dialog.path.trim_end_matches('/');
                                                            browse = Some(format!("{base}/{name}"));
                                                        }
                                                    }
                                                });
                                        });
                                    ui.add_space(8.0);
                                    ui.horizontal(|ui| {
                                        if primary_button(ui, "Open this folder").clicked()
                                            && !dialog.path.is_empty()
                                        {
                                            open = Some((host.clone(), dialog.path.clone()));
                                        }
                                        if let Some(general) = &general_dir
                                            && ghost_button(ui, "General chats on this host", false)
                                                .on_hover_text(format!(
                                                    "Chats not about any project, run in {general}"
                                                ))
                                                .clicked()
                                        {
                                            open_general = Some((host.clone(), general.clone()));
                                        }
                                    });
                                }
                                Some(ConnState::Connecting) | None => {
                                    ui.label(
                                        egui::RichText::new(format!("Connecting to {host}…"))
                                            .size(FS_SMALL)
                                            .color(c_text_muted()),
                                    );
                                }
                                Some(ConnState::Failed(_)) => {}
                            }
                        }
                        if let Some(error) = &dialog.error {
                            ui.add_space(6.0);
                            ui.label(
                                egui::RichText::new(error)
                                    .size(FS_TINY)
                                    .color(c_error_fg()),
                            );
                        }
                        ui.add_space(12.0);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ghost_button(ui, "Close", false).clicked() {
                                close = true;
                            }
                        });
                    });
            });
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            close = true;
        }
        if close {
            self.remote.dialog = None;
            return;
        }
        if connect {
            self.remote_dialog_connect();
        }
        if let Some(path) = browse {
            self.remote_browse(&path);
        }
        if let Some((host, path)) = open {
            self.open_remote_folder(&host, &path);
        }
        if let Some((host, path)) = open_general {
            // The folder may not exist on the host yet.
            let created = crate::remote::client::call(
                &host,
                "fs.op",
                json!({"path": path, "op": "create_dir_all"}),
                Duration::from_secs(10),
            );
            match created {
                Ok(_) => self.open_remote_folder(&host, &path),
                Err(e) => {
                    if let Some(dialog) = self.remote.dialog.as_mut() {
                        dialog.error = Some(e);
                    }
                }
            }
        }
    }
}

/// Sessions a remote workspace starts with until its chat list arrives.
pub(crate) fn placeholder_sessions() -> Vec<Session> {
    vec![OxiApp::blank_session("New chat")]
}
