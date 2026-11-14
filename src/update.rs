//! App version, GitHub Releases update check, and in-place self-update.
//!
//! The version is compiled in from `Cargo.toml` via `CARGO_PKG_VERSION`. CI bumps
//! `Cargo.toml` on release (see `.github/workflows/release.yml`) and builds from the
//! release tag, so released binaries always carry the right version with no manual step.

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use sha2::{Digest, Sha256};

/// Version of the running binary, from `Cargo.toml` at compile time.
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// GitHub repository the app checks for new releases (and links to from About).
pub const REPO_URL: &str = "https://github.com/maziluiosif/oxi";

/// The latest published GitHub release, as far as an update check is concerned.
#[derive(Debug, Clone)]
pub struct ReleaseInfo {
    /// Version from the release's `tag_name`, without the leading `v`.
    pub version: String,
    /// Web page of the release, for "View release".
    pub html_url: String,
}

/// Parse `X.Y.Z` (optionally prefixed with `v`; anything from a `-` or `+` on is
/// ignored) into a comparable tuple. CI only ever tags `vX.Y.Z`, so a full semver
/// implementation would be overkill here.
pub fn parse_semver(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.trim().trim_start_matches('v');
    let core = s.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// True when `latest` is a strictly newer version than `current`. Unparseable versions
/// never count as newer (a malformed tag must not nag the user to "update").
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_semver(latest), parse_semver(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

/// Fetch the latest release from the GitHub API. GitHub requires a `User-Agent` header;
/// unauthenticated calls are rate-limited to 60/hour per IP, which is plenty for a check
/// on startup plus a manual button.
pub async fn fetch_latest_release(client: &reqwest::Client) -> Result<ReleaseInfo, String> {
    let url = "https://api.github.com/repos/maziluiosif/oxi/releases/latest";
    let resp = client
        .get(url)
        .header("User-Agent", format!("oxi/{APP_VERSION}"))
        .header("Accept", "application/vnd.github+json")
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("GitHub API: HTTP {}", resp.status().as_u16()));
    }
    let body: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let tag = body
        .get("tag_name")
        .and_then(|v| v.as_str())
        .ok_or("GitHub API: response has no tag_name")?;
    let html_url = body
        .get("html_url")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| format!("{REPO_URL}/releases/latest"));
    Ok(ReleaseInfo {
        version: tag.trim_start_matches('v').to_string(),
        html_url,
    })
}

/// Install scripts served from GitHub Pages (`docs/`); `oxi update` reruns them.
#[cfg(not(windows))]
const INSTALL_SH_URL: &str = "https://maziluiosif.github.io/oxi/install.sh";
#[cfg(windows)]
const INSTALL_PS1_URL: &str = "https://maziluiosif.github.io/oxi/install.ps1";

/// `oxi update [version]`: reinstall from the install script, in place of the running binary.
/// Returns the process exit code.
pub fn run_cli_update(version: Option<String>) -> i32 {
    let mut env: Vec<(&str, String)> = Vec::new();
    if let Some(v) = version {
        env.push(("OXI_VERSION", v));
    }
    // Update where this binary lives rather than the script's default location, unless the user
    // already chose one or this is a `cargo` build.
    if let Some((key, dir)) = install_dir_override()
        && std::env::var_os(key).is_none()
    {
        env.push((key, dir.to_string_lossy().into_owned()));
    }
    match spawn_installer(&env) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("oxi: error: could not run the installer: {e}");
            1
        }
    }
}

/// The install script variable that points at the running install, and its value.
fn install_dir_override() -> Option<(&'static str, std::path::PathBuf)> {
    let dir = running_install()?.parent()?.to_path_buf();
    if cfg!(target_os = "macos") {
        Some(("OXI_APP_DIR", dir))
    } else if cfg!(windows) {
        Some(("OXI_INSTALL_DIR", dir))
    } else {
        Some(("OXI_BIN_DIR", dir))
    }
}

/// What an update replaces: `oxi.app` on macOS, the executable elsewhere. `None` for `cargo`
/// builds and for a macOS binary that is not inside an app bundle. Resolved once: after an update
/// Linux reports the running executable as deleted.
pub fn running_install() -> Option<PathBuf> {
    static INSTALL: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    INSTALL.get_or_init(find_running_install).clone()
}

fn find_running_install() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    // Follow the `~/.local/bin/oxi` symlink to the real install. Not on Windows: there the result
    // is a `\\?\C:\…` verbatim path, which PowerShell's path cmdlets reject.
    #[cfg(not(windows))]
    let exe = exe.canonicalize().ok()?;
    if exe.components().any(|c| c.as_os_str() == "target") {
        return None;
    }
    if cfg!(target_os = "macos") {
        // `<dir>/oxi.app/Contents/MacOS/oxi`
        let app = exe.ancestors().nth(3)?;
        if app.extension()? != "app" {
            return None;
        }
        Some(app.to_path_buf())
    } else {
        Some(exe)
    }
}

/// Release asset for this platform; names match the build matrix in `release.yml`.
fn release_asset() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("oxi-macos-arm64.tar.gz"),
        ("linux", "x86_64") => Some("oxi-linux-x86_64.tar.gz"),
        ("windows", "x86_64") => Some("oxi-windows-x86_64.zip"),
        _ => None,
    }
}

/// True when this binary can replace itself with a release build.
pub fn can_self_update() -> bool {
    release_asset().is_some() && running_install().is_some()
}

/// Download `info`'s release asset, check it against the release's `SHA256SUMS`, and swap it in
/// place of the running install. The running process keeps its already-open image; the new
/// version starts on the next launch. Returns the replaced path, which [`request_relaunch`] takes.
pub async fn download_and_install(
    client: &reqwest::Client,
    info: &ReleaseInfo,
) -> Result<PathBuf, String> {
    let asset = release_asset().ok_or("no prebuilt release for this platform")?;
    let target = running_install().ok_or("this build is not a release install")?;
    let base = format!("{REPO_URL}/releases/download/v{}", info.version);

    let sums = fetch_bytes(client, &format!("{base}/SHA256SUMS")).await?;
    let sums = String::from_utf8_lossy(&sums);
    let expected = expected_sha256(&sums, asset).ok_or(format!("{asset} is not in SHA256SUMS"))?;
    let bytes = fetch_bytes(client, &format!("{base}/{asset}")).await?;
    let actual: String = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if !actual.eq_ignore_ascii_case(&expected) {
        return Err(format!("checksum mismatch for {asset}"));
    }

    install_archive(&bytes, &target)?;
    Ok(target)
}

pub(crate) async fn fetch_bytes(client: &reqwest::Client, url: &str) -> Result<Vec<u8>, String> {
    let resp = client
        .get(url)
        .header("User-Agent", format!("oxi/{APP_VERSION}"))
        .timeout(std::time::Duration::from_secs(300))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("{url}: HTTP {}", resp.status().as_u16()));
    }
    Ok(resp.bytes().await.map_err(|e| e.to_string())?.to_vec())
}

/// The checksum `sha256sum` wrote for `asset` (`<hex>  <name>`, or `<hex> *<name>` in binary mode).
pub(crate) fn expected_sha256(sums: &str, asset: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, name) = line.split_once(char::is_whitespace)?;
        (name.trim().trim_start_matches('*') == asset).then(|| hash.to_string())
    })
}

/// Unpack the archive next to `target` (so the swap is a same-volume rename) and swap it in.
fn install_archive(bytes: &[u8], target: &Path) -> Result<(), String> {
    let dir = target.parent().ok_or("install has no parent directory")?;
    let staging = dir.join(format!(".oxi-update-{}", std::process::id()));
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(&staging).map_err(|e| format!("cannot write to {}: {e}", dir.display()))?;
    let result = unpack(bytes, &staging).and_then(|()| {
        let name = target.file_name().ok_or("install has no file name")?;
        swap_in(&staging.join(name), target)
    });
    let _ = fs::remove_dir_all(&staging);
    result
}

fn unpack(bytes: &[u8], into: &Path) -> Result<(), String> {
    if cfg!(windows) {
        zip::ZipArchive::new(Cursor::new(bytes))
            .and_then(|mut zip| zip.extract(into))
            .map_err(|e| format!("could not unpack the update: {e}"))
    } else {
        // `unpack` refuses entries that would land outside `into` and keeps file modes.
        tar::Archive::new(flate2::read::GzDecoder::new(Cursor::new(bytes)))
            .unpack(into)
            .map_err(|e| format!("could not unpack the update: {e}"))
    }
}

/// Replace `target` with `new`. A running executable (or the bundle holding it) can be renamed
/// on every platform, though Windows will not delete it, so the old copy is moved aside first and
/// moved back if the new one cannot take its place.
fn swap_in(new: &Path, target: &Path) -> Result<(), String> {
    if !new.exists() {
        return Err("the update archive is missing oxi".into());
    }
    let old = old_copy_path(target);
    remove_path(&old);
    fs::rename(target, &old).map_err(|e| format!("cannot replace {}: {e}", target.display()))?;
    if let Err(e) = fs::rename(new, target) {
        let _ = fs::rename(&old, target);
        return Err(format!("cannot replace {}: {e}", target.display()));
    }
    // Windows keeps the running exe locked; `cleanup_previous_install` removes it next launch.
    remove_path(&old);
    Ok(())
}

fn old_copy_path(target: &Path) -> PathBuf {
    let mut name = target.file_name().unwrap_or_default().to_os_string();
    name.push(".old");
    target.with_file_name(name)
}

fn remove_path(path: &Path) {
    if path.is_dir() {
        let _ = fs::remove_dir_all(path);
    } else {
        let _ = fs::remove_file(path);
    }
}

/// Remove the copy an update moved aside (Windows cannot delete a running exe). Called at startup.
pub fn cleanup_previous_install() {
    if cfg!(windows)
        && let Some(target) = running_install()
    {
        remove_path(&old_copy_path(&target));
    }
}

/// The install to start once the window has closed, set by "Restart to update".
static RELAUNCH: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Start `install` after the app exits (see [`relaunch_if_requested`]).
pub fn request_relaunch(install: PathBuf) {
    *RELAUNCH.lock().unwrap_or_else(|e| e.into_inner()) = Some(install);
}

/// Forget a pending relaunch, e.g. when the user cancels the quit it was waiting on.
pub fn cancel_relaunch() {
    RELAUNCH.lock().unwrap_or_else(|e| e.into_inner()).take();
}

/// Launch the updated install if "Restart to update" asked for it. Called after the event loop.
pub fn relaunch_if_requested() {
    let Some(install) = RELAUNCH.lock().unwrap_or_else(|e| e.into_inner()).take() else {
        return;
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = if cfg!(target_os = "macos") {
        // Through Launch Services, so the new process is a proper app (Dock icon, menu bar).
        // `-n`: this process may still be registered as the running oxi.
        let mut cmd = std::process::Command::new("open");
        cmd.arg("-n").arg(&install);
        if !args.is_empty() {
            cmd.arg("--args").args(&args);
        }
        cmd.spawn()
    } else {
        std::process::Command::new(&install).args(&args).spawn()
    };
    if let Err(e) = result {
        log::warn!("could not relaunch {}: {e}", install.display());
    }
}

#[cfg(not(windows))]
fn spawn_installer(env: &[(&str, String)]) -> std::io::Result<i32> {
    let status = std::process::Command::new("sh")
        .args(["-c", &format!("curl -fsSL {INSTALL_SH_URL} | sh")])
        .envs(env.iter().map(|(k, v)| (k, v)))
        .status()?;
    Ok(status.code().unwrap_or(1))
}

/// oxi.exe is a GUI-subsystem binary: the terminal that launched it does not wait for it and has
/// no console to print to, so the installer runs in its own console window. oxi exits right away,
/// which also frees `oxi.exe` for the script to overwrite.
#[cfg(windows)]
fn spawn_installer(env: &[(&str, String)]) -> std::io::Result<i32> {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
    let script = format!(
        "try {{ irm {INSTALL_PS1_URL} | iex }} catch {{ Write-Host \"oxi: error: $_\" -ForegroundColor Red }}; \
         Read-Host 'Press Enter to close' | Out-Null"
    );
    std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &script,
        ])
        .envs(env.iter().map(|(k, v)| (k, v)))
        .creation_flags(CREATE_NEW_CONSOLE)
        .spawn()?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_and_v_prefixed() {
        assert_eq!(parse_semver("0.8.0"), Some((0, 8, 0)));
        assert_eq!(parse_semver("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_semver(" v10.0.42 "), Some((10, 0, 42)));
    }

    #[test]
    fn ignores_prerelease_and_build_suffixes() {
        assert_eq!(parse_semver("1.2.3-rc.1"), Some((1, 2, 3)));
        assert_eq!(parse_semver("1.2.3+build5"), Some((1, 2, 3)));
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(parse_semver(""), None);
        assert_eq!(parse_semver("latest"), None);
        assert_eq!(parse_semver("1.2.3.4"), None);
        assert_eq!(parse_semver("1.x.0"), None);
    }

    #[test]
    fn newer_comparison() {
        assert!(is_newer("0.9.0", "0.8.0"));
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(!is_newer("0.8.0", "0.8.0"));
        assert!(!is_newer("0.7.9", "0.8.0"));
        // Malformed tags never count as an available update.
        assert!(!is_newer("latest", "0.8.0"));
    }

    #[test]
    fn finds_asset_checksum_in_text_and_binary_mode() {
        let sums = "aa11  oxi-linux-x86_64.tar.gz\nbb22 *oxi-windows-x86_64.zip\n";
        assert_eq!(
            expected_sha256(sums, "oxi-linux-x86_64.tar.gz").as_deref(),
            Some("aa11")
        );
        assert_eq!(
            expected_sha256(sums, "oxi-windows-x86_64.zip").as_deref(),
            Some("bb22")
        );
        assert_eq!(expected_sha256(sums, "oxi-macos-arm64.tar.gz"), None);
    }

    #[test]
    fn swap_replaces_target_and_leaves_no_old_copy() {
        let dir = std::env::temp_dir().join(format!("oxi-swap-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("staging")).unwrap();
        let target = dir.join("oxi");
        fs::write(&target, "old").unwrap();
        fs::write(dir.join("staging/oxi"), "new").unwrap();
        swap_in(&dir.join("staging/oxi"), &target).unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "new");
        assert!(!old_copy_path(&target).exists());
        // A missing new copy leaves the install untouched.
        assert!(swap_in(&dir.join("staging/oxi"), &target).is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), "new");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn app_version_is_parseable() {
        assert!(parse_semver(APP_VERSION).is_some());
    }
}
