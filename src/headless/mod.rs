//! `oxi headless`: the chat engine without the window, driven over stdio by editor plugins
//! (the Sublime Text package in `editors/sublime`).
//!
//! One JSON object per line in each direction. The editor sends requests,
//! `{"id": 1, "method": "chats.list", "params": {...}}`, and gets
//! `{"id": 1, "result": ...}` or `{"id": 1, "error": "..."}` back. oxi pushes notifications
//! without an id, `{"method": "message", "params": {...}}`, while turns stream.
//!
//! Chats are the same session files the app uses, so a conversation started in the editor
//! shows up in oxi and the other way round. A chat's id is its session file path. Logs go to
//! stderr and `oxi.log`; stdout carries only the protocol.
//!
//! One server, the daemon in [`daemon`], serves every connected editor: replies go to the
//! editor that asked, notifications go to all of them, so each one viewing a chat sees it
//! stream. Chats changed on disk by the app are read again and announced (`chat_reloaded`).

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Value, json};

use crate::agent::runner::{AgentExecutor, AgentRunRequest};
use crate::agent::{AgentEvent, AgentOutcome, ApprovalDecision};
use crate::model::{
    AssistantBlock, ChatMessage, MsgRole, RouteNote, Session, SessionConfig, ToolStatus,
    UserAttachment, make_session_title,
};
use crate::session_store;
use crate::settings::{AppSettings, LlmProviderKind};

mod daemon;
pub mod phone;
mod remote_app;
mod settings_api;
pub use remote_app::HISTORY_MISMATCH;

pub const SUBCOMMAND: &str = "headless";
/// Bumped when a request or notification changes shape incompatibly.
const PROTOCOL_VERSION: u32 = 1;
/// Streaming snapshots are coalesced to at most one per chat per interval.
const FLUSH_EVERY: Duration = Duration::from_millis(60);
/// Tool output sent to the editor: the tail, where the result and errors are.
const TOOL_OUTPUT_TAIL: usize = 4000;
const TOOL_ARGS_MAX: usize = 4000;
const DIFF_MAX: usize = 200_000;
/// How often chats loaded here are checked for changes the app wrote to their files.
const DISK_CHECK_EVERY: Duration = Duration::from_secs(1);

enum Input {
    /// An editor connected; what the server sends it goes through `writer`.
    Connected {
        client: u64,
        writer: Sender<Arc<str>>,
    },
    Disconnected {
        client: u64,
    },
    Request {
        client: u64,
        request: Value,
    },
    Agent {
        chat: String,
        run: u64,
        event: AgentEvent,
    },
    /// The run's worker hung up; after `Finished` this is a no-op.
    AgentGone {
        chat: String,
        run: u64,
    },
    /// Result of a request answered off the main thread.
    Reply {
        to: ReplyTo,
        result: Result<Value, String>,
    },
    /// An ACP agent started a turn on its own.
    Autonomous,
}

/// Who a reply goes to: the editor that asked, and its request id.
struct ReplyTo {
    client: u64,
    id: Value,
}

struct Run {
    id: u64,
    approval_tx: Sender<ApprovalDecision>,
    cancel: Arc<AtomicBool>,
    usage_target: Option<(LlmProviderKind, String)>,
}

struct Prompt {
    text: String,
    attachments: Vec<UserAttachment>,
    plan: Option<bool>,
    evidence: Option<bool>,
}

struct Chat {
    root: String,
    session: Session,
    run: Option<Run>,
    queued: VecDeque<Prompt>,
    plan_mode: bool,
    evidence_mode: crate::evidence::Mode,
    pending_approval: Option<Value>,
    status: Option<String>,
    /// PTYs the agent opened; dropping one kills its command, so they live as long as it runs.
    terminals: Vec<crate::terminal::TerminalSession>,
    /// The streaming reply changed since the last snapshot was sent.
    dirty: bool,
    /// The session file's mtime when this process last read or wrote it.
    disk_mtime: Option<SystemTime>,
}

impl Chat {
    fn new(root: String, session: Session) -> Self {
        Self {
            root,
            session,
            run: None,
            queued: VecDeque::new(),
            plan_mode: false,
            evidence_mode: false.into(),
            pending_approval: None,
            status: None,
            terminals: Vec::new(),
            dirty: false,
            disk_mtime: None,
        }
    }

    fn id(&self) -> Option<&str> {
        self.session.session_file.as_deref()
    }

    fn last_assistant_mut(&mut self) -> Option<&mut ChatMessage> {
        self.session
            .messages
            .iter_mut()
            .rev()
            .find(|m| m.role == MsgRole::Assistant)
    }

    fn last_assistant_index(&self) -> Option<usize> {
        self.session
            .messages
            .iter()
            .rposition(|m| m.role == MsgRole::Assistant)
    }

    fn save(&mut self) -> Result<(), String> {
        let result = session_store::save_session_messages(&self.root, &mut self.session);
        self.disk_mtime = self.id().and_then(file_mtime);
        result
    }
}

fn file_mtime(path: &str) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn empty_session(title: String, config: SessionConfig) -> Session {
    Session {
        delegated: Default::default(),
        title,
        messages: Vec::new(),
        config: Some(config),
        session_file: None,
        messages_loaded: true,
        input_text: String::new(),
        pending_images: Vec::new(),
        pending_texts: Vec::new(),
        modified: SystemTime::now(),
        chars_per_token: None,
        agent_context: None,
        wire_cache: None,
        disk_mtime: None,
    }
}

/// Process entry point for `oxi headless`; returns the exit code.
///
/// - no option: relay stdio to the daemon, starting it if needed (what editors run);
/// - `--daemon`: be the daemon;
/// - `--stdio`: serve one editor on stdio in this process;
/// - `--status`, `--stop`: show or stop the running daemon.
pub fn run() -> i32 {
    crate::logging::init();
    if crate::remote::service::managed() {
        // Like the daemon `oxi server` runs: keys from its file, never a keychain prompt.
        // SAFETY: no other thread runs yet.
        unsafe { std::env::set_var(crate::secrets::SERVER_FILE_ENV, "1") };
    }
    match std::env::args().nth(2).as_deref() {
        None => daemon::bridge().unwrap_or_else(|| serve(false)),
        Some("--daemon") => {
            // `--keep`: run until stopped, for `oxi server`'s LaunchAgent.
            let keep = std::env::args().nth(3).as_deref() == Some("--keep");
            KEEP_RUNNING.store(keep, Ordering::Relaxed);
            serve(true)
        }
        Some("--stdio") => serve(false),
        Some("--status") => daemon::status(),
        Some("--stop") => daemon::stop(),
        Some(other) => {
            eprintln!("oxi headless: unknown option {other}");
            2
        }
    }
}

/// The daemon doesn't exit when idle (`oxi headless --daemon --keep`).
static KEEP_RUNNING: AtomicBool = AtomicBool::new(false);

/// Stop a daemon running now, if any (`oxi server install` replaces it).
pub fn stop_running_daemon() {
    let _ = daemon::stop_quietly();
}

/// The running daemon's `daemon.hello` answer (pid, version, clients, ...), if one runs.
pub fn daemon_hello() -> Option<Value> {
    daemon::hello()
}

/// Start a daemon detached from this process; `keep` keeps it running when idle.
pub fn start_daemon(keep: bool) -> std::io::Result<()> {
    daemon::spawn_daemon(keep)
}

/// Run the server: as the daemon, for every editor that connects, else for the one on stdio.
fn serve(as_daemon: bool) -> i32 {
    let (tx, rx) = mpsc::channel::<Input>();
    let _guard = if as_daemon {
        match daemon::listen(tx.clone()) {
            Ok(Some(guard)) => Some(guard),
            Ok(None) => {
                log::info!("headless: a daemon already runs");
                return 0;
            }
            Err(e) => {
                log::error!("headless: could not start the daemon: {e}");
                eprintln!("oxi headless: {e}");
                return 1;
            }
        }
    } else {
        daemon::add_stdio_client(&tx);
        None
    };
    let executor = match AgentExecutor::new() {
        Ok(executor) => executor,
        Err(e) => {
            eprintln!("oxi headless: {e}");
            return 1;
        }
    };
    {
        let tx = tx.clone();
        crate::agent::acp::set_autonomous_waker(move || {
            let _ = tx.send(Input::Autonomous);
        });
    }
    if as_daemon {
        log::info!("headless: daemon {} listening", std::process::id());
    }
    let mut server = Server::new(tx, executor, as_daemon);
    server.run(&rx);
    0
}

struct Server {
    tx: Sender<Input>,
    settings: AppSettings,
    settings_mtime: Option<SystemTime>,
    executor: AgentExecutor,
    tunnels: crate::compute::TunnelManager,
    acp: crate::agent::acp::AcpManager,
    mcp: crate::agent::mcp::McpManager,
    chats: HashMap<String, Chat>,
    /// Provider/model last picked in the editor; new chats start with it.
    last_config: Option<SessionConfig>,
    /// Git requests run one after another on their own thread, in the order they came.
    git_jobs: Option<Sender<(ReplyTo, String, crate::git::GitOp)>>,
    next_run: u64,
    last_flush: Instant,
    /// Connected editors.
    clients: HashMap<u64, Sender<Arc<str>>>,
    /// Clients (the oxi app connected over SSH) that take raw agent events instead of the
    /// coalesced message snapshots; see `remote_app`.
    event_clients: std::collections::HashSet<u64>,
    /// Some editor has connected, so the stdio server can stop once none is left.
    had_client: bool,
    daemon: bool,
    exe: Value,
    idle_since: Option<Instant>,
    last_disk_check: Instant,
    keep_awake: crate::keep_awake::KeepAwake,
}

impl Server {
    fn new(tx: Sender<Input>, executor: AgentExecutor, daemon: bool) -> Self {
        crate::agent::tools::cleanup_stale_spill_files();
        let settings = AppSettings::load();
        crate::agent::activity_log::set_enabled(settings.activity_log_enabled);
        Self {
            tx,
            settings_mtime: settings_mtime(),
            settings,
            executor,
            tunnels: crate::compute::TunnelManager::spawn(),
            acp: crate::agent::acp::AcpManager::spawn(),
            mcp: crate::agent::mcp::McpManager::new(),
            chats: HashMap::new(),
            last_config: None,
            git_jobs: None,
            next_run: 1,
            last_flush: Instant::now(),
            clients: HashMap::new(),
            event_clients: Default::default(),
            had_client: false,
            daemon,
            exe: daemon::exe_identity(),
            idle_since: None,
            last_disk_check: Instant::now(),
            keep_awake: Default::default(),
        }
    }

    /// Pick up settings changed in the app (keys, providers, approvals) since the last turn.
    fn refresh_settings(&mut self) {
        let mtime = settings_mtime();
        if mtime != self.settings_mtime {
            self.settings = AppSettings::load();
            self.settings_mtime = mtime;
        }
    }

    /// Change the settings file without clobbering edits the app made since it was read.
    fn update_settings(&mut self, change: impl FnOnce(&mut AppSettings) -> bool) {
        let mut settings = AppSettings::load();
        if change(&mut settings)
            && let Err(e) = settings.save()
        {
            log::warn!("headless: could not save settings: {e}");
        }
        self.settings = settings;
        self.settings_mtime = settings_mtime();
    }

    /// Serve until an editor stops the server or, see [`Self::should_exit`], it is done.
    fn run(&mut self, rx: &mpsc::Receiver<Input>) {
        loop {
            match rx.recv_timeout(FLUSH_EVERY) {
                Ok(input) => {
                    if !self.handle(input) {
                        break;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            self.flush_if_due();
            self.check_disk_if_due();
            let working = self.settings.keep_awake_while_working && self.working();
            self.keep_awake.set(working);
            if self.should_exit() {
                break;
            }
        }
        self.shutdown();
    }

    /// Notifications go to every editor: any of them may be showing the chat.
    fn notify(&self, method: &str, params: Value) {
        let line: Arc<str> = json!({"method": method, "params": params})
            .to_string()
            .into();
        // Event clients rebuild the streaming reply from the events themselves.
        let snapshot = method == "message";
        for (client, writer) in &self.clients {
            if snapshot && self.event_clients.contains(client) {
                continue;
            }
            let _ = writer.send(line.clone());
        }
    }

    fn reply(&self, to: ReplyTo, result: Result<Value, String>) {
        let Some(writer) = self.clients.get(&to.client) else {
            return;
        };
        let message = match result {
            Ok(result) => json!({"id": to.id, "result": result}),
            Err(error) => json!({"id": to.id, "error": error}),
        };
        let _ = writer.send(message.to_string().into());
    }

    /// A chat runs or has prompts waiting.
    fn busy(&self) -> bool {
        self.chats
            .values()
            .any(|chat| chat.run.is_some() || !chat.queued.is_empty())
    }

    /// A turn is running and not stopped on an approval prompt (see `OxiApp::agent_is_working`).
    fn working(&self) -> bool {
        self.chats
            .values()
            .any(|chat| chat.run.is_some() && chat.pending_approval.is_none())
    }

    /// The stdio server stops with its editor; the daemon once it was idle for a while.
    fn should_exit(&mut self) -> bool {
        if !self.clients.is_empty() || self.busy() {
            self.idle_since = None;
            return false;
        }
        if !self.daemon {
            return self.had_client;
        }
        if KEEP_RUNNING.load(Ordering::Relaxed) {
            return false;
        }
        self.idle_since.get_or_insert_with(Instant::now).elapsed() >= daemon::IDLE_EXIT
    }

    /// Returns `false` when an editor asked the server to exit.
    fn handle(&mut self, input: Input) -> bool {
        match input {
            Input::Connected { client, writer } => {
                self.clients.insert(client, writer);
                self.had_client = true;
                log::info!(
                    "headless: editor {client} connected ({} now)",
                    self.clients.len()
                );
            }
            Input::Disconnected { client } => {
                self.event_clients.remove(&client);
                if self.clients.remove(&client).is_some() {
                    log::info!(
                        "headless: editor {client} left ({} now)",
                        self.clients.len()
                    );
                }
            }
            Input::Request { client, request } => return self.handle_request(client, request),
            Input::Agent { chat, run, event } => self.apply_event(&chat, run, event),
            Input::AgentGone { chat, run } => self.agent_gone(&chat, run),
            Input::Reply { to, result } => self.reply(to, result),
            Input::Autonomous => self.attach_autonomous_turns(),
        }
        true
    }

    fn handle_request(&mut self, client: u64, request: Value) -> bool {
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let to = ReplyTo {
            client,
            id: id.clone(),
        };
        let method = request["method"].as_str().unwrap_or_default().to_string();
        let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
        // File calls of a remote app can take a while (a search of a large tree): on a worker,
        // so turns keep streaming meanwhile.
        if method.starts_with("fs.") {
            let tx = self.tx.clone();
            let _ = std::thread::Builder::new()
                .name("oxi-headless-fs".into())
                .spawn(move || {
                    let result = remote_app::fs_request(&method, &params);
                    if !id.is_null() {
                        let _ = tx.send(Input::Reply { to, result });
                    }
                });
            return true;
        }
        let result = match method.as_str() {
            "daemon.hello" => Ok(self.hello(&params)),
            "initialize" => Ok(self.initialize()),
            "chats.list" => self.list_chats(&params),
            "workspaces.list" => Ok(self.list_workspaces()),
            // Phones check their connection with it; switching networks leaves dead sockets.
            "ping" => Ok(Value::Null),
            "chat.open" => self.open_chat(&params),
            "chat.set_config" => self.set_config(&params),
            "chat.rename" => self.rename_chat(&params),
            "chat.delete" => self.delete_chat(&params),
            "prompt.send" => self.send_prompt(&params),
            "approval.respond" => self.respond_to_approval(&params),
            "cancel" => self.cancel(&params),
            "subagent.cancel" => match self.cancel_subagent(&params, to) {
                Ok(()) => return true,
                Err(e) => Err(e),
            },
            "models.list" => match self.list_models(&params, to) {
                // Answered from a worker thread once the agent is up.
                Ok(None) => return true,
                Ok(Some(result)) => Ok(result),
                Err(e) => Err(e),
            },
            "chat.image" => self.chat_image(&params),
            "clipboard.image" => clipboard_image(),
            // Answered from worker threads.
            "git" => match self.git(&params, to) {
                Ok(()) => return true,
                Err(e) => Err(e),
            },
            "git.commit_message" => match self.commit_message(&params, to) {
                Ok(()) => return true,
                Err(e) => Err(e),
            },
            "quota.get" => match self.quota(&params, to) {
                Ok(()) => return true,
                Err(e) => Err(e),
            },
            // The editor is leaving; the daemon carries on for the others and the turns running.
            "shutdown" => {
                self.reply(to, Ok(Value::Null));
                self.clients.remove(&client);
                return true;
            }
            "client.features" => Ok(self.client_features(client, &params)),
            "server.info" => Ok(remote_app::server_info()),
            "chat.get" => self.get_chat(&params),
            "settings.get" => self.settings_get(),
            "settings.set" => self.settings_set(&params),
            "settings.set_secret" => self.settings_set_secret(&params),
            "chat.save" => self.save_chat(&params),
            "daemon.stop" => {
                log::info!("headless: editor {client} stopped the server");
                self.reply(to, Ok(Value::Null));
                return false;
            }
            other => Err(format!("unknown method: {other}")),
        };
        if !id.is_null() {
            self.reply(ReplyTo { client, id }, result);
        }
        true
    }

    fn hello(&self, params: &Value) -> Value {
        json!({
            "protocol": PROTOCOL_VERSION,
            "version": env!("CARGO_PKG_VERSION"),
            "pid": std::process::id(),
            "daemon": self.daemon,
            // Only a newer build replaces the daemon, and only while no other editor uses it:
            // editors running different oxi builds would otherwise take turns restarting it.
            "stale": self.daemon
                && self.clients.len() <= 1
                && params.get("exe").is_some_and(|exe| daemon::newer_build(exe, &self.exe)),
            "busy": self.busy(),
            "clients": self.clients.len(),
            "chats_running": self.chats.values().filter(|c| c.run.is_some()).count(),
        })
    }

    fn initialize(&mut self) -> Value {
        self.refresh_settings();
        let oauth = crate::oauth::load_oauth_store();
        let providers: Vec<Value> = self
            .settings
            .configured_provider_kinds(&oauth)
            .into_iter()
            .map(|kind| {
                json!({
                    "id": kind.slug(),
                    "label": crate::app::composer::composer_provider_label(kind),
                    "description": kind.label(),
                    "acp": kind.is_acp(),
                    "model": self.settings.provider(kind).model_id,
                })
            })
            .collect();
        json!({
            "protocol": PROTOCOL_VERSION,
            "version": env!("CARGO_PKG_VERSION"),
            "providers": providers,
        })
    }

    /// Folders to pick from on a phone: the app's sidebar workspaces and every folder with
    /// chats, most recently used first.
    fn list_workspaces(&mut self) -> Value {
        self.refresh_settings();
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        let mut push = |path: &str, modified: Option<SystemTime>, chats: usize| {
            if path.contains("://") || !seen.insert(path.to_string()) || !Path::new(path).is_dir() {
                return;
            }
            let name = Path::new(path)
                .file_name()
                .map_or_else(|| path.to_string(), |n| n.to_string_lossy().to_string());
            out.push(json!({
                "path": path,
                "name": name,
                "modified": modified.map(unix_secs),
                "chats": chats,
            }));
        };
        for (path, modified, chats) in session_store::workspaces_with_chats() {
            push(&path, Some(modified), chats);
        }
        for entry in &self.settings.workspaces {
            push(&entry.root_path, None, 0);
        }
        json!(out)
    }

    fn list_chats(&mut self, params: &Value) -> Result<Value, String> {
        let root = root_param(params)?;
        let chats: Vec<Value> = session_store::load_workspace_sessions(&root)
            .into_iter()
            .filter_map(|session| {
                let id = session.session_file.clone()?;
                let live = self.chats.get(&id);
                let title = live.map_or(&session.title, |c| &c.session.title);
                let config = live
                    .and_then(|c| c.session.config.as_ref())
                    .or(session.config.as_ref());
                Some(json!({
                    "id": id,
                    "title": title,
                    "modified": unix_secs(live.map_or(session.modified, |c| c.session.modified)),
                    "provider": config.map(|c| c.provider.slug()),
                    "running": live.is_some_and(|c| c.run.is_some()),
                    "approval": live.is_some_and(|c| c.pending_approval.is_some()),
                }))
            })
            .collect();
        Ok(json!(chats))
    }

    /// The chat with this id, read from disk on first use and again when the app changed it.
    fn load_chat(&mut self, root: &str, id: &str) -> Result<&mut Chat, String> {
        if self.chats.contains_key(id) {
            self.reload_if_changed(id);
        }
        if !self.chats.contains_key(id) {
            let session =
                session_store::load_session(id).ok_or_else(|| format!("Chat not found: {id}"))?;
            let mut chat = Chat::new(root.to_string(), session);
            chat.disk_mtime = file_mtime(id);
            self.chats.insert(id.to_string(), chat);
        }
        Ok(self.chats.get_mut(id).expect("chat just inserted"))
    }

    /// Read an idle chat again when another process (the app) wrote its file, and tell the
    /// editors; forget it when the file is gone.
    fn reload_if_changed(&mut self, id: &str) {
        let Some(chat) = self.chats.get(id) else {
            return;
        };
        if chat.run.is_some() || !chat.queued.is_empty() {
            return;
        }
        let root = chat.root.clone();
        match std::fs::metadata(id) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.chats.remove(id);
                self.acp.close(id);
                self.notify("chat_removed", json!({"chat": id, "root": root}));
                self.notify("chats_changed", json!({"root": root}));
            }
            Ok(meta) => {
                let mtime = meta.modified().ok();
                if mtime == chat.disk_mtime {
                    return;
                }
                let Some(session) = session_store::load_session(id) else {
                    return;
                };
                log::info!("headless: {id} changed on disk; reading it again");
                let chat = self.chats.get_mut(id).expect("chat checked above");
                chat.session = session;
                chat.disk_mtime = mtime;
                chat.status = None;
                chat.dirty = false;
                chat.pending_approval = None;
                self.notify("chat_reloaded", json!({"chat": id}));
                self.notify("chats_changed", json!({"root": root}));
            }
            Err(_) => {}
        }
    }

    fn check_disk_if_due(&mut self) {
        if self.clients.is_empty() || self.last_disk_check.elapsed() < DISK_CHECK_EVERY {
            return;
        }
        self.last_disk_check = Instant::now();
        let ids: Vec<String> = self.chats.keys().cloned().collect();
        for id in ids {
            self.reload_if_changed(&id);
        }
    }

    fn open_chat(&mut self, params: &Value) -> Result<Value, String> {
        let root = root_param(params)?;
        let id = str_param(params, "chat")?;
        self.load_chat(&root, &id)?;
        let config = self.ensure_config(&id);
        let chat = &self.chats[&id];
        Ok(json!({
            "id": id,
            "title": chat.session.title,
            "config": config_json(&config),
            "plan": chat.plan_mode,
            "evidence": chat.evidence_mode.enabled(),
            "running": chat.run.is_some(),
            "queued": chat.queued.len(),
            "approval": chat.pending_approval,
            "status": chat.status,
            "context": chat.session.agent_context.map(|(_, used, size)| json!({"used": used, "size": size})),
            "messages": chat.session.messages.iter().map(message_json).collect::<Vec<_>>(),
        }))
    }

    /// What a new chat in `root` starts with: the editor's last pick, else the workspace's ACP
    /// agent, else the app's selected provider.
    fn default_config(&self, root: &str) -> SessionConfig {
        if let Some(config) = &self.last_config {
            return config.clone();
        }
        self.settings
            .acp_workspace_preferences
            .get(root)
            .filter(|preference| preference.config.provider.is_acp())
            .map(|preference| preference.config.clone())
            .unwrap_or_else(|| SessionConfig::from_provider(self.settings.active_config()))
    }

    fn ensure_config(&mut self, id: &str) -> SessionConfig {
        let chat = &self.chats[id];
        if let Some(config) = chat.session.config.clone() {
            return config;
        }
        let config = self.default_config(&chat.root.clone());
        self.chats.get_mut(id).expect("chat exists").session.config = Some(config.clone());
        config
    }

    /// `config` with the provider/model/effort fields present in `params` applied.
    fn apply_config_params(
        &self,
        root: &str,
        mut config: SessionConfig,
        params: &Value,
    ) -> Result<SessionConfig, String> {
        if let Some(slug) = params["provider"].as_str() {
            let provider = provider_from_slug(slug)?;
            if provider != config.provider {
                config = self
                    .settings
                    .acp_workspace_preferences
                    .get(root)
                    .filter(|p| provider.is_acp() && p.config.provider == provider)
                    .map(|p| p.config.clone())
                    .unwrap_or_else(|| {
                        SessionConfig::from_provider(self.settings.provider(provider))
                    });
            }
        }
        if let Some(model) = params["model"].as_str() {
            config.model_id = model.trim().to_string();
        }
        if let Some(effort) = params["effort"].as_str() {
            config.effort = effort.trim().to_string();
        }
        Ok(config)
    }

    fn set_config(&mut self, params: &Value) -> Result<Value, String> {
        let root = root_param(params)?;
        let Some(id) = params["chat"].as_str().map(str::to_string) else {
            // A chat not sent yet: remember the pick for when it is.
            let base = self.default_config(&root);
            let config = self.apply_config_params(&root, base, params)?;
            self.last_config = Some(config.clone());
            return Ok(config_json(&config));
        };
        self.load_chat(&root, &id)?;
        let base = self.ensure_config(&id);
        let config = self.apply_config_params(&root, base.clone(), params)?;
        self.last_config = Some(config.clone());
        let chat = self.chats.get_mut(&id).expect("chat loaded");
        if config != base {
            chat.session.config = Some(config.clone());
            chat.session.wire_cache = None;
            if chat.run.is_none() {
                chat.save()?;
            }
            self.notify(
                "config",
                json!({"chat": id, "config": config_json(&config)}),
            );
        }
        Ok(config_json(&config))
    }

    fn rename_chat(&mut self, params: &Value) -> Result<Value, String> {
        let root = root_param(params)?;
        let id = str_param(params, "chat")?;
        let title = str_param(params, "title")?.trim().to_string();
        if title.is_empty() {
            return Err("The title is empty".into());
        }
        let chat = self.load_chat(&root, &id)?;
        chat.session.title = title.clone();
        if chat.run.is_none() {
            chat.save()?;
        }
        self.notify("title", json!({"chat": id, "title": title}));
        self.notify("chats_changed", json!({"root": root}));
        Ok(Value::Null)
    }

    fn delete_chat(&mut self, params: &Value) -> Result<Value, String> {
        let root = root_param(params)?;
        let id = str_param(params, "chat")?;
        if let Some(chat) = self.chats.remove(&id)
            && let Some(run) = &chat.run
        {
            run.cancel.store(true, Ordering::SeqCst);
        }
        self.acp.close(&id);
        crate::app::sessions::delete_session_file_from_disk(Path::new(&id))?;
        self.notify("chat_removed", json!({"chat": id, "root": root}));
        self.notify("chats_changed", json!({"root": root}));
        Ok(Value::Null)
    }

    fn send_prompt(&mut self, params: &Value) -> Result<Value, String> {
        self.refresh_settings();
        let root = root_param(params)?;
        let prompt = Prompt {
            text: params["text"]
                .as_str()
                .unwrap_or_default()
                .trim()
                .to_string(),
            attachments: params["attachments"]
                .as_array()
                .map(|items| items.iter().filter_map(attachment_param).collect())
                .unwrap_or_default(),
            plan: params["plan"].as_bool(),
            evidence: params["evidence"].as_bool(),
        };
        if prompt.text.is_empty() && prompt.attachments.is_empty() {
            return Err("Nothing to send".into());
        }
        let chat = match params["chat"].as_str() {
            Some(id) => {
                self.load_chat(&root, id)?;
                let chat = self.chats.get_mut(id).expect("chat loaded");
                remote_app::check_app_history(chat, params)?;
                if chat.run.is_some() {
                    chat.queued.push_back(prompt);
                    let queued = chat.queued.len();
                    self.notify(
                        "turn",
                        json!({"chat": id, "running": true, "queued": queued}),
                    );
                    return Ok(json!({"chat": id, "queued": queued}));
                }
                self.chats.remove(id).expect("chat loaded")
            }
            None => {
                let base = self.default_config(&root);
                let config = self.apply_config_params(&root, base, params)?;
                Chat::new(root.clone(), empty_session(String::new(), config))
            }
        };
        let mut chat = chat;
        self.apply_app_prompt_params(&mut chat, params)?;
        let id = self.start_turn(chat, prompt)?;
        let session = &self.chats[&id].session;
        let title = session.title.clone();
        // The prompt as stored here, with its @mentions expanded, for the app's copy.
        let user = session.messages.len().checked_sub(2);
        let stored =
            user.map(|i| crate::remote::protocol::messages_to_json(&session.messages[i..i + 1]));
        self.notify("chats_changed", json!({"root": root}));
        Ok(json!({"chat": id, "title": title, "queued": 0, "user_index": user, "user": stored}))
    }

    fn worker_channel(&self, chat: &str, run: u64) -> Sender<AgentEvent> {
        let (worker_tx, worker_rx) = mpsc::channel::<AgentEvent>();
        let tx = self.tx.clone();
        let chat = chat.to_string();
        std::thread::Builder::new()
            .name("oxi-headless-events".into())
            .spawn(move || {
                for event in worker_rx {
                    let input = Input::Agent {
                        chat: chat.clone(),
                        run,
                        event,
                    };
                    if tx.send(input).is_err() {
                        return;
                    }
                }
                let _ = tx.send(Input::AgentGone { chat, run });
            })
            .expect("spawn agent event relay");
        worker_tx
    }

    /// Append the prompt and an empty reply, save, and start the agent. Takes the chat out of
    /// the map and puts it back under its session file path, which a new chat only gets here.
    fn start_turn(&mut self, mut chat: Chat, prompt: Prompt) -> Result<String, String> {
        let config = match chat.session.config.clone() {
            Some(config) => config,
            None => {
                let config = self.default_config(&chat.root);
                chat.session.config = Some(config.clone());
                config
            }
        };
        if chat.session.messages.is_empty() && chat.session.session_file.is_none() {
            let image = prompt
                .attachments
                .iter()
                .any(|a| matches!(a, UserAttachment::Image { .. }));
            chat.session.title = if !prompt.text.is_empty() {
                make_session_title(&prompt.text)
            } else if image {
                "Image".to_string()
            } else {
                "Pasted text".to_string()
            };
        }
        if let Some(plan) = prompt.plan {
            chat.plan_mode = plan;
        }
        if let Some(evidence) = prompt.evidence {
            chat.evidence_mode.set(evidence);
        }
        let cwd = PathBuf::from(chat.root.trim());
        let expanded = crate::app::mentions::expand_at_mentions(&prompt.text, &cwd);
        let wire_candidate = chat.session.wire_cache.take();
        let direct = (config.provider != LlmProviderKind::Router).then(|| {
            Box::new(RouteNote::direct(
                config.provider,
                &config.model_id,
                &config.effort,
            ))
        });
        let usage_target = direct
            .as_ref()
            .map(|note| (note.provider, note.model.clone()));
        chat.session
            .messages
            .push(ChatMessage::user(&expanded, &prompt.attachments));
        chat.session
            .messages
            .push(ChatMessage::streaming_assistant(direct));
        chat.status = None;
        if let Err(e) = chat.save() {
            // Without a session file there is no chat id; report the failure to the caller.
            if chat.session.session_file.is_none() {
                return Err(format!("Save session: {e}"));
            }
            log::warn!("headless: save session: {e}");
        }
        let id = chat.id().expect("saved chat has a file").to_string();

        let mut settings = self.settings.clone();
        config.apply_to_settings(&mut settings);
        let run_id = self.next_run;
        self.next_run += 1;
        let (approval_tx, approval_rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let undo_journal = Arc::new(std::sync::Mutex::new(
            crate::agent::tools::TurnUndoJournal::default(),
        ));
        let len = chat.session.messages.len();
        let request = AgentRunRequest {
            settings,
            tunnels: self.tunnels.clone(),
            acp: self.acp.clone(),
            mcp: self.mcp.clone(),
            acp_session_key: id.clone(),
            cwd,
            chat_for_history: chat.session.messages[..len - 1].to_vec(),
            approval_rx,
            cancel: cancel.clone(),
            wire_candidate,
            chars_per_token: chat
                .session
                .chars_per_token
                .unwrap_or(crate::agent::DEFAULT_CHARS_PER_TOKEN),
            plan_mode: chat.plan_mode,
            evidence_mode: chat.evidence_mode.clone(),
            undo_journal,
        };
        let worker_tx = self.worker_channel(&id, run_id);
        let _join = crate::agent::spawn_agent_run(&self.executor, request, worker_tx);
        chat.run = Some(Run {
            id: run_id,
            approval_tx,
            cancel,
            usage_target,
        });
        chat.pending_approval = None;
        self.chats.insert(id.clone(), chat);
        self.send_message_snapshot(&id, len - 2);
        self.send_message_snapshot(&id, len - 1);
        self.send_turn_state(&id, None);
        Ok(id)
    }

    /// Name chats with the titles their ACP agents generated, while a chat still has the
    /// automatic first-prompt title. A chat that is answering gets it after the turn.
    fn apply_agent_titles(&mut self) {
        for (key, title) in crate::agent::acp::take_agent_titles() {
            let Some(chat) = self.chats.get_mut(&key) else {
                continue;
            };
            if !self.settings.auto_title_chats {
                continue;
            }
            if chat.run.is_some() {
                crate::agent::acp::defer_title(&key, title);
                continue;
            }
            let Some(first) = chat
                .session
                .messages
                .iter()
                .find(|m| m.role == MsgRole::User && !m.is_summary)
            else {
                continue;
            };
            if chat.session.title != make_session_title(&first.text) || chat.session.title == title
            {
                continue;
            }
            chat.session.title = title.clone();
            if let Err(e) = chat.save() {
                log::warn!("saving the agent's chat title: {e}");
                continue;
            }
            let root = chat.root.clone();
            self.notify("title", json!({"chat": key, "title": title}));
            self.notify("chats_changed", json!({"root": root}));
        }
    }

    /// Show the turns ACP agents started on their own, as soon as each chat is free.
    fn attach_autonomous_turns(&mut self) {
        self.apply_agent_titles();
        let mut changed = std::collections::HashSet::new();
        for (key, root, params) in crate::agent::acp::take_delegated() {
            self.forward_event(
                &key,
                &AgentEvent::Delegated {
                    root: root.clone(),
                    params: params.clone(),
                },
            );
            if let Some(chat) = self.chats.get_mut(&key) {
                chat.session.delegated.apply(&root, &params);
                changed.insert(key);
            }
        }
        for key in changed {
            if let Some(chat) = self.chats.get_mut(&key)
                && let Err(e) = chat.save()
            {
                log::warn!("saving subagent transcript: {e}");
            }
        }
        for key in crate::agent::acp::pending_autonomous() {
            // Agents of chats this process never ran belong to another oxi process.
            let Some(chat) = self.chats.get_mut(&key) else {
                crate::agent::acp::claim_autonomous(&key);
                continue;
            };
            if chat.run.is_some() {
                continue;
            }
            crate::agent::acp::claim_autonomous(&key);
            let route = chat
                .session
                .messages
                .iter()
                .rev()
                .find_map(|m| m.route.clone())
                .filter(|r| r.provider.is_acp());
            let usage_target = route.as_ref().map(|r| (r.provider, r.model.clone()));
            chat.session
                .messages
                .push(ChatMessage::streaming_assistant(route));
            chat.status = None;
            let mut settings = self.settings.clone();
            if let Some(config) = &chat.session.config {
                config.apply_to_settings(&mut settings);
            }
            let run_id = self.next_run;
            self.next_run += 1;
            let (approval_tx, approval_rx) = mpsc::channel();
            let cancel = Arc::new(AtomicBool::new(false));
            let run = crate::agent::AcpAttachRun {
                settings,
                acp: self.acp.clone(),
                acp_session_key: key.clone(),
                cwd: PathBuf::from(chat.root.trim()),
                approval_rx,
                cancel: cancel.clone(),
                plan_mode: chat.plan_mode,
                undo_journal: Arc::new(std::sync::Mutex::new(
                    crate::agent::tools::TurnUndoJournal::default(),
                )),
            };
            chat.run = Some(Run {
                id: run_id,
                approval_tx,
                cancel,
                usage_target,
            });
            chat.pending_approval = None;
            let index = chat.session.messages.len() - 1;
            let worker_tx = self.worker_channel(&key, run_id);
            let _join = crate::agent::spawn_acp_attach(&self.executor, run, worker_tx);
            self.send_message_snapshot(&key, index);
            self.send_turn_state(&key, None);
        }
    }

    fn apply_event(&mut self, id: &str, run_id: u64, event: AgentEvent) {
        let Some(chat) = self.chats.get_mut(id) else {
            return;
        };
        if chat.run.as_ref().is_none_or(|run| run.id != run_id) {
            return;
        }
        if !matches!(event, AgentEvent::StreamRetry { .. }) && chat.status.take().is_some() {
            self.notify("status", json!({"chat": id, "text": null}));
        }
        let Some(chat) = self.chats.get_mut(id) else {
            return;
        };
        chat.terminals.retain(|t| t.is_alive());
        self.forward_event(id, &event);
        let Some(chat) = self.chats.get_mut(id) else {
            return;
        };
        match event {
            AgentEvent::Delegated { root, params } => {
                chat.session.delegated.apply(&root, &params);
                chat.dirty = true;
            }
            AgentEvent::AgentStart | AgentEvent::AssistantMessageDone => {}
            AgentEvent::AcpTerminal(pending) => {
                if let Some(terminal) = pending.0.lock().unwrap_or_else(|e| e.into_inner()).take() {
                    chat.terminals.push(terminal);
                }
            }
            AgentEvent::TextStart => {
                if let Some(m) = chat.last_assistant_mut() {
                    m.begin_text_block();
                }
            }
            AgentEvent::TextDelta(delta) => {
                if let Some(m) = chat.last_assistant_mut() {
                    m.append_answer(&delta);
                }
            }
            AgentEvent::ThinkingDelta(delta) => {
                if let Some(m) = chat.last_assistant_mut() {
                    m.append_thinking(&delta);
                }
            }
            AgentEvent::ToolUpdate(update) => {
                if let Some(m) = chat.last_assistant_mut() {
                    crate::model::apply_tool_update(&mut m.blocks, *update);
                }
            }
            AgentEvent::ToolStart {
                name,
                tool_call_id,
                args,
            } => {
                if let Some(m) = chat.last_assistant_mut() {
                    m.start_tool(&name, non_empty(&tool_call_id), args.as_ref());
                }
            }
            AgentEvent::ToolOutput {
                tool_call_id,
                text,
                truncated,
            } => {
                if let Some(m) = chat.last_assistant_mut() {
                    crate::model::set_tool_output_on_blocks(
                        &mut m.blocks,
                        non_empty(&tool_call_id),
                        &text,
                        truncated,
                    );
                }
            }
            AgentEvent::ToolEnd {
                tool_call_id,
                is_error,
                full_output_path,
                diff,
            } => {
                if let Some(m) = chat.last_assistant_mut() {
                    m.finish_tool(non_empty(&tool_call_id), is_error, full_output_path, diff);
                }
            }
            AgentEvent::ApprovalResolved => {
                chat.pending_approval = None;
                self.notify("approval", json!({"chat":id,"approval":null}));
            }
            AgentEvent::ApprovalRequest { name, args } => {
                let summary = crate::agent::approval_summary(&name, &args);
                let allow_prefix = (name == "bash")
                    .then(|| crate::agent::suggest_bash_allow_prefix(&summary))
                    .flatten();
                let approval = json!({
                    "tool": name,
                    "summary": summary,
                    "allow_prefix": allow_prefix,
                });
                chat.pending_approval = Some(approval.clone());
                // Flush first so the tool the approval is about is on screen.
                self.flush_chat(id);
                self.notify("approval", json!({"chat": id, "approval": approval}));
                return;
            }
            AgentEvent::StreamRetry { attempt, reason } => {
                log::info!("headless: stream retry (attempt {attempt}): {reason}");
                if let Some(m) = chat.last_assistant_mut() {
                    m.reset_streaming_tail();
                }
                let text = format!("Retrying (attempt {attempt}): {reason}");
                chat.status = Some(text.clone());
                chat.dirty = true;
                self.notify("status", json!({"chat": id, "text": text}));
                return;
            }
            AgentEvent::Routed(note) => {
                if let Some(run) = chat.run.as_mut() {
                    run.usage_target = Some((note.provider, note.model.clone()));
                }
                if let Some(m) = chat.last_assistant_mut() {
                    m.route = Some(note);
                }
            }
            AgentEvent::ContextUsage { used, size } => {
                let previous = chat.session.agent_context.map(|(_, u, s)| (u, s));
                if let Some(provider) = chat.session.config.as_ref().map(|c| c.provider) {
                    chat.session.agent_context = Some((provider, used, size));
                }
                // Agents repeat the figure after every update; only changes are worth sending.
                if previous != Some((used, size)) {
                    self.notify("context", json!({"chat": id, "used": used, "size": size}));
                }
                return;
            }
            AgentEvent::Usage(usage)
            | AgentEvent::SubagentUsage(usage)
            | AgentEvent::ExternalUsage(usage) => {
                if let Some((kind, model)) = chat.run.as_ref().and_then(|r| r.usage_target.as_ref())
                {
                    let billing = self.settings.router.billing(*kind);
                    crate::router::ledger::record(*kind, model, billing, &usage);
                }
                return;
            }
            AgentEvent::TurnChanges(changes) => {
                if let Some(m) = chat.last_assistant_mut() {
                    m.changes = Some(changes);
                }
            }
            AgentEvent::Evidence(report) => {
                if let Some(m) = chat.last_assistant_mut() {
                    m.evidence = Some(report);
                }
            }
            AgentEvent::Finished(outcome) => {
                self.finish_turn(id, outcome);
                return;
            }
        }
        if let Some(chat) = self.chats.get_mut(id) {
            chat.dirty = true;
        }
    }

    fn agent_gone(&mut self, id: &str, run_id: u64) {
        let running = self
            .chats
            .get(id)
            .and_then(|chat| chat.run.as_ref())
            .is_some_and(|run| run.id == run_id);
        if running {
            self.finish_turn(
                id,
                AgentOutcome::Failed {
                    error: "Agent stopped unexpectedly.".into(),
                },
            );
        }
    }

    fn finish_turn(&mut self, id: &str, outcome: AgentOutcome) {
        let Some(chat) = self.chats.get_mut(id) else {
            return;
        };
        let (label, error) = match outcome {
            AgentOutcome::Success { wire_cache } => {
                chat.session.wire_cache = wire_cache;
                ("success", None)
            }
            AgentOutcome::Failed { error } => {
                chat.session.wire_cache = None;
                if let Some(m) = chat.last_assistant_mut() {
                    m.append_answer(&format!("\n[Error] {error}\n"));
                }
                ("failed", Some(error))
            }
            AgentOutcome::Cancelled => {
                chat.session.wire_cache = None;
                ("cancelled", None)
            }
        };
        if let Some(m) = chat.last_assistant_mut() {
            m.finish_streaming();
        }
        chat.run = None;
        chat.pending_approval = None;
        chat.status = None;
        chat.dirty = true;
        if let Err(e) = chat.save() {
            log::warn!("headless: save session: {e}");
        }
        let root = chat.root.clone();
        let next = (label == "success")
            .then(|| chat.queued.pop_front())
            .flatten();
        if label != "success" {
            chat.queued.clear();
        }
        self.flush_chat(id);
        self.notify("approval", json!({"chat": id, "approval": null}));
        self.send_turn_state(id, Some((label, error)));
        if let Some(prompt) = next
            && let Some(chat) = self.chats.remove(id)
            && let Err(e) = self.start_turn(chat, prompt)
        {
            log::warn!("headless: queued prompt failed: {e}");
        }
        self.notify("chats_changed", json!({"root": root}));
        self.attach_autonomous_turns();
    }

    fn respond_to_approval(&mut self, params: &Value) -> Result<Value, String> {
        let id = str_param(params, "chat")?;
        let (root, tool) = {
            let chat = self.chats.get(&id).ok_or("Chat is not running")?;
            let tool = chat
                .pending_approval
                .as_ref()
                .and_then(|a| a["tool"].as_str())
                .unwrap_or_default()
                .to_string();
            (chat.root.clone(), tool)
        };
        let decision = match params["decision"].as_str().unwrap_or_default() {
            "approve" => ApprovalDecision::Approve,
            "approve_rest" => ApprovalDecision::ApproveRest,
            "deny" => ApprovalDecision::Deny,
            "allow_prefix" => {
                let prefix = str_param(params, "prefix")?;
                self.update_settings(|settings| {
                    if settings.bash_allowlist.contains(&prefix) {
                        return false;
                    }
                    settings.bash_allowlist.push(prefix.clone());
                    true
                });
                ApprovalDecision::AllowPrefix(prefix)
            }
            "allow_tool" => {
                if tool.trim().is_empty() || tool == "bash" {
                    return Err("This tool can't be allowed for the whole workspace".into());
                }
                let root_path = PathBuf::from(root.trim());
                self.update_settings(|settings| settings.allow_tool_in(&root_path, &tool));
                ApprovalDecision::AllowTool(tool)
            }
            other => return Err(format!("unknown decision: {other}")),
        };
        let chat = self.chats.get_mut(&id).ok_or("Chat is not running")?;
        let run = chat.run.as_ref().ok_or("Chat is not running")?;
        let _ = run.approval_tx.send(decision);
        chat.pending_approval = None;
        self.notify("approval", json!({"chat": id, "approval": null}));
        Ok(Value::Null)
    }

    fn cancel(&mut self, params: &Value) -> Result<Value, String> {
        let id = str_param(params, "chat")?;
        if let Some(chat) = self.chats.get_mut(&id) {
            chat.queued.clear();
            if let Some(run) = &chat.run {
                run.cancel.store(true, Ordering::SeqCst);
            }
        }
        Ok(Value::Null)
    }

    /// Models the provider offers. ACP agents advertise theirs once launched, so this starts
    /// the chat's agent (which the next prompt then reuses) and answers from a worker thread.
    fn list_models(&mut self, params: &Value, to: ReplyTo) -> Result<Option<Value>, String> {
        self.refresh_settings();
        let root = root_param(params)?;
        let kind = provider_from_slug(&str_param(params, "provider")?)?;
        let cfg = self.settings.provider(kind).clone();
        if !kind.is_acp() {
            return Ok(Some(json!({"models": [], "current": cfg.model_id})));
        }
        let chat = params["chat"].as_str().map(str::to_string);
        let session_key = chat.unwrap_or_else(|| format!("mem:headless:{root}"));
        let request = crate::agent::acp::AcpWarm {
            session_key,
            cwd: PathBuf::from(root.trim()),
            command_line: cfg.effective_acp_command(),
            env: cfg.acp_env(),
            model: cfg.model_id.clone(),
            effort: cfg.effort.clone(),
            mcp_servers: self.settings.mcp_servers.clone(),
        };
        let acp = self.acp.clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = crate::runtime::runtime()
                .and_then(|rt| rt.block_on(acp.warm(request)))
                .map(|models| json!({"models": models, "current": cfg.model_id}));
            let _ = tx.send(Input::Reply { to, result });
        });
        Ok(None)
    }

    /// An image attached to a user message, as base64.
    fn chat_image(&mut self, params: &Value) -> Result<Value, String> {
        use base64::Engine as _;
        let root = root_param(params)?;
        let id = str_param(params, "chat")?;
        let message = params["message"]
            .as_u64()
            .ok_or("missing parameter: message")? as usize;
        let index = params["index"].as_u64().ok_or("missing parameter: index")? as usize;
        let chat = self.load_chat(&root, &id)?;
        match chat
            .session
            .messages
            .get(message)
            .and_then(|m| m.attachments.get(index))
        {
            Some(UserAttachment::Image { mime, data }) => Ok(json!({
                "mime": mime,
                "data": base64::engine::general_purpose::STANDARD.encode(data),
            })),
            _ => Err("No image there".into()),
        }
    }

    fn cancel_subagent(&mut self, params: &Value, to: ReplyTo) -> Result<(), String> {
        let root = root_param(params)?;
        let id = str_param(params, "chat")?;
        let native = str_param(params, "sessionId")?;
        let child = str_param(params, "child")?;
        let task = params["task"].as_bool().unwrap_or(false);
        let chat = self.load_chat(&root, &id)?;
        if !chat
            .session
            .delegated
            .chats
            .iter()
            .any(|c| c.root == native && c.id == child && c.task == task)
        {
            return Err("Unknown subagent in this conversation".into());
        }
        let acp = self.acp.clone();
        let tx = self.tx.clone();
        crate::runtime::runtime()?.spawn(async move {
            let result = acp
                .cancel_child(id, native, child, task)
                .await
                .map(|_| Value::Null);
            let _ = tx.send(Input::Reply { to, result });
        });
        Ok(())
    }

    /// Queue a git operation on the workspace's repository; the reply is the repository
    /// state after it (see [`git_state_json`]).
    fn git(&mut self, params: &Value, to: ReplyTo) -> Result<(), String> {
        let root = root_param(params)?;
        let op = git_op_param(params)?;
        let jobs = match &self.git_jobs {
            Some(jobs) => jobs.clone(),
            None => {
                let (jobs, rx) = mpsc::channel::<(ReplyTo, String, crate::git::GitOp)>();
                let tx = self.tx.clone();
                std::thread::Builder::new()
                    .name("oxi-headless-git".into())
                    .spawn(move || {
                        for (to, root, op) in rx {
                            let state = crate::git::run_op(&root, op);
                            let result = Ok(git_state_json(&state));
                            if tx.send(Input::Reply { to, result }).is_err() {
                                return;
                            }
                        }
                    })
                    .map_err(|e| format!("Start the git worker: {e}"))?;
                self.git_jobs = Some(jobs.clone());
                jobs
            }
        };
        jobs.send((to, root, op))
            .map_err(|_| "The git worker stopped".to_string())
    }

    /// Write a commit message for the staged changes (or all of them when nothing is
    /// staged) with the model set for commit messages in the app.
    fn commit_message(&mut self, params: &Value, to: ReplyTo) -> Result<(), String> {
        self.refresh_settings();
        let root = root_param(params)?;
        let settings = self.settings.clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = (|| {
                let state = crate::git::run_op(&root, crate::git::GitOp::CollectCommitDiff);
                let diff = state.commit_diff.ok_or_else(|| {
                    state
                        .error
                        .unwrap_or_else(|| "No changes to summarize.".to_string())
                })?;
                let config = crate::router::helper_config(
                    &settings,
                    settings.commit_msg_config(),
                    diff.len(),
                );
                let (rx, _handle) = crate::agent::spawn_completion(crate::agent::CompleteRequest {
                    config,
                    system_prompt: settings.commit_msg_system_prompt.clone(),
                    user_prompt: format!(
                        "Write a git commit message for the following diff.\n\n```diff\n{diff}\n```"
                    ),
                    max_chars: Some(1500),
                    effort_override: Some("low".to_string()),
                });
                for event in rx {
                    if let crate::agent::CompleteEvent::Done(result) = event {
                        return result.map(|text| json!({"message": text.trim()}));
                    }
                }
                Err("Generation stopped unexpectedly.".to_string())
            })();
            let _ = tx.send(Input::Reply { to, result });
        });
        Ok(())
    }

    /// The subscription usage windows (5h, weekly) of a provider whose usage oxi can read.
    fn quota(&mut self, params: &Value, to: ReplyTo) -> Result<(), String> {
        use crate::router::quota;
        self.refresh_settings();
        let kind = provider_from_slug(&str_param(params, "provider")?)?;
        let router = &self.settings.router;
        let enabled = match kind {
            LlmProviderKind::ClaudeCodeAcp => Some(router.read_claude_code_usage),
            LlmProviderKind::CodexAcp => Some(router.read_codex_cli_usage),
            LlmProviderKind::GptCodex => Some(true),
            _ => None,
        };
        if enabled == Some(false) {
            self.reply(to, Ok(quota_json(kind, enabled, None)));
            return Ok(());
        }
        let settings = self.settings.clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            // Throttled inside: a call soon after the last one only reads the board.
            if let Ok(rt) = crate::runtime::runtime() {
                rt.block_on(quota::refresh(&settings, false));
            }
            let result = Ok(quota_json(kind, enabled, quota::snapshot(kind)));
            let _ = tx.send(Input::Reply { to, result });
        });
        Ok(())
    }

    fn send_message_snapshot(&self, id: &str, index: usize) {
        if let Some(message) = self
            .chats
            .get(id)
            .and_then(|c| c.session.messages.get(index))
        {
            self.notify(
                "message",
                json!({"chat": id, "index": index, "message": message_json(message)}),
            );
        }
    }

    fn send_turn_state(&self, id: &str, finished: Option<(&str, Option<String>)>) {
        let Some(chat) = self.chats.get(id) else {
            return;
        };
        let mut params = json!({
            "chat": id,
            "running": chat.run.is_some(),
            "queued": chat.queued.len(),
            "plan": chat.plan_mode,
            "evidence": chat.evidence_mode.enabled(),
        });
        if let Some((outcome, error)) = finished {
            params["outcome"] = json!(outcome);
            params["error"] = json!(error);
        }
        self.notify("turn", params);
    }

    fn flush_chat(&mut self, id: &str) {
        let Some(chat) = self.chats.get_mut(id) else {
            return;
        };
        if !std::mem::take(&mut chat.dirty) {
            return;
        }
        if let Some(index) = chat.last_assistant_index() {
            self.send_message_snapshot(id, index);
        }
    }

    fn flush_if_due(&mut self) {
        if self.last_flush.elapsed() < FLUSH_EVERY {
            return;
        }
        self.last_flush = Instant::now();
        let dirty: Vec<String> = self
            .chats
            .iter()
            .filter(|(_, chat)| chat.dirty)
            .map(|(id, _)| id.clone())
            .collect();
        for id in dirty {
            self.flush_chat(&id);
        }
    }

    /// Stop running turns, keep what they produced, and let the agent subprocesses go.
    fn shutdown(&mut self) {
        for (id, chat) in &mut self.chats {
            if let Some(run) = chat.run.take() {
                run.cancel.store(true, Ordering::SeqCst);
                if let Some(m) = chat.last_assistant_mut() {
                    m.finish_streaming();
                }
                if let Err(e) = chat.save() {
                    log::warn!("headless: save session on exit: {e}");
                }
            }
            self.acp.stop(id);
        }
        // Give the ACP manager a moment to stop the subprocesses before the process exits.
        std::thread::sleep(Duration::from_millis(300));
    }
}

fn settings_mtime() -> Option<SystemTime> {
    std::fs::metadata(AppSettings::config_path())
        .and_then(|m| m.modified())
        .ok()
}

fn unix_secs(time: SystemTime) -> u64 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

fn non_empty(s: &str) -> Option<&str> {
    (!s.is_empty()).then_some(s)
}

fn str_param(params: &Value, name: &str) -> Result<String, String> {
    params[name]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| format!("missing parameter: {name}"))
}

/// The workspace folder, canonicalized like the app does so both find the same chats.
fn root_param(params: &Value) -> Result<String, String> {
    let root = str_param(params, "root")?;
    let path = PathBuf::from(root.trim());
    if !path.is_dir() {
        return Err(format!("Not a folder: {root}"));
    }
    Ok(std::fs::canonicalize(&path)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string())
}

fn provider_from_slug(slug: &str) -> Result<LlmProviderKind, String> {
    LlmProviderKind::ALL
        .into_iter()
        .find(|kind| kind.slug() == slug)
        .ok_or_else(|| format!("unknown provider: {slug}"))
}

/// The image in the clipboard, as an attachment for `prompt.send`; null when there is none.
/// Editors read text from the clipboard themselves, but most cannot read images.
fn clipboard_image() -> Result<Value, String> {
    use base64::Engine as _;
    Ok(match crate::app::sessions::clipboard_image_png()? {
        Some(png) => json!({
            "mime": "image/png",
            "data": base64::engine::general_purpose::STANDARD.encode(png),
        }),
        None => Value::Null,
    })
}

/// A prompt attachment: `{"name", "text"}` for text, `{"mime", "data"}` (base64) for images.
fn attachment_param(value: &Value) -> Option<UserAttachment> {
    use base64::Engine as _;
    if let Some(data) = value["data"].as_str() {
        let mime = value["mime"].as_str().unwrap_or("image/png");
        if !mime.starts_with("image/") {
            return None;
        }
        let data = base64::engine::general_purpose::STANDARD
            .decode(data.trim())
            .ok()?;
        return Some(UserAttachment::Image {
            mime: mime.to_string(),
            data,
        });
    }
    Some(UserAttachment::Text {
        name: value["name"]
            .as_str()
            .unwrap_or("Attached text")
            .to_string(),
        text: value["text"].as_str()?.to_string(),
    })
}

fn paths_param(params: &Value) -> Result<Vec<String>, String> {
    let paths: Vec<String> = params["paths"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|p| p.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if paths.is_empty() {
        return Err("missing parameter: paths".into());
    }
    Ok(paths)
}

fn git_op_param(params: &Value) -> Result<crate::git::GitOp, String> {
    use crate::git::GitOp;
    Ok(match params["op"].as_str().unwrap_or("refresh") {
        "refresh" => GitOp::Refresh,
        "stage" => GitOp::Stage(paths_param(params)?),
        "unstage" => GitOp::Unstage(paths_param(params)?),
        "discard" => GitOp::Discard(paths_param(params)?),
        "commit" => {
            let message = str_param(params, "message")?;
            if message.trim().is_empty() {
                return Err("The commit message is empty".into());
            }
            GitOp::Commit(message)
        }
        "checkout" => GitOp::Checkout(str_param(params, "branch")?),
        "new_branch" => GitOp::NewBranch(str_param(params, "branch")?.trim().to_string()),
        "delete_branch" => GitOp::DeleteBranch {
            name: str_param(params, "branch")?,
            force: params["force"].as_bool().unwrap_or(false),
        },
        "diff" => GitOp::ShowDiff {
            path: str_param(params, "path")?,
            staged: params["staged"].as_bool().unwrap_or(false),
        },
        "show_commit" => GitOp::ShowCommit(str_param(params, "hash")?),
        "fetch" => GitOp::Fetch,
        "pull" => GitOp::Pull,
        "push" => GitOp::Push,
        other => return Err(format!("unknown git op: {other}")),
    })
}

fn git_state_json(state: &crate::git::GitState) -> Value {
    let entries = |entries: &[crate::git::GitEntry]| {
        entries
            .iter()
            .map(|e| json!({"path": e.path, "status": e.status.to_string()}))
            .collect::<Vec<_>>()
    };
    json!({
        "repo": state.repo,
        "branch": state.branch,
        "branches": state.branches,
        "ahead": state.ahead,
        "behind": state.behind,
        "staged": entries(&state.staged),
        "unstaged": entries(&state.unstaged),
        "log": state.log.iter().map(|c| json!({
            "hash": c.hash,
            "date": c.date,
            "author": c.author,
            "message": c.message,
        })).collect::<Vec<_>>(),
        "diff": state.diff.as_ref().map(|(title, text)| json!({"title": title, "text": text})),
        "error": state.error,
    })
}

fn quota_json(
    kind: LlmProviderKind,
    enabled: Option<bool>,
    snapshot: Option<crate::router::quota::QuotaSnapshot>,
) -> Value {
    let now = crate::router::quota::now_secs();
    let Some(snap) = snapshot else {
        return json!({
            "provider": kind.slug(),
            "supported": enabled.is_some(),
            "enabled": enabled.unwrap_or(false),
            "windows": [],
        });
    };
    let windows: Vec<Value> = snap
        .windows
        .iter()
        .map(|w| {
            // A window whose reset already passed is empty again.
            let (used, resets_in) = match w.resets_at {
                Some(t) if t <= now => (0.0, None),
                Some(t) => (w.used_pct, Some(t - now)),
                None => (w.used_pct, None),
            };
            json!({
                "label": w.label,
                "used_pct": used,
                "resets_in": resets_in,
                "scope": w.model_scope,
            })
        })
        .collect();
    json!({
        "provider": kind.slug(),
        "supported": true,
        "enabled": true,
        "windows": windows,
        "plan": snap.plan,
        "credits_left": snap.credits_left,
        "limit_reached": snap.limit_reached,
        "error": snap.error,
    })
}

fn config_json(config: &SessionConfig) -> Value {
    json!({
        "provider": config.provider.slug(),
        "label": crate::app::composer::composer_provider_label(config.provider),
        "model": config.model_id,
        "effort": config.effort,
    })
}

fn head(s: &str, max_chars: usize) -> (&str, bool) {
    match s.char_indices().nth(max_chars) {
        Some((end, _)) => (&s[..end], true),
        None => (s, false),
    }
}

fn tail(s: &str, max_chars: usize) -> (&str, bool) {
    let count = s.chars().count();
    if count <= max_chars {
        return (s, false);
    }
    let start = s
        .char_indices()
        .nth(count - max_chars)
        .map_or(0, |(i, _)| i);
    (&s[start..], true)
}

/// One line saying what a tool call is about: the command, path or query.
fn tool_subject(name: &str, args: Option<&Value>) -> String {
    let Some(args) = args else {
        return String::new();
    };
    let keys: &[&str] = match name {
        "bash" => &["command"],
        "move" => &["from"],
        _ => &[
            "path",
            "file_path",
            "command",
            "pattern",
            "query",
            "url",
            "description",
        ],
    };
    keys.iter()
        .find_map(|key| args.get(key).and_then(Value::as_str))
        .map(|s| s.lines().next().unwrap_or_default().to_string())
        .unwrap_or_default()
}

fn message_json(message: &ChatMessage) -> Value {
    if message.role == MsgRole::User {
        let attachments: Vec<Value> = message
            .attachments
            .iter()
            .map(|a| match a {
                UserAttachment::Text { name, .. } => json!(name),
                UserAttachment::Image { mime, .. } => json!(format!("image ({mime})")),
            })
            .collect();
        let images: Vec<Value> = message
            .attachments
            .iter()
            .enumerate()
            .filter_map(|(index, a)| match a {
                UserAttachment::Image { mime, .. } => Some(json!({"index": index, "mime": mime})),
                UserAttachment::Text { .. } => None,
            })
            .collect();
        return json!({
            "role": "user",
            "text": crate::app::mentions::strip_mention_context(&message.text),
            "summary": message.is_summary,
            "attachments": attachments,
            "images": images,
        });
    }
    let blocks: Vec<Value> = message
        .blocks
        .iter()
        .map(|block| match block {
            AssistantBlock::Thinking(text) => json!({"type": "thinking", "text": text}),
            AssistantBlock::Answer(text) => json!({"type": "answer", "text": text}),
            AssistantBlock::Tool {
                tool_call_id,
                name,
                args_summary,
                output,
                diff,
                is_error,
                full_output_path,
                output_truncated,
                metadata,
            } => {
                let args: Option<Value> = args_summary
                    .as_deref()
                    .and_then(|s| serde_json::from_str(s).ok());
                let status = match (metadata.as_deref().map(|m| m.status), is_error) {
                    (Some(ToolStatus::Pending), _) => "pending",
                    (Some(ToolStatus::InProgress), _) => "in_progress",
                    (Some(ToolStatus::Failed), _) | (None, Some(true)) => "failed",
                    (Some(ToolStatus::Interrupted), _) => "interrupted",
                    (Some(ToolStatus::Completed), _) | (None, Some(false)) => "completed",
                    (None, None) if message.streaming => "in_progress",
                    (None, None) => "completed",
                };
                let title = metadata
                    .as_deref()
                    .map(|m| m.title.trim())
                    .filter(|t| !t.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| tool_subject(name, args.as_ref()));
                let (output, output_cut) = tail(output, TOOL_OUTPUT_TAIL);
                let (diff, diff_cut) = diff
                    .as_deref()
                    .map(|d| head(d, DIFF_MAX))
                    .map_or((None, false), |(d, cut)| (Some(d), cut));
                // The checklist is rebuilt from `todo_write` args, so those stay whole.
                let args_text = args_summary.as_deref().map(|s| {
                    if name == "todo_write" {
                        s
                    } else {
                        head(s, TOOL_ARGS_MAX).0
                    }
                });
                json!({
                    "type": "tool",
                    "id": tool_call_id,
                    "name": metadata.as_deref().and_then(|m| m.name.clone()).unwrap_or_else(|| name.clone()),
                    "kind": metadata.as_deref().map(|m| m.kind.as_str()).unwrap_or_default(),
                    "title": title,
                    "status": status,
                    "args": args_text,
                    "output": output,
                    "output_truncated": *output_truncated || output_cut,
                    "diff": diff,
                    "diff_truncated": diff_cut,
                    "full_output_path": full_output_path,
                    "locations": metadata.as_deref().map(|m| {
                        m.locations.iter().map(|l| json!({"path": l.path, "line": l.line})).collect::<Vec<_>>()
                    }).unwrap_or_default(),
                })
            }
        })
        .collect();
    json!({
        "role": "assistant",
        "streaming": message.streaming,
        "blocks": blocks,
        "evidence": message.evidence,
        "route": message.route.as_deref().map(|route| json!({
            "provider": route.provider.slug(),
            "label": crate::app::composer::composer_provider_label(route.provider),
            "model": route.model,
            "reason": route.reason,
            "failover_from": route.failover_from,
            "direct": route.direct,
        })),
        "worked_secs": message.worked_duration.map(|d| d.as_secs()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_and_tail_cut_on_char_boundaries() {
        assert_eq!(head("ăîșț", 2), ("ăî", true));
        assert_eq!(head("ab", 5), ("ab", false));
        assert_eq!(tail("ăîșț", 2), ("șț", true));
        assert_eq!(tail("ab", 2), ("ab", false));
    }

    #[test]
    fn tool_subject_prefers_the_command_or_path() {
        let args = json!({"command": "cargo test\n--all", "timeout": 5});
        assert_eq!(tool_subject("bash", Some(&args)), "cargo test");
        let args = json!({"path": "src/main.rs"});
        assert_eq!(tool_subject("read", Some(&args)), "src/main.rs");
        assert_eq!(tool_subject("read", None), "");
    }

    #[test]
    fn streaming_reply_serializes_blocks_in_order() {
        let mut reply = ChatMessage::streaming_assistant(None);
        reply.append_thinking("plan");
        reply.begin_text_block();
        reply.append_answer("Hello");
        reply.start_tool("bash", Some("t1"), Some(&json!({"command": "ls"})));
        let value = message_json(&reply);
        let blocks = value["blocks"].as_array().unwrap();
        assert_eq!(blocks[0]["type"], "thinking");
        assert_eq!(blocks[1]["text"], "Hello");
        assert_eq!(blocks[2]["title"], "ls");
        assert_eq!(blocks[2]["status"], "in_progress");
        assert_eq!(value["streaming"], true);
    }

    #[test]
    fn evidence_reports_reach_headless_clients_with_their_claims() {
        let mut reply = ChatMessage::streaming_assistant(None);
        reply.evidence = Some(Box::new(crate::evidence::EvidenceReport {
            stale: false,
            workspace: None,
            turn: "turn".into(),
            requested: true,
            submitted: true,
            summary: "Verified".into(),
            claims: vec![crate::evidence::Claim {
                text: "Check passed".into(),
                evidence: vec!["e1".into()],
            }],
            ..Default::default()
        }));
        let value = message_json(&reply);
        assert_eq!(value["evidence"]["summary"], "Verified");
        assert_eq!(value["evidence"]["claims"][0]["evidence"], json!(["e1"]));
    }

    #[test]
    fn user_messages_hide_mention_context() {
        let dir = std::env::temp_dir().join(format!("oxi-headless-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "content").unwrap();
        let expanded = crate::app::mentions::expand_at_mentions("look at @a.txt", &dir);
        assert_ne!(expanded, "look at @a.txt");
        let value = message_json(&ChatMessage::user(&expanded, &[]));
        assert_eq!(value["text"], "look at @a.txt");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn image_attachments_round_trip_as_base64() {
        use base64::Engine as _;
        let data = base64::engine::general_purpose::STANDARD.encode([1u8, 2, 3]);
        let image = attachment_param(&json!({"mime": "image/png", "data": data}));
        assert!(
            matches!(&image, Some(UserAttachment::Image { mime, data }) if mime == "image/png" && data == &[1, 2, 3])
        );
        assert!(attachment_param(&json!({"mime": "text/plain", "data": data})).is_none());
        let text = attachment_param(&json!({"name": "a", "text": "b"}));
        assert!(matches!(text, Some(UserAttachment::Text { .. })));

        let message = ChatMessage::user("look", &[image.unwrap()]);
        let value = message_json(&message);
        assert_eq!(value["images"][0]["index"], 0);
        assert_eq!(value["images"][0]["mime"], "image/png");
    }

    #[test]
    fn git_ops_need_their_parameters() {
        use crate::git::GitOp;
        assert!(matches!(git_op_param(&json!({})), Ok(GitOp::Refresh)));
        assert!(matches!(
            git_op_param(&json!({"op": "stage", "paths": ["a"]})),
            Ok(GitOp::Stage(p)) if p == ["a"]
        ));
        assert!(git_op_param(&json!({"op": "stage"})).is_err());
        assert!(git_op_param(&json!({"op": "commit", "message": "  "})).is_err());
        assert!(git_op_param(&json!({"op": "rebase"})).is_err());
    }

    #[test]
    fn quota_windows_past_their_reset_read_empty() {
        use crate::router::quota::{QuotaSnapshot, UsageWindow, now_secs};
        let now = now_secs();
        let snap = QuotaSnapshot {
            windows: vec![
                UsageWindow {
                    label: "5h".into(),
                    used_pct: 40.0,
                    resets_at: Some(now + 600),
                    model_scope: None,
                },
                UsageWindow {
                    label: "7d".into(),
                    used_pct: 90.0,
                    resets_at: Some(now.saturating_sub(5)),
                    model_scope: None,
                },
            ],
            ..Default::default()
        };
        let value = quota_json(LlmProviderKind::ClaudeCodeAcp, Some(true), Some(snap));
        assert_eq!(value["windows"][0]["used_pct"], 40.0);
        assert!(value["windows"][0]["resets_in"].as_u64().unwrap() <= 600);
        assert_eq!(value["windows"][1]["used_pct"], 0.0);
        let off = quota_json(LlmProviderKind::ClaudeCodeAcp, Some(false), None);
        assert_eq!(off["enabled"], false);
        assert_eq!(off["supported"], true);
    }

    #[test]
    fn unknown_provider_slug_is_an_error() {
        assert_eq!(
            provider_from_slug("claudecodeacp"),
            Ok(LlmProviderKind::ClaudeCodeAcp)
        );
        assert!(provider_from_slug("nope").is_err());
    }
}
