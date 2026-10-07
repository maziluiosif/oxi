//! Crash-safe file replacement for oxi's own files (settings, manifests, caches) and the agent's
//! file tools.
//!
//! Every write goes to a synced temporary file next to the destination, which is then renamed
//! over it: a crash, a full disk or a killed process leaves either the old content or the new
//! one, never a truncated mix. Files the user saves from the editor are still written in place,
//! like other editors do, so their extended attributes, hard links and bind mounts survive.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// A fresh, unpredictable sibling path in `dir`, e.g. `.oxi-write-123-<random>.tmp`.
pub fn unique_temp_path(dir: &Path, prefix: &str, extension: &str) -> PathBuf {
    use rand::RngExt;
    let random: u64 = rand::rng().random();
    dir.join(format!(
        "{prefix}-{}-{random:016x}.{extension}",
        std::process::id()
    ))
}

/// Replace `path` with `content` atomically. A symlink at `path` is replaced rather than
/// followed. An existing file keeps its permissions; a new one gets the usual
/// `0666 & !umask`.
pub fn write_atomic(path: &Path, content: &[u8]) -> Result<(), String> {
    write_atomic_with(path, content, false)
}

/// Like [`write_atomic`], but the result is always readable by the owner only (Unix), for
/// app state such as `settings.json`.
pub fn write_atomic_private(path: &Path, content: &[u8]) -> Result<(), String> {
    write_atomic_with(path, content, true)
}

fn write_atomic_with(path: &Path, content: &[u8], private: bool) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let tmp = unique_temp_path(parent, ".oxi-write", "tmp");
    let existing_permissions = fs::symlink_metadata(path)
        .ok()
        .filter(|m| m.is_file())
        .map(|m| m.permissions());
    let result = (|| -> Result<(), String> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // Replacing a file: stay private until its own permissions are copied over, so a
            // restrictive original (a key, a .env) is never readable mid-write. A new file
            // gets the usual 0666 & !umask, like any other tool creating it.
            options.mode(if private || existing_permissions.is_some() {
                0o600
            } else {
                0o666
            });
        }
        let mut file = options.open(&tmp).map_err(|e| e.to_string())?;
        file.write_all(content).map_err(|e| e.to_string())?;
        file.flush().map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        if let Some(permissions) = existing_permissions.filter(|_| !private) {
            fs::set_permissions(&tmp, permissions).map_err(|e| e.to_string())?;
        }
        // Close the temporary file before renaming it. This is especially important on Windows,
        // where an open handle can make rename semantics more restrictive.
        drop(file);
        install(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Move a fully written and synced temporary file over `destination`, then sync the directory
/// so the rename itself survives a crash. On failure the temporary file is left for the caller
/// to remove.
pub fn install(tmp: &Path, destination: &Path) -> Result<(), String> {
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    #[cfg(windows)]
    if destination.exists() {
        // Move the original aside and roll back if installing the synced replacement fails,
        // so an error never leaves the destination missing. Sharing violations from
        // editors/AV are often very brief.
        let backup = unique_temp_path(parent, ".oxi-backup", "tmp");
        retry_windows_io("move original to backup", destination, || {
            fs::rename(destination, &backup)
        })?;
        if let Err(install_error) = retry_windows_io("install replacement", destination, || {
            fs::rename(tmp, destination)
        }) {
            if let Err(rollback_error) = retry_windows_io("restore original", destination, || {
                fs::rename(&backup, destination)
            }) {
                return Err(format!(
                    "{install_error}; rollback also failed: {rollback_error}; original remains at {}",
                    backup.display()
                ));
            }
            return Err(install_error);
        }
        let _ = retry_windows_io("remove replacement backup", &backup, || {
            fs::remove_file(&backup)
        });
    } else {
        retry_windows_io("install new file", destination, || {
            fs::rename(tmp, destination)
        })?;
    }
    #[cfg(not(windows))]
    fs::rename(tmp, destination).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    if let Ok(dir) = fs::File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(())
}

#[cfg(windows)]
fn retry_windows_io<T>(
    operation: &str,
    path: &Path,
    mut action: impl FnMut() -> std::io::Result<T>,
) -> Result<T, String> {
    // ERROR_ACCESS_DENIED, ERROR_SHARING_VIOLATION, and ERROR_LOCK_VIOLATION are commonly
    // transient while an editor, language server, indexer, or antivirus scans a file.
    const RETRY_DELAYS_MS: &[u64] = &[10, 25, 50, 100, 200, 400];
    for delay_ms in RETRY_DELAYS_MS {
        match action() {
            Ok(value) => return Ok(value),
            Err(error) if is_transient_windows_file_error(&error) => {
                std::thread::sleep(std::time::Duration::from_millis(*delay_ms));
            }
            Err(error) => {
                return Err(format!("Could not {operation} {}: {error}", path.display()));
            }
        }
    }
    action().map_err(|error| {
        format!(
            "Could not {operation} {} after {} attempts: {error}",
            path.display(),
            RETRY_DELAYS_MS.len() + 1
        )
    })
}

#[cfg(windows)]
fn is_transient_windows_file_error(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::PermissionDenied
        || matches!(error.raw_os_error(), Some(5 | 32 | 33))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = unique_temp_path(&std::env::temp_dir(), &format!("oxi-fsutil-{name}"), "d");
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn leftovers(dir: &Path) -> Vec<String> {
        fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(".oxi-"))
            .collect()
    }

    #[test]
    fn write_atomic_creates_and_replaces_without_leftovers() {
        let dir = temp_dir("replace");
        let path = dir.join("a.txt");
        write_atomic(&path, b"one").unwrap();
        write_atomic(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        assert!(leftovers(&dir).is_empty());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn write_atomic_creates_missing_parents() {
        let dir = temp_dir("parents");
        let path = dir.join("x/y/z.txt");
        write_atomic(&path, b"deep").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"deep");
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn modes_are_kept_defaulted_or_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("modes");
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;

        let reference = dir.join("reference");
        fs::write(&reference, "x").unwrap();
        let fresh = dir.join("fresh");
        write_atomic(&fresh, b"x").unwrap();
        assert_eq!(mode(&fresh), mode(&reference));

        let script = dir.join("run.sh");
        fs::write(&script, "old").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o750)).unwrap();
        write_atomic(&script, b"new").unwrap();
        assert_eq!(mode(&script), 0o750);

        let settings = dir.join("settings.json");
        fs::write(&settings, "{}").unwrap();
        fs::set_permissions(&settings, fs::Permissions::from_mode(0o644)).unwrap();
        write_atomic_private(&settings, b"{}").unwrap();
        assert_eq!(mode(&settings), 0o600);
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_replaces_a_symlink_instead_of_following_it() {
        let dir = temp_dir("replace-link");
        let outside = dir.join("outside.txt");
        fs::write(&outside, "keep").unwrap();
        let link = dir.join("link.txt");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        write_atomic(&link, b"new").unwrap();
        assert_eq!(fs::read(&outside).unwrap(), b"keep");
        assert!(
            !fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(&link).unwrap(), b"new");
        let _ = fs::remove_dir_all(dir);
    }
}
