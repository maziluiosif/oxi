//! One-shot completions through a subscription CLI the user already signed in to: Claude Code
//! (`claude -p`) or Codex (`codex exec`). Used for the short helpers (commit messages, chat
//! titles) when the configured helper model is the Claude or Codex agent, so they run on the
//! same login instead of needing a separate API provider.
//!
//! The run gets no tools that change anything: Claude Code with every tool turned off, Codex in
//! its read-only sandbox. It starts in an empty scratch folder, so the prompt is all the model
//! sees, and it is bounded by [`TIMEOUT`] and [`MAX_PROMPT_CHARS`].

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::settings::{LlmProviderKind, ProviderConfig};

/// Longest a helper run may take, CLI start-up included.
pub const TIMEOUT: Duration = Duration::from_secs(120);
/// Longer prompts (a huge diff) are cut to this many characters, with a note saying so.
pub const MAX_PROMPT_CHARS: usize = 120_000;
/// Claude model used when the helper is set to the agent's default.
const CLAUDE_HELPER_MODEL: &str = "haiku";
/// Tools a Claude Code without `--tools` must not use (its fallback flags).
const CLAUDE_WRITE_TOOLS: &str = "Bash,Edit,Write,MultiEdit,NotebookEdit,WebFetch,WebSearch,Task";

/// Whether `provider`'s agent has a CLI for one-shot helper completions.
pub fn supports(provider: LlmProviderKind) -> bool {
    matches!(
        provider,
        LlmProviderKind::ClaudeCodeAcp | LlmProviderKind::CodexAcp
    )
}

/// Complete `user` (with `system` instructions) on the CLI of `cfg`'s agent.
pub async fn complete(
    cfg: &ProviderConfig,
    system: &str,
    user: &str,
    max_chars: Option<usize>,
) -> Result<String, String> {
    let user = cap_prompt(user);
    let dir = crate::fsutil::unique_temp_path(&std::env::temp_dir(), "oxi-helper", "d");
    std::fs::create_dir_all(&dir).map_err(|e| format!("helper folder: {e}"))?;
    // The login shell's PATH; waiting for it must not hold a runtime worker.
    let path = crate::shell_env::path().await;
    let result = match cfg.provider {
        LlmProviderKind::ClaudeCodeAcp => {
            let program = find_claude(path.as_deref()).ok_or(
                "Claude Code's `claude` command was not found. Install Claude Code, or pick \
                 another model for commit messages in Settings.",
            )?;
            claude(&program, cfg, system, &user, &dir).await
        }
        LlmProviderKind::CodexAcp => {
            let program = find_program("codex", path.as_deref()).ok_or(
                "The `codex` command was not found. Install the Codex CLI, or pick another \
                 model for commit messages in Settings.",
            )?;
            codex(&program, cfg, system, &user, &dir).await
        }
        other => Err(format!("{} has no one-shot CLI", other.label())),
    };
    let _ = std::fs::remove_dir_all(&dir);
    let text = result?.trim().to_string();
    if text.is_empty() {
        return Err("The agent returned no text".into());
    }
    Ok(match max_chars {
        Some(cap) => text.chars().take(cap).collect(),
        None => text,
    })
}

fn cap_prompt(prompt: &str) -> String {
    match prompt.char_indices().nth(MAX_PROMPT_CHARS) {
        Some((cut, _)) => format!(
            "{}\n\n[The rest was left out to keep this short.]",
            &prompt[..cut]
        ),
        None => prompt.to_string(),
    }
}

/// `claude` on the user's `PATH` (as their login shell sets it), else the one bundled with
/// oxi's managed Claude adapter.
fn find_claude(shell_path: Option<&str>) -> Option<PathBuf> {
    find_program("claude", shell_path).or_else(crate::agent::acp::bundled_claude_cli)
}

/// `name` on `shell_path` (the login shell's `PATH`; oxi's own when `None`), or in a folder
/// installers commonly use.
fn find_program(name: &str, shell_path: Option<&str>) -> Option<PathBuf> {
    let path = shell_path
        .map(OsString::from)
        .or_else(|| std::env::var_os("PATH"))
        .unwrap_or_default();
    let home = dirs::home_dir();
    let extra = [".claude/local", ".local/bin", ".npm-global/bin", ".bun/bin"]
        .into_iter()
        .filter_map(|dir| home.as_ref().map(|h| h.join(dir)));
    let names: Vec<String> = if cfg!(windows) {
        ["exe", "cmd"]
            .iter()
            .map(|e| format!("{name}.{e}"))
            .collect()
    } else {
        vec![name.to_string()]
    };
    std::env::split_paths(&path)
        .chain(extra)
        .flat_map(|dir| names.iter().map(move |n| dir.join(n)))
        .find(|candidate| candidate.is_file())
}

async fn claude(
    program: &Path,
    cfg: &ProviderConfig,
    system: &str,
    prompt: &str,
    dir: &Path,
) -> Result<String, String> {
    let model = match cfg.model_id.trim() {
        "" | "default" => CLAUDE_HELPER_MODEL,
        model => model,
    };
    let common = |args: &mut Vec<OsString>| {
        args.extend(
            [
                "-p",
                "--output-format",
                "json",
                "--model",
                model,
                "--system-prompt",
                system,
            ]
            .map(OsString::from),
        );
    };
    let mut args = Vec::new();
    common(&mut args);
    // No tools at all; nothing to resume later.
    args.extend(["--tools", "", "--no-session-persistence"].map(OsString::from));
    let mut output = run(program, &args, prompt, dir, &cfg.acp_env()).await?;
    if !output.status.success()
        && String::from_utf8_lossy(&output.stderr).contains("unknown option")
    {
        // An older Claude Code: deny the tools that change anything instead.
        let mut args = Vec::new();
        common(&mut args);
        args.extend(["--disallowedTools", CLAUDE_WRITE_TOOLS].map(OsString::from));
        output = run(program, &args, prompt, dir, &cfg.acp_env()).await?;
    }
    parse_claude(&output.stdout).map_err(|e| {
        let stderr = tail(&output.stderr);
        if stderr.is_empty() {
            e
        } else {
            format!("{e}: {stderr}")
        }
    })
}

/// The answer in `claude -p --output-format json` output: one result object.
fn parse_claude(stdout: &[u8]) -> Result<String, String> {
    let text = String::from_utf8_lossy(stdout);
    let result: serde_json::Value = text
        .lines()
        .rev()
        .find_map(|line| serde_json::from_str(line.trim()).ok())
        .ok_or("Claude Code printed no result")?;
    let answer = result["result"].as_str().unwrap_or_default();
    if result["is_error"] == true || result["subtype"].as_str().is_some_and(|s| s != "success") {
        return Err(if answer.is_empty() {
            format!(
                "Claude Code stopped ({})",
                result["subtype"].as_str().unwrap_or("error")
            )
        } else {
            answer.to_string()
        });
    }
    Ok(answer.to_string())
}

async fn codex(
    program: &Path,
    cfg: &ProviderConfig,
    system: &str,
    prompt: &str,
    dir: &Path,
) -> Result<String, String> {
    let answer = dir.join("answer.txt");
    let mut args: Vec<OsString> = [
        "exec",
        "--skip-git-repo-check",
        "--sandbox",
        "read-only",
        "--color",
        "never",
        "-c",
        "model_reasoning_effort=\"low\"",
        "--output-last-message",
    ]
    .map(OsString::from)
    .to_vec();
    args.push(answer.clone().into_os_string());
    let model = cfg.model_id.trim();
    if !model.is_empty() && model != "default" {
        args.extend(["--model", model].map(OsString::from));
    }
    // The prompt comes on stdin.
    args.push("-".into());
    let input = format!("{system}\n\n{prompt}");
    let output = run(program, &args, &input, dir, &cfg.acp_env()).await?;
    if !output.status.success() {
        return Err(format!("codex exec failed: {}", tail(&output.stderr)));
    }
    std::fs::read_to_string(&answer).map_err(|_| "Codex wrote no answer".to_string())
}

async fn run(
    program: &Path,
    args: &[OsString],
    stdin: &str,
    dir: &Path,
    env: &[(String, String)],
) -> Result<std::process::Output, String> {
    let mut cmd = Command::new(program);
    cmd.args(args).current_dir(dir);
    crate::shell_env::apply_async(&mut cmd).await;
    // oxi may have been started from a Claude Code terminal; this run is not nested in it.
    cmd.env_remove("CLAUDECODE");
    for (key, value) in env {
        cmd.env(key, value);
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let name = program
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Could not run {name}: {e}"))?;
    if let Some(mut input) = child.stdin.take() {
        // A CLI that stops before reading (an unknown option) closes the pipe: its exit status
        // and error output say what happened, not this write.
        match input.write_all(stdin.as_bytes()).await {
            Err(e) if e.kind() != std::io::ErrorKind::BrokenPipe => {
                return Err(format!("{name}: {e}"));
            }
            _ => {}
        }
    }
    tokio::time::timeout(TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| format!("{name} did not answer within {} s", TIMEOUT.as_secs()))?
        .map_err(|e| format!("{name}: {e}"))
}

/// The last lines of a process's error output, for the message shown to the user.
fn tail(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(3)..].join(" ")
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A stand-in CLI: a shell script, made executable, in a fresh folder.
    fn fake(name: &str, script: &str) -> (PathBuf, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let dir = crate::fsutil::unique_temp_path(&std::env::temp_dir(), "oxi-fake-cli", "d");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        (dir, path)
    }

    fn config(provider: LlmProviderKind, model: &str) -> ProviderConfig {
        let mut cfg = crate::settings::AppSettings::default()
            .provider(provider)
            .clone();
        cfg.model_id = model.into();
        cfg
    }

    fn block_on<T>(f: impl std::future::Future<Output = T>) -> T {
        crate::runtime::runtime().unwrap().block_on(f)
    }

    #[test]
    fn claude_runs_without_tools_and_reads_the_json_result() {
        // Echo the arguments and stdin back as the answer, as `claude -p` would print it.
        let (dir, program) = fake(
            "claude",
            r#"input=$(cat); printf '{"type":"result","subtype":"success","is_error":false,"result":"%s | %s"}\n' "$*" "$input""#,
        );
        let cfg = config(LlmProviderKind::ClaudeCodeAcp, "default");
        let answer = block_on(claude(&program, &cfg, "Be brief.", "the diff", &dir)).unwrap();
        assert!(
            answer.contains("-p --output-format json --model haiku"),
            "{answer}"
        );
        assert!(
            answer.contains("--tools  --no-session-persistence"),
            "{answer}"
        );
        assert!(answer.ends_with("| the diff"), "{answer}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_older_claude_without_tools_flag_falls_back_and_errors_are_reported() {
        let (dir, program) = fake(
            "claude",
            r#"case "$*" in *--tools*) echo "error: unknown option '--tools'" >&2; exit 1;; esac
cat >/dev/null; printf '{"type":"result","subtype":"success","is_error":false,"result":"%s"}\n' "$*""#,
        );
        let cfg = config(LlmProviderKind::ClaudeCodeAcp, "sonnet");
        let answer = block_on(claude(&program, &cfg, "s", "p", &dir)).unwrap();
        assert!(answer.contains("--disallowedTools Bash,Edit"), "{answer}");
        assert!(answer.contains("--model sonnet"), "{answer}");

        assert_eq!(
            parse_claude(br#"{"type":"result","subtype":"error_max_turns","is_error":true}"#),
            Err("Claude Code stopped (error_max_turns)".into())
        );
        assert!(parse_claude(b"").is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn codex_runs_read_only_and_returns_the_last_message() {
        let (dir, program) = fake(
            "codex",
            r#"out=""; prev=""; for a in "$@"; do [ "$prev" = "--output-last-message" ] && out="$a"; prev="$a"; done
input=$(cat); printf 'Fix: %s\n[%s]' "$input" "$*" > "$out""#,
        );
        let cfg = config(LlmProviderKind::CodexAcp, "");
        let answer = block_on(codex(
            &program,
            &cfg,
            "Write a commit message.",
            "diff",
            &dir,
        ))
        .unwrap();
        assert!(
            answer.starts_with("Fix: Write a commit message.\n\ndiff"),
            "{answer}"
        );
        assert!(answer.contains("--sandbox read-only"), "{answer}");
        assert!(!answer.contains("--model"), "{answer}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn long_prompts_are_cut() {
        let long = "x".repeat(MAX_PROMPT_CHARS + 10);
        let capped = cap_prompt(&long);
        assert!(capped.len() < long.len() + 100);
        assert!(capped.ends_with("left out to keep this short.]"));
    }
}
