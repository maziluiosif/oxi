//! Optional network transport through the user's installed `git` executable.
//!
//! When Settings → GitHub → "Use system Git" is on, fetch and push spawn `git` so the machine's
//! own credential helpers, SSH configuration and corporate setup apply instead of libgit2 plus the
//! stored token. Integrating fetched commits still runs through libgit2 (see
//! `network::integrate_upstream`), so pull keeps the same dirty-tree and conflict rollback rules.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Common install locations probed when `git` is not on `PATH`. GUI apps launched from the
/// macOS Dock/Finder inherit a minimal `PATH` that omits Homebrew.
#[cfg(not(windows))]
const FALLBACK_LOCATIONS: &[&str] = &[
    "/opt/homebrew/bin/git",
    "/usr/local/bin/git",
    "/usr/bin/git",
];
#[cfg(windows)]
const FALLBACK_LOCATIONS: &[&str] = &[
    r"C:\Program Files\Git\cmd\git.exe",
    r"C:\Program Files (x86)\Git\cmd\git.exe",
];

/// The executable to spawn: the configured path when set, else `git` from `PATH`, else the first
/// existing [`FALLBACK_LOCATIONS`] entry. Falls back to a bare `git` so the spawn error names it.
pub(super) fn resolve_executable(configured: &str) -> PathBuf {
    let configured = configured.trim();
    if !configured.is_empty() {
        return PathBuf::from(configured);
    }
    let name = if cfg!(windows) { "git.exe" } else { "git" };
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .map(|dir| dir.join(name))
        .chain(FALLBACK_LOCATIONS.iter().map(PathBuf::from))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| PathBuf::from("git"))
}

/// Build a non-interactive `git` invocation in `workdir`. No token is injected: authentication is
/// left entirely to the user's Git configuration. `GIT_TERMINAL_PROMPT=0` makes a missing
/// credential fail fast instead of waiting on a terminal prompt nobody can answer; GUI credential
/// helpers (e.g. Git Credential Manager) still work.
pub(super) fn command(git: &Path, workdir: &Path, args: &[impl AsRef<OsStr>]) -> Command {
    let mut cmd = Command::new(git);
    cmd.args(args)
        .current_dir(workdir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

pub(super) struct Output {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    /// The most useful text to show for a failed command: stderr, else stdout, else the status.
    pub fn failure_message(&self) -> String {
        [self.stderr.trim(), self.stdout.trim()]
            .into_iter()
            .find(|text| !text.is_empty())
            .unwrap_or("git exited with an error and printed no message")
            .to_owned()
    }
}

pub(super) fn run(
    git: &Path,
    workdir: &Path,
    args: &[impl AsRef<OsStr>],
) -> Result<Output, String> {
    let output = command(git, workdir, args).output().map_err(|e| {
        format!(
            "Could not run system Git ({}): {e}. Install Git or set its path in Settings → GitHub.",
            git.display()
        )
    })?;
    Ok(Output {
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

pub(super) fn fetch_args() -> Vec<String> {
    vec!["fetch".into(), "origin".into()]
}

/// Same explicit refspec as the libgit2 push. `--porcelain` gives a locale-independent status
/// line so a non-fast-forward rejection can be told apart from auth or network failures.
pub(super) fn push_args(branch: &str) -> Vec<String> {
    vec![
        "push".into(),
        "--porcelain".into(),
        "origin".into(),
        format!("refs/heads/{branch}:refs/heads/{branch}"),
    ]
}

/// True when `git push --porcelain` reports a ref rejected because the remote has commits the
/// local branch lacks (`! <src>:<dst> [rejected] (non-fast-forward | fetch first)`).
pub(super) fn push_rejected_non_fast_forward(stdout: &str) -> bool {
    stdout.lines().any(|line| {
        line.starts_with('!')
            && (line.contains("(non-fast-forward)") || line.contains("(fetch first)"))
    })
}

/// `git --version` for the settings panel, e.g. `git version 2.45.1`.
pub fn version(configured: &str) -> Result<String, String> {
    let git = resolve_executable(configured);
    let output = run(&git, &std::env::temp_dir(), &["--version"])?;
    if output.success {
        Ok(output.stdout.trim().to_owned())
    } else {
        Err(output.failure_message())
    }
}
