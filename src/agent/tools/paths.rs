//! Path resolution under workspace root.

use std::path::{Component, Path, PathBuf};

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

/// Collapse `.` and `..` components without touching the filesystem. `..` above the start of a
/// relative path is kept, so the caller's prefix check still rejects it.
fn normalize_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Resolve a path that may not exist yet, as long as its closest existing parent stays under `cwd`.
///
/// `..` is collapsed before the check: otherwise `new/../../x` passes (its closest existing
/// parent is `cwd`) and `create_dir_all` then walks out of the workspace. The result is the
/// canonical existing parent plus the missing tail, so the write lands where it was checked.
pub(crate) fn resolve_under_cwd_for_create(cwd: &Path, user_path: &str) -> Result<PathBuf, String> {
    let p = PathBuf::from(user_path);
    let abs = normalize_lexically(&if p.is_absolute() { p } else { cwd.join(p) });
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
    let tail = abs
        .strip_prefix(existing_parent)
        .map_err(|_| err("invalid path outside workspace"))?;
    // `join("")` would append a trailing separator, which breaks writes to existing files.
    if tail.as_os_str().is_empty() {
        return Ok(parent_can);
    }
    Ok(parent_can.join(tail))
}
