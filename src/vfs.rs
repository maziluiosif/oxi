//! File access that also reaches remote workspaces: a path `ssh://host/...` (see
//! [`crate::remote`]) is read and written by that host's oxi server, any other path here.
//!
//! The editor and explorer go through this instead of `std::fs`, which is all it takes for them
//! to work on a remote workspace. Remote calls block the caller for one round trip, so their
//! metadata is reused for a moment: the editor checks open files for outside changes twice a
//! second.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use base64::Engine as _;
use serde_json::{Value, json};

/// Long enough for a large file over a slow link; the UI waits this long at worst.
const TIMEOUT: Duration = Duration::from_secs(8);
/// How long a remote file's metadata is reused.
const STAT_TTL: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Meta {
    pub len: u64,
    pub modified: Option<SystemTime>,
    pub is_dir: bool,
    pub is_file: bool,
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub is_file: bool,
}

/// `(host, path on it)` for a remote path.
fn remote(path: &Path) -> Option<(String, String)> {
    let text = path.to_str()?;
    crate::remote::split(text).map(|(host, path)| (host.to_string(), path.to_string()))
}

pub fn is_remote(path: &Path) -> bool {
    remote(path).is_some()
}

/// Remote metadata by path: when it was asked for, and what came back (`None`: no such file).
type StatCache = HashMap<PathBuf, (Instant, Option<Meta>)>;
static STATS: Mutex<Option<StatCache>> = Mutex::new(None);

fn cached_stat(path: &Path) -> Option<Option<Meta>> {
    let stats = STATS.lock().unwrap_or_else(|e| e.into_inner());
    let (at, meta) = stats.as_ref()?.get(path)?;
    (at.elapsed() < STAT_TTL).then_some(*meta)
}

fn remember_stat(path: &Path, meta: Option<Meta>) {
    STATS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashMap::new)
        .insert(path.to_path_buf(), (Instant::now(), meta));
}

fn forget_stat(path: &Path) {
    if let Some(stats) = STATS.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        stats.retain(|p, _| !p.starts_with(path));
    }
}

fn call(host: &str, method: &str, params: Value) -> io::Result<Value> {
    crate::remote::client::call(host, method, params, TIMEOUT).map_err(server_error)
}

/// Rebuild the error kind the server put in front of its message.
fn server_error(message: String) -> io::Error {
    let (kind, text) = match message.split_once(':') {
        Some(("not_found", rest)) => (io::ErrorKind::NotFound, rest),
        Some(("already_exists", rest)) => (io::ErrorKind::AlreadyExists, rest),
        Some(("permission_denied", rest)) => (io::ErrorKind::PermissionDenied, rest),
        Some(("invalid_data", rest)) => (io::ErrorKind::InvalidData, rest),
        Some(("other", rest)) => (io::ErrorKind::Other, rest),
        _ => (io::ErrorKind::Other, message.as_str()),
    };
    io::Error::new(kind, text.to_string())
}

fn meta_from(v: &Value) -> Option<Meta> {
    if v["exists"].as_bool() != Some(true) {
        return None;
    }
    Some(Meta {
        len: v["len"].as_u64().unwrap_or(0),
        modified: v["modified"]
            .as_u64()
            .map(|ms| SystemTime::UNIX_EPOCH + Duration::from_millis(ms)),
        is_dir: v["dir"].as_bool() == Some(true),
        is_file: v["file"].as_bool() == Some(true),
    })
}

pub fn metadata(path: &Path) -> io::Result<Meta> {
    let Some((host, remote_path)) = remote(path) else {
        let meta = std::fs::metadata(path)?;
        return Ok(Meta {
            len: meta.len(),
            modified: meta.modified().ok(),
            is_dir: meta.is_dir(),
            is_file: meta.is_file(),
        });
    };
    let meta = match cached_stat(path) {
        Some(meta) => meta,
        None => {
            let meta = meta_from(&call(&host, "fs.stat", json!({"path": remote_path}))?);
            remember_stat(path, meta);
            meta
        }
    };
    meta.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("{}: not found", path.display()),
        )
    })
}

pub fn modified(path: &Path) -> Option<SystemTime> {
    metadata(path).ok()?.modified
}

pub fn is_dir(path: &Path) -> bool {
    metadata(path).is_ok_and(|m| m.is_dir)
}

pub fn is_file(path: &Path) -> bool {
    metadata(path).is_ok_and(|m| m.is_file)
}

pub fn read(path: &Path) -> io::Result<Vec<u8>> {
    let Some((host, remote_path)) = remote(path) else {
        return std::fs::read(path);
    };
    let v = call(&host, "fs.read", json!({"path": remote_path}))?;
    remember_stat(path, meta_from(&v["meta"]));
    base64::engine::general_purpose::STANDARD
        .decode(v["data"].as_str().unwrap_or_default())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

pub fn read_to_string(path: &Path) -> io::Result<String> {
    if !is_remote(path) {
        return std::fs::read_to_string(path);
    }
    String::from_utf8(read(path)?).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

pub fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let Some((host, remote_path)) = remote(path) else {
        return crate::fsutil::save(path, bytes);
    };
    let data = base64::engine::general_purpose::STANDARD.encode(bytes);
    let v = call(
        &host,
        "fs.write",
        json!({"path": remote_path, "data": data}),
    )?;
    remember_stat(path, meta_from(&v));
    Ok(())
}

/// The entries of a folder, unsorted. Hidden files included.
pub fn read_dir(path: &Path) -> io::Result<Vec<Entry>> {
    let Some((host, remote_path)) = remote(path) else {
        return Ok(std::fs::read_dir(path)?
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let kind = entry.file_type().ok()?;
                Some(Entry {
                    path: entry.path(),
                    name: entry.file_name().to_string_lossy().to_string(),
                    is_dir: kind.is_dir(),
                    is_file: kind.is_file(),
                })
            })
            .collect());
    };
    let v = call(
        &host,
        "fs.list",
        json!({"path": remote_path, "files": true, "hidden": true}),
    )?;
    Ok(v["entries"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .map(|e| {
                    let name = e["name"].as_str().unwrap_or_default().to_string();
                    let is_dir = e["dir"].as_bool() == Some(true);
                    Entry {
                        path: path.join(&name),
                        name,
                        is_dir,
                        is_file: !is_dir,
                    }
                })
                .collect()
        })
        .unwrap_or_default())
}

fn op(path: &Path, op: &str, to: Option<&Path>) -> io::Result<()> {
    let (host, remote_path) = remote(path).expect("remote path");
    let mut params = json!({"path": remote_path, "op": op});
    if let Some(to) = to {
        let (to_host, to_path) = remote(to)
            .filter(|(h, _)| *h == host)
            .ok_or_else(|| io::Error::other("cannot move between hosts"))?;
        let _ = to_host;
        params["to"] = json!(to_path);
        forget_stat(to);
    }
    forget_stat(path);
    call(&host, "fs.op", params).map(|_| ())
}

/// Create an empty file; fails if it exists.
pub fn create_new_file(path: &Path) -> io::Result<()> {
    if !is_remote(path) {
        return std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map(|_| ());
    }
    op(path, "create_file", None)
}

pub fn create_dir(path: &Path) -> io::Result<()> {
    if !is_remote(path) {
        return std::fs::create_dir(path);
    }
    op(path, "create_dir", None)
}

pub fn rename(from: &Path, to: &Path) -> io::Result<()> {
    if !is_remote(from) {
        return std::fs::rename(from, to);
    }
    op(from, "rename", Some(to))
}

pub fn remove_file(path: &Path) -> io::Result<()> {
    if !is_remote(path) {
        return std::fs::remove_file(path);
    }
    op(path, "remove_file", None)
}

pub fn remove_dir(path: &Path) -> io::Result<()> {
    if !is_remote(path) {
        return std::fs::remove_dir(path);
    }
    op(path, "remove_dir", None)
}

/// `std::fs::canonicalize`, except a remote path stays as it is (the server resolves it).
pub fn canonicalize(path: &Path) -> io::Result<PathBuf> {
    if is_remote(path) {
        return Ok(path.to_path_buf());
    }
    std::fs::canonicalize(path)
}

/// The files Cmd/Ctrl+P lists under a remote `root`, relative to it.
pub fn walk_files(root: &Path) -> io::Result<Vec<String>> {
    let (host, path) = remote(root).ok_or_else(|| io::Error::other("not a remote path"))?;
    let v = crate::remote::client::call(
        &host,
        "fs.walk",
        json!({"root": path}),
        Duration::from_secs(60),
    )
    .map_err(server_error)?;
    Ok(v.as_array()
        .map(|files| {
            files
                .iter()
                .filter_map(|f| f.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default())
}

/// Metadata of a remote file as last fetched, however old: `None` when never fetched, `Some(None)`
/// when the file did not exist. Never waits for the network.
pub fn cached_metadata(path: &Path) -> Option<Option<Meta>> {
    let stats = STATS.lock().unwrap_or_else(|e| e.into_inner());
    stats.as_ref()?.get(path).map(|(_, meta)| *meta)
}

static REFRESHING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Fetch the metadata of these remote files in the background, one request per host, for
/// [`cached_metadata`]. A refresh still running makes this a no-op.
pub fn refresh_stats(paths: Vec<PathBuf>, done: impl Fn() + Send + 'static) {
    use std::sync::atomic::Ordering;
    if paths.is_empty() || REFRESHING.swap(true, Ordering::SeqCst) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("oxi-vfs-stats".into())
        .spawn(move || {
            let mut by_host: HashMap<String, Vec<(PathBuf, String)>> = HashMap::new();
            for path in paths {
                if let Some((host, remote_path)) = remote(&path) {
                    by_host.entry(host).or_default().push((path, remote_path));
                }
            }
            for (host, files) in by_host {
                let list: Vec<&str> = files.iter().map(|(_, p)| p.as_str()).collect();
                if let Ok(v) = call(&host, "fs.stat_many", json!({ "paths": list })) {
                    for (path, remote_path) in &files {
                        remember_stat(path, meta_from(&v[remote_path.as_str()]));
                    }
                }
            }
            REFRESHING.store(false, Ordering::SeqCst);
            done();
        });
    if spawned.is_err() {
        REFRESHING.store(false, Ordering::SeqCst);
    }
}
