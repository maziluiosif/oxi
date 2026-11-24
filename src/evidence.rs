//! Evidence mode: an agent proves its change works instead of only saying so.
//!
//! Native provider loops and oxi's ACP MCP server offer the same evidence tools. Captures (screenshots
//! of a URL or an app window, screen recordings, files a command writes) and checks (commands
//! such as the test suite) are made by oxi itself, so what they show is not the agent's word. The
//! agent ends with `evidence_submit`: a summary and claims, each citing the evidence behind it.
//! A claim is attested only when everything it cites was captured or run by oxi (or, for a file
//! the agent supplied, written during the turn) and every cited check passed.
//!
//! The MCP server runs as its own process. Both sides meet in a folder per agent connection
//! (`<data dir>/evidence/<id>`, handed to the server in [`DIR_ENV`]): the app writes the current
//! turn's marker before each prompt, the server writes items and the submission under the turn's
//! folder, and the app collects them into an [`EvidenceReport`] when the turn ends.
//! Native runs pass their storage and execution limits explicitly, without process-wide state.

pub mod capture;
pub mod exec;
mod policy;
pub mod tools;
pub use policy::{Mode, Policy, WorkspaceState};

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Environment variable carrying the connection's evidence folder to the MCP server.
pub const DIR_ENV: &str = "OXI_EVIDENCE_DIR";

/// Follow-up prompts oxi sends when an evidence-mode turn ends with evidence missing.
pub const MAX_FOLLOWUPS: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Before,
    After,
    Check,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ItemKind {
    Image,
    Video,
    Check,
}

/// Who produced an item: oxi (captured or ran it) or the agent (a file it supplied).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    Oxi,
    Agent,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EvidenceItem {
    #[serde(default)]
    pub workspace: Option<WorkspaceState>,
    pub id: String,
    pub kind: ItemKind,
    pub phase: Phase,
    pub label: String,
    pub origin: Origin,
    /// The capture (png, gif, mov, mp4, webm), for images and videos.
    #[serde(default)]
    pub file: Option<String>,
    /// Animated GIF (or poster frame) standing in for a video in the chat.
    #[serde(default)]
    pub preview: Option<String>,
    /// An agent-supplied file was written during the turn.
    #[serde(default)]
    pub fresh: bool,
    /// How it was made, e.g. "Chrome headless · http://localhost:3000".
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub timed_out: bool,
    /// Tail of the command's output.
    #[serde(default)]
    pub output: String,
    #[serde(default)]
    pub duration_ms: u64,
}

impl EvidenceItem {
    pub fn is_visual(&self) -> bool {
        matches!(self.kind, ItemKind::Image | ItemKind::Video)
    }

    pub fn passed(&self) -> bool {
        self.kind == ItemKind::Check && self.exit_code == Some(0) && !self.timed_out
    }

    pub fn failed(&self) -> bool {
        self.kind == ItemKind::Check && !self.passed()
    }

    /// oxi can vouch for it: a check that passed, or a capture oxi made (or a file the agent
    /// wrote during the turn) that is still on disk.
    pub fn attested(&self) -> bool {
        match self.kind {
            ItemKind::Check => self.passed(),
            ItemKind::Image | ItemKind::Video => {
                (self.origin == Origin::Oxi || self.fresh)
                    && self.file.as_deref().is_some_and(|f| Path::new(f).is_file())
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    pub text: String,
    #[serde(default)]
    pub evidence: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaimStatus {
    /// Everything it cites is evidence oxi vouches for.
    Attested,
    /// Cites nothing, unknown ids, or only files oxi cannot vouch for.
    Declared,
    /// A check it cites failed.
    Contradicted,
}

/// What one turn proved. Persisted with the assistant message.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EvidenceReport {
    /// The workspace or a cited check/capture changed after the proof was produced.
    #[serde(default)]
    pub stale: bool,
    #[serde(default)]
    pub workspace: Option<WorkspaceState>,
    pub turn: String,
    /// The turn ran in evidence mode (so missing evidence is worth flagging).
    #[serde(default)]
    pub requested: bool,
    #[serde(default)]
    pub submitted: bool,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub items: Vec<EvidenceItem>,
    #[serde(default)]
    pub claims: Vec<Claim>,
}

/// The before and after captures sharing a label.
pub struct VisualPair<'a> {
    pub label: &'a str,
    pub before: Option<&'a EvidenceItem>,
    pub after: Option<&'a EvidenceItem>,
}

fn label_key(label: &str) -> String {
    label.trim().to_lowercase()
}

impl EvidenceReport {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty() && self.claims.is_empty() && self.summary.is_empty()
    }

    pub fn item(&self, id: &str) -> Option<&EvidenceItem> {
        self.items.iter().find(|item| item.id == id.trim())
    }

    pub fn claim_status(&self, claim: &Claim) -> ClaimStatus {
        let cited: Vec<Option<&EvidenceItem>> =
            claim.evidence.iter().map(|id| self.item(id)).collect();
        if cited.iter().flatten().any(|item| item.failed()) {
            return ClaimStatus::Contradicted;
        }
        if self.stale {
            return ClaimStatus::Declared;
        }
        if !cited.is_empty() && cited.iter().all(|item| item.is_some_and(|i| i.attested())) {
            ClaimStatus::Attested
        } else {
            ClaimStatus::Declared
        }
    }

    /// Captures grouped by label, in the order labels first appear. The latest capture of a
    /// phase wins (an agent may retake one).
    pub fn pairs(&self) -> Vec<VisualPair<'_>> {
        let mut pairs: Vec<(String, VisualPair<'_>)> = Vec::new();
        for item in self.items.iter().filter(|i| i.is_visual()) {
            let key = label_key(&item.label);
            let index = match pairs.iter().position(|(k, _)| *k == key) {
                Some(index) => index,
                None => {
                    pairs.push((
                        key,
                        VisualPair {
                            label: &item.label,
                            before: None,
                            after: None,
                        },
                    ));
                    pairs.len() - 1
                }
            };
            let pair = &mut pairs[index].1;
            match item.phase {
                Phase::Before => pair.before = Some(item),
                // A capture without a phase reads as the result.
                Phase::After | Phase::Check => pair.after = Some(item),
            }
        }
        pairs.into_iter().map(|(_, pair)| pair).collect()
    }

    pub fn checks(&self) -> impl Iterator<Item = &EvidenceItem> {
        self.items.iter().filter(|i| i.kind == ItemKind::Check)
    }

    /// What an evidence-mode turn still owes, phrased for the agent. Empty when complete.
    pub fn problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        if self.stale {
            problems.push("The workspace changed after this evidence was produced. Verify the new state and submit fresh evidence.".into());
        }
        if !self.submitted {
            problems.push(
                "You have not called `evidence_submit`. Finish with it: a short summary and the claims you make, each citing evidence ids."
                    .to_string(),
            );
        }
        for pair in self.pairs() {
            match (pair.before, pair.after) {
                (None, Some(_)) => problems.push(format!(
                    "\"{}\" has an after capture but no before. Call `evidence_base_checkout` for a copy of the code as it was before this turn, run it there, and capture it with phase \"before\" and the same label.",
                    pair.label
                )),
                (Some(_), None) => problems.push(format!(
                    "\"{}\" has a before capture but no after. Capture the changed version with phase \"after\" and the same label.",
                    pair.label
                )),
                _ => {}
            }
        }
        for claim in &self.claims {
            match self.claim_status(claim) {
                ClaimStatus::Attested => {}
                ClaimStatus::Declared => problems.push(format!(
                    "The claim \"{}\" has no evidence oxi can vouch for. Capture or run something that shows it (evidence_capture / evidence_run) and cite those ids, or drop the claim.",
                    claim.text
                )),
                ClaimStatus::Contradicted => problems.push(format!(
                    "The claim \"{}\" cites a check that failed. Fix the problem and run the check again, or correct the claim.",
                    claim.text
                )),
            }
        }
        problems
    }

    /// Counts of attested, declared and contradicted claims.
    pub fn tally(&self) -> (usize, usize, usize) {
        self.claims.iter().fold((0, 0, 0), |(a, d, c), claim| {
            match self.claim_status(claim) {
                ClaimStatus::Attested => (a + 1, d, c),
                ClaimStatus::Declared => (a, d + 1, c),
                ClaimStatus::Contradicted => (a, d, c + 1),
            }
        })
    }
}

/// Prepended to the user's message in evidence mode.
pub const PROMPT_PREFIX: &str = "[Evidence mode] Verify changes when proof is needed. For an explanation or other task that makes no changes, do not create captures or run checks just because this mode is enabled. Reuse existing evidence only when it still covers the current workspace state; changes since that proof need fresh verification. If the task changes the workspace, or the user explicitly asks for proof, use oxi's evidence tools:\n\
1. Before editing anything, capture each user-visible thing you will change with `evidence_capture` (phase \"before\"): a page of the running app (url), an app window (window), the screen, or an image/video a command writes (command + output). Use `seconds` for a short recording when motion or interaction matters, with `during` to drive it.\n\
2. Make the change.\n\
3. Capture the same things again with phase \"after\" and the same labels.\n\
4. Run the checks that matter (tests, build, a curl) with `evidence_run`; oxi runs them and records the exit code.\n\
5. Finish with `evidence_submit`: a short summary and your claims, each citing the evidence ids that show it. A claim without evidence oxi made itself counts as unproven.\n\
If you already edited before capturing \"before\", `evidence_base_checkout` gives you a copy of the code as it was when this turn started.";

/// The follow-up sent when an evidence-mode turn ended with `problems`.
pub fn followup_prompt(problems: &[String]) -> String {
    let mut text = String::from(
        "[oxi evidence check] Your evidence is incomplete. Before finishing, address:\n",
    );
    for problem in problems {
        text.push_str("- ");
        text.push_str(problem);
        text.push('\n');
    }
    text.push_str("Then call `evidence_submit` again.");
    text
}

// ── Store ───────────────────────────────────────────────────────────────────────────────────

/// The turn the MCP server files evidence under, written by the app before each prompt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnMarker {
    #[serde(default)]
    pub baseline: Option<WorkspaceState>,
    pub turn: String,
    pub started_ms: u64,
    pub cwd: String,
    /// Repository root and snapshot tree of the work tree when the turn started.
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub base_tree: Option<String>,
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

/// A fresh folder for one agent connection.
pub fn new_conn_dir() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    crate::app_dirs::data_dir()
        .join("evidence")
        .join(format!("{nanos:x}-{:x}", std::process::id()))
}

fn marker_path(conn_dir: &Path) -> PathBuf {
    conn_dir.join("turn.json")
}

pub fn turn_dir(conn_dir: &Path, turn: &str) -> PathBuf {
    conn_dir.join(turn)
}

/// Start filing evidence under a new turn.
pub fn begin_turn(conn_dir: &Path, marker: &TurnMarker) -> Result<(), String> {
    std::fs::create_dir_all(turn_dir(conn_dir, &marker.turn)).map_err(|e| e.to_string())?;
    let json = serde_json::to_vec_pretty(marker).map_err(|e| e.to_string())?;
    crate::fsutil::write_atomic(&marker_path(conn_dir), &json)
}

pub fn current_turn(conn_dir: &Path) -> Option<TurnMarker> {
    let bytes = std::fs::read(marker_path(conn_dir)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn items_dir(turn_dir: &Path) -> PathBuf {
    turn_dir.join("items")
}

/// Reserve the next free item id in a turn: `e1`, `e2`, ...
///
/// The oxi MCP server runs tool calls in parallel, and a capture takes a while between picking
/// its id and saving its item, so the id is claimed up front by creating its (still empty) item
/// file; [`save_item`] later replaces it, and [`collect`] skips one never filled.
pub fn next_id(turn_dir: &Path) -> String {
    let dir = items_dir(turn_dir);
    let _ = std::fs::create_dir_all(&dir);
    let count = std::fs::read_dir(&dir)
        .map(|entries| entries.flatten().count())
        .unwrap_or(0);
    let mut n = count + 1;
    loop {
        let path = dir.join(format!("e{n}.json"));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => return format!("e{n}"),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => n += 1,
            // Unwritable directory: saving the item fails later with the real error.
            Err(_) => return format!("e{n}"),
        }
    }
}

pub fn save_item(turn_dir: &Path, item: &EvidenceItem) -> Result<(), String> {
    let dir = items_dir(turn_dir);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut item = item.clone();
    item.workspace = item
        .cwd
        .as_deref()
        .and_then(|cwd| WorkspaceState::capture(Path::new(cwd)).ok());
    let json = serde_json::to_vec_pretty(&item).map_err(|e| e.to_string())?;
    crate::fsutil::write_atomic(&dir.join(format!("{}.json", item.id)), &json)
}

#[derive(Serialize, Deserialize)]
struct Submission {
    #[serde(default)]
    workspace: Option<WorkspaceState>,
    summary: String,
    claims: Vec<Claim>,
}

pub fn save_submission(turn_dir: &Path, summary: &str, claims: &[Claim]) -> Result<(), String> {
    let workspace = turn_dir
        .parent()
        .and_then(current_turn)
        .and_then(|m| WorkspaceState::capture(Path::new(&m.cwd)).ok());
    let json = serde_json::to_vec_pretty(&Submission {
        workspace,
        summary: summary.to_string(),
        claims: claims.to_vec(),
    })
    .map_err(|e| e.to_string())?;
    crate::fsutil::write_atomic(&turn_dir.join("submit.json"), &json)
}

/// Everything filed under `turn`, items in id order.
pub fn collect(conn_dir: &Path, turn: &str, requested: bool) -> EvidenceReport {
    let dir = turn_dir(conn_dir, turn);
    let mut items: Vec<EvidenceItem> = std::fs::read_dir(items_dir(&dir))
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| std::fs::read(entry.path()).ok())
                .filter_map(|bytes| serde_json::from_slice(&bytes).ok())
                .collect()
        })
        .unwrap_or_default();
    items.sort_by_key(|item: &EvidenceItem| {
        item.id
            .trim_start_matches('e')
            .parse::<u64>()
            .unwrap_or(u64::MAX)
    });
    let submission: Option<Submission> = std::fs::read(dir.join("submit.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    let submitted = submission.is_some();
    let (summary, claims, workspace) = submission
        .map(|s| (s.summary, s.claims, s.workspace))
        .unwrap_or_default();
    EvidenceReport {
        stale: false,
        workspace,
        turn: turn.to_string(),
        requested,
        submitted,
        summary,
        items,
        claims,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(id: &str, exit: i32) -> EvidenceItem {
        EvidenceItem {
            workspace: None,
            id: id.into(),
            kind: ItemKind::Check,
            phase: Phase::Check,
            label: "tests".into(),
            origin: Origin::Oxi,
            file: None,
            preview: None,
            fresh: false,
            source: String::new(),
            command: Some("cargo test".into()),
            cwd: None,
            exit_code: Some(exit),
            timed_out: false,
            output: String::new(),
            duration_ms: 0,
        }
    }

    fn capture(id: &str, label: &str, phase: Phase, file: &Path, origin: Origin) -> EvidenceItem {
        EvidenceItem {
            workspace: None,
            id: id.into(),
            kind: ItemKind::Image,
            phase,
            label: label.into(),
            origin,
            file: Some(file.to_string_lossy().into_owned()),
            preview: None,
            fresh: false,
            source: String::new(),
            command: None,
            cwd: None,
            exit_code: None,
            timed_out: false,
            output: String::new(),
            duration_ms: 0,
        }
    }

    #[test]
    fn claims_are_attested_only_by_evidence_oxi_vouches_for() {
        let dir = tempfile_dir("claims");
        let png = dir.join("a.png");
        std::fs::write(&png, b"png").unwrap();
        let report = EvidenceReport {
            stale: false,
            workspace: None,
            items: vec![
                check("e1", 0),
                check("e2", 1),
                capture("e3", "Home", Phase::After, &png, Origin::Oxi),
                capture("e4", "Home", Phase::Before, &png, Origin::Agent),
            ],
            ..Default::default()
        };
        let status = |ids: &[&str]| {
            report.claim_status(&Claim {
                text: "x".into(),
                evidence: ids.iter().map(|s| s.to_string()).collect(),
            })
        };
        assert_eq!(status(&["e1", "e3"]), ClaimStatus::Attested);
        assert_eq!(status(&["e1", "e2"]), ClaimStatus::Contradicted);
        assert_eq!(status(&[]), ClaimStatus::Declared);
        assert_eq!(status(&["e9"]), ClaimStatus::Declared);
        // A file the agent supplied that predates the turn proves nothing.
        assert_eq!(status(&["e4"]), ClaimStatus::Declared);
    }

    #[test]
    fn problems_name_missing_submission_pairs_and_unproven_claims() {
        let dir = tempfile_dir("problems");
        let png = dir.join("a.png");
        std::fs::write(&png, b"png").unwrap();
        let mut report = EvidenceReport {
            stale: false,
            workspace: None,
            items: vec![capture("e1", "Login page", Phase::After, &png, Origin::Oxi)],
            claims: vec![Claim {
                text: "It works".into(),
                evidence: vec![],
            }],
            ..Default::default()
        };
        let problems = report.problems();
        assert_eq!(problems.len(), 3, "{problems:#?}");
        assert!(problems[1].contains("evidence_base_checkout"));
        report.submitted = true;
        report.items.push(capture(
            "e2",
            " login PAGE",
            Phase::Before,
            &png,
            Origin::Oxi,
        ));
        report.claims[0].evidence = vec!["e1".into(), "e2".into()];
        assert!(report.problems().is_empty());
        let pairs = report.pairs();
        assert_eq!(pairs.len(), 1);
        assert!(pairs[0].before.is_some() && pairs[0].after.is_some());
    }

    #[test]
    fn parallel_calls_never_share_an_item_id() {
        let dir = tempfile_dir("parallel_ids").join("t1");
        let ids: Vec<String> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..16).map(|_| scope.spawn(|| next_id(&dir))).collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let unique: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len(), "duplicate ids: {ids:?}");
        // Reserved but never saved items are not reported.
        assert!(collect(dir.parent().unwrap(), "t1", false).items.is_empty());
    }

    #[test]
    fn store_round_trips_a_turn() {
        let conn = tempfile_dir("store");
        let work = conn.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let marker = TurnMarker {
            baseline: None,
            turn: "t1".into(),
            started_ms: 1,
            cwd: work.to_string_lossy().into_owned(),
            repo: None,
            base_tree: None,
        };
        begin_turn(&conn, &marker).unwrap();
        assert_eq!(current_turn(&conn), Some(marker));
        let dir = turn_dir(&conn, "t1");
        for n in 1..=10 {
            let id = next_id(&dir);
            assert_eq!(id, format!("e{n}"));
            save_item(&dir, &check(&id, 0)).unwrap();
        }
        save_submission(
            &dir,
            "Done",
            &[Claim {
                text: "Tests pass".into(),
                evidence: vec!["e1".into()],
            }],
        )
        .unwrap();
        let report = collect(&conn, "t1", true);
        assert!(report.submitted && report.requested);
        assert_eq!(report.items.len(), 10);
        assert_eq!(report.items[9].id, "e10");
        assert_eq!(report.tally(), (1, 0, 0));
    }

    pub(super) fn tempfile_dir(name: &str) -> PathBuf {
        let dir = crate::app_dirs::data_dir()
            .join("evidence-tests")
            .join(format!("{name}-{}", now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
