//! Storage shared by the global scratchpad editor and agent tool.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

static STORAGE_LOCK: Mutex<()> = Mutex::new(());
pub(crate) const MAX_BYTES: usize = 2 * 1024 * 1024;

pub(crate) fn path() -> PathBuf {
    crate::settings::AppSettings::config_path()
        .parent()
        .map_or_else(
            || PathBuf::from("scratchpad.md"),
            |dir| dir.join("scratchpad.md"),
        )
}

pub(crate) fn lock() -> MutexGuard<'static, ()> {
    STORAGE_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Caller holds the storage lock across reads and writes.
pub(crate) fn read(path: &Path) -> Result<String, String> {
    match std::fs::read_to_string(path) {
        Ok(content) => Ok(content),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error.to_string()),
    }
}

fn write(path: &Path, content: &str) -> Result<(), String> {
    if content.len() > MAX_BYTES {
        return Err("Scratchpad exceeds the 2 MiB text editor limit.".into());
    }
    let parent = path
        .parent()
        .ok_or("Scratchpad path has no parent directory.")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    crate::fsutil::write_atomic(path, content.as_bytes())
}

pub(crate) fn save(path: &Path, baseline: &str, content: &str) -> Result<(), String> {
    let _guard = lock();
    if read(path)? != baseline {
        return Err("Scratchpad changed on disk. Your unsaved text was preserved; copy it before reloading the scratchpad.".into());
    }
    write(path, content)
}

pub(crate) fn update(path: &Path, mode: &str, content: &str) -> Result<(String, String), String> {
    if !matches!(mode, "append" | "rewrite") {
        return Err("Scratchpad mode must be `append` or `rewrite`.".into());
    }
    let _guard = lock();
    let before = read(path)?;
    let after = if mode == "append" {
        format!("{before}{content}")
    } else {
        content.to_string()
    };
    write(path, &after)?;
    Ok((before, after))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_path() -> PathBuf {
        std::env::temp_dir().join(format!(
            "oxi-scratchpad-{}-{}.md",
            std::process::id(),
            rand::random::<u64>()
        ))
    }

    #[test]
    fn append_rewrite_and_clear() {
        let path = test_path();
        assert_eq!(
            update(&path, "append", "Salut\n").unwrap(),
            (String::new(), "Salut\n".into())
        );
        assert_eq!(
            update(&path, "append", "notițe").unwrap().1,
            "Salut\nnotițe"
        );
        assert_eq!(update(&path, "rewrite", "nou").unwrap().1, "nou");
        update(&path, "rewrite", "").unwrap();
        assert_eq!(read(&path).unwrap(), "");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn invalid_mode_and_io_errors_preserve_content() {
        let path = test_path();
        update(&path, "rewrite", "original").unwrap();
        assert!(update(&path, "delete", "bad").is_err());
        assert_eq!(read(&path).unwrap(), "original");
        assert!(update(&path.join("child"), "append", "bad").is_err());
        assert!(update(&path, "rewrite", &"x".repeat(MAX_BYTES + 1)).is_err());
        assert_eq!(read(&path).unwrap(), "original");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn autosave_refuses_to_overwrite_agent_changes() {
        let path = test_path();
        save(&path, "", "original").unwrap();
        update(&path, "append", "\nagent").unwrap();
        assert!(save(&path, "original", "manual edits").is_err());
        assert_eq!(read(&path).unwrap(), "original\nagent");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn concurrent_appends_are_not_lost() {
        let path = test_path();
        std::thread::scope(|scope| {
            for _ in 0..12 {
                scope.spawn(|| {
                    update(&path, "append", "x").unwrap();
                });
            }
        });
        assert_eq!(read(&path).unwrap(), "x".repeat(12));
        std::fs::remove_file(path).unwrap();
    }
}
