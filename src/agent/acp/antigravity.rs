//! Official native Antigravity ACP distribution (pinned to the reviewed registry release).
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;

use tokio::process::Command;
use tokio::sync::Mutex;

use crate::settings::ProviderConfig;

const VERSION: &str = "1.4.0";

pub(super) fn needs_authentication(init: &serde_json::Value, error: &str) -> bool {
    is_antigravity_agent(init) && error_requires_authentication(error)
}

fn is_antigravity_agent(init: &serde_json::Value) -> bool {
    let by_name = init["agentInfo"]["name"]
        .as_str()
        .is_some_and(|name| name.to_ascii_lowercase().contains("antigravity"));
    let by_auth_methods = init["authMethods"].as_array().is_some_and(|methods| {
        methods.iter().any(|m| {
            matches!(
                m["id"].as_str(),
                Some("oauth-personal" | "oauth-business" | "gemini-api-key")
            )
        })
    });
    by_name || by_auth_methods
}

fn error_requires_authentication(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("authentication required")
        || lower.contains("unauthenticated")
        || lower.contains("not authenticated")
}

pub(super) fn auth_method(env: &[(String, String)]) -> &'static str {
    if env
        .iter()
        .any(|(key, value)| key == "GEMINI_API_KEY" && !value.trim().is_empty())
    {
        "gemini-api-key"
    } else {
        "oauth-personal"
    }
}

pub(super) fn is_default(command: &str) -> bool {
    command.trim() == ProviderConfig::DEFAULT_ANTIGRAVITY_ACP_COMMAND
}

fn distribution(os: &str, arch: &str) -> Result<(&'static str, &'static str), String> {
    match (os, arch) {
        ("macos", "aarch64") => Ok(("macos", "darwin-arm64")),
        ("macos", "x86_64") => Ok(("macos", "darwin-x86_64")),
        ("linux", "aarch64") => Ok(("linux", "linux-arm64")),
        ("linux", "x86_64") => Ok(("linux", "linux-x86_64")),
        ("windows", "aarch64") => Ok(("windows", "windows-arm64")),
        ("windows", "x86_64") => Ok(("windows", "windows-x86_64")),
        _ => Err(format!(
            "Antigravity has no official ACP binary for {os}/{arch}. Set a custom Agent command in Settings."
        )),
    }
}

fn root() -> PathBuf {
    crate::app_dirs::data_dir()
        .join("acp")
        .join("antigravity")
        .join(VERSION)
}

fn binary(root: &Path) -> PathBuf {
    root.join(if cfg!(windows) {
        "agy_acp_server.exe"
    } else {
        "agy_acp_server.par"
    })
}

pub(crate) fn is_managed_installed() -> bool {
    binary(&root()).is_file()
}

/// The installed Antigravity CLI (`agy`) when it supports the native `agy acp` subcommand
/// (CLI v1.5+). Checked once per binary path and cached for the process lifetime.
async fn installed_cli_acp() -> Option<PathBuf> {
    let agy = find_agy_cli().await?;
    static SUPPORTED: OnceLock<std::sync::Mutex<std::collections::HashMap<PathBuf, bool>>> =
        OnceLock::new();
    let cache = SUPPORTED.get_or_init(Default::default);
    if let Some(&ok) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(&agy) {
        return ok.then_some(agy);
    }
    let ok = cli_supports_acp(&agy).await;
    cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(agy.clone(), ok);
    ok.then_some(agy)
}

async fn find_agy_cli() -> Option<PathBuf> {
    let path = super::install::shell_path()
        .await
        .or_else(|| std::env::var("PATH").ok())
        .unwrap_or_default();
    let exts: &[&str] = if cfg!(windows) {
        &["", ".exe", ".cmd", ".bat"]
    } else {
        &[""]
    };
    let mut dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
    if let Some(home) = dirs::home_dir() {
        for rel in [".local/bin", ".antigravity/bin"] {
            dirs.push(home.join(rel));
        }
    }
    #[cfg(unix)]
    for sys in ["/opt/homebrew/bin", "/usr/local/bin"] {
        dirs.push(PathBuf::from(sys));
    }
    dirs.into_iter().find_map(|dir| {
        exts.iter()
            .map(|ext| dir.join(format!("agy{ext}")))
            .find(|p| p.is_file())
    })
}

async fn cli_supports_acp(agy: &Path) -> bool {
    let mut cmd = Command::new(agy);
    cmd.args(["acp", "--help"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    tokio::time::timeout(Duration::from_secs(5), cmd.status())
        .await
        .ok()
        .and_then(Result::ok)
        .is_some_and(|s| s.success())
}

pub(super) async fn is_ready() -> bool {
    is_managed_installed() || installed_cli_acp().await.is_some()
}

pub(super) async fn resolve() -> Result<String, String> {
    if let Some(agy) = installed_cli_acp().await {
        return Ok(format!("{} acp", super::install::shell_quote(&agy)));
    }
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let _guard = LOCK.get_or_init(|| Mutex::new(())).lock().await;
    let root = root();
    if !binary(&root).is_file() {
        let (folder, target) = distribution(std::env::consts::OS, std::env::consts::ARCH)?;
        let url = format!(
            "https://dl.google.com/agy-extensions/releases/{folder}/agy-acp-server-{VERSION}-{target}.zip"
        );
        let client = reqwest::Client::builder()
            // ~110 MB: bound stalls, not the whole transfer, so slow links still finish.
            .connect_timeout(Duration::from_secs(30))
            .read_timeout(Duration::from_secs(60))
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| e.to_string())?;
        let response = client
            .get(&url)
            .send()
            .await
            .map_err(|e| {
                format!(
                    "Could not download Antigravity ACP: {e}. Retry or set a custom Agent command."
                )
            })?
            .error_for_status()
            .map_err(|e| format!("Antigravity ACP download failed: {e}"))?;
        let bytes = response.bytes().await.map_err(|e| e.to_string())?;
        let install_root = root.clone();
        tokio::task::spawn_blocking(move || install(&bytes, &install_root))
            .await
            .map_err(|e| e.to_string())??;
    }
    let command = super::install::shell_quote(&binary(&root));
    Ok(if cfg!(target_os = "linux") {
        format!("{command} --uid=")
    } else {
        command
    })
}

fn install(bytes: &[u8], root: &Path) -> Result<(), String> {
    let parent = root
        .parent()
        .ok_or("Antigravity install has no parent directory")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let staging = parent.join(format!(
        ".staging-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir(&staging).map_err(|e| e.to_string())?;
    let result = (|| {
        extract(bytes, &staging)?;
        if !binary(&staging).is_file() {
            return Err("Antigravity archive is missing the ACP server".to_string());
        }
        // A leftover folder without the server (interrupted or edited install) would block the rename.
        if root.exists() {
            std::fs::remove_dir_all(root).map_err(|e| e.to_string())?;
        }
        std::fs::rename(&staging, root)
            .map_err(|e| format!("Could not install Antigravity ACP: {e}"))
    })();
    let _ = std::fs::remove_dir_all(&staging);
    result
}

fn extract(bytes: &[u8], root: &Path) -> Result<(), String> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| e.to_string())?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
        let path = entry
            .enclosed_name()
            .ok_or("Unsafe path in Antigravity archive")?;
        if entry.is_symlink() {
            return Err("Symlink in Antigravity archive".into());
        }
        let path = root.join(path);
        if entry.is_dir() {
            std::fs::create_dir_all(&path).map_err(|e| e.to_string())?;
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut file = std::fs::File::create(&path).map_err(|e| e.to_string())?;
        std::io::copy(&mut entry, &mut file).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // The server and its localharness companion both need execution permission.
            let mode = entry.unix_mode().unwrap_or(0o755) & 0o777;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn distributions_cover_official_registry_platforms() {
        for os in ["macos", "linux", "windows"] {
            for arch in ["aarch64", "x86_64"] {
                assert!(distribution(os, arch).is_ok());
            }
        }
        assert!(distribution("linux", "riscv64").is_err());
        assert!(distribution("freebsd", "x86_64").is_err());
        assert!(is_default(ProviderConfig::DEFAULT_ANTIGRAVITY_ACP_COMMAND));
        assert!(!is_default("/custom/agy_acp_server.par --debug"));
    }

    #[test]
    fn native_install_keeps_companion_and_rejects_traversal() {
        fn archive(names: &[&str]) -> Vec<u8> {
            let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
            for name in names {
                zip.start_file(
                    *name,
                    zip::write::SimpleFileOptions::default().unix_permissions(0o755),
                )
                .unwrap();
                zip.write_all(b"fixture").unwrap();
            }
            zip.finish().unwrap().into_inner()
        }
        let root = std::env::temp_dir().join(format!("oxi-agy-install-{}", rand::random::<u64>()));
        let server = if cfg!(windows) {
            "agy_acp_server.exe"
        } else {
            "agy_acp_server.par"
        };
        install(&archive(&[server, "localharness_external"]), &root).unwrap();
        assert!(binary(&root).is_file());
        assert!(root.join("localharness_external").is_file());
        assert!(extract(&archive(&["../escape"]), &root).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_ne!(
                std::fs::metadata(binary(&root))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o111,
                0
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recognizes_antigravity_auth_across_standalone_and_cli_servers() {
        use serde_json::json;
        let standalone = json!({"agentInfo": {"name": "antigravity-acp"}});
        let cli_acp = json!({
            "agentInfo": {"name": "Antigravity ACP Server", "version": "1.5.13"},
            "authMethods": [{"id": "oauth-personal"}, {"id": "gemini-api-key"}]
        });
        let by_methods_only = json!({
            "agentInfo": {"name": "custom-wrapper"},
            "authMethods": [{"id": "oauth-personal"}]
        });
        let other = json!({"agentInfo": {"name": "other-agent"}});

        assert!(needs_authentication(&standalone, "Authentication required"));
        assert!(needs_authentication(&cli_acp, "Authentication required"));
        assert!(needs_authentication(&cli_acp, "User is unauthenticated"));
        assert!(needs_authentication(&by_methods_only, "Not authenticated"));
        assert!(!needs_authentication(&cli_acp, "invalid workspace"));
        assert!(!needs_authentication(&other, "Authentication required"));

        assert_eq!(auth_method(&[]), "oauth-personal");
        assert_eq!(
            auth_method(&[("GEMINI_API_KEY".into(), "  ".into())]),
            "oauth-personal"
        );
        assert_eq!(
            auth_method(&[("GEMINI_API_KEY".into(), "test-key".into())]),
            "gemini-api-key"
        );
    }
}
