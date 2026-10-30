//! The GitHub pull request of a branch, through the GitHub CLI (`gh`) and its login: oxi's own
//! token only has the Contents permission, which can't open pull requests. Reading runs
//! `gh pr view` for the checked-out branch; opening one pushes the branch and runs
//! `gh pr create`. Both block, so callers run them off the UI thread.

use std::path::Path;
use std::process::Command;

use serde_json::Value;

/// Fields `gh pr view` / `gh pr list` are asked for.
const FIELDS: &str = "number,title,state,isDraft,url,baseRefName,headRefName,statusCheckRollup";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Checks {
    pub passed: usize,
    pub failed: usize,
    pub pending: usize,
    /// Names of the failed checks.
    pub failing: Vec<String>,
}

impl Checks {
    pub fn total(&self) -> usize {
        self.passed + self.failed + self.pending
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrState {
    Open,
    Draft,
    Merged,
    Closed,
}

impl PrState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Open => "Open",
            Self::Draft => "Draft",
            Self::Merged => "Merged",
            Self::Closed => "Closed",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub state: PrState,
    pub base: String,
    pub head: String,
    pub checks: Checks,
}

/// `gh` with the user's login-shell environment, in `dir`.
fn gh(dir: &Path) -> Command {
    let mut cmd = Command::new("gh");
    crate::shell_env::apply(&mut cmd);
    cmd.current_dir(dir)
        .env("GH_PROMPT_DISABLED", "1")
        .env("NO_COLOR", "1")
        .stdin(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd
}

/// Run `cmd`, returning its stdout, or its stderr (first line) as the error.
fn run(mut cmd: Command, what: &str) -> Result<String, String> {
    let out = cmd.output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "The GitHub CLI (gh) is not installed. Install it and run `gh auth login`.".to_owned()
        } else {
            format!("{what}: {e}")
        }
    })?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let message = stderr
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("failed")
        .to_owned();
    Err(message)
}

/// Read the counts of a `statusCheckRollup` array (check runs and commit statuses).
fn parse_checks(rollup: &Value) -> Checks {
    let mut checks = Checks::default();
    for item in rollup.as_array().into_iter().flatten() {
        let name = item["name"]
            .as_str()
            .or_else(|| item["context"].as_str())
            .unwrap_or("check")
            .to_owned();
        // Check runs have a status and, once completed, a conclusion; statuses have a state.
        let outcome = match item["status"].as_str() {
            Some(status) if status != "COMPLETED" => "PENDING",
            Some(_) => item["conclusion"].as_str().unwrap_or("PENDING"),
            None => item["state"].as_str().unwrap_or("PENDING"),
        };
        match outcome {
            "SUCCESS" | "NEUTRAL" | "SKIPPED" => checks.passed += 1,
            "PENDING" | "EXPECTED" | "QUEUED" | "IN_PROGRESS" | "" => checks.pending += 1,
            _ => {
                checks.failed += 1;
                checks.failing.push(name);
            }
        }
    }
    checks
}

/// Read one pull request object of `gh`'s JSON output.
pub fn parse(value: &Value) -> Option<PullRequest> {
    let state = match (value["state"].as_str()?, value["isDraft"].as_bool()) {
        ("MERGED", _) => PrState::Merged,
        ("CLOSED", _) => PrState::Closed,
        (_, Some(true)) => PrState::Draft,
        _ => PrState::Open,
    };
    Some(PullRequest {
        number: value["number"].as_u64()?,
        title: value["title"].as_str().unwrap_or_default().to_owned(),
        url: value["url"].as_str().unwrap_or_default().to_owned(),
        state,
        base: value["baseRefName"].as_str().unwrap_or_default().to_owned(),
        head: value["headRefName"].as_str().unwrap_or_default().to_owned(),
        checks: parse_checks(&value["statusCheckRollup"]),
    })
}

/// The pull request of the branch checked out in `dir`, `None` when it has none.
pub fn view(dir: &Path) -> Result<Option<PullRequest>, String> {
    let mut cmd = gh(dir);
    cmd.args(["pr", "view", "--json", FIELDS]);
    match run(cmd, "gh pr view") {
        Ok(out) => {
            let value: Value =
                serde_json::from_str(&out).map_err(|e| format!("gh pr view: {e}"))?;
            Ok(parse(&value))
        }
        Err(e) if e.contains("no pull requests found") => Ok(None),
        Err(e) => Err(e),
    }
}

/// What a new pull request is opened with.
#[derive(Clone, Debug, Default)]
pub struct NewPullRequest {
    pub title: String,
    pub body: String,
    /// Target branch; empty: the repository's default branch.
    pub base: String,
    pub draft: bool,
}

/// Push the checked-out branch of `dir` (setting its upstream) and open a pull request for it.
pub fn create(dir: &Path, new: &NewPullRequest) -> Result<PullRequest, String> {
    let mut push = Command::new("git");
    crate::shell_env::apply(&mut push);
    push.current_dir(dir)
        .args(["push", "--set-upstream", "origin", "HEAD"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null());
    run(push, "git push").map_err(|e| format!("Could not push the branch: {e}"))?;
    let mut cmd = gh(dir);
    cmd.args(["pr", "create", "--title", &new.title, "--body", &new.body]);
    if !new.base.trim().is_empty() {
        cmd.args(["--base", new.base.trim()]);
    }
    if new.draft {
        cmd.arg("--draft");
    }
    run(cmd, "gh pr create")?;
    view(dir)?.ok_or_else(|| "The pull request was opened but could not be read back.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_pull_request_reads_its_state_and_checks() {
        let pr = parse(&json!({
            "number": 285, "title": "Delegation", "url": "https://github.com/o/r/pull/285",
            "state": "OPEN", "isDraft": false, "baseRefName": "dev", "headRefName": "feat",
            "statusCheckRollup": [
                {"__typename": "CheckRun", "name": "ci", "status": "COMPLETED", "conclusion": "SUCCESS"},
                {"__typename": "CheckRun", "name": "audit", "status": "COMPLETED", "conclusion": "FAILURE"},
                {"__typename": "CheckRun", "name": "mac", "status": "IN_PROGRESS", "conclusion": ""},
                {"__typename": "StatusContext", "context": "deploy", "state": "PENDING"},
                {"__typename": "StatusContext", "context": "lint", "state": "ERROR"},
                {"__typename": "CheckRun", "name": "skip", "status": "COMPLETED", "conclusion": "SKIPPED"}
            ]
        }))
        .unwrap();
        assert_eq!(pr.number, 285);
        assert_eq!(pr.state, PrState::Open);
        assert_eq!((pr.base.as_str(), pr.head.as_str()), ("dev", "feat"));
        assert_eq!(
            pr.checks,
            Checks {
                passed: 2,
                failed: 2,
                pending: 2,
                failing: vec!["audit".into(), "lint".into()],
            }
        );
        let draft = parse(&json!({"number": 1, "state": "OPEN", "isDraft": true})).unwrap();
        assert_eq!(draft.state, PrState::Draft);
        assert_eq!(draft.checks.total(), 0);
        let merged = parse(&json!({"number": 2, "state": "MERGED", "isDraft": true})).unwrap();
        assert_eq!(merged.state, PrState::Merged);
        assert!(parse(&json!({"state": "OPEN"})).is_none());
    }
}
