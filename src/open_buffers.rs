//! The editor's open files, as ACP agents see them. An agent reading a file through the client
//! (`fs/read_text_file`) gets the unsaved text of an open tab, and a file it writes through the
//! client lands in the open tab as one undoable edit instead of a "changed on disk" conflict.
//! The UI publishes its tabs each frame; agents read and write from their own threads.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Default)]
struct Buffers {
    /// Open text tabs: their content revision and, while it differs from the disk, the text.
    open: HashMap<PathBuf, (u64, Option<Arc<str>>)>,
    /// Content agents wrote to open files, for the UI to put in the tab.
    written: Vec<(PathBuf, String)>,
}

static BUFFERS: Mutex<Option<Buffers>> = Mutex::new(None);

#[cfg(test)]
pub(crate) static TEST_LOCK: Mutex<()> = Mutex::new(());

/// Wakes the UI when an agent wrote an open file.
static WAKER: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

fn with<R>(f: impl FnOnce(&mut Buffers) -> R) -> R {
    let mut buffers = BUFFERS.lock().unwrap_or_else(|e| e.into_inner());
    f(buffers.get_or_insert_with(Buffers::default))
}

/// Install the UI wake-up. Only the first call takes effect.
pub fn set_waker(wake: impl Fn() + Send + Sync + 'static) {
    if WAKER.get().is_none() {
        let _ = WAKER.set(Box::new(wake));
    }
}

/// One open text tab: path, content revision, and its text when it has unsaved changes.
pub struct OpenTab<'a> {
    pub path: &'a Path,
    pub revision: u64,
    pub unsaved: Option<&'a str>,
}

/// Replace the published tabs. The text is copied only when a tab's revision changed.
pub fn publish<'a>(tabs: impl Iterator<Item = OpenTab<'a>>) {
    with(|buffers| {
        let mut open = HashMap::new();
        for tab in tabs {
            let previous = buffers.open.remove(tab.path);
            let text = pending_text(buffers, tab.path).or_else(|| match (previous, tab.unsaved) {
                (_, None) => None,
                (Some((revision, Some(text))), Some(_)) if revision == tab.revision => Some(text),
                (_, Some(text)) => Some(Arc::from(text)),
            });
            open.insert(tab.path.to_path_buf(), (tab.revision, text));
        }
        buffers.open = open;
    });
}

/// The unsaved text of the tab showing `path`, if it has unsaved changes.
pub fn unsaved(path: &Path) -> Option<Arc<str>> {
    with(|buffers| buffers.open.get(path)?.1.clone())
}

/// Write to disk and publish the successful edit together. The UI cannot drain the edit
/// before the disk write finishes, and failed writes never replace an editor buffer. The write
/// is crash-safe like an editor save ([`crate::fsutil::save`]): a crash or a full disk mid-way
/// leaves the old content, never a truncated file.
pub fn write_text(path: &Path, content: &str) -> std::io::Result<()> {
    let open = with(|buffers| -> std::io::Result<bool> {
        crate::fsutil::save(path, content.as_bytes())?;
        let Some((_, text)) = buffers.open.get_mut(path) else {
            return Ok(false);
        };
        // Reads must see this write immediately, including while the UI drains the queue
        // and before it publishes the updated tab on its next frame.
        *text = Some(Arc::from(content));
        buffers.written.retain(|(p, _)| p != path);
        buffers
            .written
            .push((path.to_path_buf(), content.to_owned()));
        Ok(true)
    })?;
    if open && let Some(wake) = WAKER.get() {
        wake();
    }
    Ok(())
}

/// The latest successful agent write takes precedence over a tab publication that raced it.
fn pending_text(buffers: &Buffers, path: &Path) -> Option<Arc<str>> {
    buffers.written.iter().find_map(|(p, text)| {
        if p == path {
            Some(Arc::from(text.as_str()))
        } else {
            None
        }
    })
}

/// An agent write to `path` is waiting for the UI.
pub fn has_agent_write(path: &Path) -> bool {
    with(|buffers| buffers.written.iter().any(|(p, _)| p == path))
}

/// The agent writes to open files since the last call.
pub fn take_agent_writes() -> Vec<(PathBuf, String)> {
    with(|buffers| std::mem::take(&mut buffers.written))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsaved_text_and_agent_writes_follow_the_published_tabs() {
        let _guard = TEST_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("oxi-open-buffers-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        let (dirty, clean) = (dir.join("dirty.rs"), dir.join("clean.rs"));
        let tabs = |text: &'static str, revision| {
            [
                OpenTab {
                    path: &dirty,
                    revision,
                    unsaved: Some(text),
                },
                OpenTab {
                    path: &clean,
                    revision: 1,
                    unsaved: None,
                },
            ]
        };
        publish(tabs("fn a() {}", 1).into_iter());
        assert_eq!(unsaved(&dirty).as_deref(), Some("fn a() {}"));
        assert_eq!(unsaved(&clean), None);
        // Same revision: the published text is kept as it was.
        publish(tabs("ignored", 1).into_iter());
        assert_eq!(unsaved(&dirty).as_deref(), Some("fn a() {}"));
        publish(tabs("fn b() {}", 2).into_iter());
        assert_eq!(unsaved(&dirty).as_deref(), Some("fn b() {}"));

        write_text(&clean, "new").unwrap();
        write_text(&dir.join("closed.rs"), "x").unwrap();
        assert!(has_agent_write(&clean));
        let writes: Vec<_> = take_agent_writes()
            .into_iter()
            .filter(|(p, _)| p.starts_with(&dir))
            .collect();
        assert_eq!(writes, vec![(clean.clone(), "new".to_owned())]);
        assert!(!has_agent_write(&clean));

        publish(std::iter::empty());
        assert_eq!(unsaved(&dirty), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Agent writes go through the crash-safe save: a linked file stays a link to the edited
    /// target, and no temporary files are left next to it.
    #[cfg(unix)]
    #[test]
    fn agent_writes_keep_links_and_leave_no_temporary_files() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!(
            "oxi-open-buffers-link-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("real.rs");
        let link = dir.join("link.rs");
        std::fs::write(&target, "old").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        write_text(&link, "new").unwrap();

        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");
        let names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(names.len(), 2, "unexpected files: {names:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
