//! bash, grep, find, ls and directory walk helpers.

use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use glob::glob;
use regex::Regex;
use serde_json::Value;
use walkdir::{DirEntry, WalkDir};

use super::file_ops::truncate_out;
use super::paths::{err, resolve_under_cwd};

const GREP_MAX_MATCHES: usize = 100;
const FIND_MAX: usize = 1000;
const LS_MAX: usize = 500;
const BASH_MAX_SECONDS: f64 = 30.0;

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

/// Deny-list patterns checked against the lowercased command string.
/// Each entry is a substring match — if the command contains it, the command is refused.
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
    // Collapse multiple spaces so tricks like `sudo  cmd` still match.
    let normalized: String = lowered.split_whitespace().collect::<Vec<_>>().join(" ");
    for denied in DENIED_BASH_PATTERNS {
        if normalized.contains(denied) {
            return Err(format!("Refusing risky bash command containing: {denied}"));
        }
    }
    reject_risky_rm(&normalized)?;
    reject_remote_script_pipe(&normalized)?;
    reject_recursive_world_writable(&normalized)?;
    Ok(())
}

fn shell_segments(normalized: &str) -> impl Iterator<Item = &str> {
    normalized
        .split([';', '\n'])
        .flat_map(|segment| segment.split("&&"))
        .flat_map(|segment| segment.split("||"))
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
}

fn reject_risky_rm(normalized: &str) -> Result<(), String> {
    for segment in shell_segments(normalized) {
        let words: Vec<&str> = segment.split_whitespace().collect();
        if words.first() != Some(&"rm") {
            continue;
        }

        let flags = words
            .iter()
            .skip(1)
            .take_while(|word| word.starts_with('-') && *word != &"--")
            .flat_map(|word| word.trim_start_matches('-').chars())
            .collect::<Vec<_>>();
        let recursive = flags.iter().any(|flag| matches!(flag, 'r' | 'R'));
        let force = flags.contains(&'f');
        if !recursive || !force {
            continue;
        }

        let targets = words
            .iter()
            .skip(1)
            .filter(|word| !word.starts_with('-') && **word != "--");
        for target in targets {
            if is_risky_rm_target(target) {
                return Err(format!("Refusing risky recursive delete target: {target}"));
            }
        }
    }
    Ok(())
}

fn is_risky_rm_target(target: &str) -> bool {
    matches!(target, "." | ".." | "/" | "~")
        || target.starts_with('/')
        || target.starts_with("~/")
        || target.contains("../")
        || target.ends_with("/..")
}

fn reject_remote_script_pipe(normalized: &str) -> Result<(), String> {
    let downloads = ["curl ", "wget "];
    let interpreters = ["| sh", "| bash", "| zsh", "| python", "| perl", "| ruby"];
    if downloads.iter().any(|cmd| normalized.contains(cmd))
        && interpreters.iter().any(|pipe| normalized.contains(pipe))
    {
        return Err("Refusing remote download piped to an interpreter".to_string());
    }
    Ok(())
}

fn reject_recursive_world_writable(normalized: &str) -> Result<(), String> {
    for segment in shell_segments(normalized) {
        let words: Vec<&str> = segment.split_whitespace().collect();
        if words.first() == Some(&"chmod")
            && words.contains(&"777")
            && words
                .iter()
                .any(|word| word.starts_with('-') && word.contains('r'))
        {
            return Err("Refusing recursive chmod 777".to_string());
        }
    }
    Ok(())
}

pub(crate) fn tool_bash(cwd: &Path, args: &Value) -> Result<String, String> {
    let cmd = args
        .get("command")
        .and_then(|x| x.as_str())
        .ok_or_else(|| err("missing command"))?;
    validate_bash_command(cmd)?;
    let timeout_s = args
        .get("timeout")
        .and_then(|x| x.as_f64().or_else(|| x.as_u64().map(|u| u as f64)))
        .unwrap_or(15.0)
        .clamp(0.1, BASH_MAX_SECONDS);
    let start = Instant::now();
    let mut child = if cfg!(unix) {
        Command::new("/bin/sh")
            .arg("-c")
            .arg(cmd)
            .current_dir(cwd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?
    } else {
        Command::new("cmd")
            .args(["/C", cmd])
            .current_dir(cwd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?
    };
    let timeout = Some(Duration::from_secs_f64(timeout_s));
    let status = loop {
        if let Some(t) = timeout {
            if start.elapsed() > t {
                let _ = child.kill();
                return Ok(truncate_out(format!(
                    "[timeout after {}s]\n",
                    t.as_secs_f64()
                )));
            }
        }
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(e.to_string()),
        }
    };
    let mut out = String::new();
    if let Some(mut o) = child.stdout {
        let _ = o.read_to_string(&mut out);
    }
    if let Some(mut e) = child.stderr {
        let mut err = String::new();
        let _ = e.read_to_string(&mut err);
        if !err.is_empty() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&err);
        }
    }
    Ok(truncate_out(format!(
        "exit code: {}\n{}",
        status.code().unwrap_or(-1),
        out
    )))
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
