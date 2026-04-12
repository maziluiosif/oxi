//! Path resolution under workspace root.

use std::path::{Path, PathBuf};

pub(crate) fn err(s: impl Into<String>) -> String {
    s.into()
}

pub fn resolve_under_cwd(cwd: &Path, user_path: &str) -> Result<PathBuf, String> {
    let p = PathBuf::from(user_path);
    let abs = if p.is_absolute() { p } else { cwd.join(p) };
    let cwd_can = cwd.canonicalize().map_err(|e| e.to_string())?;
    let abs_can = abs.canonicalize().map_err(|e| e.to_string())?;
    if !abs_can.starts_with(&cwd_can) {
        return Err("Path escapes workspace root".to_string());
    }
    Ok(abs_can)
}

/// Resolve a path that may not exist yet, as long as its closest existing parent stays under `cwd`.
pub(crate) fn resolve_under_cwd_for_create(cwd: &Path, user_path: &str) -> Result<PathBuf, String> {
    let p = PathBuf::from(user_path);
    let abs = if p.is_absolute() { p } else { cwd.join(p) };
    let cwd_can = cwd.canonicalize().map_err(|e| e.to_string())?;

    let mut existing_parent = abs.as_path();
    while !existing_parent.exists() {
        existing_parent = existing_parent
            .parent()
            .ok_or_else(|| err("invalid path outside workspace"))?;
    }

    let parent_can = existing_parent.canonicalize().map_err(|e| e.to_string())?;
    if !parent_can.starts_with(&cwd_can) {
        return Err("Path escapes workspace root".to_string());
    }
    Ok(abs)
}
