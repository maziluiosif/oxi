//! One SSH connection per remote host: `ssh <host> oxi headless` with the headless protocol on
//! its stdio.
//!
//! Requests get their answer on a channel ([`Pending`]). Agent events of the chats the app
//! follows go straight onto the chat's event channel, the same kind a local turn feeds, so the
//! app handles a remote turn with its usual code. Everything else the server announces reaches
//! the app as a [`Note`]. The reader thread does the routing, in the order lines arrive, which
//! is what makes attaching to a running turn exact: events before the snapshot's answer are in
//! the snapshot, events after it are not.
//!
//! A dropped connection is retried with a growing pause while the app wants the host; turns
//! being followed are attached again once it is back (they kept running on the server).

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::protocol;
use crate::agent::{AgentEvent, AgentOutcome};

/// The answer to one request.
pub type Pending = Receiver<Result<Value, String>>;
pub type Waker = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnState {
    Connecting,
    Connected,
    /// The last attempt failed; another follows after a pause.
    Failed(String),
}

/// What the server said that isn't an answer or a followed chat's event.
#[derive(Debug)]
pub enum Note {
    State {
        host: String,
        state: ConnState,
    },
    /// A server notification (`chats_changed`, `title`, `turn`, `agent_event` of a chat the app
    /// doesn't follow, …).
    Server {
        host: String,
        method: String,
        params: Value,
    },
    /// A followed turn's chat as the server has it after a reconnect; applied before the
    /// events that follow it.
    Resync {
        host: String,
        chat: String,
        result: Value,
    },
}

enum Route {
    Live(Sender<AgentEvent>),
    /// Waiting for the snapshot `chat.get` (request `id`) before passing events on.
    Snapshot {
        id: u64,
        tx: Sender<AgentEvent>,
    },
}

enum Waiting {
    Plain(Sender<Result<Value, String>>),
    /// `prompt.send`: on success the chat in the answer is followed with `tx`. On a history
    /// mismatch the request is sent again with `history`.
    Turn {
        reply: Sender<Result<Value, String>>,
        tx: Sender<AgentEvent>,
        params: Value,
        history: Option<Value>,
    },
    /// `chat.get` that a route is waiting on.
    Snapshot {
        reply: Option<Sender<Result<Value, String>>>,
        chat: String,
    },
}

struct Link {
    stdin: ChildStdin,
    child: Child,
}

struct Shared {
    host: String,
    command: String,
    notes: Sender<Note>,
    waker: Waker,
    next_id: AtomicU64,
    link: Mutex<Option<Link>>,
    waiting: Mutex<HashMap<u64, Waiting>>,
    routes: Mutex<HashMap<String, Route>>,
    /// Workspace of each followed chat, for asking for it again after a reconnect.
    roots: Mutex<HashMap<String, String>>,
    state: Mutex<ConnState>,
    /// Set once the app dropped the client; the connection thread stops.
    closed: AtomicBool,
    /// Ask the connection thread to try again now instead of after its pause.
    retry_now: AtomicBool,
    info: Mutex<Value>,
}

pub struct Client {
    shared: Arc<Shared>,
}

/// Every connected client by host, for code without the app at hand (see [`call`]).
static REGISTRY: Mutex<Option<HashMap<String, std::sync::Weak<Shared>>>> = Mutex::new(None);

/// Send a request to `host`'s server and wait for the answer, at most `timeout`. For the file
/// calls of [`crate::vfs`]; the app's own requests use [`Client::request`].
pub fn call(host: &str, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
    let shared = REGISTRY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|clients| clients.get(host))
        .and_then(std::sync::Weak::upgrade)
        .ok_or_else(|| format!("Not connected to {host}"))?;
    if *shared.state.lock().unwrap_or_else(|e| e.into_inner()) != ConnState::Connected {
        return Err(format!("Not connected to {host}"));
    }
    let (tx, rx) = mpsc::channel();
    let id = shared.next_id.fetch_add(1, Ordering::Relaxed);
    shared.send(id, method, &params, Waiting::Plain(tx));
    match rx.recv_timeout(timeout) {
        Ok(result) => result,
        Err(_) => {
            shared
                .waiting
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
            Err(format!("{host} did not answer in time"))
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.shared.closed.store(true, Ordering::SeqCst);
        if let Some(mut link) = self
            .shared
            .link
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            let _ = link.child.kill();
        }
    }
}

/// What runs on the host: `oxi-server` (see [`super::service`]) or else an installed oxi.
pub fn default_remote_command() -> String {
    let candidates = [
        "$HOME/.local/bin/oxi-server",
        "$HOME/.local/bin/oxi",
        "/Applications/oxi.app/Contents/MacOS/oxi",
        "/opt/homebrew/bin/oxi",
        "/usr/local/bin/oxi",
        "/usr/bin/oxi",
    ];
    format!(
        "sh -c 'for o in {}; do [ -x \"$o\" ] && exec \"$o\" headless; done; \
         command -v oxi >/dev/null && exec oxi headless; \
         echo \"oxi is not installed on this host\" >&2; exit 127'",
        candidates
            .iter()
            .map(|c| format!("\"{c}\""))
            .collect::<Vec<_>>()
            .join(" ")
    )
}

impl Client {
    /// Start connecting to `host` (an `ssh` destination: a `~/.ssh/config` alias, `user@host`).
    pub fn connect(host: &str, command: Option<&str>, notes: Sender<Note>, waker: Waker) -> Self {
        let shared = Arc::new(Shared {
            host: host.to_string(),
            command: command
                .filter(|c| !c.trim().is_empty())
                .map(str::to_string)
                .unwrap_or_else(default_remote_command),
            notes,
            waker,
            next_id: AtomicU64::new(1),
            link: Mutex::new(None),
            waiting: Mutex::new(HashMap::new()),
            routes: Mutex::new(HashMap::new()),
            roots: Mutex::new(HashMap::new()),
            state: Mutex::new(ConnState::Connecting),
            closed: AtomicBool::new(false),
            retry_now: AtomicBool::new(false),
            info: Mutex::new(Value::Null),
        });
        REGISTRY
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert_with(HashMap::new)
            .insert(host.to_string(), Arc::downgrade(&shared));
        let thread_shared = Arc::clone(&shared);
        let _ = std::thread::Builder::new()
            .name(format!("oxi-remote-{host}"))
            .spawn(move || connection_loop(&thread_shared));
        Self { shared }
    }

    pub fn state(&self) -> ConnState {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// `server.info` of the connected server (`Null` before the first connection).
    pub fn info(&self) -> Value {
        self.shared
            .info
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Skip the pause before the next connection attempt.
    pub fn retry_now(&self) {
        self.shared.retry_now.store(true, Ordering::SeqCst);
    }

    pub fn request(&self, method: &str, params: Value) -> Pending {
        let (tx, rx) = mpsc::channel();
        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        self.shared.send(id, method, &params, Waiting::Plain(tx));
        rx
    }

    /// Send without wanting the answer.
    pub fn notify(&self, method: &str, params: Value) {
        drop(self.request(method, params));
    }

    /// Start a turn (`prompt.send`). The chat's events arrive on the returned channel, which
    /// closes after `Finished`. `history` goes along only if the server's transcript turns out
    /// to differ from the one `params["history_hash"]` describes.
    pub fn start_turn(&self, params: Value, history: Value) -> (Pending, Receiver<AgentEvent>) {
        let (reply, reply_rx) = mpsc::channel();
        let (tx, rx) = mpsc::channel();
        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        self.shared.send(
            id,
            "prompt.send",
            &params,
            Waiting::Turn {
                reply,
                tx,
                params: params.clone(),
                history: Some(history),
            },
        );
        (reply_rx, rx)
    }

    /// Follow a chat whose turn runs on the server without the app having started it. The
    /// snapshot answer comes on the returned `Pending`; the events after it on the channel.
    pub fn attach(&self, root: &str, chat: &str) -> (Pending, Receiver<AgentEvent>) {
        let (reply, reply_rx) = mpsc::channel();
        let (tx, rx) = mpsc::channel();
        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        self.shared.remember_root(chat, root);
        self.shared
            .routes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(chat.to_string(), Route::Snapshot { id, tx });
        self.shared.send(
            id,
            "chat.get",
            &json!({"root": root, "chat": chat}),
            Waiting::Snapshot {
                reply: Some(reply),
                chat: chat.to_string(),
            },
        );
        (reply_rx, rx)
    }

    /// Whether the app follows this chat's events.
    pub fn follows(&self, chat: &str) -> bool {
        self.shared
            .routes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(chat)
    }
}

impl Shared {
    fn remember_root(&self, chat: &str, root: &str) {
        self.roots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(chat.to_string(), root.to_string());
    }

    fn wake(&self) {
        (self.waker)();
    }

    fn note(&self, note: Note) {
        let _ = self.notes.send(note);
        self.wake();
    }

    fn set_state(&self, state: ConnState) {
        let changed = {
            let mut current = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let changed = *current != state;
            *current = state.clone();
            changed
        };
        if changed {
            self.note(Note::State {
                host: self.host.clone(),
                state,
            });
        }
    }

    /// Write a request, or fail it at once when there is no connection.
    fn send(&self, id: u64, method: &str, params: &Value, waiting: Waiting) {
        let line = json!({"id": id, "method": method, "params": params}).to_string();
        self.waiting
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, waiting);
        let written = {
            let mut link = self.link.lock().unwrap_or_else(|e| e.into_inner());
            link.as_mut().is_some_and(|link| {
                link.stdin.write_all(line.as_bytes()).is_ok()
                    && link.stdin.write_all(b"\n").is_ok()
                    && link.stdin.flush().is_ok()
            })
        };
        if !written {
            let waiting = self
                .waiting
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
            if let Some(waiting) = waiting {
                self.fail(waiting, format!("Not connected to {}", self.host));
            }
        }
    }

    fn fail(&self, waiting: Waiting, error: String) {
        match waiting {
            Waiting::Plain(reply) => {
                let _ = reply.send(Err(error));
            }
            Waiting::Turn { reply, tx, .. } => {
                let _ = tx.send(AgentEvent::Finished(AgentOutcome::Failed {
                    error: error.clone(),
                }));
                let _ = reply.send(Err(error));
            }
            Waiting::Snapshot { reply, chat } => {
                if let Some(reply) = reply {
                    let _ = reply.send(Err(error));
                }
                // Leave the route; a reconnect attaches it again.
                let _ = chat;
            }
        }
        self.wake();
    }

    /// Route one line from the server.
    fn handle_line(&self, line: &str) {
        let Ok(message) = serde_json::from_str::<Value>(line) else {
            log::warn!("remote {}: unreadable line", self.host);
            return;
        };
        if let Some(id) = message.get("id").and_then(Value::as_u64) {
            let result = match message.get("error") {
                Some(error) if !error.is_null() => {
                    Err(error.as_str().unwrap_or("error").to_string())
                }
                _ => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
            };
            self.answer(id, result);
            return;
        }
        let method = message["method"].as_str().unwrap_or_default().to_string();
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        if method == "agent_event" {
            let chat = params["chat"].as_str().unwrap_or_default();
            let mut routes = self.routes.lock().unwrap_or_else(|e| e.into_inner());
            match routes.get(chat) {
                Some(Route::Live(tx)) => {
                    if let Some(event) = protocol::event_from_json(&params["event"]) {
                        let finished = matches!(event, AgentEvent::Finished(_));
                        let gone = tx.send(event).is_err();
                        if finished || gone {
                            routes.remove(chat);
                        }
                        drop(routes);
                        self.wake();
                    }
                    return;
                }
                // Already in the snapshot being read.
                Some(Route::Snapshot { .. }) => return,
                None => {}
            }
        }
        self.note(Note::Server {
            host: self.host.clone(),
            method,
            params,
        });
    }

    fn answer(&self, id: u64, result: Result<Value, String>) {
        let Some(waiting) = self
            .waiting
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id)
        else {
            return;
        };
        match waiting {
            Waiting::Plain(reply) => {
                let _ = reply.send(result);
            }
            Waiting::Turn {
                reply,
                tx,
                mut params,
                history,
            } => match result {
                Ok(value) => {
                    // A retransmitted prompt the server had already run: nothing more comes
                    // for it, so end the turn here (the app then reads the chat's copy).
                    let finished_before = value["duplicate"] == true && value["running"] != true;
                    if let Some(chat) = value["chat"].as_str() {
                        if let Some(root) = params["root"].as_str() {
                            self.remember_root(chat, root);
                        }
                        if !finished_before {
                            self.routes
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .insert(chat.to_string(), Route::Live(tx.clone()));
                        }
                    }
                    if finished_before {
                        let _ = tx.send(AgentEvent::Finished(AgentOutcome::Success {
                            wire_cache: None,
                        }));
                        self.wake();
                    }
                    let _ = reply.send(Ok(value));
                }
                Err(e) if e == crate::headless::HISTORY_MISMATCH && history.is_some() => {
                    params["history"] = history.unwrap_or(Value::Null);
                    let id = self.next_id.fetch_add(1, Ordering::Relaxed);
                    let resend = params.clone();
                    self.send(
                        id,
                        "prompt.send",
                        &resend,
                        Waiting::Turn {
                            reply,
                            tx,
                            params,
                            history: None,
                        },
                    );
                    return;
                }
                Err(e) => {
                    self.fail(
                        Waiting::Turn {
                            reply,
                            tx,
                            params,
                            history,
                        },
                        e,
                    );
                    return;
                }
            },
            Waiting::Snapshot { reply, chat } => {
                let mut routes = self.routes.lock().unwrap_or_else(|e| e.into_inner());
                let route = routes.remove(&chat);
                let running = result
                    .as_ref()
                    .is_ok_and(|v| v["running"].as_bool() == Some(true));
                match (route, reply) {
                    // `attach`: the app applies the snapshot, then the events.
                    (Some(Route::Snapshot { tx, .. }), Some(reply)) => {
                        if running {
                            routes.insert(chat, Route::Live(tx));
                        }
                        drop(routes);
                        let _ = reply.send(result);
                    }
                    // A reconnect: the app replaces its copy, and the run ends if the turn
                    // finished while the connection was down.
                    (Some(Route::Snapshot { tx, .. }), None) => {
                        drop(routes);
                        if let Ok(value) = result {
                            self.note(Note::Resync {
                                host: self.host.clone(),
                                chat: chat.clone(),
                                result: value,
                            });
                            if running {
                                self.routes
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .insert(chat, Route::Live(tx));
                            } else {
                                let _ = tx.send(AgentEvent::Finished(AgentOutcome::Success {
                                    wire_cache: None,
                                }));
                            }
                        }
                    }
                    (route, reply) => {
                        if let Some(route) = route {
                            routes.insert(chat, route);
                        }
                        drop(routes);
                        if let Some(reply) = reply {
                            let _ = reply.send(result);
                        }
                    }
                }
            }
        }
        self.wake();
    }

    /// The connection is gone: fail what waits, and mark followed turns for re-attaching.
    fn disconnected(&self, reason: &str) {
        let waiting: Vec<Waiting> = self
            .waiting
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain()
            .map(|(_, w)| w)
            .collect();
        for w in waiting {
            self.fail(w, format!("Connection to {} lost: {reason}", self.host));
        }
        let mut routes = self.routes.lock().unwrap_or_else(|e| e.into_inner());
        for route in routes.values_mut() {
            let tx = match route {
                Route::Live(tx) | Route::Snapshot { tx, .. } => tx.clone(),
            };
            let _ = tx.send(AgentEvent::StreamRetry {
                attempt: 0,
                reason: format!("connection to {} lost; reconnecting", self.host),
            });
            *route = Route::Snapshot { id: 0, tx };
        }
    }

    /// Back online: ask for every followed chat again.
    fn reattach(&self) {
        let chats: Vec<String> = self
            .routes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect();
        for chat in chats {
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            if let Some(Route::Snapshot { id: route_id, .. }) = self
                .routes
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get_mut(&chat)
            {
                *route_id = id;
            }
            let root = self
                .roots
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&chat)
                .cloned()
                .unwrap_or_default();
            self.send(
                id,
                "chat.get",
                &json!({"root": root, "chat": chat, "from": 0}),
                Waiting::Snapshot { reply: None, chat },
            );
        }
    }
}

fn spawn_ssh(shared: &Shared) -> std::io::Result<Child> {
    let mut command = Command::new("ssh");
    command
        .args([
            "-T",
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=10",
            "-o",
            "ServerAliveInterval=15",
            "-o",
            "ServerAliveCountMax=4",
            "--",
            &shared.host,
            &shared.command,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // ssh-agent and the user's PATH, as in their terminal.
    crate::shell_env::apply(&mut command);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command.spawn()
}

/// Connect, serve until the connection drops, pause, repeat; until the client is dropped.
fn connection_loop(shared: &Arc<Shared>) {
    let pauses = [1u64, 2, 5, 10, 20, 30];
    let mut failures = 0usize;
    while !shared.closed.load(Ordering::SeqCst) {
        shared.set_state(ConnState::Connecting);
        let started = Instant::now();
        let error = match serve_once(shared) {
            Ok(()) => "the server closed the connection".to_string(),
            Err(e) => e,
        };
        if shared.closed.load(Ordering::SeqCst) {
            return;
        }
        log::warn!("remote {}: {error}", shared.host);
        shared.disconnected(&error);
        shared.set_state(ConnState::Failed(error));
        // A connection that lasted a while starts the pauses over.
        if started.elapsed() > Duration::from_secs(60) {
            failures = 0;
        }
        let pause = Duration::from_secs(pauses[failures.min(pauses.len() - 1)]);
        failures += 1;
        let until = Instant::now() + pause;
        while Instant::now() < until && !shared.closed.load(Ordering::SeqCst) {
            if shared.retry_now.swap(false, Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

fn serve_once(shared: &Arc<Shared>) -> Result<(), String> {
    let mut child = spawn_ssh(shared).map_err(|e| format!("could not run ssh: {e}"))?;
    let stdin = child.stdin.take().ok_or("ssh: no stdin")?;
    let stdout = child.stdout.take().ok_or("ssh: no stdout")?;
    let stderr = child.stderr.take();
    // The last lines ssh or the remote shell printed, for the error shown when it fails.
    let errors = Arc::new(Mutex::new(Vec::<String>::new()));
    if let Some(stderr) = stderr {
        let errors = Arc::clone(&errors);
        let host = shared.host.clone();
        let _ = std::thread::Builder::new()
            .name("oxi-remote-stderr".into())
            .spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    log::info!("remote {host} stderr: {line}");
                    let mut errors = errors.lock().unwrap_or_else(|e| e.into_inner());
                    errors.push(line);
                    let excess = errors.len().saturating_sub(5);
                    errors.drain(..excess);
                }
            });
    }
    *shared.link.lock().unwrap_or_else(|e| e.into_inner()) = Some(Link { stdin, child });

    let (hello_tx, hello_rx) = mpsc::channel();
    let hello_id = shared.next_id.fetch_add(1, Ordering::Relaxed);
    shared.send(
        hello_id,
        "client.features",
        &json!({"events": true, "client": "oxi-app", "version": env!("CARGO_PKG_VERSION")}),
        Waiting::Plain(hello_tx),
    );
    let (info_tx, info_rx) = mpsc::channel();
    let info_id = shared.next_id.fetch_add(1, Ordering::Relaxed);
    shared.send(info_id, "server.info", &json!({}), Waiting::Plain(info_tx));

    let mut reader = BufReader::new(stdout);
    let mut connected = false;
    let mut line = String::new();
    let result = loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break Err(ssh_error(&errors, "the connection closed")),
            Ok(_) => {}
            Err(e) => break Err(ssh_error(&errors, &e.to_string())),
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        shared.handle_line(trimmed);
        if !connected && let Ok(features) = hello_rx.try_recv() {
            let ok = features.as_ref().is_ok_and(|f| {
                f["features"]
                    .as_array()
                    .is_some_and(|list| list.iter().any(|x| x == protocol::FEATURE))
            });
            if !ok {
                break Err(
                    "the oxi on this host is too old for the app; update it and run `oxi server install` there"
                        .into(),
                );
            }
            connected = true;
        }
        if connected && let Ok(Ok(info)) = info_rx.try_recv() {
            *shared.info.lock().unwrap_or_else(|e| e.into_inner()) = info;
            shared.set_state(ConnState::Connected);
            shared.reattach();
        }
    };
    if let Some(mut link) = shared.link.lock().unwrap_or_else(|e| e.into_inner()).take() {
        let _ = link.child.kill();
        let _ = link.child.wait();
    }
    // Drain what the reader still holds so the pipe closes cleanly.
    let _ = reader.read_to_end(&mut Vec::new());
    result
}

fn ssh_error(errors: &Mutex<Vec<String>>, fallback: &str) -> String {
    let errors = errors.lock().unwrap_or_else(|e| e.into_inner());
    let text = errors
        .iter()
        .filter(|l| !l.trim().is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    if text.is_empty() {
        fallback.to_string()
    } else {
        text
    }
}
