//! Computer use for agents through Vercel Labs' `agent-desktop`: desktop apps seen and operated
//! via the OS accessibility tree.
//!
//! oxi downloads the tool itself and hands agents `oxi computer-use-mcp`, which fetches the newest
//! release and then becomes `agent-desktop mcp`. Going through oxi keeps the same path working on
//! an SSH server: the agent starts this command there and it installs on that machine. Accessibility
//! access belongs to the process that started the tool, so oxi is what the user grants it to.

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::settings::McpServerConfig;
use crate::update::{expected_sha256, fetch_bytes, is_newer, parse_semver};

/// Command line argument that runs oxi as this MCP server instead of the app.
pub const SUBCOMMAND: &str = "computer-use-mcp";
/// MCP server name; agents expose the tools as e.g. `mcp__agent-desktop__desktop_click`.
pub const SERVER_NAME: &str = "agent-desktop";
const REPO: &str = "vercel-labs/agent-desktop";
/// Prefix of every tool the server exposes.
const TOOL_PREFIX: &str = "desktop_";
/// `desktop_wait` and post-action waits run 30 s by default.
const CALL_TIMEOUT_SECS: u32 = 120;
/// How long an agent's start may wait on an update when a working copy is already installed.
const UPDATE_BUDGET: Duration = Duration::from_secs(15);

/// The MCP server to give agents: this binary, which brings the tool up to date first.
pub fn server_config() -> Option<McpServerConfig> {
    Some(McpServerConfig {
        name: SERVER_NAME.to_string(),
        command: std::env::current_exe().ok()?.to_string_lossy().into_owned(),
        args: vec![SUBCOMMAND.to_string()],
        timeout_secs: Some(CALL_TIMEOUT_SECS),
        ..Default::default()
    })
}

/// Whether `name` is an agent-desktop tool, however the agent spells MCP tool names
/// (`mcp__agent-desktop__desktop_click`, `agent-desktop.desktop_click`, ...).
pub fn is_tool(name: &str) -> bool {
    let name = name.trim();
    let tool = name
        .strip_prefix("mcp__")
        .and_then(|rest| rest.strip_prefix(SERVER_NAME))
        .and_then(|rest| rest.strip_prefix("__"))
        .or_else(|| {
            name.strip_prefix(SERVER_NAME)
                .and_then(|rest| rest.strip_prefix(['.', '/', ':']))
        });
    tool.is_some_and(|tool| {
        tool.strip_prefix(TOOL_PREFIX).is_some_and(|rest| {
            !rest.is_empty() && rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
    })
}

/// Whether an ACP tool call is one of the agent-desktop tools.
pub fn is_acp_tool(tool: &Value) -> bool {
    [
        &tool["title"],
        &tool["name"],
        &tool["_meta"]["claudeCode"]["toolName"],
    ]
    .into_iter()
    .any(|v| v.as_str().is_some_and(is_tool))
}

/// `oxi computer-use-mcp`: update, then turn into `agent-desktop mcp` on the same stdio.
pub fn run_mcp() -> i32 {
    let binary = crate::runtime::runtime().and_then(|rt| rt.block_on(ensure_latest()));
    let binary = match binary {
        Ok(binary) => binary,
        Err(e) => {
            eprintln!("oxi: could not install {SERVER_NAME}: {e}");
            return 1;
        }
    };
    let mut command = Command::new(binary);
    command.arg("mcp");
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        eprintln!("oxi: could not start {SERVER_NAME}: {}", command.exec());
        1
    }
    #[cfg(not(unix))]
    match command.status() {
        Ok(status) => status.code().unwrap_or(1),
        Err(e) => {
            eprintln!("oxi: could not start {SERVER_NAME}: {e}");
            1
        }
    }
}

/// Blocking; call off the UI thread. Installs or updates when `install`, opens the permission
/// guide on this machine's desktop when `guide`, and reports `{version, accessibility}`.
pub fn refresh(install: bool, guide: bool) -> Result<Value, String> {
    if install {
        crate::runtime::runtime()?.block_on(ensure_latest())?;
    }
    if guide {
        open_permission_guide()?;
    }
    status()
}

fn status() -> Result<Value, String> {
    let Some(installed) = installed(&install_root()) else {
        return Ok(json!({ "version": null, "accessibility": null }));
    };
    let output = Command::new(&installed.binary)
        .arg("status")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("could not run {SERVER_NAME}: {e}"))?;
    let envelope: Value =
        serde_json::from_slice(&output.stdout).map_err(|e| format!("{SERVER_NAME} status: {e}"))?;
    Ok(json!({
        "version": installed.version,
        "accessibility": envelope["data"]["permissions"]["accessibility"]["state"],
    }))
}

/// Opens System Settings with agent-desktop's guide; macOS only, the others need no grant.
fn open_permission_guide() -> Result<(), String> {
    let installed = installed(&install_root()).ok_or("agent-desktop is not installed yet")?;
    let child = Command::new(&installed.binary)
        .args(["permissions", "--guide"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("could not run {SERVER_NAME}: {e}"))?;
    // The guide stays up until the access is given; reap it whenever that is.
    std::thread::spawn(move || {
        let mut child = child;
        let _ = child.wait();
    });
    Ok(())
}

/// The newest release, installed if need be. A working copy is good enough when GitHub cannot be
/// reached or is slow: an agent should not lose the tool over that.
async fn ensure_latest() -> Result<PathBuf, String> {
    let root = install_root();
    let current = installed(&root);
    let update = update(&root, current.as_ref().map(|c| c.version.as_str()));
    let result = match &current {
        Some(_) => tokio::time::timeout(UPDATE_BUDGET, update)
            .await
            .unwrap_or_else(|_| Err("timed out".to_string())),
        None => update.await,
    };
    match (result, current) {
        (Ok(Some(binary)), _) => Ok(binary),
        (Ok(None), Some(current)) => Ok(current.binary),
        (Ok(None), None) => Err("nothing installed".to_string()),
        (Err(e), Some(current)) => {
            eprintln!("oxi: keeping {SERVER_NAME} {}: {e}", current.version);
            Ok(current.binary)
        }
        (Err(e), None) => Err(e),
    }
}

/// Install the latest release unless `current` is already that one.
async fn update(root: &Path, current: Option<&str>) -> Result<Option<PathBuf>, String> {
    let client = crate::runtime::http_client();
    let latest = latest_version(&client).await?;
    if current.is_some_and(|current| !is_newer(&latest, current)) {
        return Ok(None);
    }
    install(&client, root, &latest).await.map(Some)
}

async fn latest_version(client: &reqwest::Client) -> Result<String, String> {
    let resp = client
        .get(format!(
            "https://api.github.com/repos/{REPO}/releases/latest"
        ))
        .header("User-Agent", format!("oxi/{}", crate::update::APP_VERSION))
        .header("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("GitHub API: HTTP {}", resp.status().as_u16()));
    }
    let body: Value = resp.json().await.map_err(|e| e.to_string())?;
    body["tag_name"]
        .as_str()
        .map(|tag| tag.trim_start_matches('v').to_string())
        .ok_or_else(|| "GitHub API: response has no tag_name".to_string())
}

async fn install(client: &reqwest::Client, root: &Path, version: &str) -> Result<PathBuf, String> {
    let target = release_target().ok_or("no agent-desktop build for this platform")?;
    let asset = format!("agent-desktop-v{version}-{target}.tar.gz");
    let base = format!("https://github.com/{REPO}/releases/download/v{version}");

    let sums = fetch_bytes(client, &format!("{base}/checksums.txt")).await?;
    let sums = String::from_utf8_lossy(&sums);
    let expected =
        expected_sha256(&sums, &asset).ok_or(format!("{asset} is not in checksums.txt"))?;
    let bytes = fetch_bytes(client, &format!("{base}/{asset}")).await?;
    let actual: String = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if !actual.eq_ignore_ascii_case(&expected) {
        return Err(format!("checksum mismatch for {asset}"));
    }
    place(&bytes, root, version)
}

/// Unpack a verified archive into `root/<version>`. Every version has its own folder because the
/// tool and its helper must match, and running copies (other chats) stay intact that way.
fn place(archive: &[u8], root: &Path, version: &str) -> Result<PathBuf, String> {
    let staging = root.join(format!(".staging-{}", std::process::id()));
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(&staging).map_err(|e| format!("{}: {e}", staging.display()))?;
    let destination = root.join(version);
    let unpacked = tar::Archive::new(flate2::read::GzDecoder::new(Cursor::new(archive)))
        .unpack(&staging)
        .map_err(|e| format!("could not unpack {SERVER_NAME}: {e}"))
        .and_then(|()| {
            if staging.join(binary_name()).is_file() {
                Ok(())
            } else {
                Err(format!("the archive has no {}", binary_name()))
            }
        });
    // A chat starting at the same moment may have installed it first; its copy is as good.
    let result = unpacked.and_then(|()| {
        if destination.join(binary_name()).is_file() {
            return Ok(());
        }
        fs::rename(&staging, &destination).map_err(|e| format!("{}: {e}", destination.display()))
    });
    let _ = fs::remove_dir_all(&staging);
    result?;
    prune(root);
    Ok(destination.join(binary_name()))
}

/// Keep the newest two: chats still running the previous version need its helper.
fn prune(root: &Path) {
    for (_, dir) in versions(root).into_iter().skip(2) {
        let _ = fs::remove_dir_all(dir);
    }
}

struct Installed {
    version: String,
    binary: PathBuf,
}

fn installed(root: &Path) -> Option<Installed> {
    let (_, dir) = versions(root).into_iter().next()?;
    Some(Installed {
        version: dir.file_name()?.to_string_lossy().into_owned(),
        binary: dir.join(binary_name()),
    })
}

/// Complete installs under `root`, newest first.
fn versions(root: &Path) -> Vec<((u64, u64, u64), PathBuf)> {
    let mut versions: Vec<_> = fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|dir| dir.join(binary_name()).is_file())
        .filter_map(|dir| {
            let version = parse_semver(dir.file_name()?.to_str()?)?;
            Some((version, dir))
        })
        .collect();
    versions.sort_by_key(|(version, _)| std::cmp::Reverse(*version));
    versions
}

fn install_root() -> PathBuf {
    crate::app_dirs::data_dir().join(SERVER_NAME)
}

fn binary_name() -> &'static str {
    if cfg!(windows) {
        "agent-desktop.exe"
    } else {
        SERVER_NAME
    }
}

fn release_target() -> Option<&'static str> {
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("windows", "aarch64") => "aarch64-pc-windows-msvc",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("oxi-computer-use-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn archive(files: &[&str]) -> Vec<u8> {
        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut tar = tar::Builder::new(encoder);
        for name in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(2);
            header.set_mode(0o755);
            header.set_cksum();
            tar.append_data(&mut header, name, &b"hi"[..]).unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn recognises_only_agent_desktop_tools() {
        assert!(is_tool("mcp__agent-desktop__desktop_click"));
        assert!(is_tool("agent-desktop.desktop_snapshot"));
        assert!(is_tool("agent-desktop/desktop_skills_get"));
        assert!(!is_tool("mcp__other__desktop_click"));
        assert!(!is_tool("mcp__agent-desktop__other"));
        assert!(!is_tool("agent-desktop click @s1:e1"));
        assert!(!is_tool("agent-desktop.desktop_click; rm -rf ~"));
        assert!(is_acp_tool(&json!({
            "title": "Click",
            "_meta": { "claudeCode": { "toolName": "mcp__agent-desktop__desktop_click" } },
        })));
    }

    #[test]
    fn places_versions_side_by_side_and_keeps_the_newest_two() {
        let root = scratch("versions");
        let bundle = archive(&[binary_name(), "agent-desktop-macos-helper"]);
        for version in ["1.2.0", "1.10.0", "1.9.0"] {
            place(&bundle, &root, version).unwrap();
        }
        let newest = installed(&root).unwrap();
        assert_eq!(newest.version, "1.10.0");
        assert!(newest.binary.is_file());
        let kept: Vec<_> = versions(&root)
            .into_iter()
            .map(|(version, _)| version)
            .collect();
        assert_eq!(kept, [(1, 10, 0), (1, 9, 0)]);
    }

    #[test]
    fn an_archive_without_the_tool_installs_nothing() {
        let root = scratch("broken");
        let err = place(&archive(&["README.md"]), &root, "1.0.0").unwrap_err();
        assert!(err.contains("archive has no"), "{err}");
        assert!(installed(&root).is_none());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
    }
}
