//! Local installation checks. Call off the UI thread: resolving the login-shell PATH may wait
//! for shell startup. Detection never launches an agent, installs a package, or changes auth.

use std::path::Path;

use super::{LlmProviderKind, ProviderConfig};

pub(super) fn acp_client_installed(config: &ProviderConfig) -> bool {
    let path = crate::shell_env::vars()
        .get("PATH")
        .cloned()
        .or_else(|| std::env::var("PATH").ok())
        .unwrap_or_default();
    acp_client_installed_on_path(config, &path)
}

fn acp_client_installed_on_path(config: &ProviderConfig, path: &str) -> bool {
    if !config.acp_command.trim().is_empty()
        && config.acp_command.trim() != config.default_acp_command()
    {
        if let Some(program) = command_program(&config.acp_command) {
            // A package runner is not evidence that this provider's client is installed.
            let name = Path::new(program)
                .file_stem()
                .and_then(|name| name.to_str());
            if !matches!(name, Some("npx" | "npm" | "uvx")) {
                return executable_on_path(program, path);
            }
        } else {
            return false;
        }
    }
    use LlmProviderKind::*;
    let programs: &[&str] = match config.provider {
        ClaudeCodeAcp => &["claude"],
        CodexAcp => &["codex"],
        CursorAcp => &["agent"],
        MuseCodeAcp => &["muse"],
        CopilotAcp => &["copilot"],
        OpenCodeAcp => &["opencode"],
        AntigravityAcp => &["antigravity", "agy", "agy_acp_server.par", "agy_acp_server"],
        _ => return false,
    };
    programs
        .iter()
        .any(|program| executable_on_path(program, path))
}

fn command_program(command: &str) -> Option<&str> {
    let command = command.trim();
    if let Some(quote) = command.chars().next().filter(|c| *c == '\'' || *c == '"') {
        command[1..].split_once(quote).map(|(program, _)| program)
    } else {
        command.split_whitespace().next()
    }
}

fn executable_on_path(program: &str, path: &str) -> bool {
    if program.is_empty() {
        return false;
    }
    let program_path = Path::new(program);
    if program_path.is_absolute() {
        return is_executable(program_path);
    }
    // A relative path depends on the session's cwd, which is not the app's cwd.
    if program.contains('/') || program.contains('\\') {
        return false;
    }
    std::env::split_paths(path)
        .filter(|dir| dir.is_absolute())
        .any(|dir| {
            if cfg!(windows) {
                ["", ".exe", ".cmd", ".bat", ".com"]
                    .iter()
                    .any(|ext| is_executable(&dir.join(format!("{program}{ext}"))))
            } else {
                is_executable(&dir.join(program))
            }
        })
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "oxi-provider-detection-{}-{}",
                std::process::id(),
                rand::random::<u64>()
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn executable(dir: &Path, name: &str) {
        let file = dir.join(if cfg!(windows) {
            format!("{name}.cmd")
        } else {
            name.into()
        });
        std::fs::write(&file, "").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    #[test]
    fn detects_only_installed_clients_not_npx_or_cached_adapters() {
        let dir = TempDir::new();
        for program in ["claude", "codex", "npx", "muse-acp", "claude-agent-acp"] {
            executable(dir.path(), program);
        }
        let path = dir.path().to_str().unwrap();
        for kind in LlmProviderKind::ALL.into_iter().filter(|k| k.is_acp()) {
            assert_eq!(
                acp_client_installed_on_path(&ProviderConfig::new(kind), path),
                matches!(
                    kind,
                    LlmProviderKind::ClaudeCodeAcp | LlmProviderKind::CodexAcp
                ),
                "{kind:?}"
            );
        }
    }

    #[test]
    fn custom_commands_support_quoted_absolute_paths_and_missing_binaries() {
        let dir = TempDir::new();
        executable(dir.path(), "custom agent");
        let mut config = ProviderConfig::new(LlmProviderKind::CodexAcp);
        let binary = dir.path().join(if cfg!(windows) {
            "custom agent.cmd"
        } else {
            "custom agent"
        });
        config.acp_command = format!("'{}' --acp", binary.display());
        assert!(acp_client_installed_on_path(&config, ""));
        std::fs::remove_file(binary).unwrap();
        assert!(!acp_client_installed_on_path(&config, ""));
        config.acp_command = "missing-agent --acp".into();
        assert!(!acp_client_installed_on_path(&config, ""));
    }

    #[test]
    fn package_runner_alone_does_not_make_custom_npx_commands_available() {
        let dir = TempDir::new();
        executable(dir.path(), "npx");
        let path = dir.path().to_str().unwrap();
        let mut config = ProviderConfig::new(LlmProviderKind::ClaudeCodeAcp);
        config.acp_command = "npx -y @zed-industries/claude-code-acp".into();
        assert!(!acp_client_installed_on_path(&config, path));
        executable(dir.path(), "claude");
        assert!(acp_client_installed_on_path(&config, path));
    }

    #[cfg(unix)]
    #[test]
    fn ignores_non_executable_files_and_directories() {
        let dir = TempDir::new();
        std::fs::write(dir.path().join("claude"), "").unwrap();
        std::fs::create_dir(dir.path().join("codex")).unwrap();
        for kind in [LlmProviderKind::ClaudeCodeAcp, LlmProviderKind::CodexAcp] {
            assert!(!acp_client_installed_on_path(
                &ProviderConfig::new(kind),
                dir.path().to_str().unwrap()
            ));
        }
    }
}
