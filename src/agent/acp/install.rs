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
use tokio::sync::Mutex as AsyncMutex;

/// An ACP adapter published on npm that oxi knows how to install and launch directly.
pub(super) struct Adapter {
    package: &'static str,
    bin: &'static str,
    /// Arguments the binary needs to speak ACP; part of the default command line.
    args: &'static [&'static str],
}

const ADAPTERS: [Adapter; 5] = [
    Adapter {
        package: "@agentclientprotocol/claude-agent-acp",
        bin: "claude-agent-acp",
        args: &[],
    },
    Adapter {
        package: "@agentclientprotocol/codex-acp",
        bin: "codex-acp",
        args: &[],
    },
    Adapter {
        package: "@brokkai/muse-acp",
        bin: "muse-acp",
        args: &[],
    },
    Adapter {
        package: "@github/copilot",
        bin: "copilot",
        args: &["--acp"],
    },
    Adapter {
        package: "opencode-ai",
        bin: "opencode",
        args: &["acp"],
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

    /// `program` followed by the adapter's ACP arguments.
    fn launch_line(&self, program: String) -> String {
        std::iter::once(program)
            .chain(self.args.iter().map(|a| a.to_string()))
            .collect::<Vec<_>>()
            .join(" ")
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
    crate::app_dirs::data_dir().join("acp")
}

/// The managed adapter a launch command refers to: `npx [-y|--yes] <package>[@tag]` followed by
/// exactly the adapter's own arguments. Custom commands (extra flags, pinned versions, other
/// tools) are left alone.
pub(super) fn adapter_for(command_line: &str) -> Option<&'static Adapter> {
    let mut parts = command_line.split_whitespace();
    if parts.next()? != "npx" {
        return None;
    }
    let mut spec = parts.next()?;
    if spec == "-y" || spec == "--yes" {
        spec = parts.next()?;
    }
    let rest: Vec<&str> = parts.collect();
    let package = spec.strip_suffix("@latest").unwrap_or(spec);
    ADAPTERS
        .iter()
        .find(|a| a.package == package && a.args == rest.as_slice())
}

/// The command line to actually spawn for `command_line`: the managed install of a known adapter
/// (installing it first when missing), or `command_line` unchanged for anything else.
pub(super) async fn resolve_command(command_line: &str) -> Result<String, String> {
    if super::antigravity::is_default(command_line) {
        return super::antigravity::resolve().await;
    }
    let Some(adapter) = adapter_for(command_line) else {
        return Ok(command_line.to_string());
    };
    if let Some(bin) = adapter.installed_bin() {
        return Ok(adapter.launch_line(shell_quote(&bin)));
    }
    let lock = adapter.lock();
    let _guard = lock.lock().await;
    if adapter.installed_bin().is_none()
        && let Err(e) = install_latest(adapter).await
    {
        log::warn!("could not install ACP adapter {}: {e}", adapter.package);
    }
    Ok(match adapter.installed_bin() {
        Some(bin) => adapter.launch_line(shell_quote(&bin)),
        None => adapter.launch_line(format!("npx -y {}@latest", adapter.package)),
    })
}

/// The launch line of the current managed install `command_line` refers to, without installing
/// anything; `None` for other commands or when nothing is installed yet.
pub(super) fn installed_launch(command_line: &str) -> Option<String> {
    let adapter = adapter_for(command_line.trim())?;
    Some(adapter.launch_line(shell_quote(&adapter.installed_bin()?)))
}

/// Whether `command_line` starts without installing anything first: a managed adapter that is
/// already installed (it was used before), or another command whose program exists.
pub(super) async fn is_ready(command_line: &str) -> bool {
    if super::antigravity::is_default(command_line) {
        return super::antigravity::is_ready();
    }
    if let Some(adapter) = adapter_for(command_line) {
        return adapter.installed_bin().is_some();
    }
    let Some(program) = first_word(command_line) else {
        return false;
    };
    if program.contains('/') || program.contains('\\') {
        return Path::new(&program).is_file();
    }
    let path = match shell_path().await {
        Some(path) => path,
        None => std::env::var("PATH").unwrap_or_default(),
    };
    let exts: &[&str] = if cfg!(windows) {
        &["", ".exe", ".cmd", ".bat"]
    } else {
        &[""]
    };
    std::env::split_paths(&path).any(|dir| {
        exts.iter()
            .any(|ext| dir.join(format!("{program}{ext}")).is_file())
    })
}

/// The program of a command line, unquoting a leading `"…"`.
fn first_word(command_line: &str) -> Option<String> {
    let s = command_line.trim_start();
    let word = match s.strip_prefix('"') {
        Some(rest) => rest.split('"').next()?,
        None => s.split_whitespace().next()?,
    };
    (!word.is_empty()).then(|| word.to_string())
}

/// Update every adapter that has a managed install and hasn't been checked recently (or every one,
/// when `force`), then prune superseded versions. Silent: failures only reach stderr. Whether a
/// new version was installed.
pub(super) async fn update_installed(force: bool) -> bool {
    let mut installed = false;
    for adapter in &ADAPTERS {
        if adapter.installed_bin().is_none() || !(force || check_due(adapter)) {
            continue;
        }
        let lock = adapter.lock();
        let _guard = lock.lock().await;
        match update(adapter).await {
            Ok(new) => installed |= new,
            Err(e) => log::warn!(
                "background update of ACP adapter {} failed: {e}",
                adapter.package
            ),
        }
        prune(adapter);
    }
    installed
}

fn check_due(adapter: &Adapter) -> bool {
    std::fs::metadata(adapter.root().join("checked"))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_none_or(|age| age >= CHECK_INTERVAL)
}

/// Whether a new version was installed.
async fn update(adapter: &Adapter) -> Result<bool, String> {
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
    let new = adapter.installed_version().as_deref() != Some(latest);
    if new {
        install_latest(adapter).await?;
    } else if let Some(version) = adapter.installed_version() {
        // An install npm left without its native binary fails at every launch; repair it.
        add_platform_packages(&adapter.root().join(version)).await?;
    }
    touch(&adapter.root().join("checked"))?;
    Ok(new)
}

/// The Claude Code binary that the managed Claude adapter's agent SDK ships in its platform
/// package, for one-shot `claude -p` helpers when no `claude` is on `PATH`.
pub(super) fn bundled_claude_cli() -> Option<PathBuf> {
    let adapter = ADAPTERS.iter().find(|a| a.bin == "claude-agent-acp")?;
    let version = adapter.installed_version()?;
    let scope = adapter
        .root()
        .join(version)
        .join("node_modules")
        .join("@anthropic-ai");
    let suffix = platform_suffix()?;
    let name = if cfg!(windows) {
        "claude.exe"
    } else {
        "claude"
    };
    std::fs::read_dir(scope)
        .ok()?
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(&suffix))
        .find_map(|entry| find_file(&entry.path(), name, 3))
}

/// A file called `name` at most `depth` folders below `dir`.
fn find_file(dir: &Path, name: &str, depth: usize) -> Option<PathBuf> {
    let candidate = dir.join(name);
    if candidate.is_file() {
        return Some(candidate);
    }
    if depth == 0 {
        return None;
    }
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
        .find_map(|entry| find_file(&entry.path(), name, depth - 1))
}

/// The suffix npm platform packages carry for this machine, as in `claude-agent-sdk-darwin-arm64`.
fn platform_suffix() -> Option<String> {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        "linux" => "linux",
        "windows" => "win32",
        _ => return None,
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        _ => return None,
    };
    Some(format!("-{os}-{arch}"))
}

/// Optional dependencies for this platform (`name@version`) that packages installed in
/// `version_dir` declare but npm did not install. npm sometimes skips them, and an agent SDK
/// without its native binary fails every session ("Claude native binary not found").
fn missing_platform_packages(version_dir: &Path) -> Vec<String> {
    let Some(suffix) = platform_suffix() else {
        return Vec::new();
    };
    let modules = version_dir.join("node_modules");
    let mut manifests = Vec::new();
    for entry in std::fs::read_dir(&modules).into_iter().flatten().flatten() {
        let path = entry.path();
        if entry.file_name().to_string_lossy().starts_with('@') {
            for scoped in std::fs::read_dir(&path).into_iter().flatten().flatten() {
                manifests.push(scoped.path().join("package.json"));
            }
        } else {
            manifests.push(path.join("package.json"));
        }
    }
    let mut missing = Vec::new();
    for manifest in manifests {
        let Some(manifest) = std::fs::read_to_string(&manifest)
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        else {
            continue;
        };
        let Some(optional) = manifest["optionalDependencies"].as_object() else {
            continue;
        };
        for (name, version) in optional {
            let spec = format!("{name}@{}", version.as_str().unwrap_or("latest"));
            if name.ends_with(&suffix)
                && !modules.join(name).join("package.json").exists()
                && !missing.contains(&spec)
            {
                missing.push(spec);
            }
        }
    }
    missing
}

/// Install the platform packages npm skipped in `version_dir`; an error when they stay missing.
async fn add_platform_packages(version_dir: &Path) -> Result<(), String> {
    let missing = missing_platform_packages(version_dir);
    if missing.is_empty() {
        return Ok(());
    }
    log::info!(
        "ACP adapter in {} lacks {}; installing",
        version_dir.display(),
        missing.join(", ")
    );
    let dir = version_dir.to_string_lossy().into_owned();
    let mut args = vec![
        "install",
        "--prefix",
        &dir,
        "--no-audit",
        "--no-fund",
        "--no-package-lock",
        "--loglevel=error",
    ];
    args.extend(missing.iter().map(String::as_str));
    npm_output(&args, version_dir, INSTALL_TIMEOUT).await?;
    let still = missing_platform_packages(version_dir);
    if still.is_empty() {
        Ok(())
    } else {
        Err(format!("npm did not install {}", still.join(", ")))
    }
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
    .await;
    let result = match result {
        Ok(_) => add_platform_packages(&staging).await,
        Err(e) => Err(e),
    }
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
    // No console on a Windows GUI process: without CREATE_NO_WINDOW every update check flashes
    // a terminal window.
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    crate::shell_env::apply_async(&mut cmd).await;
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

/// `PATH` for agent subprocesses: the user's login-shell `PATH` followed by oxi's own (see
/// [`crate::shell_env`]). `None` when nothing needs adding.
pub(super) async fn shell_path() -> Option<String> {
    crate::shell_env::path().await
}

pub(super) fn shell_quote(path: &Path) -> String {
    let s = path.to_string_lossy();
    if cfg!(windows) {
        format!("\"{s}\"")
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

fn write_atomic(path: &Path, contents: &str) -> Result<(), String> {
    crate::fsutil::write_atomic(path, contents.as_bytes())
}

fn touch(path: &Path) -> Result<(), String> {
    std::fs::write(path, b"").map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_platform_packages_npm_skipped() {
        let Some(suffix) = platform_suffix() else {
            return;
        };
        let dir = crate::app_dirs::test_root().join("acp-platform-packages");
        let sdk = dir.join("node_modules/@anthropic-ai/claude-agent-sdk");
        std::fs::create_dir_all(&sdk).unwrap();
        let ours = format!("@anthropic-ai/claude-agent-sdk{suffix}");
        let manifest = serde_json::json!({
            "name": "@anthropic-ai/claude-agent-sdk",
            "optionalDependencies": {
                ours.clone(): "0.3.300",
                "@anthropic-ai/claude-agent-sdk-other-arch": "0.3.300",
                format!("@anthropic-ai/claude-agent-sdk{suffix}-musl"): "0.3.300",
            },
        });
        std::fs::write(sdk.join("package.json"), manifest.to_string()).unwrap();
        assert_eq!(
            missing_platform_packages(&dir),
            vec![format!("{ours}@0.3.300")]
        );

        let native = dir.join("node_modules").join(&ours);
        std::fs::create_dir_all(&native).unwrap();
        std::fs::write(native.join("package.json"), "{}").unwrap();
        assert!(missing_platform_packages(&dir).is_empty());
    }

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
        assert_eq!(
            adapter_for(crate::settings::ProviderConfig::DEFAULT_MUSE_ACP_COMMAND).map(|a| a.bin),
            Some("muse-acp")
        );
        for cmd in [
            crate::settings::ProviderConfig::DEFAULT_COPILOT_ACP_COMMAND,
            "npx @github/copilot@latest --acp",
        ] {
            assert_eq!(adapter_for(cmd).map(|a| a.bin), Some("copilot"), "{cmd}");
        }
        assert_eq!(
            adapter_for(crate::settings::ProviderConfig::DEFAULT_OPENCODE_ACP_COMMAND)
                .map(|a| a.bin),
            Some("opencode")
        );
    }

    #[test]
    fn installed_launch_only_covers_managed_adapters() {
        assert_eq!(installed_launch("agent acp"), None);
        assert_eq!(
            installed_launch("npx -y @agentclientprotocol/claude-agent-acp@0.20.0"),
            None
        );
    }

    #[test]
    fn first_word_unquotes_the_program() {
        assert_eq!(
            first_word("\"/a b/python\" -u x").as_deref(),
            Some("/a b/python")
        );
        assert_eq!(first_word("  agent acp").as_deref(), Some("agent"));
        assert_eq!(first_word("   "), None);
    }

    #[tokio::test]
    async fn custom_commands_are_ready_only_when_the_program_exists() {
        let exe = std::env::current_exe().unwrap();
        assert!(is_ready(&format!("\"{}\" --acp", exe.display())).await);
        assert!(!is_ready("/definitely/not/here --acp").await);
        assert!(!is_ready("oxi-no-such-program-4e1f acp").await);
    }

    #[test]
    fn launch_line_appends_the_acp_arguments() {
        let copilot = adapter_for("npx -y @github/copilot --acp").unwrap();
        assert_eq!(copilot.launch_line("copilot".into()), "copilot --acp");
        let codex = adapter_for("npx -y @agentclientprotocol/codex-acp").unwrap();
        assert_eq!(codex.launch_line("codex-acp".into()), "codex-acp");
    }

    #[test]
    fn leaves_custom_commands_alone() {
        for cmd in [
            "agent acp",
            "npx -y @agentclientprotocol/claude-agent-acp@0.20.0",
            "npx -y @agentclientprotocol/claude-agent-acp --debug",
            "npx -y @zed-industries/claude-code-acp",
            "codex-acp",
            "npx -y @github/copilot",
            "npx -y @github/copilot --acp --port 8080",
            "npx -y @agentclientprotocol/codex-acp --acp",
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
