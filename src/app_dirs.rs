//! Folders oxi keeps its own files in.
//!
//! Under `cargo test` every lookup lands in a scratch folder owned by the test process instead,
//! so running the tests can never touch the user's settings, chats, caches or work trees.

use std::path::PathBuf;

/// `<platform config dir>/oxi`: settings, themes, the usage ledger and the logs.
pub fn config_dir() -> PathBuf {
    root(Kind::Config).join("oxi")
}

/// `<platform data dir>/oxi`: downloaded models, ACP adapters and caches, work trees.
pub fn data_dir() -> PathBuf {
    root(Kind::Data).join("oxi")
}

/// `~/.config/oxi` on every platform: where chats are stored by default (see
/// `session_store::paths::agent_dir`).
pub fn home_config_dir() -> PathBuf {
    root(Kind::HomeConfig).join("oxi")
}

/// The user's home folder: `$HOME` when set, else the platform's profile folder. Windows rarely
/// sets `HOME`; falling back to the working directory there scattered chats into whatever folder
/// oxi was started from.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
}

#[derive(Clone, Copy)]
enum Kind {
    Config,
    Data,
    HomeConfig,
}

#[cfg(not(test))]
fn root(kind: Kind) -> PathBuf {
    let dir = match kind {
        Kind::Config => dirs::config_dir(),
        Kind::Data => dirs::data_dir(),
        Kind::HomeConfig => home_dir().map(|home| home.join(".config")),
    };
    dir.unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(test)]
fn root(kind: Kind) -> PathBuf {
    let name = match kind {
        Kind::Config => "config",
        Kind::Data => "data",
        Kind::HomeConfig => "home-config",
    };
    test_root().join(name)
}

/// Scratch folder standing in for the user's folders while this test process runs.
#[cfg(test)]
pub fn test_root() -> PathBuf {
    static ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    ROOT.get_or_init(|| std::env::temp_dir().join(format!("oxi-test-dirs-{}", std::process::id())))
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tests_never_resolve_to_the_real_folders() {
        for dir in [config_dir(), data_dir(), home_config_dir()] {
            assert!(dir.starts_with(test_root()), "{}", dir.display());
        }
        assert_ne!(config_dir(), data_dir());
    }
}
