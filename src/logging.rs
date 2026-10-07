//! Diagnostics log and crash log.
//!
//! `log` records go to stderr and to `<config_dir>/oxi/oxi.log`, so a problem leaves a trace
//! even when oxi was started from the Dock, Start menu or a launcher and stderr goes nowhere.
//! oxi's own records are kept from `info` up (`OXI_LOG=debug` or `trace` for more); other
//! crates only from `warn`. The file is rotated to `oxi.log.1` at 1 MiB.
//!
//! Panics are appended to `crash.log` in the same folder, with the thread name and a
//! backtrace, before the default hook prints them.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use log::{Level, LevelFilter, Log, Metadata, Record};

/// Size at which a log file is moved aside to `<name>.1` and started over.
const MAX_LOG_BYTES: u64 = 1024 * 1024;

pub fn log_path() -> PathBuf {
    crate::app_dirs::config_dir().join("oxi.log")
}

pub fn crash_log_path() -> PathBuf {
    crate::app_dirs::config_dir().join("crash.log")
}

struct LogFile {
    path: PathBuf,
    file: Option<File>,
    len: u64,
}

impl LogFile {
    fn open(path: PathBuf) -> Option<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).ok()?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok()?;
        let len = file.metadata().map(|m| m.len()).unwrap_or(0);
        Some(Self {
            path,
            file: Some(file),
            len,
        })
    }

    fn append(&mut self, bytes: &[u8]) {
        if self.len > 0 && self.len + bytes.len() as u64 > MAX_LOG_BYTES {
            // Close before moving it aside: Windows may refuse to rename a file that's open.
            self.file = None;
            rotate(&self.path);
            match Self::open(self.path.clone()) {
                Some(fresh) => *self = fresh,
                None => return,
            }
        }
        if let Some(file) = self.file.as_mut()
            && file.write_all(bytes).is_ok()
        {
            self.len += bytes.len() as u64;
        }
    }
}

/// Move `path` to `<path>.1`, replacing an older rotation.
fn rotate(path: &Path) {
    let mut rotated = path.as_os_str().to_owned();
    rotated.push(".1");
    let _ = fs::rename(path, PathBuf::from(rotated));
}

struct Logger {
    own_level: LevelFilter,
    file: Mutex<Option<LogFile>>,
    repeats: Mutex<Repeats>,
}

/// Collapses a record repeated back to back (a driver warning every frame, say) into one line
/// plus a count, so it can't flood the terminal or churn through log rotations.
#[derive(Default)]
struct Repeats {
    last: Option<(Level, String, String)>,
    count: u64,
}

impl Repeats {
    /// Whether to write this record now. Returns the "repeated N times" note owed for the
    /// previous run of duplicates, if one just ended.
    fn observe(&mut self, level: Level, target: &str, message: &str) -> (bool, Option<String>) {
        if let Some((last_level, last_target, last_message)) = &self.last
            && *last_level == level
            && last_target == target
            && last_message == message
        {
            self.count += 1;
            // Still show a sign of life now and then during a long run of duplicates.
            if self.count.is_power_of_two() && self.count >= 1024 {
                return (false, Some(self.note()));
            }
            return (false, None);
        }
        let note = (self.count > 0).then(|| self.note());
        self.last = Some((level, target.to_string(), message.to_string()));
        self.count = 0;
        (true, note)
    }

    fn note(&self) -> String {
        let (level, target, _) = self.last.as_ref().expect("a record was seen");
        format_line(
            *level,
            target,
            &format!("(previous message repeated {} more times)", self.count),
        )
    }
}

impl Logger {
    fn level_for(&self, target: &str) -> LevelFilter {
        if target == "oxi" || target.starts_with("oxi::") {
            self.own_level
        } else {
            LevelFilter::Warn
        }
    }
}

impl Log for Logger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.level_for(metadata.target())
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let message = record.args().to_string();
        let (write, note) = self
            .repeats
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .observe(record.level(), record.target(), &message);
        let mut out = note.unwrap_or_default();
        if write {
            out.push_str(&format_line(record.level(), record.target(), &message));
        }
        if out.is_empty() {
            return;
        }
        let _ = std::io::stderr().write_all(out.as_bytes());
        let mut file = self.file.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(file) = file.as_mut() {
            file.append(out.as_bytes());
        }
    }

    fn flush(&self) {}
}

fn format_line(level: Level, target: &str, message: &str) -> String {
    format!(
        "{} {level:<5} {target}: {message}\n",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f")
    )
}

/// Start of `text` for a log line: payloads (SSE data, tool output) can be long and may carry
/// conversation content, which shouldn't be copied wholesale into a file on disk.
pub fn excerpt(text: &str) -> String {
    const MAX_CHARS: usize = 200;
    match text.char_indices().nth(MAX_CHARS) {
        Some((cut, _)) => format!("{}… ({} bytes)", &text[..cut], text.len()),
        None => text.to_string(),
    }
}

static LOGGER: OnceLock<Logger> = OnceLock::new();

fn parse_level(value: Option<&str>) -> LevelFilter {
    match value.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
        Some("off") => LevelFilter::Off,
        Some("error") => LevelFilter::Error,
        Some("warn") | Some("warning") => LevelFilter::Warn,
        Some("debug") => LevelFilter::Debug,
        Some("trace") => LevelFilter::Trace,
        _ => LevelFilter::Info,
    }
}

/// Install the logger and the panic hook. Call once, first thing in `main`.
pub fn init() {
    let own_level = parse_level(std::env::var("OXI_LOG").ok().as_deref());
    let logger = LOGGER.get_or_init(|| Logger {
        own_level,
        file: Mutex::new(LogFile::open(log_path())),
        repeats: Mutex::default(),
    });
    if log::set_logger(logger).is_ok() {
        log::set_max_level(own_level.max(LevelFilter::Warn));
    }
    install_panic_hook();
    log::info!(
        "oxi {} starting on {}/{}",
        crate::update::APP_VERSION,
        std::env::consts::OS,
        std::env::consts::ARCH
    );
}

/// Record panics to `crash.log` (and `oxi.log`) before the default hook prints to stderr, so a
/// crash in a background thread (agent/network) leaves a trace the user can find and report
/// even if they never saw the terminal.
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let thread = thread.name().unwrap_or("<unnamed>");
        let backtrace = std::backtrace::Backtrace::force_capture();
        let entry = format!(
            "==== {} · oxi {} ({}/{}) · thread '{thread}' ====\n{info}\n\nstack backtrace:\n{backtrace}\n\n",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f %:z"),
            crate::update::APP_VERSION,
            std::env::consts::OS,
            std::env::consts::ARCH,
        );
        if let Some(mut crash_log) = LogFile::open(crash_log_path()) {
            crash_log.append(entry.as_bytes());
        }
        // `try_lock`: the panic may come from inside the logger while it holds the file.
        if let Some(logger) = LOGGER.get()
            && let Ok(mut file) = logger.file.try_lock()
            && let Some(file) = file.as_mut()
        {
            let line = format_line(
                Level::Error,
                "oxi::panic",
                &format!("thread '{thread}' {info}"),
            );
            file.append(line.as_bytes());
        }
        default_hook(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_parse_with_info_as_default() {
        assert_eq!(parse_level(None), LevelFilter::Info);
        assert_eq!(parse_level(Some("DEBUG")), LevelFilter::Debug);
        assert_eq!(parse_level(Some(" warning ")), LevelFilter::Warn);
        assert_eq!(parse_level(Some("off")), LevelFilter::Off);
        assert_eq!(parse_level(Some("nonsense")), LevelFilter::Info);
    }

    #[test]
    fn excerpts_cut_long_payloads_on_char_boundaries() {
        assert_eq!(excerpt("short"), "short");
        let long = "é".repeat(300);
        let cut = excerpt(&long);
        assert!(cut.starts_with(&"é".repeat(200)));
        assert!(cut.ends_with("(600 bytes)"));
    }

    #[test]
    fn dependencies_are_held_to_warnings() {
        let logger = Logger {
            own_level: LevelFilter::Debug,
            file: Mutex::new(None),
            repeats: Mutex::default(),
        };
        assert_eq!(logger.level_for("oxi::agent::net"), LevelFilter::Debug);
        assert_eq!(logger.level_for("oxi"), LevelFilter::Debug);
        assert_eq!(logger.level_for("oxide"), LevelFilter::Warn);
        assert_eq!(logger.level_for("wgpu_core::device"), LevelFilter::Warn);
    }

    #[test]
    fn back_to_back_duplicates_collapse_into_a_count() {
        let mut repeats = Repeats::default();
        assert_eq!(repeats.observe(Level::Warn, "wgpu", "lost"), (true, None));
        for _ in 0..3 {
            assert_eq!(repeats.observe(Level::Warn, "wgpu", "lost"), (false, None));
        }
        let (write, note) = repeats.observe(Level::Warn, "wgpu", "found");
        assert!(write);
        assert!(note.unwrap().contains("repeated 3 more times"));
        // A different level or target is a different message.
        assert!(repeats.observe(Level::Error, "wgpu", "found").0);
        assert!(repeats.observe(Level::Error, "naga", "found").0);
    }

    #[test]
    fn log_file_rotates_past_the_size_cap() {
        let dir = crate::fsutil::unique_temp_path(&std::env::temp_dir(), "oxi-logging", "d");
        let path = dir.join("oxi.log");
        let mut file = LogFile::open(path.clone()).unwrap();
        let chunk = vec![b'x'; 600 * 1024];
        file.append(&chunk);
        file.append(&chunk);
        assert_eq!(fs::metadata(&path).unwrap().len(), chunk.len() as u64);
        assert_eq!(
            fs::metadata(dir.join("oxi.log.1")).unwrap().len(),
            chunk.len() as u64
        );
        let _ = fs::remove_dir_all(dir);
    }
}
