//! Linux application-menu entry (`~/.local/share/applications/oxi.desktop`) and icon.
//!
//! Written by the app rather than only by `install.sh`, because the in-app update swaps just the
//! binary: existing installs would otherwise never get a menu entry. Created once; a user who
//! deletes it keeps it deleted (a marker in the data dir remembers). An entry oxi wrote itself is
//! refreshed when the binary moves.

#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};

/// Key that marks an entry as written by oxi (so a hand-made `oxi.desktop` is never touched).
#[cfg(target_os = "linux")]
const MANAGED_KEY: &str = "X-Oxi-Managed=true";

/// Install or refresh the entry on a background thread. No-op off Linux and for `cargo` builds.
pub(crate) fn ensure_in_background() {
    #[cfg(target_os = "linux")]
    std::thread::Builder::new()
        .name("oxi-desktop-entry".into())
        .spawn(|| {
            if let Err(e) = ensure() {
                log::warn!("could not write the application menu entry: {e}");
            }
        })
        .ok();
}

#[cfg(target_os = "linux")]
fn ensure() -> std::io::Result<()> {
    let Some(exe) = crate::update::running_install() else {
        return Ok(());
    };
    let Some(data_home) = data_home() else {
        return Ok(());
    };
    let entry_path = data_home.join("applications/oxi.desktop");
    let marker = crate::app_dirs::data_dir().join("desktop-entry-installed");
    let wanted = entry(&launch_path(&exe));
    match std::fs::read_to_string(&entry_path) {
        Ok(existing) => {
            if existing.contains(MANAGED_KEY) && existing != wanted {
                crate::fsutil::write_atomic(&entry_path, wanted.as_bytes())
                    .map_err(std::io::Error::other)?;
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if marker.exists() {
                return Ok(());
            }
            std::fs::create_dir_all(entry_path.parent().unwrap_or(&data_home))?;
            crate::fsutil::write_atomic(&entry_path, wanted.as_bytes())
                .map_err(std::io::Error::other)?;
        }
        Err(e) => return Err(e),
    }
    write_icon(&data_home)?;
    if !marker.exists() {
        std::fs::create_dir_all(crate::app_dirs::data_dir())?;
        crate::fsutil::write_atomic(&marker, b"").map_err(std::io::Error::other)?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn data_home() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| crate::app_dirs::home_dir().map(|h| h.join(".local/share")))
}

#[cfg(target_os = "linux")]
fn write_icon(data_home: &Path) -> std::io::Result<()> {
    const ICON: &[u8] = include_bytes!("../assets/windows/app-icon-256.png");
    let path = data_home.join("icons/hicolor/256x256/apps/oxi.png");
    if std::fs::read(&path).is_ok_and(|bytes| bytes == ICON) {
        return Ok(());
    }
    std::fs::create_dir_all(path.parent().unwrap_or(data_home))?;
    crate::fsutil::write_atomic(&path, ICON).map_err(std::io::Error::other)
}

/// What the entry should launch. A Homebrew install resolves into a versioned `Cellar` folder
/// that the next `brew upgrade` deletes, so point at the stable `<prefix>/bin/oxi` link instead.
#[cfg(target_os = "linux")]
fn launch_path(exe: &Path) -> PathBuf {
    let text = exe.to_string_lossy();
    if let Some(at) = text.find("/Cellar/") {
        let linked = Path::new(&text[..at]).join("bin/oxi");
        if linked.exists() {
            return linked;
        }
    }
    exe.to_path_buf()
}

#[cfg(target_os = "linux")]
fn entry(exe: &Path) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=oxi\n\
         GenericName=AI Coding Agent\n\
         Comment=Chat with coding agents, edit and review code\n\
         Exec={}\n\
         Icon=oxi\n\
         Terminal=false\n\
         Categories=Development;IDE;\n\
         StartupWMClass=oxi\n\
         {MANAGED_KEY}\n",
        exec_arg(&exe.to_string_lossy())
    )
}

/// Quote a path for `Exec=` per the Desktop Entry spec: inside double quotes, `"`, `` ` ``, `$`
/// and `\` are backslash-escaped, and the value's own escaping doubles every backslash.
#[cfg(target_os = "linux")]
fn exec_arg(path: &str) -> String {
    let plain = |c: char| c.is_ascii_alphanumeric() || "/._-+".contains(c);
    if path.chars().all(plain) {
        return path.to_string();
    }
    let mut quoted = String::from("\"");
    for c in path.chars() {
        match c {
            '"' | '`' | '$' => {
                quoted.push_str("\\\\");
                quoted.push(c);
            }
            '\\' => quoted.push_str("\\\\\\\\"),
            '%' => quoted.push_str("%%"),
            _ => quoted.push(c),
        }
    }
    quoted.push('"');
    quoted
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn exec_quotes_only_when_needed() {
        assert_eq!(exec_arg("/home/u/.local/bin/oxi"), "/home/u/.local/bin/oxi");
        assert_eq!(exec_arg("/home/u/my apps/oxi"), "\"/home/u/my apps/oxi\"");
        assert_eq!(exec_arg("/a$b/oxi"), "\"/a\\\\$b/oxi\"");
    }

    #[test]
    fn entry_is_marked_managed() {
        let text = entry(Path::new("/home/u/.local/bin/oxi"));
        assert!(text.contains("Exec=/home/u/.local/bin/oxi\n"));
        assert!(text.contains(MANAGED_KEY));
    }
}
