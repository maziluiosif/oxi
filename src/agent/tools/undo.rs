//! Per-turn, in-memory undo journal for reversible workspace mutations.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
enum FileState {
    Missing,
    File {
        bytes: Vec<u8>,
        permissions: fs::Permissions,
    },
    Directory {
        permissions: fs::Permissions,
    },
}

impl FileState {
    fn read(path: &Path) -> Result<Self, String> {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::Missing),
            Err(e) => return Err(format!("Cannot snapshot {}: {e}", path.display())),
        };
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "Symbolic links are not supported by reversible file operations: {}",
                path.display()
            ));
        }
        if metadata.is_file() {
            return Ok(Self::File {
                bytes: fs::read(path)
                    .map_err(|e| format!("Cannot snapshot {}: {e}", path.display()))?,
                permissions: metadata.permissions(),
            });
        }
        if metadata.is_dir() {
            return Ok(Self::Directory {
                permissions: metadata.permissions(),
            });
        }
        Err(format!("Unsupported filesystem object: {}", path.display()))
    }

    fn same_content(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Missing, Self::Missing) => true,
            (Self::File { bytes: a, .. }, Self::File { bytes: b, .. }) => a == b,
            (Self::Directory { .. }, Self::Directory { .. }) => true,
            _ => false,
        }
    }

    fn restore(&self, path: &Path) -> Result<(), String> {
        remove_current(path)?;
        match self {
            Self::Missing => Ok(()),
            Self::File { bytes, permissions } => {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                fs::write(path, bytes).map_err(|e| format!("Restore {}: {e}", path.display()))?;
                fs::set_permissions(path, permissions.clone())
                    .map_err(|e| format!("Restore permissions for {}: {e}", path.display()))
            }
            Self::Directory { permissions } => {
                fs::create_dir_all(path)
                    .map_err(|e| format!("Restore directory {}: {e}", path.display()))?;
                fs::set_permissions(path, permissions.clone())
                    .map_err(|e| format!("Restore permissions for {}: {e}", path.display()))
            }
        }
    }
}

fn remove_current(path: &Path) -> Result<(), String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.to_string()),
    };
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir(path).map_err(|e| format!("Remove directory {}: {e}", path.display()))
    } else {
        fs::remove_file(path).map_err(|e| format!("Remove {}: {e}", path.display()))
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
            .ok_or_else(|| "internal: path was not journaled before mutation".to_string())?;
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

    /// Restore only when every tracked path still equals the state left by the agent. This avoids
    /// silently overwriting edits made by the user after the response completed.
    pub fn restore(&self, workspace: &Path) -> Result<(), String> {
        if let Some(reason) = &self.non_reversible_reason {
            return Err(reason.clone());
        }
        for (path, entry) in &self.entries {
            let current = FileState::read(path)?;
            if !current.same_content(&entry.after) {
                return Err(format!(
                    "{} changed after the response; restore was cancelled to protect your edits.",
                    path.display()
                ));
            }
        }
        // A directory created by the agent may contain other journaled paths. Ensure it contains
        // no untracked descendants (for example a file the user added after the response) before
        // beginning restoration, otherwise rollback could become partial.
        for (path, entry) in &self.entries {
            if matches!(entry.before, FileState::Missing)
                && matches!(entry.after, FileState::Directory { .. })
                && path.exists()
            {
                let mut pending = vec![path.clone()];
                while let Some(dir) = pending.pop() {
                    for child in fs::read_dir(&dir).map_err(|e| e.to_string())? {
                        let child = child.map_err(|e| e.to_string())?.path();
                        if !self.entries.contains_key(&child) {
                            return Err(format!(
                                "{} was added after the response; restore was cancelled to protect your edits.",
                                child.display()
                            ));
                        }
                        if child.is_dir() {
                            pending.push(child);
                        }
                    }
                }
            }
        }

        // Files must be restored before their parent directories are removed. Deepest-first also
        // correctly reverses a sequence that deleted children and then their directory.
        let mut entries: Vec<_> = self.entries.iter().collect();
        entries.sort_by_key(|(path, _)| std::cmp::Reverse(path.components().count()));
        for (path, entry) in entries {
            entry.before.restore(path)?;
            if matches!(entry.before, FileState::Missing) {
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
        Ok(())
    }
}
