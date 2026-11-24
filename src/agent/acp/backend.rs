//! Backend protocol selection and command-line parsing for external coding agents.
//!
//! Claude Code (`claude`) and Codex (`codex app-server`) speak their official structured
//! protocols directly over stdio; all other external agents (Antigravity, Cursor, Copilot,
//! OpenCode, Muse, or any custom ACP adapter including an explicit `npx … claude-agent-acp`
//! or `codex-acp` override) use the Agent Client Protocol (ACP).

use std::path::{Path, PathBuf};

/// Transport protocol spoken with the external agent subprocess.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BackendKind {
    /// Generic Agent Client Protocol (JSON-RPC 2.0 `initialize`/`session/*`).
    Acp,
    /// OpenAI Codex CLI official app-server (`codex app-server --stdio`).
    CodexNative,
    /// Anthropic Claude Code CLI official `stream-json` + `control_request` protocol.
    ClaudeNative,
}

impl BackendKind {
    /// Classify a configured launch command into its wire protocol.
    pub(super) fn detect(command_line: &str) -> Self {
        let words = split_shell_words(command_line);
        let Some(first) = words.first() else {
            return Self::Acp;
        };
        let args = &words[1..];
        let stem = binary_stem(first);
        if stem == "claude"
            && !args
                .iter()
                .any(|a| a.eq_ignore_ascii_case("acp") || a == "--acp")
        {
            return Self::ClaudeNative;
        }
        if stem == "codex"
            && (args.is_empty() || args.iter().any(|a| a == "app-server"))
            && !args
                .iter()
                .any(|a| a.eq_ignore_ascii_case("acp") || a == "--acp")
        {
            return Self::CodexNative;
        }
        Self::Acp
    }
}

/// Lowercased executable stem (without directory or `.exe`/`.cmd`/`.bat` extension).
pub(super) fn binary_stem(program: &str) -> String {
    let file_name = program
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(program)
        .to_ascii_lowercase();
    for ext in [".exe", ".cmd", ".bat", ".com"] {
        if let Some(stripped) = file_name.strip_suffix(ext) {
            return stripped.to_owned();
        }
    }
    file_name
}

/// Minimal POSIX/Windows-friendly shell-word splitter preserving single/double quotes and
/// backslash escapes outside single quotes.
pub(super) fn split_shell_words(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    let mut token_started = false;

    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if escaped {
            current.push(ch);
            escaped = false;
            token_started = true;
            continue;
        }
        if in_single {
            if ch == '\'' {
                in_single = false;
            } else {
                current.push(ch);
            }
            token_started = true;
            continue;
        }
        if in_double {
            match ch {
                '"' => in_double = false,
                '\\' if matches!(chars.peek(), Some('"' | '\\')) => escaped = true,
                _ => current.push(ch),
            }
            token_started = true;
            continue;
        }
        match ch {
            '\'' => {
                in_single = true;
                token_started = true;
            }
            '"' => {
                in_double = true;
                token_started = true;
            }
            '\\' if !cfg!(windows) => {
                escaped = true;
                token_started = true;
            }
            c if c.is_whitespace() => {
                if token_started {
                    out.push(std::mem::take(&mut current));
                    token_started = false;
                }
            }
            c => {
                current.push(c);
                token_started = true;
            }
        }
    }
    if token_started {
        out.push(current);
    }
    out
}

/// Resolve a native CLI executable on the login-shell `PATH` plus common install directories.
pub(super) async fn resolve_cli_binary(
    program: &str,
    extra_home_dirs: &[&str],
    fallback: Option<PathBuf>,
) -> Option<PathBuf> {
    let program = program.trim();
    if program.is_empty() {
        return None;
    }
    if program.contains('/') || program.contains('\\') {
        let p = PathBuf::from(program);
        return p.is_file().then_some(p);
    }
    let path_env = super::install::shell_path()
        .await
        .or_else(|| std::env::var("PATH").ok())
        .unwrap_or_default();
    let exts: &[&str] = if cfg!(windows) {
        &["", ".exe", ".cmd", ".bat", ".com"]
    } else {
        &[""]
    };
    let mut dirs: Vec<PathBuf> = std::env::split_paths(&path_env).collect();
    if let Some(home) = dirs::home_dir() {
        for rel in extra_home_dirs {
            dirs.push(home.join(rel));
        }
    }
    #[cfg(unix)]
    for sys in ["/opt/homebrew/bin", "/usr/local/bin"] {
        dirs.push(PathBuf::from(sys));
    }
    for dir in dirs {
        for ext in exts {
            let candidate = dir.join(format!("{program}{ext}"));
            if is_runnable_file(&candidate) {
                return Some(candidate);
            }
        }
    }
    fallback.filter(|p| is_runnable_file(p))
}

fn is_runnable_file(path: &Path) -> bool {
    let Ok(meta) = path.metadata() else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_native_and_acp_backends() {
        assert_eq!(BackendKind::detect("claude"), BackendKind::ClaudeNative);
        assert_eq!(
            BackendKind::detect("/Users/u/.local/bin/claude --chrome"),
            BackendKind::ClaudeNative
        );
        assert_eq!(
            BackendKind::detect(r#""C:\Program Files\Claude\claude.exe""#),
            BackendKind::ClaudeNative
        );
        assert_eq!(
            BackendKind::detect("codex app-server"),
            BackendKind::CodexNative
        );
        assert_eq!(
            BackendKind::detect("/opt/homebrew/bin/codex app-server -c model_verbosity=low"),
            BackendKind::CodexNative
        );
        assert_eq!(BackendKind::detect("codex"), BackendKind::CodexNative);

        // Explicit ACP commands stay on ACP.
        assert_eq!(
            BackendKind::detect("npx -y @agentclientprotocol/claude-agent-acp"),
            BackendKind::Acp
        );
        assert_eq!(
            BackendKind::detect("npx -y @agentclientprotocol/codex-acp"),
            BackendKind::Acp
        );
        assert_eq!(BackendKind::detect("codex-acp"), BackendKind::Acp);
        assert_eq!(BackendKind::detect("claude-agent-acp"), BackendKind::Acp);
        assert_eq!(BackendKind::detect("agy acp"), BackendKind::Acp);
        assert_eq!(BackendKind::detect("agent acp"), BackendKind::Acp);
        assert_eq!(BackendKind::detect("codex acp"), BackendKind::Acp);
    }

    #[test]
    fn splits_quoted_and_unquoted_shell_words() {
        assert_eq!(
            split_shell_words(r#""/path with spaces/claude" --flag 'a b'"#),
            vec!["/path with spaces/claude", "--flag", "a b"]
        );
        assert_eq!(
            split_shell_words("  codex   app-server "),
            vec!["codex", "app-server"]
        );
    }
}
