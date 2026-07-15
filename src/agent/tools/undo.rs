//! Per-turn, in-memory undo journal for workspace file mutations.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
struct FileState {
    bytes: Option<Vec<u8>>,
    permissions: Option<fs::Permissions>,
}

impl FileState {
    fn read(path: &Path) -> Result<Self, String> {
        match fs::read(path) {
            Ok(bytes) => Ok(Self {
                bytes: Some(bytes),
                permissions: fs::metadata(path).ok().map(|m| m.permissions()),
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self {
                bytes: None,
                permissions: None,
            }),
            Err(e) => Err(format!("Cannot snapshot {}: {e}", path.display())),
        }
    }
}

#[derive(Clone, Debug)]
struct UndoEntry {
    before: FileState,
    after: FileState,
}

/// Shared by all tool calls in one assistant turn. It deliberately lives only in memory: after an
/// app restart the transcript is retained, but Oxi no longer promises that the old turn is undoable.
#[derive(Clone, Debug, Default)]
pub struct TurnUndoJournal {
    entries: HashMap<PathBuf, UndoEntry>,
    non_reversible_reason: Option<String>,
}

impl TurnUndoJournal {
    pub fn record_before(&mut self, path: &Path) -> Result<(), String> {
        if self.entries.contains_key(path) {
            return Ok(());
        }
        let state = FileState::read(path)?;
        self.entries.insert(
            path.to_path_buf(),
            UndoEntry {
                before: state.clone(),
                after: state,
            },
        );
        Ok(())
    }

    pub fn record_after(&mut self, path: &Path) -> Result<(), String> {
        let after = FileState::read(path)?;
        let entry = self
            .entries
            .get_mut(path)
            .ok_or_else(|| "internal: file was not journaled before mutation".to_string())?;
        entry.after = after;
        Ok(())
    }

    pub fn mark_non_reversible(&mut self, reason: impl Into<String>) {
        if self.non_reversible_reason.is_none() {
            self.non_reversible_reason = Some(reason.into());
        }
    }

    pub fn unavailable_reason(&self) -> Option<&str> {
        self.non_reversible_reason.as_deref()
    }

    /// Restore only when every tracked file still equals the state left by the agent. This avoids
    /// silently overwriting edits made by the user after the response completed.
    pub fn restore(&self, workspace: &Path) -> Result<(), String> {
        if let Some(reason) = &self.non_reversible_reason {
            return Err(reason.clone());
        }
        for (path, entry) in &self.entries {
            let current = FileState::read(path)?;
            if current.bytes != entry.after.bytes {
                return Err(format!(
                    "{} changed after the response; restore was cancelled to protect your edits.",
                    path.display()
                ));
            }
        }
        for (path, entry) in &self.entries {
            match &entry.before.bytes {
                Some(bytes) => {
                    if let Some(parent) = path.parent() {
                        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                    }
                    fs::write(path, bytes)
                        .map_err(|e| format!("Restore {}: {e}", path.display()))?;
                    if let Some(permissions) = &entry.before.permissions {
                        fs::set_permissions(path, permissions.clone()).map_err(|e| {
                            format!("Restore permissions for {}: {e}", path.display())
                        })?;
                    }
                }
                None => {
                    if path.exists() {
                        fs::remove_file(path).map_err(|e| {
                            format!("Remove {} during restore: {e}", path.display())
                        })?;
                    }
                    let mut parent = path.parent();
                    while let Some(dir) = parent {
                        if dir == workspace || !dir.starts_with(workspace) {
                            break;
                        }
                        if fs::remove_dir(dir).is_err() {
                            break;
                        }
                        parent = dir.parent();
                    }
                }
            }
        }
        Ok(())
    }
}
