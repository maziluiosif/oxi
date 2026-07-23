//! bash, grep, find, ls and directory walk helpers.

use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use glob::glob;

/// Shared stdout/stderr snapshots and the callback metadata for one pipe reader.
type LiveStreams = Arc<Mutex<(Vec<u8>, Vec<u8>)>>;
type LivePipe = (LiveStreams, usize, super::ToolOutputCallback);
use regex::Regex;
use serde_json::Value;
use walkdir::{DirEntry, WalkDir};

use super::file_ops::truncate_out;
use super::paths::{err, resolve_under_cwd};

const GREP_MAX_MATCHES: usize = 100;
const FIND_MAX: usize = 1000;
const LS_MAX: usize = 500;
/// Default timeout applied when the `bash` call omits its own `timeout` argument.
const BASH_DEFAULT_TIMEOUT_SECS: f64 = 15.0;

fn should_skip_search_entry(entry: &DirEntry) -> bool {
    let name = entry.file_name().to_string_lossy();
    matches!(name.as_ref(), ".git" | "target" | "node_modules")
}

fn is_reasonably_small_text_file(path: &Path) -> bool {
    const MAX_SEARCH_FILE_BYTES: u64 = 2 * 1024 * 1024;
    match fs::metadata(path) {
        Ok(meta) if meta.len() <= MAX_SEARCH_FILE_BYTES => {}
        _ => return false,
    }
    fs::read(path)
        .map(|bytes| !bytes.contains(&0))
        .unwrap_or(false)
}

/// Deny-list patterns checked against the lowercased, quote/backslash-stripped command
/// string. Each entry is a substring match — if the command contains it, the command is
/// refused.
///
/// This is a best-effort deterrent against obviously destructive one-liners, not a
/// sandbox: it does not understand shell quoting, variable expansion, command
/// substitution, or encoding (e.g. `$(echo c3Vkbw== | base64 -d)` sails through
/// untouched). The real backstop is [`crate::agent::approval::ApprovalGate`], which
/// shows the user the raw command and requires explicit approval before any `bash` call
/// runs — treat this list as reducing accidental/obvious damage, not as a security
/// boundary on its own.
const DENIED_BASH_PATTERNS: &[&str] = &[
    // Destructive filesystem operations
    "rm -rf /",
    "rm -fr /",
    "rm -rf --no-preserve-root",
    // Privilege escalation
    "sudo ",
    "doas ",
    "su -c",
    "su root",
    "pkexec ",
    // Disk/partition destruction
    "mkfs",
    "dd if=",
    "fdisk ",
    "parted ",
    "wipefs ",
    // System-level danger
    "shutdown ",
    "reboot",
    "init 0",
    "init 6",
    "systemctl poweroff",
    "systemctl reboot",
    "halt",
    // Fork bombs and resource exhaustion
    ":(){ :",
    ".(){.|.",
    // Network exfiltration / reverse shells
    "/dev/tcp/",
    "/dev/udp/",
    "nc -e",
    "ncat -e",
    "bash -i >& /dev/tcp",
    // Crontab manipulation
    "crontab -r",
    // Kernel module loading
    "insmod ",
    "modprobe ",
    "rmmod ",
    // iptables flush (could lock out SSH)
    "iptables -f",
    "iptables --flush",
    // chmod 777 on root is almost always a mistake
    "chmod -r 777 /",
    "chmod 777 /",
    // Overwriting critical files
    "> /dev/sda",
    ">/dev/sda",
    "> /etc/passwd",
    ">/etc/passwd",
    "> /etc/shadow",
    ">/etc/shadow",
];

/// Validate a bash command against the deny-list. Returns `Ok(())` if allowed, `Err(reason)`
/// if the command matches a denied pattern.
pub(crate) fn validate_bash_command(cmd: &str) -> Result<(), String> {
    let lowered = cmd.to_ascii_lowercase();
    // Strip quote/backslash characters commonly used to split a denied pattern across a
    // shell-syntax boundary (e.g. `s\udo`, `s"u"do`, `'sudo' cmd`) before matching, so
    // these trivial obfuscations still get caught.
    let unquoted: String = lowered
        .chars()
        .filter(|c| !matches!(c, '\\' | '\'' | '"'))
        .collect();
    // Collapse multiple spaces so tricks like `sudo  cmd` still match.
    let normalized: String = unquoted.split_whitespace().collect::<Vec<_>>().join(" ");
    for denied in DENIED_BASH_PATTERNS {
        if normalized.contains(denied) {
            return Err(format!("Refusing risky bash command containing: {denied}"));
        }
    }
    Ok(())
}

/// Remove credentials and agent-hostile variables from a child shell environment.
fn sanitize_bash_env(cmd: &mut Command) {
    const STRIP: &[&str] = &[
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
        "AZURE_CLIENT_SECRET",
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "OPENROUTER_API_KEY",
        "GITHUB_TOKEN",
        "GH_TOKEN",
        "NPM_TOKEN",
        "HF_TOKEN",
        "HUGGING_FACE_HUB_TOKEN",
        "SSH_AUTH_SOCK",
    ];
    for key in STRIP {
        cmd.env_remove(key);
    }
    // Keep the child rooted: do not inherit a custom CDPATH that could redirect relative paths.
    cmd.env_remove("CDPATH");
}

fn spawn_pipe_reader<R: Read + Send + 'static>(
    mut reader: R,
    live: Option<LivePipe>,
) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        // Keep draining after the display cap so the child can never block on a full pipe.
        const CAP: usize = super::MAX_TOOL_OUTPUT_CHARS * 2;
        let mut kept = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) if kept.len() < CAP => {
                    let take = n.min(CAP - kept.len());
                    kept.extend_from_slice(&buf[..take]);
                    if let Some((shared, stream, callback)) = &live {
                        let snapshot = {
                            let mut streams = shared.lock().unwrap_or_else(|e| e.into_inner());
                            let target = if *stream == 0 {
                                &mut streams.0
                            } else {
                                &mut streams.1
                            };
                            if target.len() < super::MAX_TOOL_OUTPUT_CHARS {
                                let live_take =
                                    take.min(super::MAX_TOOL_OUTPUT_CHARS - target.len());
                                target.extend_from_slice(&buf[..live_take]);
                            }
                            let mut text = String::from_utf8_lossy(&streams.0).into_owned();
                            if !streams.1.is_empty() {
                                if !text.is_empty() {
                                    text.push('\n');
                                }
                                text.push_str(&String::from_utf8_lossy(&streams.1));
                            }
                            text
                        };
                        callback(snapshot);
                    }
                }
                Ok(_) => {}
            }
        }
        kept
    })
}

fn terminate_child_tree(child: &mut Child) {
    #[cfg(unix)]
    {
        // The shell is placed in its own process group before exec. A negative pid targets the
        // complete group, preventing timeout-created grandchildren from continuing unnoticed.
        let pgid = -(child.id() as i32);
        // SAFETY: kill is called with a process-group id created for this child. Errors simply
        // mean the process already exited or permission was denied; Child::kill remains fallback.
        unsafe {
            libc::kill(pgid, libc::SIGKILL);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

pub(crate) fn tool_bash_streaming(
    cwd: &Path,
    args: &Value,
    max_secs: u32,
    on_output: Option<super::ToolOutputCallback>,
) -> Result<String, String> {
    let cmd = args
        .get("command")
        .and_then(|x| x.as_str())
        .ok_or_else(|| err("missing command"))?;
    validate_bash_command(cmd)?;
    let cap = (max_secs as f64).max(0.1);
    let timeout_s = args
        .get("timeout")
        .and_then(|x| x.as_f64().or_else(|| x.as_u64().map(|u| u as f64)))
        .unwrap_or(BASH_DEFAULT_TIMEOUT_SECS.min(cap))
        .clamp(0.1, cap);
    let start = Instant::now();
    // Use compile-time cfg blocks rather than cfg!(...), because cfg! only evaluates to a
    // boolean and still type-checks both branches. The Unix-only CommandExt/libc calls would
    // therefore break Windows builds even though that branch could never execute there.
    #[cfg(unix)]
    let mut child = {
        use std::os::unix::process::CommandExt;

        let mut c = Command::new("/bin/sh");
        c.arg("-c")
            .arg(cmd)
            .current_dir(cwd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        sanitize_bash_env(&mut c);
        // SAFETY: setpgid(0, 0) only changes the soon-to-exec child's process group and does
        // not access parent memory. It enables reliable whole-tree termination on timeout.
        unsafe {
            c.pre_exec(|| {
                if libc::setpgid(0, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        c.spawn().map_err(|e| e.to_string())?
    };
    #[cfg(windows)]
    let mut child = {
        use std::os::windows::process::CommandExt;

        let mut c = Command::new("cmd");
        c.args(["/C", cmd])
            .current_dir(cwd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        sanitize_bash_env(&mut c);
        // No console on a Windows GUI process: without CREATE_NO_WINDOW every shell
        // tool call would flash a `cmd` window. Keep the child headless.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
        c.spawn().map_err(|e| e.to_string())?
    };
    let live_streams = on_output
        .as_ref()
        .map(|_| Arc::new(Mutex::new((Vec::new(), Vec::new()))));
    let stdout_live = on_output
        .as_ref()
        .zip(live_streams.as_ref())
        .map(|(cb, shared)| (Arc::clone(shared), 0, Arc::clone(cb)));
    let stderr_live = on_output
        .as_ref()
        .zip(live_streams.as_ref())
        .map(|(cb, shared)| (Arc::clone(shared), 1, Arc::clone(cb)));
    let stdout_reader = child
        .stdout
        .take()
        .map(|pipe| spawn_pipe_reader(pipe, stdout_live));
    let stderr_reader = child
        .stderr
        .take()
        .map(|pipe| spawn_pipe_reader(pipe, stderr_live));
    let timeout = Duration::from_secs_f64(timeout_s);
    let (status, timed_out) = loop {
        if start.elapsed() > timeout {
            terminate_child_tree(&mut child);
            break (None, true);
        }
        match child.try_wait() {
            Ok(Some(s)) => break (Some(s), false),
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => {
                terminate_child_tree(&mut child);
                return Err(e.to_string());
            }
        }
    };
    let stdout = stdout_reader
        .and_then(|h| h.join().ok())
        .unwrap_or_default();
    let stderr = stderr_reader
        .and_then(|h| h.join().ok())
        .unwrap_or_default();
    let mut out = String::from_utf8_lossy(&stdout).into_owned();
    if !stderr.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&String::from_utf8_lossy(&stderr));
    }
    let prefix = if timed_out {
        format!("[timeout after {}s]\n", timeout.as_secs_f64())
    } else {
        format!(
            "exit code: {}\n",
            status.and_then(|s| s.code()).unwrap_or(-1)
        )
    };
    Ok(truncate_out(format!("{prefix}{out}")))
}

pub(crate) fn tool_grep(cwd: &Path, args: &Value) -> Result<String, String> {
    let pattern = args
        .get("pattern")
        .and_then(|x| x.as_str())
        .ok_or_else(|| err("missing pattern"))?;
    let literal = args
        .get("literal")
        .and_then(|x| x.as_bool())
        .unwrap_or(false);
    let re = if literal {
        Regex::new(&regex::escape(pattern)).map_err(|e| e.to_string())?
    } else {
        Regex::new(pattern).map_err(|e| e.to_string())?
    };
    let search_root = args
        .get("path")
        .and_then(|x| x.as_str())
        .map(|p| resolve_under_cwd(cwd, p))
        .transpose()?
        .unwrap_or_else(|| cwd.to_path_buf());
    let limit = args
        .get("limit")
        .and_then(|x| x.as_u64())
        .unwrap_or(GREP_MAX_MATCHES as u64) as usize;
    let mut matches = 0usize;
    let mut out = String::new();
    if search_root.is_file() {
        let txt = fs::read_to_string(&search_root).map_err(|e| e.to_string())?;
        for (i, line) in txt.lines().enumerate() {
            if re.is_match(line) {
                matches += 1;
                let rel = search_root.strip_prefix(cwd).unwrap_or(&search_root);
                out.push_str(&format!("{}:{}:{}\n", rel.display(), i + 1, line));
                if matches >= limit {
                    break;
                }
            }
        }
    } else {
        'outer: for entry in WalkDir::new(&search_root)
            .into_iter()
            .filter_entry(|e| !should_skip_search_entry(e))
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let p = entry.path();
            if !is_reasonably_small_text_file(p) {
                continue;
            }
            if let Ok(txt) = fs::read_to_string(p) {
                for (i, line) in txt.lines().enumerate() {
                    if re.is_match(line) {
                        matches += 1;
                        let rel = p.strip_prefix(cwd).unwrap_or(p);
                        out.push_str(&format!("{}:{}:{}\n", rel.display(), i + 1, line));
                        if matches >= limit {
                            break 'outer;
                        }
                    }
                }
            }
        }
    }
    if matches >= limit {
        out.push_str(&format!("\n[match limit {limit} reached]\n"));
    }
    Ok(truncate_out(if out.is_empty() {
        "No matches".to_string()
    } else {
        out
    }))
}

pub(crate) fn tool_find(cwd: &Path, args: &Value) -> Result<String, String> {
    let pattern = args
        .get("pattern")
        .and_then(|x| x.as_str())
        .ok_or_else(|| err("missing pattern"))?;
    let base = args
        .get("path")
        .and_then(|x| x.as_str())
        .map(|p| resolve_under_cwd(cwd, p))
        .transpose()?
        .unwrap_or_else(|| cwd.to_path_buf());
    let limit = args
        .get("limit")
        .and_then(|x| x.as_u64())
        .unwrap_or(FIND_MAX as u64) as usize;
    let glob_pat = format!("{}/{}", base.display(), pattern);
    let mut out = String::new();
    let mut n = 0usize;
    for entry in glob(&glob_pat).map_err(|e| e.to_string())? {
        let path = entry.map_err(|e| e.to_string())?;
        if path.components().any(|c| {
            matches!(
                c.as_os_str().to_string_lossy().as_ref(),
                ".git" | "target" | "node_modules"
            )
        }) {
            continue;
        }
        let rel = path.strip_prefix(cwd).unwrap_or(&path);
        out.push_str(&format!("{}\n", rel.display()));
        n += 1;
        if n >= limit {
            out.push_str(&format!("\n[limit {limit} reached]\n"));
            break;
        }
    }
    Ok(truncate_out(out))
}

/// Ranked keyword search over workspace text files for natural-language queries.
pub(crate) fn tool_codebase_search(cwd: &Path, args: &Value) -> Result<String, String> {
    let query = args
        .get("query")
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| err("missing query"))?;
    let base = args
        .get("path")
        .and_then(|x| x.as_str())
        .map(|p| resolve_under_cwd(cwd, p))
        .transpose()?
        .unwrap_or_else(|| cwd.to_path_buf());
    let limit = args
        .get("limit")
        .and_then(|x| x.as_u64())
        .unwrap_or(12)
        .clamp(1, 40) as usize;

    let terms: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
        .map(|t| t.trim().to_lowercase())
        .filter(|t| t.len() >= 2)
        .collect();
    if terms.is_empty() {
        return Err(err("query has no usable keywords"));
    }

    let mut hits: Vec<(i32, String)> = Vec::new();
    for entry in WalkDir::new(&base)
        .into_iter()
        .filter_entry(|e| !should_skip_search_entry(e))
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if !is_reasonably_small_text_file(path) {
            continue;
        }
        let Ok(contents) = fs::read_to_string(path) else {
            continue;
        };
        let lower = contents.to_lowercase();
        let path_lower = path.to_string_lossy().to_lowercase();
        let mut score = 0i32;
        for term in &terms {
            if path_lower.contains(term) {
                score += 8;
            }
            let count = lower.matches(term.as_str()).count() as i32;
            score += count.min(20);
        }
        if score <= 0 {
            continue;
        }
        let rel = path.strip_prefix(cwd).unwrap_or(path);
        let mut snippet = String::new();
        for (i, line) in contents.lines().enumerate() {
            let ll = line.to_lowercase();
            if terms.iter().any(|t| ll.contains(t)) {
                snippet = format!("L{}: {}", i + 1, line.trim());
                break;
            }
        }
        hits.push((
            score,
            if snippet.is_empty() {
                format!("{score}\t{}", rel.display())
            } else {
                format!("{score}\t{}\n  {snippet}", rel.display())
            },
        ));
    }
    hits.sort_by_key(|b| std::cmp::Reverse(b.0));
    hits.truncate(limit);
    if hits.is_empty() {
        return Ok(format!("No matches for {query:?}"));
    }
    let mut out = format!(
        "codebase_search results for {query:?} (top {}):\n\n",
        hits.len()
    );
    for (_, line) in hits {
        out.push_str(&line);
        out.push('\n');
    }
    Ok(truncate_out(out))
}

pub(crate) fn tool_git_status(cwd: &Path, _args: &Value) -> Result<String, String> {
    let repo = git2::Repository::discover(cwd).map_err(|e| format!("git status failed: {e}"))?;
    let branch = repo
        .head()
        .ok()
        .and_then(|h| h.shorthand().ok().map(str::to_owned))
        .unwrap_or_else(|| "HEAD (detached)".into());
    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(true).recurse_untracked_dirs(true);
    let statuses = repo.statuses(Some(&mut opts)).map_err(|e| e.to_string())?;
    let mut text = format!("## {branch}\n");
    for entry in statuses.iter() {
        let s = entry.status();
        let x = if s.contains(git2::Status::INDEX_NEW) {
            'A'
        } else if s.contains(git2::Status::INDEX_MODIFIED) {
            'M'
        } else if s.contains(git2::Status::INDEX_DELETED) {
            'D'
        } else if s.contains(git2::Status::INDEX_RENAMED) {
            'R'
        } else {
            ' '
        };
        let y = if s.contains(git2::Status::WT_NEW) {
            '?'
        } else if s.contains(git2::Status::WT_MODIFIED) {
            'M'
        } else if s.contains(git2::Status::WT_DELETED) {
            'D'
        } else if s.contains(git2::Status::WT_RENAMED) {
            'R'
        } else {
            ' '
        };
        text.push_str(&format!(
            "{x}{y} {}\n",
            entry.path().unwrap_or("(non-UTF-8 path)")
        ));
    }
    if statuses.is_empty() {
        text.push_str("(clean working tree)\n");
    }
    Ok(truncate_out(text))
}

pub(crate) fn tool_git_diff(cwd: &Path, args: &Value) -> Result<String, String> {
    let staged = args
        .get("staged")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let path = args.get("path").and_then(|v| v.as_str());
    if let Some(p) = path {
        let _ = resolve_under_cwd(cwd, p)?;
    }
    let repo = git2::Repository::discover(cwd).map_err(|e| format!("git diff failed: {e}"))?;
    let mut opts = git2::DiffOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .show_untracked_content(true);
    if let Some(p) = path {
        opts.pathspec(p);
    }
    let revision = args.get("revision").and_then(|v| v.as_str());
    let diff = if let Some(rev) = revision {
        let tree = repo
            .revparse_single(rev)
            .and_then(|o| o.peel_to_tree())
            .map_err(|e| e.to_string())?;
        repo.diff_tree_to_workdir_with_index(Some(&tree), Some(&mut opts))
    } else if staged {
        let tree = repo.head().ok().and_then(|h| h.peel_to_tree().ok());
        repo.diff_tree_to_index(tree.as_ref(), None, Some(&mut opts))
    } else {
        repo.diff_index_to_workdir(None, Some(&mut opts))
    }
    .map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    diff.print(git2::DiffFormat::Patch, |_d, _h, line| {
        if matches!(line.origin(), '+' | '-' | ' ') {
            bytes.push(line.origin() as u8);
        }
        bytes.extend_from_slice(line.content());
        true
    })
    .map_err(|e| e.to_string())?;
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if text.trim().is_empty() {
        text = "(no diff)".into();
    }
    Ok(truncate_out(text))
}

pub(crate) fn tool_ls(cwd: &Path, args: &Value) -> Result<String, String> {
    let base = args
        .get("path")
        .and_then(|x| x.as_str())
        .map(|p| resolve_under_cwd(cwd, p))
        .transpose()?
        .unwrap_or_else(|| cwd.to_path_buf());
    let limit = args
        .get("limit")
        .and_then(|x| x.as_u64())
        .unwrap_or(LS_MAX as u64) as usize;
    let rd = fs::read_dir(&base).map_err(|e| e.to_string())?;
    let mut names: Vec<String> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let total = names.len();
    names.truncate(limit);
    let mut out = names.join("\n");
    if total > limit {
        out.push_str(&format!("\n[limit {limit} reached]\n"));
    }
    if out.is_empty() {
        out = "[empty directory]".to_string();
    }
    Ok(truncate_out(out))
}
