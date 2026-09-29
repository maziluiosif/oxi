//! Managed installs of the npm-distributed ACP adapters.
//!
//! The default launch commands are `npx -y <package>`, but npx caches the first version it
//! downloads and never looks for a newer one, so the adapter (and the agent SDK bundled inside
//! it) silently freezes on whatever was current the day it was first used. Instead, oxi installs
//! each adapter into its own data dir with npm and keeps it current in the background, without
//! surfacing any of this to the user:
//!
//! ```text
//! <data_dir>/oxi/acp/<package-slug>/
//!     current        version string of the install to launch
//!     checked        touched after every successful update check
//!     <version>/     `npm install --prefix` output for that version
//! ```
//!
//! Each version lives in its own directory and the `current` pointer is swapped atomically, so an
//! update never touches files a running agent may still load. Superseded versions are pruned once
//! they are old enough that no live process should be using them. Everything is best effort: when
//! npm is missing or offline, launches fall back to `npx -y <package>@latest`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime};

use tokio::process::Command;
use tokio::sync::{Mutex as AsyncMutex, OnceCell};

/// An ACP adapter published on npm that oxi knows how to install and launch directly.
pub(super) struct Adapter {
    package: &'static str,
    bin: &'static str,
}

const ADAPTERS: [Adapter; 2] = [
    Adapter {
        package: "@agentclientprotocol/claude-agent-acp",
        bin: "claude-agent-acp",
    },
    Adapter {
        package: "@agentclientprotocol/codex-acp",
        bin: "codex-acp",
    },
];

/// Minimum time between background update checks for one adapter.
const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
/// Superseded versions older than this are deleted.
const PRUNE_AFTER: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const INSTALL_TIMEOUT: Duration = Duration::from_secs(300);
const VIEW_TIMEOUT: Duration = Duration::from_secs(30);

impl Adapter {
    fn slug(&self) -> String {
        self.package.trim_start_matches('@').replace('/', "-")
    }

    fn root(&self) -> PathBuf {
        base_dir().join(self.slug())
    }

    fn bin_path(&self, version_dir: &Path) -> PathBuf {
        let bin_dir = version_dir.join("node_modules").join(".bin");
        if cfg!(windows) {
            bin_dir.join(format!("{}.cmd", self.bin))
        } else {
            bin_dir.join(self.bin)
        }
    }

    /// The launchable binary of the current install, if there is one.
    fn installed_bin(&self) -> Option<PathBuf> {
        let version = std::fs::read_to_string(self.root().join("current")).ok()?;
        let version = version.trim();
        if version.is_empty() {
            return None;
        }
        let bin = self.bin_path(&self.root().join(version));
        bin.exists().then_some(bin)
    }

    fn installed_version(&self) -> Option<String> {
        self.installed_bin()?;
        std::fs::read_to_string(self.root().join("current"))
            .ok()
            .map(|v| v.trim().to_string())
    }

    /// Serializes installs and updates of this adapter within the process.
    fn lock(&self) -> Arc<AsyncMutex<()>> {
        type Locks = std::sync::Mutex<HashMap<&'static str, Arc<AsyncMutex<()>>>>;
        static LOCKS: OnceLock<Locks> = OnceLock::new();
        let mut locks = LOCKS
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        locks.entry(self.package).or_default().clone()
    }
}

fn base_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("oxi")
        .join("acp")
}

/// The managed adapter a launch command refers to: `npx [-y|--yes] <package>[@tag]` with nothing
/// else on the line. Custom commands (extra flags, pinned versions, other tools) are left alone.
pub(super) fn adapter_for(command_line: &str) -> Option<&'static Adapter> {
    let mut parts = command_line.split_whitespace();
    if parts.next()? != "npx" {
        return None;
    }
    let mut spec = parts.next()?;
    if spec == "-y" || spec == "--yes" {
        spec = parts.next()?;
    }
    if parts.next().is_some() {
        return None;
    }
    let package = spec.strip_suffix("@latest").unwrap_or(spec);
    ADAPTERS.iter().find(|a| a.package == package)
}

/// The command line to actually spawn for `command_line`: the managed install of a known adapter
/// (installing it first when missing), or `command_line` unchanged for anything else.
pub(super) async fn resolve_command(command_line: &str) -> String {
    let Some(adapter) = adapter_for(command_line) else {
        return command_line.to_string();
    };
    if let Some(bin) = adapter.installed_bin() {
        return shell_quote(&bin);
    }
    let lock = adapter.lock();
    let _guard = lock.lock().await;
    if adapter.installed_bin().is_none()
        && let Err(e) = install_latest(adapter).await
    {
        eprintln!("[acp] could not install {}: {e}", adapter.package);
    }
    match adapter.installed_bin() {
        Some(bin) => shell_quote(&bin),
        None => format!("npx -y {}@latest", adapter.package),
    }
}

/// Update every adapter that has a managed install and hasn't been checked recently, then prune
/// superseded versions. Silent: failures only reach stderr.
pub(super) async fn update_installed() {
    for adapter in &ADAPTERS {
        if adapter.installed_bin().is_none() || !check_due(adapter) {
            continue;
        }
        let lock = adapter.lock();
        let _guard = lock.lock().await;
        if let Err(e) = update(adapter).await {
            eprintln!("[acp] background update of {} failed: {e}", adapter.package);
        }
        prune(adapter);
    }
}

fn check_due(adapter: &Adapter) -> bool {
    std::fs::metadata(adapter.root().join("checked"))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_none_or(|age| age >= CHECK_INTERVAL)
}

async fn update(adapter: &Adapter) -> Result<(), String> {
    let latest = npm_output(
        &["view", adapter.package, "version"],
        &base_dir(),
        VIEW_TIMEOUT,
    )
    .await?;
    let latest = latest.trim();
    if latest.is_empty() {
        return Err("npm view returned no version".into());
    }
    if adapter.installed_version().as_deref() != Some(latest) {
        install_latest(adapter).await?;
    }
    touch(&adapter.root().join("checked"))
}

/// `npm install` the latest version into a fresh staging dir, move it to `<root>/<version>` and
/// point `current` at it.
async fn install_latest(adapter: &Adapter) -> Result<(), String> {
    let root = adapter.root();
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let staging = root.join(format!(".staging-{}-{nanos}", std::process::id()));
    let spec = format!("{}@latest", adapter.package);
    let staging_arg = staging.to_string_lossy().into_owned();
    let result = npm_output(
        &[
            "install",
            "--prefix",
            &staging_arg,
            "--no-audit",
            "--no-fund",
            "--no-package-lock",
            "--loglevel=error",
            &spec,
        ],
        &root,
        INSTALL_TIMEOUT,
    )
    .await
    .and_then(|_| {
        let manifest = staging
            .join("node_modules")
            .join(adapter.package)
            .join("package.json");
        let manifest = std::fs::read_to_string(&manifest).map_err(|e| e.to_string())?;
        let manifest: serde_json::Value =
            serde_json::from_str(&manifest).map_err(|e| e.to_string())?;
        let version = manifest
            .get("version")
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty() && !v.starts_with('.') && !v.contains(['/', '\\']))
            .ok_or("installed package has no version")?
            .to_string();
        if !adapter.bin_path(&staging).exists() {
            return Err(format!("installed package has no `{}` binary", adapter.bin));
        }
        let target = root.join(&version);
        if adapter.bin_path(&target).exists() {
            let _ = std::fs::remove_dir_all(&staging);
        } else {
            let _ = std::fs::remove_dir_all(&target);
            std::fs::rename(&staging, &target).map_err(|e| e.to_string())?;
        }
        write_atomic(&root.join("current"), &version)?;
        touch(&root.join("checked"))
    });
    if staging.exists() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result
}

/// Delete versions other than `current` (and abandoned staging dirs) that are old enough that no
/// running agent should still be loaded from them.
fn prune(adapter: &Adapter) {
    let Some(current) = adapter.installed_version() else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(adapter.root()) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == current || !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .is_some_and(|age| age >= PRUNE_AFTER);
        if old {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

async fn npm_output(args: &[&str], cwd: &Path, timeout: Duration) -> Result<String, String> {
    let mut cmd = Command::new(if cfg!(windows) { "npm.cmd" } else { "npm" });
    cmd.args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(path) = shell_path().await {
        cmd.env("PATH", path);
    }
    let out = tokio::time::timeout(timeout, cmd.output())
        .await
        .map_err(|_| format!("npm {} timed out", args[0]))?
        .map_err(|e| format!("could not run npm: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!("npm {} failed: {}", args[0], stderr.trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// `PATH` for agent subprocesses: the user's login-shell `PATH` followed by oxi's own. An app
/// launched from Finder or a desktop launcher inherits only a minimal system `PATH`, which misses
/// Homebrew, nvm, `~/.local/bin` and friends, so `npx`, `npm` and `agent` would not be found.
/// `None` when nothing needs adding (or on Windows, where GUI apps get the full `PATH`).
pub(super) async fn shell_path() -> Option<String> {
    static PATH: OnceCell<Option<String>> = OnceCell::const_new();
    PATH.get_or_init(|| async {
        let login = login_shell_path().await?;
        let current = std::env::var("PATH").unwrap_or_default();
        let mut seen = std::collections::HashSet::new();
        let merged: Vec<&str> = login
            .split(':')
            .chain(current.split(':'))
            .filter(|p| !p.is_empty() && seen.insert(*p))
            .collect();
        let merged = merged.join(":");
        (merged != current).then_some(merged)
    })
    .await
    .clone()
}

#[cfg(windows)]
async fn login_shell_path() -> Option<String> {
    None
}

#[cfg(not(windows))]
async fn login_shell_path() -> Option<String> {
    const MARK: &str = "__OXI_PATH__";
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "/bin/sh".to_string());
    // Interactive + login so PATH edits in both profile and rc files (nvm, fnm, …) are seen; the
    // markers skip anything the rc files print.
    let mut cmd = Command::new(shell);
    cmd.args(["-ilc", &format!("printf '{MARK}%s{MARK}' \"$PATH\"")])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let out = tokio::time::timeout(Duration::from_secs(5), cmd.output())
        .await
        .ok()?
        .ok()?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let start = stdout.find(MARK)? + MARK.len();
    let len = stdout[start..].find(MARK)?;
    let path = stdout[start..start + len].trim();
    (!path.is_empty()).then(|| path.to_string())
}

fn shell_quote(path: &Path) -> String {
    let s = path.to_string_lossy();
    if cfg!(windows) {
        format!("\"{s}\"")
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

fn write_atomic(path: &Path, contents: &str) -> Result<(), String> {
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&tmp, contents).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

fn touch(path: &Path) -> Result<(), String> {
    std::fs::write(path, b"").map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_default_adapter_commands() {
        for cmd in [
            "npx -y @agentclientprotocol/claude-agent-acp",
            "npx @agentclientprotocol/claude-agent-acp",
            "npx --yes @agentclientprotocol/claude-agent-acp@latest",
            "  npx -y   @agentclientprotocol/claude-agent-acp  ",
        ] {
            assert_eq!(adapter_for(cmd).map(|a| a.bin), Some("claude-agent-acp"));
        }
        assert_eq!(
            adapter_for("npx -y @agentclientprotocol/codex-acp").map(|a| a.bin),
            Some("codex-acp")
        );
    }

    #[test]
    fn leaves_custom_commands_alone() {
        for cmd in [
            "agent acp",
            "npx -y @agentclientprotocol/claude-agent-acp@0.20.0",
            "npx -y @agentclientprotocol/claude-agent-acp --debug",
            "npx -y @zed-industries/claude-code-acp",
            "codex-acp",
            "",
        ] {
            assert!(adapter_for(cmd).is_none(), "{cmd}");
        }
    }

    #[test]
    fn quotes_paths_for_the_shell() {
        if !cfg!(windows) {
            assert_eq!(
                shell_quote(Path::new("/a b/it's/bin")),
                r"'/a b/it'\''s/bin'"
            );
        }
    }
}
