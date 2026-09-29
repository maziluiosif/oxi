//! `diagnostics`: run the project's own type-checker / linter and hand back compact
//! `file:line:col: message` lines, so the agent can verify an edit without composing build
//! commands (and without paging through full build logs).
//!
//! Checkers are picked from marker files in the target directory. Commands run through the same
//! shell path as `bash` (timeouts, whole-tree kill, env sanitizing), so the approval policy for
//! shell commands applies to this tool as well.

use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Value, json};

use super::paths::{err, resolve_under_cwd};

/// Location lines kept per checker; the rest are counted.
const MAX_LOCATION_LINES: usize = 150;
/// Raw output tail shown when a checker failed without any parseable location lines.
const RAW_TAIL_LINES: usize = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Checker {
    Cargo,
    Tsc,
    Go,
    Python,
}

impl Checker {
    fn id(self) -> &'static str {
        match self {
            Checker::Cargo => "cargo",
            Checker::Tsc => "tsc",
            Checker::Go => "go",
            Checker::Python => "python",
        }
    }

    fn from_id(id: &str) -> Option<Self> {
        [Checker::Cargo, Checker::Tsc, Checker::Go, Checker::Python]
            .into_iter()
            .find(|c| c.id() == id)
    }

    /// Commands to try in order; the next one runs only if the previous was not found.
    fn commands(self) -> &'static [&'static str] {
        match self {
            Checker::Cargo => &["cargo check --all-targets --message-format=short --quiet"],
            Checker::Tsc => &["npx --no-install tsc --noEmit --pretty false"],
            Checker::Go => &["go vet ./..."],
            Checker::Python => {
                if cfg!(windows) {
                    &[
                        "ruff check --output-format=concise .",
                        "python -m compileall -q .",
                    ]
                } else {
                    &[
                        "ruff check --output-format=concise .",
                        "python3 -m compileall -q .",
                    ]
                }
            }
        }
    }
}

fn detect(dir: &Path) -> Vec<Checker> {
    let has = |name: &str| dir.join(name).exists();
    let mut out = Vec::new();
    if has("Cargo.toml") {
        out.push(Checker::Cargo);
    }
    if has("tsconfig.json") {
        out.push(Checker::Tsc);
    }
    if has("go.mod") {
        out.push(Checker::Go);
    }
    if has("pyproject.toml") || has("setup.py") || has("requirements.txt") || has("ruff.toml") {
        out.push(Checker::Python);
    }
    out
}

pub(super) fn tool_diagnostics(
    cwd: &Path,
    args: &Value,
    timeout_cap: u32,
) -> Result<String, String> {
    let dir = match args.get("path").and_then(|p| p.as_str()) {
        Some(p) if !p.trim().is_empty() => resolve_under_cwd(cwd, p)?,
        _ => cwd.to_path_buf(),
    };
    if !dir.is_dir() {
        return Err(err("path must be a directory inside the workspace"));
    }
    let checkers = match args.get("checker").and_then(|c| c.as_str()) {
        Some(id) => vec![
            Checker::from_id(id)
                .ok_or_else(|| err("checker must be one of: cargo, tsc, go, python"))?,
        ],
        None => detect(&dir),
    };
    if checkers.is_empty() {
        return Ok(
            "No supported project found here (looked for Cargo.toml, tsconfig.json, go.mod, \
             pyproject.toml/setup.py/requirements.txt). Run the project's checker with bash instead."
                .into(),
        );
    }
    let sections: Vec<String> = checkers
        .into_iter()
        .map(|checker| run_checker(checker, &dir, timeout_cap))
        .collect();
    Ok(sections.join("\n\n"))
}

fn run_checker(checker: Checker, dir: &Path, timeout_cap: u32) -> String {
    let mut last = String::new();
    for command in checker.commands() {
        let raw = match super::shell_search::tool_bash_streaming(
            dir,
            &json!({ "command": command, "timeout": timeout_cap }),
            timeout_cap,
            None,
        ) {
            Ok(out) => out,
            Err(e) => return format!("{}: could not run `{command}`: {e}", checker.id()),
        };
        let (status, body) = split_status(&raw);
        if is_not_found(status, body) {
            last = format!("{}: `{command}` is not available", checker.id());
            continue;
        }
        return summarize(command, status, body);
    }
    last
}

/// Separate the `exit code: N` / `[timeout …]` header the bash tool puts first.
fn split_status(raw: &str) -> (Option<i32>, &str) {
    let (first, rest) = raw.split_once('\n').unwrap_or((raw, ""));
    let code = first
        .strip_prefix("exit code: ")
        .and_then(|c| c.trim().parse().ok());
    if first.starts_with("[timeout") {
        return (None, raw);
    }
    (code, rest)
}

fn is_not_found(status: Option<i32>, body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    matches!(status, Some(127) | Some(9009))
        || (status != Some(0)
            && (lower.contains("command not found")
                || lower.contains("is not recognized as an internal or external command")
                || lower.contains("could not determine executable to run")
                || lower.contains("no module named compileall")))
}

static LOCATION_RE: LazyLock<Regex> = LazyLock::new(|| {
    // `path:line:col: msg` (cargo short, go, ruff) or `path(line,col): msg` (tsc).
    Regex::new(r"^\s*(?:vet: )?(?P<file>[^\s:()][^:()]*?)(?::(?P<l1>\d+):(?P<c1>\d+):|\((?P<l2>\d+),(?P<c2>\d+)\):)\s*(?P<msg>.+)$")
        .expect("valid diagnostics regex")
});

fn summarize(command: &str, status: Option<i32>, body: &str) -> String {
    let mut seen = std::collections::HashSet::new();
    let locations: Vec<String> = body
        .lines()
        .filter_map(|line| {
            let caps = LOCATION_RE.captures(line)?;
            let line_no = caps.name("l1").or(caps.name("l2"))?.as_str();
            let col = caps.name("c1").or(caps.name("c2"))?.as_str();
            let entry = format!(
                "{}:{line_no}:{col}: {}",
                caps["file"].trim().replace('\\', "/"),
                caps["msg"].trim()
            );
            seen.insert(entry.clone()).then_some(entry)
        })
        .collect();
    let errors = locations
        .iter()
        .filter(|l| l.contains(": error") || l.contains(" error "))
        .count();
    let warnings = locations.iter().filter(|l| l.contains(": warning")).count();

    let mut out = match status {
        None => format!("`{command}` timed out"),
        Some(code) if locations.is_empty() && code == 0 => {
            return format!("`{command}`: no errors or warnings.");
        }
        Some(code) => format!(
            "`{command}` (exit {code}): {} location(s){}",
            locations.len(),
            if errors + warnings > 0 {
                format!(", {errors} error(s), {warnings} warning(s)")
            } else {
                String::new()
            }
        ),
    };
    for line in locations.iter().take(MAX_LOCATION_LINES) {
        out.push('\n');
        out.push_str(line);
    }
    if locations.len() > MAX_LOCATION_LINES {
        out.push_str(&format!(
            "\n… {} more",
            locations.len() - MAX_LOCATION_LINES
        ));
    }
    if locations.is_empty() && status != Some(0) {
        let lines: Vec<&str> = body.lines().collect();
        let tail = &lines[lines.len().saturating_sub(RAW_TAIL_LINES)..];
        out.push_str("\nNo file locations recognized; output tail:\n");
        out.push_str(&tail.join("\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cargo_tsc_go_and_ruff_lines() {
        let body = "src/main.rs:3:13: error[E0308]: mismatched types\n\
                    src/lib.rs:2:9: warning: unused variable: `x`\n\
                    error: could not compile `demo` (bin \"demo\") due to 1 previous error\n\
                    src\\app.ts(4,7): error TS2322: Type 'string' is not assignable to type 'number'.\n\
                    ./main.go:5:2: undefined: foo\n\
                    a.py:1:8: F401 [*] `os` imported but unused\n\
                    src/main.rs:3:13: error[E0308]: mismatched types\n";
        let out = summarize("check", Some(101), body);
        assert!(
            out.contains("5 location(s), 2 error(s), 1 warning(s)"),
            "{out}"
        );
        assert!(out.contains("src/main.rs:3:13: error[E0308]: mismatched types"));
        assert!(out.contains("src/app.ts:4:7: error TS2322"));
        assert!(out.contains("./main.go:5:2: undefined: foo"));
        assert!(out.contains("a.py:1:8: F401"));
    }

    #[test]
    fn clean_run_and_unparsed_failure() {
        assert_eq!(
            summarize("go vet ./...", Some(0), ""),
            "`go vet ./...`: no errors or warnings."
        );
        let out = summarize("python3 -m compileall -q .", Some(1), "SyntaxError: bad\n");
        assert!(out.contains("output tail:\nSyntaxError: bad"), "{out}");
    }

    #[test]
    fn detects_project_kinds() {
        let dir = std::env::temp_dir().join(format!("oxi-diag-detect-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]").unwrap();
        std::fs::write(dir.join("tsconfig.json"), "{}").unwrap();
        assert_eq!(detect(&dir), vec![Checker::Cargo, Checker::Tsc]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn missing_tools_are_detected() {
        assert!(is_not_found(Some(127), "sh: ruff: command not found"));
        assert!(is_not_found(
            Some(1),
            "'ruff' is not recognized as an internal or external command"
        ));
        assert!(!is_not_found(Some(1), "src/a.rs:1:1: error: nope"));
    }
}
