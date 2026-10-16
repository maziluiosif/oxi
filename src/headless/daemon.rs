//! How editors reach the headless server.
//!
//! `oxi headless`, what editor plugins start, is a thin bridge: it connects to the daemon,
//! starting it when none runs, then copies stdin to the daemon and the daemon's output to
//! stdout. The daemon (`oxi headless --daemon`) is one long-lived server per user that every
//! bridge talks to, so several editors or windows show the same chat live, and a turn keeps
//! running after the editor that started it closes. It exits once no editor has been
//! connected and nothing has run for [`IDLE_EXIT`].
//!
//! A connection opens with a `daemon.hello` request, which the bridge sends and answers
//! itself; the rest is the protocol in the module above. On Unix the daemon listens on a
//! socket in a folder only the user can open. On Windows it listens on a loopback port that it
//! writes, with a random token the hello must carry, to a file in the config folder.
//!
//! When the oxi binary changed since the daemon started (an update or a rebuild) and the
//! daemon is idle, the bridge stops it and starts the new one. `oxi headless --stdio` runs the
//! server in the editor's own process, as before the daemon; the bridge falls back to it when
//! no daemon can be reached.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::net::Shutdown;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Value, json};

use super::Input;

/// The daemon exits after this long with no editor connected and nothing running.
pub(super) const IDLE_EXIT: Duration = Duration::from_secs(15 * 60);
const HELLO_ID: &str = "oxi-bridge-hello";
const START_TIMEOUT: Duration = Duration::from_secs(10);

#[cfg(unix)]
type Stream = std::os::unix::net::UnixStream;
#[cfg(windows)]
type Stream = std::net::TcpStream;

fn run_dir() -> PathBuf {
    crate::app_dirs::config_dir().join("run")
}

/// Create `dir` readable by the user only.
fn private_dir(dir: &std::path::Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn lock_path() -> PathBuf {
    run_dir().join("headless.lock")
}

#[cfg(unix)]
fn socket_path() -> PathBuf {
    let path = run_dir().join("headless.sock");
    // A socket path holds about 100 bytes; a long home folder overflows it.
    if path.as_os_str().len() < 100 {
        return path;
    }
    let user = std::env::var("USER").unwrap_or_else(|_| "user".into());
    std::env::temp_dir()
        .join(format!("oxi-{user}"))
        .join("headless.sock")
}

#[cfg(windows)]
fn endpoint_path() -> PathBuf {
    run_dir().join("headless.endpoint")
}

/// The running binary, so a bridge can tell a daemon started from an older build.
pub(super) fn exe_identity() -> Value {
    let exe = std::env::current_exe().and_then(|p| p.canonicalize());
    let modified = exe
        .as_ref()
        .ok()
        .and_then(|p| fs::metadata(p).ok())
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64);
    json!({
        "path": exe.ok().map(|p| p.to_string_lossy().to_string()),
        "modified": modified,
    })
}

// ---- daemon side ------------------------------------------------------------------------

/// Held while the daemon runs: the lock that keeps a second daemon out, and the endpoint file
/// that is removed again on exit.
pub(super) struct DaemonGuard {
    endpoint: PathBuf,
    _lock: File,
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.endpoint);
    }
}

/// Take the daemon lock and accept editors on a thread, each registered with the server as a
/// client. `Ok(None)` when another daemon already runs.
pub(super) fn listen(tx: Sender<Input>) -> io::Result<Option<DaemonGuard>> {
    private_dir(&run_dir())?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock_path())?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(fs::TryLockError::WouldBlock) => return Ok(None),
        Err(fs::TryLockError::Error(e)) => return Err(e),
    }
    let next_client = Arc::new(AtomicU64::new(1));

    #[cfg(unix)]
    let endpoint = {
        use std::os::unix::fs::PermissionsExt;
        let path = socket_path();
        if let Some(parent) = path.parent() {
            private_dir(parent)?;
        }
        // Left behind by a daemon that did not exit cleanly; the lock says none runs now.
        let _ = fs::remove_file(&path);
        let listener = std::os::unix::net::UnixListener::bind(&path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        std::thread::Builder::new()
            .name("oxi-headless-accept".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    match stream {
                        Ok(stream) => accept(stream, None, &tx, &next_client),
                        Err(e) => log::warn!("headless: accept: {e}"),
                    }
                }
            })?;
        path
    };

    #[cfg(windows)]
    let endpoint = {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        let token = format!("{:032x}", rand::random::<u128>());
        let path = endpoint_path();
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, json!({"port": port, "token": token}).to_string())?;
        fs::rename(&tmp, &path)?;
        let token: Arc<str> = token.into();
        std::thread::Builder::new()
            .name("oxi-headless-accept".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    match stream {
                        Ok(stream) => accept(stream, Some(token.clone()), &tx, &next_client),
                        Err(e) => log::warn!("headless: accept: {e}"),
                    }
                }
            })?;
        path
    };

    Ok(Some(DaemonGuard {
        endpoint,
        _lock: lock,
    }))
}

/// Check the editor's hello on its own thread, then register it.
fn accept(
    stream: Stream,
    #[allow(unused_variables)] token: Option<Arc<str>>,
    tx: &Sender<Input>,
    next_client: &AtomicU64,
) {
    let tx = tx.clone();
    let client = next_client.fetch_add(1, Ordering::Relaxed);
    let spawned = std::thread::Builder::new()
        .name("oxi-headless-client".into())
        .spawn(move || {
            let Ok(write_half) = stream.try_clone() else {
                return;
            };
            let _ = stream.set_read_timeout(Some(START_TIMEOUT));
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                return;
            }
            let Ok(hello) = serde_json::from_str::<Value>(&line) else {
                return;
            };
            if hello["method"] != "daemon.hello" {
                log::warn!("headless: a connection did not start with daemon.hello");
                return;
            }
            #[cfg(windows)]
            {
                if hello["params"]["token"].as_str() != token.as_deref() {
                    log::warn!("headless: a connection had the wrong token");
                    return;
                }
            }
            let _ = reader.get_ref().set_read_timeout(None);
            let close_half = write_half.try_clone().ok();
            add_client(
                &tx,
                client,
                Box::new(reader),
                Box::new(write_half),
                Box::new(move || {
                    if let Some(stream) = close_half {
                        let _ = stream.shutdown(Shutdown::Both);
                    }
                }),
            );
            let _ = tx.send(Input::Request {
                client,
                request: hello,
            });
        });
    if let Err(e) = spawned {
        log::warn!("headless: could not serve a connection: {e}");
    }
}

/// Register a client with the server: a thread writes what the server sends it, another reads
/// its requests. `close` runs once the server dropped the client and its output is written.
pub(super) fn add_client(
    tx: &Sender<Input>,
    client: u64,
    reader: Box<dyn BufRead + Send>,
    mut writer: Box<dyn Write + Send>,
    close: Box<dyn FnOnce() + Send>,
) {
    let (writer_tx, writer_rx) = mpsc::channel::<Arc<str>>();
    let _ = tx.send(Input::Connected {
        client,
        writer: writer_tx,
    });
    let _ = std::thread::Builder::new()
        .name("oxi-headless-write".into())
        .spawn(move || {
            for line in writer_rx {
                let ok = writer.write_all(line.as_bytes()).is_ok()
                    && writer.write_all(b"\n").is_ok()
                    && writer.flush().is_ok();
                if !ok {
                    break;
                }
            }
            close();
        });
    let tx = tx.clone();
    let _ = std::thread::Builder::new()
        .name("oxi-headless-read".into())
        .spawn(move || {
            for line in reader.lines() {
                let Ok(line) = line else { break };
                if line.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str::<Value>(&line) {
                    Ok(request) => {
                        if tx.send(Input::Request { client, request }).is_err() {
                            return;
                        }
                    }
                    Err(e) => log::warn!("headless: ignoring malformed request: {e}"),
                }
            }
            let _ = tx.send(Input::Disconnected { client });
        });
}

/// The editor on this process's stdio as the server's only client (`--stdio`).
pub(super) fn add_stdio_client(tx: &Sender<Input>) {
    add_client(
        tx,
        0,
        Box::new(BufReader::new(io::stdin())),
        Box::new(io::stdout()),
        Box::new(|| {}),
    );
}

// ---- bridge side ------------------------------------------------------------------------

struct Connection {
    stream: Stream,
    reader: BufReader<Stream>,
    hello: Value,
    /// Lines that arrived before the hello's answer, for the editor.
    early: Vec<String>,
}

#[cfg(unix)]
fn connect() -> io::Result<(Stream, Option<String>)> {
    Ok((Stream::connect(socket_path())?, None))
}

#[cfg(windows)]
fn connect() -> io::Result<(Stream, Option<String>)> {
    let endpoint: Value = serde_json::from_str(&fs::read_to_string(endpoint_path())?)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let port = endpoint["port"]
        .as_u64()
        .and_then(|p| u16::try_from(p).ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no port"))?;
    let token = endpoint["token"].as_str().map(str::to_string);
    let stream = Stream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_secs(2),
    )?;
    Ok((stream, token))
}

fn handshake((stream, token): (Stream, Option<String>)) -> io::Result<Connection> {
    let hello = json!({
        "id": HELLO_ID,
        "method": "daemon.hello",
        "params": {"token": token, "exe": exe_identity()},
    });
    let mut writer = stream.try_clone()?;
    writer.write_all(format!("{hello}\n").as_bytes())?;
    writer.flush()?;
    stream.set_read_timeout(Some(START_TIMEOUT))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut early = Vec::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the daemon closed the connection",
            ));
        }
        let message: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
        if message["id"] == HELLO_ID {
            stream.set_read_timeout(None)?;
            if let Some(error) = message["error"].as_str() {
                return Err(io::Error::other(error.to_string()));
            }
            return Ok(Connection {
                stream,
                reader,
                hello: message["result"].clone(),
                early,
            });
        }
        early.push(line.trim_end().to_string());
    }
}

fn spawn_daemon() -> io::Result<()> {
    let exe = std::env::current_exe()?;
    let mut command = std::process::Command::new(exe);
    command
        .args([super::SUBCOMMAND, "--daemon"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if let Some(home) = crate::app_dirs::home_dir() {
        command.current_dir(home);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Its own process group: signals meant for the editor's children don't reach it.
        command.process_group(0);
        command.spawn()?;
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        // Out of the editor's job too, where the job allows it, so closing it doesn't kill us.
        command.creation_flags(
            DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB,
        );
        if command.spawn().is_err() {
            command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
            command.spawn()?;
        }
    }
    Ok(())
}

/// Connect to the daemon that is starting, waiting for it to listen.
fn wait_for_daemon() -> io::Result<Connection> {
    let started = Instant::now();
    loop {
        match connect() {
            Ok(connection) => return handshake(connection),
            Err(e) if started.elapsed() > START_TIMEOUT => return Err(e),
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

/// Wait until the daemon that was asked to stop has let go of its lock.
fn wait_until_stopped() {
    let started = Instant::now();
    while started.elapsed() < START_TIMEOUT {
        if let Ok(lock) = OpenOptions::new().write(true).open(lock_path())
            && lock.try_lock().is_ok()
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn request_stop(connection: Connection) {
    let Connection {
        mut stream,
        mut reader,
        ..
    } = connection;
    let stop = json!({"id": "oxi-bridge-stop", "method": "daemon.stop"});
    if stream.write_all(format!("{stop}\n").as_bytes()).is_ok() {
        // Read until the daemon hangs up.
        let _ = reader.get_ref().set_read_timeout(Some(START_TIMEOUT));
        let _ = io::copy(&mut reader, &mut io::sink());
    }
}

/// A connection to a daemon running the current binary, started if needed.
fn daemon_connection() -> io::Result<Connection> {
    let mut restarted = false;
    loop {
        let connection = match connect() {
            Ok(connection) => handshake(connection)?,
            Err(_) => {
                private_dir(&run_dir())?;
                spawn_daemon()?;
                wait_for_daemon()?
            }
        };
        let stale = connection.hello["stale"].as_bool().unwrap_or(false);
        let busy = connection.hello["busy"].as_bool().unwrap_or(true);
        if stale && !restarted {
            if busy {
                log::info!("headless: the daemon runs an older oxi but is busy; using it");
            } else {
                log::info!("headless: restarting the daemon on the current oxi binary");
                request_stop(connection);
                wait_until_stopped();
                restarted = true;
                continue;
            }
        }
        return Ok(connection);
    }
}

/// `oxi headless`: relay stdio to the daemon. `None` when no daemon could be reached, and
/// the caller serves the editor in this process instead.
pub(super) fn bridge() -> Option<i32> {
    let connection = match daemon_connection() {
        Ok(connection) => connection,
        Err(e) => {
            log::warn!("headless: no daemon ({e}); serving the editor in this process");
            return None;
        }
    };
    let Connection {
        stream,
        mut reader,
        early,
        ..
    } = connection;
    {
        let mut out = io::stdout().lock();
        for line in early {
            if writeln!(out, "{line}").is_err() {
                return Some(0);
            }
        }
        let _ = out.flush();
    }
    let Ok(mut to_daemon) = stream.try_clone() else {
        return None;
    };
    let _ = std::thread::Builder::new()
        .name("oxi-bridge-stdin".into())
        .spawn(move || {
            let _ = io::copy(&mut io::stdin().lock(), &mut to_daemon);
            // The editor is gone: tell the daemon, which then closes our side as well.
            let _ = to_daemon.shutdown(Shutdown::Write);
        });
    // Stdout is line buffered: each line reaches the editor once it is complete.
    let _ = io::copy(&mut reader, &mut io::stdout().lock());
    drop(stream);
    Some(0)
}

/// `oxi headless --status`.
pub(super) fn status() -> i32 {
    match connect().and_then(handshake) {
        Ok(connection) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&connection.hello).unwrap_or_default()
            );
            0
        }
        Err(_) => {
            println!("oxi headless: no daemon is running");
            1
        }
    }
}

/// `oxi headless --stop`: stop the daemon, cancelling the turns it runs.
pub(super) fn stop() -> i32 {
    match connect().and_then(handshake) {
        Ok(connection) => {
            request_stop(connection);
            wait_until_stopped();
            println!("oxi headless: daemon stopped");
            0
        }
        Err(_) => {
            println!("oxi headless: no daemon is running");
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ChatMessage, SessionConfig};

    struct TestClient {
        stream: Stream,
        reader: BufReader<Stream>,
        notes: Vec<Value>,
        next: u64,
    }

    impl TestClient {
        fn connect() -> Self {
            let connection = connect().and_then(handshake).expect("daemon answers");
            assert_eq!(connection.hello["daemon"], true);
            assert_eq!(connection.hello["stale"], false);
            let _ = connection
                .stream
                .set_read_timeout(Some(Duration::from_secs(10)));
            Self {
                stream: connection.stream,
                reader: connection.reader,
                notes: connection
                    .early
                    .iter()
                    .filter_map(|line| serde_json::from_str(line).ok())
                    .collect(),
                next: 1,
            }
        }

        fn read(&mut self) -> Option<Value> {
            let mut line = String::new();
            match self.reader.read_line(&mut line) {
                Ok(0) | Err(_) => None,
                Ok(_) => serde_json::from_str(&line).ok(),
            }
        }

        fn request(&mut self, method: &str, params: Value) -> Value {
            let id = self.next;
            self.next += 1;
            let line = json!({"id": id, "method": method, "params": params});
            self.stream
                .write_all(format!("{line}\n").as_bytes())
                .expect("write request");
            loop {
                let message = self.read().unwrap_or_else(|| panic!("{method}: no answer"));
                if message["id"] == id {
                    assert!(message.get("error").is_none(), "{method}: {message}");
                    return message["result"].clone();
                }
                self.notes.push(message);
            }
        }

        fn wait_for(&mut self, method: &str) -> Value {
            if let Some(i) = self.notes.iter().position(|m| m["method"] == method) {
                return self.notes.remove(i)["params"].clone();
            }
            loop {
                let message = self
                    .read()
                    .unwrap_or_else(|| panic!("no {method} notification"));
                if message["method"] == method {
                    return message["params"].clone();
                }
                self.notes.push(message);
            }
        }
    }

    fn saved_chat(root: &str) -> String {
        let config =
            SessionConfig::from_provider(crate::settings::AppSettings::default().active_config());
        let mut session = super::super::empty_session("first".into(), config);
        session.messages.push(ChatMessage::user("hello", &[]));
        let mut reply = ChatMessage::streaming_assistant(None);
        reply.append_answer("hi");
        reply.finish_streaming();
        session.messages.push(reply);
        crate::session_store::save_session_messages(root, &mut session).expect("save chat");
        session.session_file.expect("saved chat has a file")
    }

    #[test]
    fn one_daemon_serves_several_editors_live() {
        crate::secrets::use_mock_store();
        let root = crate::app_dirs::test_root().join("daemon-workspace");
        fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap().to_string_lossy().to_string();
        let chat = saved_chat(&root);

        let (tx, rx) = mpsc::channel();
        let guard = listen(tx.clone())
            .unwrap()
            .expect("the test daemon is the only one");
        assert!(
            listen(tx.clone()).unwrap().is_none(),
            "a second daemon must not start"
        );
        let server = std::thread::spawn(move || {
            let executor = crate::agent::runner::AgentExecutor::new().unwrap();
            let mut server = super::super::Server::new(tx, executor, true);
            server.run(&rx);
            drop(guard);
        });

        let mut a = TestClient::connect();
        let mut b = TestClient::connect();
        assert!(a.request("initialize", json!({}))["protocol"].is_number());
        let opened = b.request("chat.open", json!({"root": root, "chat": chat}));
        assert_eq!(opened["messages"].as_array().unwrap().len(), 2);

        // What one editor changes, the other sees.
        a.request(
            "chat.rename",
            json!({"root": root, "chat": chat, "title": "renamed"}),
        );
        assert_eq!(b.wait_for("title")["title"], "renamed");
        a.request(
            "chat.set_config",
            json!({"root": root, "chat": chat, "model": "test-model"}),
        );
        assert_eq!(b.wait_for("config")["config"]["model"], "test-model");

        // The app writes the chat: the editors are told to read it again.
        std::thread::sleep(Duration::from_millis(20));
        let mut session = crate::session_store::load_session(&chat).unwrap();
        session
            .messages
            .push(ChatMessage::user("from the app", &[]));
        crate::session_store::save_session_messages(&root, &mut session).unwrap();
        assert_eq!(b.wait_for("chat_reloaded")["chat"], chat.as_str());
        let reopened = b.request("chat.open", json!({"root": root, "chat": chat}));
        assert_eq!(reopened["messages"].as_array().unwrap().len(), 3);

        // An editor leaving doesn't stop the daemon for the others.
        a.request("shutdown", json!({}));
        assert!(
            a.read().is_none(),
            "the daemon hangs up on an editor that left"
        );
        let hello = b.request("daemon.hello", json!({}));
        assert_eq!(hello["clients"], 1);

        b.request("chat.delete", json!({"root": root, "chat": chat}));
        assert_eq!(b.wait_for("chat_removed")["chat"], chat.as_str());

        b.request("daemon.stop", json!({}));
        server.join().unwrap();
        assert!(
            connect().is_err(),
            "the endpoint is gone once the daemon stopped"
        );
        let (tx, _rx) = mpsc::channel();
        assert!(listen(tx).unwrap().is_some(), "the lock is free again");
    }
}
