//! Hand a file or folder to the operating system: open it with its default app, or show it in
//! the file manager.
//!
//! Paths come from the workspace, i.e. from whatever repository the user cloned, so they never
//! go through a shell: on Windows `cmd /C start` would run `a&calc&.png` as three commands.

use std::path::Path;
use std::process::Command;

/// Open `path` (a file or a folder) with the system's default handler.
pub fn open_path(path: &Path) {
    #[cfg(target_os = "macos")]
    let command = {
        let mut command = Command::new("open");
        command.arg(path);
        command
    };
    #[cfg(target_os = "windows")]
    let command = {
        let mut command = Command::new("explorer");
        command.arg(path);
        command
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let command = {
        let mut command = Command::new("xdg-open");
        command.arg(path);
        command
    };
    spawn(command, path);
}

/// Label of the [`reveal_path`] action on this platform.
pub fn reveal_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "Reveal in Finder"
    } else {
        "Reveal in File Manager"
    }
}

/// Show `path` selected in the file manager (Linux: open its folder).
pub fn reveal_path(path: &Path) {
    #[cfg(target_os = "macos")]
    let command = {
        let mut command = Command::new("open");
        command.arg("-R").arg(path);
        command
    };
    #[cfg(target_os = "windows")]
    let command = {
        use std::os::windows::process::CommandExt;
        // Explorer only understands the path quoted after the comma (`/select,"C:\a b\c"`);
        // the regular argument quoting would wrap the whole switch and select nothing.
        // Windows paths cannot contain `"`, so the quotes cannot be broken out of.
        let mut command = Command::new("explorer");
        command.raw_arg(format!("/select,\"{}\"", path.display()));
        command
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let command = {
        let mut command = Command::new("xdg-open");
        command.arg(path.parent().unwrap_or(path));
        command
    };
    spawn(command, path);
}

fn spawn(mut command: Command, path: &Path) {
    // Reap the child in the background so it doesn't linger as a zombie on Unix.
    match command.spawn() {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(e) => log::warn!("could not open {}: {e}", path.display()),
    }
}
