//! Shells for the terminal panel. Output goes to the web UI as `pty://<id>` events carrying
//! base64 bytes (xterm.js decodes UTF-8 across chunk boundaries itself); `pty-exit://<id>`
//! tells it the shell ended.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use base64::Engine as _;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use tauri::{AppHandle, Emitter, State};

#[derive(Default)]
pub struct Terminals {
    sessions: Mutex<HashMap<u32, Session>>,
}

struct Session {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
}

static NEXT_ID: AtomicU32 = AtomicU32::new(1);

impl Terminals {
    pub fn kill_all(&self) {
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        for (_, mut session) in sessions.drain() {
            let _ = session.child.kill();
        }
    }
}

#[tauri::command]
pub fn pty_spawn(
    app: AppHandle,
    terminals: State<'_, Terminals>,
    cwd: Option<String>,
    cols: u16,
    rows: u16,
) -> Result<u32, String> {
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: rows.max(2),
            cols: cols.max(10),
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| e.to_string())?;
    let mut command = shell_command();
    if let Some(cwd) = cwd.filter(|c| std::path::Path::new(c).is_dir()) {
        command.cwd(cwd);
    } else if let Some(home) = dirs::home_dir() {
        command.cwd(home);
    }
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    command.env("TERM_PROGRAM", "oxi");
    let child = pair
        .slave
        .spawn_command(command)
        .map_err(|e| format!("Could not start the shell: {e}"))?;
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
    let writer = pair.master.take_writer().map_err(|e| e.to_string())?;
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    std::thread::Builder::new()
        .name(format!("oxi-pty-{id}"))
        .spawn(move || {
            let mut buf = [0u8; 16 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let data = base64::engine::general_purpose::STANDARD.encode(&buf[..n]);
                        let _ = app.emit(&format!("pty://{id}"), data);
                    }
                }
            }
            let _ = app.emit(&format!("pty-exit://{id}"), ());
        })
        .map_err(|e| e.to_string())?;
    terminals
        .sessions
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(
            id,
            Session {
                master: pair.master,
                writer,
                child,
            },
        );
    Ok(id)
}

fn shell_command() -> CommandBuilder {
    if cfg!(windows) {
        let shell = std::env::var("COMSPEC").unwrap_or_else(|_| "powershell.exe".into());
        return CommandBuilder::new(shell);
    }
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/bin/zsh".into());
    let mut command = CommandBuilder::new(shell);
    // A login shell: the user's PATH even when the app was started from the Dock.
    command.arg("-l");
    command
}

#[tauri::command]
pub fn pty_write(terminals: State<'_, Terminals>, id: u32, data: String) -> Result<(), String> {
    let mut sessions = terminals.sessions.lock().unwrap_or_else(|e| e.into_inner());
    let session = sessions.get_mut(&id).ok_or("no such terminal")?;
    session
        .writer
        .write_all(data.as_bytes())
        .and_then(|_| session.writer.flush())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn pty_resize(
    terminals: State<'_, Terminals>,
    id: u32,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let sessions = terminals.sessions.lock().unwrap_or_else(|e| e.into_inner());
    let session = sessions.get(&id).ok_or("no such terminal")?;
    session
        .master
        .resize(PtySize {
            rows: rows.max(2),
            cols: cols.max(10),
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn pty_kill(terminals: State<'_, Terminals>, id: u32) {
    let session = terminals
        .sessions
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&id);
    if let Some(mut session) = session {
        let _ = session.child.kill();
    }
}
