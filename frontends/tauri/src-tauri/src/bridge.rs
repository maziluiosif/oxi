//! `oxi headless` as a child process: lines the web UI sends go to its stdin, lines it prints
//! come back as `oxi://line` events. It relays to the per-user oxi daemon (starting one when
//! none runs), so the chats here are the ones the app, Sublime and the phone see.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Mutex;

use tauri::{AppHandle, Emitter, Manager, State};

#[derive(Default)]
pub struct Bridge {
    inner: Mutex<Option<Running>>,
}

struct Running {
    child: Child,
    stdin: ChildStdin,
    binary: PathBuf,
    generation: u64,
}

static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl Bridge {
    pub fn stop(&self) {
        let Some(mut running) = self.inner.lock().unwrap_or_else(|e| e.into_inner()).take()
        else {
            return;
        };
        // Detach: the daemon keeps the turns running for the other clients.
        let _ = writeln!(running.stdin, r#"{{"id":0,"method":"shutdown"}}"#);
        let _ = running.stdin.flush();
        std::thread::sleep(std::time::Duration::from_millis(150));
        let _ = running.child.kill();
        let _ = running.child.wait();
    }
}

/// Start the bridge unless it runs; returns the oxi executable used. `binary` overrides the
/// lookup (a path the user picked in Settings).
#[tauri::command]
pub fn oxi_start(
    app: AppHandle,
    bridge: State<'_, Bridge>,
    binary: Option<String>,
) -> Result<String, String> {
    let mut inner = bridge.inner.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(running) = inner.as_mut() {
        if running.child.try_wait().ok().flatten().is_none() {
            return Ok(running.binary.to_string_lossy().to_string());
        }
        *inner = None;
    }
    let binary = binary
        .filter(|b| !b.trim().is_empty())
        .map(PathBuf::from)
        .or_else(find_oxi)
        .ok_or("oxi was not found. Install it, or set its path in Settings → Connection.")?;
    let mut command = Command::new(&binary);
    command
        .arg("headless")
        .current_dir(dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        // Its own process group: Ctrl+C in the terminal that ran `tauri dev` doesn't reach it.
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("Could not start {}: {e}", binary.display()))?;
    let stdin = child.stdin.take().ok_or("no stdin")?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let stderr = child.stderr.take().ok_or("no stderr")?;
    let generation = GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    {
        let app = app.clone();
        std::thread::Builder::new()
            .name("oxi-bridge-out".into())
            .spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    if !line.trim().is_empty() {
                        let _ = app.emit("oxi://line", line);
                    }
                }
                // A stopped/replaced bridge must not disconnect its replacement.
                // stop() removes Running before waiting for the old process.
                let bridge = app.state::<Bridge>();
                let inner = bridge.inner.lock().unwrap_or_else(|e| e.into_inner());
                if inner.as_ref().is_some_and(|running| running.generation == generation) {
                    let _ = app.emit("oxi://exit", generation);
                }
            })
            .map_err(|e| e.to_string())?;
    }
    {
        let app = app.clone();
        std::thread::Builder::new()
            .name("oxi-bridge-err".into())
            .spawn(move || {
                for line in BufReader::new(stderr).lines() {
                    let Ok(line) = line else { break };
                    let _ = app.emit("oxi://log", line);
                }
            })
            .map_err(|e| e.to_string())?;
    }
    let path = binary.to_string_lossy().to_string();
    *inner = Some(Running {
        child,
        stdin,
        binary,
        generation,
    });
    Ok(path)
}

#[tauri::command]
pub fn oxi_send(bridge: State<'_, Bridge>, line: String) -> Result<(), String> {
    let mut inner = bridge.inner.lock().unwrap_or_else(|e| e.into_inner());
    let running = inner.as_mut().ok_or("oxi is not running")?;
    let result = running
        .stdin
        .write_all(line.as_bytes())
        .and_then(|_| running.stdin.write_all(b"\n"))
        .and_then(|_| running.stdin.flush());
    if let Err(e) = result {
        let generation = running.generation;
        *inner = None;
        return Err(format!("oxi stopped ({e}; bridge {generation})"));
    }
    Ok(())
}

#[tauri::command]
pub fn oxi_stop(bridge: State<'_, Bridge>) {
    bridge.stop();
}

/// The executable `oxi_start` would use, for Settings.
#[tauri::command]
pub fn oxi_binary() -> Option<String> {
    find_oxi().map(|p| p.to_string_lossy().to_string())
}

/// `$OXI_PATH`, the usual install locations, then PATH. An app started from the Dock gets a
/// short PATH, so the install locations come before it.
fn find_oxi() -> Option<PathBuf> {
    let exe = if cfg!(windows) { "oxi.exe" } else { "oxi" };
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(path) = std::env::var_os("OXI_PATH") {
        candidates.push(PathBuf::from(path));
    }
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".local/bin").join(exe));
        candidates.push(home.join(".cargo/bin").join(exe));
        candidates.push(home.join("Applications/oxi.app/Contents/MacOS/oxi"));
        if cfg!(windows) {
            candidates.push(home.join("AppData/Local/Programs/oxi").join(exe));
        }
    }
    candidates.push(PathBuf::from("/Applications/oxi.app/Contents/MacOS/oxi"));
    candidates.push(PathBuf::from("/usr/local/bin").join(exe));
    candidates.push(PathBuf::from("/opt/homebrew/bin").join(exe));
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|dir| dir.join(exe)));
    }
    candidates.into_iter().find(|p| is_file(p))
}

fn is_file(path: &Path) -> bool {
    path.metadata().is_ok_and(|m| m.is_file())
}
