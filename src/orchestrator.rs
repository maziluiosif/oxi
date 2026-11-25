//! The Orchestrator pseudo-provider: the user talks to one lead agent (Settings > Orchestrator),
//! which hands work to worker agents on the user's other providers, follows them, and merges
//! their changes into the main workspace.
//!
//! It is built on [`crate::delegation`]: the lead's tools drop requests in its chat's mailbox,
//! the app runs each worker as a chat of its own and keeps its status there. On top of that:
//!
//! - Roles. A lead chat's agent session key ends in `#lead`, a worker's in `#worker`; the
//!   mailbox name carries the role, so oxi's MCP server gives the lead these tools and workers
//!   none (no recursion).
//! - Placement, decided by oxi rather than the model: a worker that only reads runs in the
//!   workspace; a single editing worker edits the workspace too; as soon as two edit at once,
//!   each new one gets a git work tree of its own, branched from a snapshot of the workspace
//!   (uncommitted and untracked files included, see [`crate::git::handoff`]).
//! - Merging: a work tree's changes since that snapshot are applied to the main workspace as
//!   uncommitted changes (three-way, with conflict markers when the user or another worker
//!   changed the same lines), ready for review in the Changes panel.
//! - Follow-ups go to the same worker chat, so it keeps its context; reports of workers started
//!   together come back to the lead as one message once all of them ended.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::delegation::{Status, TaskState};
use crate::settings::LlmProviderKind;

pub fn is_worker_report(text: &str) -> bool {
    text.starts_with("[Worker finished]\n") || text.starts_with("[Workers finished]\n")
}

pub const SOLO_NOTICE: &str = "[oxi] This workspace runs the Orchestrator lead solo. Worker orchestration is unavailable over SSH or headless connections.";

pub const TEAM: &str = "team";
pub const SPAWN: &str = "spawn_workers";
pub const STATUS: &str = "worker_status";
pub const MESSAGE: &str = "message_worker";
pub const DIFF: &str = "worker_diff";
pub const MERGE: &str = "merge_worker";
pub const STOP: &str = "stop_worker";
pub const NAMES: [&str; 7] = [TEAM, SPAWN, STATUS, MESSAGE, DIFF, MERGE, STOP];

/// How long a tool waits for the app to take a request.
const ACCEPT_TIMEOUT: Duration = Duration::from_secs(20);
/// Longest prompt or follow-up handed to a worker.
const MAX_PROMPT: usize = 100_000;
/// Workers per `spawn_workers` call.
const MAX_BATCH: usize = 8;
/// A worker's report in `worker_status` beyond this is cut (the whole one is in its chat).
const MAX_REPORT: usize = 12_000;

/// A chat's part in an orchestration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Lead,
    Worker,
}

impl Role {
    pub fn slug(self) -> &'static str {
        match self {
            Role::Lead => "lead",
            Role::Worker => "worker",
        }
    }

    /// The agent session key of a chat with this role, from its plain key.
    pub fn session_key(self, base: &str) -> String {
        format!("{base}#{}", self.slug())
    }

    pub fn of_session_key(key: &str) -> Option<Role> {
        if key.ends_with("#lead") {
            Some(Role::Lead)
        } else if key.ends_with("#worker") {
            Some(Role::Worker)
        } else {
            None
        }
    }

    /// The role a mailbox (see [`crate::delegation::dir_for`]) belongs to.
    pub fn of_dir(dir: &Path) -> Option<Role> {
        let name = dir.file_name()?.to_string_lossy();
        if name.ends_with("-lead") {
            Some(Role::Lead)
        } else if name.ends_with("-worker") {
            Some(Role::Worker)
        } else {
            None
        }
    }
}

/// Whether a worker may change files.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    #[default]
    Edit,
    ReadOnly,
}

/// One worker of a [`crate::delegation::Request::Spawn`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerRequest {
    pub task: String,
    /// Provider slug.
    pub provider: String,
    #[serde(default)]
    pub model: String,
    pub title: String,
    pub prompt: String,
    #[serde(default)]
    pub access: Access,
    /// Give it a work tree even when it is the only one editing (e.g. to compare attempts).
    #[serde(default)]
    pub isolate: bool,
    /// Original agent label if rerouted because that subscription was out of quota.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rerouted_from: Option<String>,
}

/// What a worker's [`Status`] adds to a delegated task's.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerInfo {
    /// Workers started by the same `spawn_workers` call.
    pub batch: String,
    pub access: Access,
    /// The main workspace.
    pub root: String,
    /// Where it works: the workspace, or its work tree.
    pub cwd: String,
    /// Commit holding the workspace as it was when the worker started; its changes are counted
    /// from there.
    #[serde(default)]
    pub base: Option<String>,
    /// What it is doing now, e.g. "Edit src/main.rs".
    #[serde(default)]
    pub activity: Option<String>,
    /// It waits for the user to approve something.
    #[serde(default)]
    pub needs_approval: bool,
    /// Its work tree was merged or removed.
    #[serde(default)]
    pub released: bool,
    /// Prompts it got: the task, then follow-ups.
    #[serde(default)]
    pub turns: u32,
    /// Original agent label if rerouted because that subscription was out of quota.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rerouted_from: Option<String>,
}

/// The lead's team, written by the app into the lead's mailbox before each of its turns.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Team {
    pub members: Vec<Member>,
    pub max_workers: usize,
    /// The workspace is on another machine: no work trees, one editing worker at a time.
    #[serde(default)]
    pub remote: bool,
}

impl Team {
    /// Best team member that still has quota left, ignoring specialization notes.
    /// Prefers the member with the lowest active usage percentage (most quota left),
    /// breaking ties by the order configured in Settings > Orchestrator.
    #[cfg(test)]
    pub fn fallback_member(&self) -> Option<&Member> {
        self.fallback_member_for(0)
    }

    /// Distribute rerouted workers in a batch across available (non-exhausted) team members.
    /// Candidates are sorted by `used_pct` ascending (unknown usage defaults to 50%,
    /// tie-breaking by settings order). Prefers members below 85% used when any exist.
    pub fn fallback_member_for(&self, reroute_index: usize) -> Option<&Member> {
        let mut candidates: Vec<(usize, &Member)> = self
            .members
            .iter()
            .enumerate()
            .filter(|(_, m)| !m.exhausted)
            .collect();
        if candidates.is_empty() {
            return None;
        }
        candidates.sort_by(|(i, a), (j, b)| {
            let ua = a.used_pct.unwrap_or(50.0);
            let ub = b.used_pct.unwrap_or(50.0);
            ua.total_cmp(&ub).then_with(|| i.cmp(j))
        });
        let preferred: Vec<&Member> = candidates
            .iter()
            .filter(|(_, m)| m.used_pct.unwrap_or(50.0) < 85.0)
            .map(|(_, m)| *m)
            .collect();
        let pool: Vec<&Member> = if preferred.is_empty() {
            candidates.into_iter().map(|(_, m)| m).collect()
        } else {
            preferred
        };
        pool.get(reroute_index % pool.len()).copied()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Member {
    /// Name the lead uses, e.g. `codex`.
    pub agent: String,
    /// Provider slug.
    pub provider: String,
    /// e.g. "Codex".
    pub label: String,
    /// Model used when the lead names none.
    pub model: String,
    /// The lead may pick another of the provider's models.
    pub any_model: bool,
    #[serde(default)]
    pub note: String,
    /// Subscription usage, when known.
    #[serde(default)]
    pub quota: Option<String>,
    /// True when this member has no quota/credit left right now (0% left, limit reached, or cooling down).
    #[serde(default)]
    pub exhausted: bool,
    /// Highest active usage percentage (0–100) for this member's model, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used_pct: Option<f64>,
}

/// Point `settings` (whose active provider is the Orchestrator) at the lead agent for a turn.
/// Returns the lead provider; other providers are left alone.
pub fn apply_lead(settings: &mut crate::settings::AppSettings) -> LlmProviderKind {
    if settings.active_provider != LlmProviderKind::Orchestrator {
        return settings.active_provider;
    }
    let cfg = lead_config(settings);
    let lead = cfg.provider;
    *settings.provider_mut(lead) = cfg;
    settings.active_provider = lead;
    lead
}

pub fn lead_config(settings: &crate::settings::AppSettings) -> crate::settings::ProviderConfig {
    let lead = settings.orchestrator.lead_provider;
    let mut cfg = settings
        .provider(if lead.is_pseudo() {
            LlmProviderKind::AntigravityAcp
        } else {
            lead
        })
        .clone();
    let model = settings.orchestrator.lead_model.trim();
    if !model.is_empty() && cfg.model_id.trim() != model {
        cfg.context_window = None;
        cfg.model_id = model.to_string();
    }
    cfg
}

pub fn team_path(dir: &Path) -> PathBuf {
    dir.join("team.json")
}

pub fn write_team(dir: &Path, team: &Team) {
    if let Err(e) = crate::delegation::write_json(&team_path(dir), team) {
        log::warn!("orchestrator team: {e}");
    }
}

pub fn read_team(dir: &Path) -> Option<Team> {
    serde_json::from_slice(&std::fs::read(team_path(dir)).ok()?).ok()
}

/// The name the lead uses for `kind`: its short label when that reads back as the same
/// provider, else its slug.
pub fn agent_name(kind: LlmProviderKind) -> String {
    let short = crate::app::composer::composer_provider_label(kind)
        .to_ascii_lowercase()
        .replace(' ', "-");
    if crate::delegation::parse_provider(&short) == Some(kind) {
        short
    } else {
        kind.slug().to_string()
    }
}

/// Told to the lead with every turn (and as oxi's MCP server instructions).
pub const INSTRUCTIONS: &str = "You are the lead of the user's agent team in oxi: the user talks only to you. \
Answer questions and make small changes yourself. For substantial work, plan it, then hand pieces to workers \
with spawn_workers, one call for everything that should run at the same time. Pick each worker from the team \
by the user's notes, its strengths and its quota left. If a team member is out of quota (0% left / limit \
reached / rate limited), skip it and use the other team members that still have quota left, even if their \
settings notes specialize them for different work. Workers see nothing of this conversation: give each a \
complete, self-contained prompt (goal, relevant files, constraints, how to verify). Use access read_only for \
research and review, edit for changes. oxi decides where workers run: one editing worker edits the workspace; \
when several edit at once each gets its own git work tree, started from the workspace as it is now. Avoid \
parallel workers editing the same files unless you want to compare attempts. Their reports come back to this \
chat as a message when they end, so you may end your turn while they run; worker_status shows progress \
meanwhile. Check results with worker_diff, ask the same worker for fixes with message_worker (it keeps its \
context), and have another agent review risky changes. Bring work-tree changes into the workspace with \
merge_worker (they land as uncommitted changes the user reviews; resolve conflicts it reports), and \
stop_worker with discard for work you drop. Do not use your own built-in subagents for work your team should \
do. End by telling the user what was done, by whom, and what is left.";

/// The note in front of the user's message on each of the lead's turns.
pub fn turn_preamble(team: Option<&Team>, workers: &[Status]) -> String {
    let mut text = format!("[oxi orchestrator]\n{INSTRUCTIONS}\n\n");
    text.push_str(&team_summary(team, workers));
    text.push_str("\n[/oxi orchestrator]");
    text
}

fn team_summary(team: Option<&Team>, workers: &[Status]) -> String {
    let mut text = String::from("Team:\n");
    match team {
        Some(team) if !team.members.is_empty() => {
            for m in &team.members {
                text.push_str(&format!("- {}: {}", m.agent, m.label));
                if !m.model.is_empty() {
                    text.push_str(&format!(" · {}", m.model));
                }
                if m.any_model {
                    text.push_str(" (other models allowed)");
                }
                if m.exhausted {
                    text.push_str(" · [out of quota]");
                }
                if let Some(quota) = &m.quota {
                    text.push_str(&format!(" · {quota}"));
                }
                if !m.note.trim().is_empty() {
                    text.push_str(&format!(" — {}", m.note.trim()));
                }
                text.push('\n');
            }
            let any_exhausted = team.members.iter().any(|m| m.exhausted);
            if any_exhausted {
                let mut available: Vec<String> = Vec::new();
                for m in team.members.iter().filter(|m| !m.exhausted) {
                    let shares_exhausted_agent = team
                        .members
                        .iter()
                        .any(|o| o.exhausted && o.agent.eq_ignore_ascii_case(&m.agent));
                    let label = if shares_exhausted_agent && !m.model.is_empty() {
                        format!("{} ({})", m.agent, m.model)
                    } else {
                        m.agent.clone()
                    };
                    if !available.contains(&label) {
                        available.push(label);
                    }
                }
                if available.is_empty() {
                    text.push_str("All team members are currently out of quota.\n");
                } else {
                    text.push_str(&format!(
                        "Out-of-quota members cannot be used right now; use {} for any task regardless of their notes.\n",
                        available.join(", ")
                    ));
                }
            }
            text.push_str(&format!(
                "At most {} workers at once.{}\n",
                team.max_workers,
                if team.remote {
                    " Remote workspace: no work trees, one editing worker at a time."
                } else {
                    ""
                }
            ));
        }
        _ => text.push_str("(no agents available: set them up in oxi's Settings > Orchestrator)\n"),
    }
    let live: Vec<_> = workers
        .iter()
        .filter(|s| s.worker.as_ref().is_some_and(|w| !w.released) || !s.state.ended())
        .collect();
    if !live.is_empty() {
        text.push_str("Workers:\n");
        for s in live {
            text.push_str(&format!("- {}\n", worker_line(s, None)));
        }
    }
    text
}

/// Definitions of the lead's tools, in the OpenAI function format.
pub fn tool_definitions() -> Vec<Value> {
    let def = |name: &str, description: String, parameters: Value| {
        json!({
            "type": "function",
            "function": { "name": name, "description": description, "parameters": parameters }
        })
    };
    let id = json!({ "type": "string", "description": "Worker id from spawn_workers" });
    vec![
        def(
            TEAM,
            "The agents you can start workers on (with the user's notes and quota left) and the workers of this chat.".into(),
            json!({ "type": "object", "properties": {} }),
        ),
        def(
            SPAWN,
            format!("Start workers, each a separate agent chat the user can watch. Put everything that should run at once in one call (at most {MAX_BATCH}). Returns their ids and where each runs (workspace or its own work tree). Reports come back to this chat when they all ended."),
            json!({
                "type": "object",
                "properties": {
                    "workers": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "agent": { "type": "string", "description": "Team member, e.g. codex" },
                                "model": { "type": "string", "description": "Optional: the member's model by default" },
                                "title": { "type": "string", "description": "Short title" },
                                "prompt": { "type": "string", "description": "The complete task; the worker sees nothing else" },
                                "access": { "type": "string", "enum": ["edit", "read_only"], "description": "read_only for research/review" },
                                "isolate": { "type": "boolean", "description": "Own work tree even if it is the only editor (to compare attempts)" }
                            },
                            "required": ["agent", "prompt"]
                        }
                    }
                },
                "required": ["workers"]
            }),
        ),
        def(
            STATUS,
            format!("Progress of workers: state, current activity, changed files, and the report of those that ended. wait (any | all, up to {}s) waits for the listed running workers to end.", crate::delegation::MAX_WAIT.as_secs()),
            json!({
                "type": "object",
                "properties": {
                    "ids": { "type": "array", "items": { "type": "string" }, "description": "Optional: all of this chat's workers by default" },
                    "wait": { "type": "string", "enum": ["none", "any", "all"] },
                    "wait_seconds": { "type": "integer" }
                }
            }),
        ),
        def(
            MESSAGE,
            "Send a follow-up (fix this, also do that) to a worker that ended, in its own chat so it keeps its context. Its new report comes back like the first.".into(),
            json!({
                "type": "object",
                "properties": { "id": id, "text": { "type": "string" } },
                "required": ["id", "text"]
            }),
        ),
        def(
            DIFF,
            "A worker's changes since it started, as a patch (or only the file summary).".into(),
            json!({
                "type": "object",
                "properties": { "id": id, "stat_only": { "type": "boolean" } },
                "required": ["id"]
            }),
        ),
        def(
            MERGE,
            "Apply an ended worker's work-tree changes to the main workspace as uncommitted changes, then remove its work tree. Reports conflicts, left as conflict markers.".into(),
            json!({
                "type": "object",
                "properties": { "id": id },
                "required": ["id"]
            }),
        ),
        def(
            STOP,
            "Stop a running worker. discard also removes its work tree and changes.".into(),
            json!({
                "type": "object",
                "properties": { "id": id, "discard": { "type": "boolean" } },
                "required": ["id"]
            }),
        ),
    ]
}

/// Run one of the lead's tools against mailbox `dir`.
pub fn call(dir: Option<&Path>, name: &str, args: &Value) -> Result<String, String> {
    let dir = dir
        .filter(|d| Role::of_dir(d) == Some(Role::Lead))
        .ok_or("These tools work in an oxi Orchestrator chat, under the oxi desktop app.")?;
    match name {
        TEAM => Ok(team_summary(read_team(dir).as_ref(), &workers(dir))),
        SPAWN => spawn(dir, args),
        STATUS => status(dir, args),
        MESSAGE => message(dir, args),
        DIFF => diff(dir, args),
        MERGE => merge(dir, args),
        STOP => stop(dir, args),
        other => Err(format!("unknown tool {other}")),
    }
}

/// This lead's workers, oldest first.
pub fn workers(dir: &Path) -> Vec<Status> {
    let mut list: Vec<(std::time::SystemTime, Status)> = std::fs::read_dir(dir.join("status"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| {
            let modified = e
                .metadata()
                .and_then(|m| m.created().or(m.modified()))
                .ok()?;
            let status: Status = serde_json::from_slice(&std::fs::read(e.path()).ok()?).ok()?;
            status.worker.is_some().then_some((modified, status))
        })
        .collect();
    list.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.task.cmp(&b.1.task)));
    list.into_iter().map(|(_, s)| s).collect()
}

fn worker(dir: &Path, id: &str) -> Result<Status, String> {
    crate::delegation::read_status(dir, id)
        .filter(|s| s.worker.is_some())
        .ok_or_else(|| format!("No worker `{id}` in this chat."))
}

fn who(status: &Status) -> String {
    let label = crate::delegation::parse_provider(&status.provider)
        .map(|k| crate::app::composer::composer_provider_label(k).to_string())
        .unwrap_or_else(|| status.provider.clone());
    if status.model.is_empty() {
        label
    } else {
        format!("{label} · {}", status.model)
    }
}

fn state_word(state: TaskState) -> &'static str {
    match state {
        TaskState::Starting => "starting",
        TaskState::Running => "running",
        TaskState::Completed => "done",
        TaskState::Failed => "failed",
        TaskState::Cancelled => "stopped",
        TaskState::Rejected => "not started",
    }
}

/// One line about a worker; `changes` is its diff summary when known.
fn worker_line(status: &Status, changes: Option<&str>) -> String {
    let mut line = format!(
        "{} · {} · \"{}\" · {}",
        status.task,
        who(status),
        status.title,
        state_word(status.state)
    );
    if let Some(w) = &status.worker {
        line.push_str(match w.access {
            Access::ReadOnly => " · read-only",
            Access::Edit if w.cwd != w.root => " · own work tree",
            Access::Edit => " · edits the workspace",
        });
        if w.released {
            line.push_str(" (merged/removed)");
        }
        if let Some(from) = &w.rerouted_from {
            line.push_str(&format!(" · rerouted from {from} (out of quota)"));
        }
        if w.needs_approval {
            line.push_str(" · waiting for the user's approval");
        } else if !status.state.ended()
            && let Some(activity) = &w.activity
        {
            line.push_str(&format!(" · now: {activity}"));
        }
    }
    if let Some(changes) = changes {
        line.push_str(&format!(" · {changes}"));
    }
    if let Some(error) = &status.error {
        line.push_str(&format!(" · {error}"));
    }
    line
}

fn changes_of(status: &Status) -> Option<String> {
    let w = status.worker.as_ref()?;
    if w.access == Access::ReadOnly || w.released {
        return None;
    }
    let base = w.base.as_deref()?;
    Some(
        crate::git::handoff::short_stat(Path::new(&w.cwd), base)
            .unwrap_or_else(|| "no changes".into()),
    )
}

fn cut(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n… (cut; the whole text is in the worker's chat)",
        &text[..end]
    )
}

fn spawn(dir: &Path, args: &Value) -> Result<String, String> {
    let list = args["workers"]
        .as_array()
        .filter(|l| !l.is_empty())
        .ok_or("workers is required: a list of {agent, prompt, ...}")?;
    if list.len() > MAX_BATCH {
        return Err(format!("At most {MAX_BATCH} workers per call."));
    }
    let team = read_team(dir).unwrap_or_default();
    let mut requests = Vec::new();
    let mut reroute_index = 0usize;
    for (i, w) in list.iter().enumerate() {
        let agent = crate::delegation::string_arg(w, "agent")
            .ok_or_else(|| format!("workers[{i}].agent is required"))?;
        let kind = crate::delegation::parse_provider(agent);
        let member = team
            .members
            .iter()
            .find(|m| m.agent.eq_ignore_ascii_case(agent))
            .or_else(|| {
                let slug = kind?.slug();
                team.members.iter().find(|m| m.provider == slug)
            })
            .ok_or_else(|| {
                format!(
                    "`{agent}` is not on the team ({}).",
                    team.members
                        .iter()
                        .map(|m| m.agent.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })?;
        let requested_model = crate::delegation::string_arg(w, "model").unwrap_or("");
        let (resolved_member, model, custom_model_override) =
            if requested_model.is_empty() || requested_model.eq_ignore_ascii_case(&member.model) {
                (member, member.model.clone(), false)
            } else {
                let other = team.members.iter().find(|m| {
                    m.provider == member.provider && m.model.eq_ignore_ascii_case(requested_model)
                });
                match other {
                    Some(m) => (m, m.model.clone(), false),
                    None if member.any_model => (member, requested_model.to_string(), true),
                    None => {
                        return Err(format!(
                            "{} may only use {} here (Settings > Orchestrator).",
                            member.label, member.model
                        ));
                    }
                }
            };
        // If the selected team slot is out of quota (and the lead didn't explicitly pick a
        // different model on an `any_model` slot, which the app checks live per model), fall
        // back to an available team member regardless of its settings note, distributing
        // multiple rerouted workers across available subscriptions.
        let (provider, model, rerouted_from) = if resolved_member.exhausted
            && !custom_model_override
            && let Some(fallback) = team.fallback_member_for(reroute_index)
        {
            reroute_index += 1;
            (
                fallback.provider.clone(),
                fallback.model.clone(),
                Some(resolved_member.label.clone()),
            )
        } else {
            (resolved_member.provider.clone(), model, None)
        };
        let prompt = crate::delegation::string_arg(w, "prompt")
            .ok_or_else(|| format!("workers[{i}].prompt is required"))?;
        if prompt.len() > MAX_PROMPT {
            return Err(format!("workers[{i}].prompt is over {MAX_PROMPT} bytes."));
        }
        let access = match w.get("access") {
            None => Access::Edit,
            Some(value) => serde_json::from_value(value.clone()).map_err(|_| {
                format!("workers[{i}].access must be `edit` or `read_only`; got {value}.")
            })?,
        };
        requests.push(WorkerRequest {
            task: format!("w{:08x}", rand::random::<u32>()),
            provider,
            model,
            title: crate::delegation::string_arg(w, "title")
                .map(str::to_owned)
                .unwrap_or_else(|| crate::model::make_session_title(prompt)),
            prompt: prompt.to_string(),
            access,
            isolate: w["isolate"].as_bool().unwrap_or(false),
            rerouted_from,
        });
    }
    let batch = format!("b{:08x}", rand::random::<u32>());
    let ids: Vec<String> = requests.iter().map(|r| r.task.clone()).collect();
    let file = crate::delegation::requests_dir(dir)
        .join(format!("{}-{batch}.json", crate::delegation::now_millis()));
    crate::delegation::write_json(
        &file,
        &crate::delegation::Request::Spawn {
            batch,
            workers: requests,
        },
    )?;
    let started = Instant::now();
    loop {
        let statuses: Vec<Option<Status>> = ids
            .iter()
            .map(|id| crate::delegation::read_status(dir, id))
            .collect();
        if statuses.iter().all(Option::is_some) {
            let lines: Vec<String> = statuses
                .into_iter()
                .flatten()
                .map(|s| format!("- {}", worker_line(&s, None)))
                .collect();
            return Ok(format!(
                "{}\nTheir reports come back to this chat when they all ended; {STATUS} shows progress meanwhile.",
                lines.join("\n")
            ));
        }
        if started.elapsed() >= ACCEPT_TIMEOUT {
            let _ = std::fs::remove_file(&file);
            return Err("The oxi app did not take the request.".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn status(dir: &Path, args: &Value) -> Result<String, String> {
    let ids: Vec<String> = args["ids"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let wait = args["wait"].as_str().unwrap_or("none");
    let limit = Duration::from_secs(args["wait_seconds"].as_u64().unwrap_or(if wait == "none" {
        0
    } else {
        crate::delegation::MAX_WAIT.as_secs()
    }))
    .min(crate::delegation::MAX_WAIT);
    let started = Instant::now();
    let pick = |dir: &Path| -> Result<Vec<Status>, String> {
        if ids.is_empty() {
            Ok(workers(dir))
        } else {
            ids.iter().map(|id| worker(dir, id)).collect()
        }
    };
    let running_at_start: Vec<String> = pick(dir)?
        .into_iter()
        .filter(|s| !s.state.ended())
        .map(|s| s.task)
        .collect();
    let list = loop {
        let list = pick(dir)?;
        let watched: Vec<&Status> = list
            .iter()
            .filter(|s| running_at_start.contains(&s.task))
            .collect();
        let done = match wait {
            "any" => watched.is_empty() || watched.iter().any(|s| s.state.ended()),
            "all" => watched.iter().all(|s| s.state.ended()),
            _ => true,
        };
        if done || started.elapsed() >= limit {
            break list;
        }
        std::thread::sleep(Duration::from_millis(500));
    };
    if list.is_empty() {
        return Ok("No workers yet.".into());
    }
    let mut text = String::new();
    for s in &list {
        text.push_str(&format!("- {}\n", worker_line(s, changes_of(s).as_deref())));
        if s.state.ended()
            && let Some(report) = s.result.as_deref().filter(|r| !r.trim().is_empty())
        {
            text.push_str(&format!("  Report:\n{}\n", cut(report.trim(), MAX_REPORT)));
            // The lead has it now; the app need not send it again.
            let _ =
                crate::fsutil::write_atomic(&crate::delegation::fetched_path(dir, &s.task), b"");
        }
    }
    Ok(text.trim_end().to_string())
}

/// Each request gets its own reply. Concurrent and repeated refusals must reach the caller
/// that made the request, even when neither the worker state nor its error changed.
fn worker_request(
    dir: &Path,
    request: impl FnOnce(u64) -> crate::delegation::Request,
) -> Result<String, String> {
    let reply = rand::random::<u64>();
    let file = crate::delegation::requests_dir(dir)
        .join(format!("{}-{reply}.json", crate::delegation::now_millis()));
    let response = reply_path(dir, reply);
    crate::delegation::write_json(&file, &request(reply))?;
    let started = Instant::now();
    loop {
        if let Ok(bytes) = std::fs::read(&response) {
            let _ = std::fs::remove_file(&response);
            return serde_json::from_slice(&bytes)
                .map_err(|e| format!("Invalid worker response: {e}"))?;
        }
        if started.elapsed() >= ACCEPT_TIMEOUT && std::fs::remove_file(&file).is_ok() {
            return Err("The oxi app did not take the request.".into());
        }
        if started.elapsed() >= Duration::from_secs(120) {
            return Err(
                "The request was taken but has not returned. Check worker_status before retrying."
                    .into(),
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn reply_path(dir: &Path, reply: u64) -> PathBuf {
    dir.join("replies").join(format!("{reply}.json"))
}

pub fn reply(dir: &Path, id: u64, result: &Result<String, String>) {
    if let Err(e) = crate::delegation::write_json(&reply_path(dir, id), result) {
        log::warn!("worker reply: {e}");
    }
}

fn message(dir: &Path, args: &Value) -> Result<String, String> {
    let id = crate::delegation::string_arg(args, "id").ok_or("id is required")?;
    let text = crate::delegation::string_arg(args, "text").ok_or("text is required")?;
    if text.len() > MAX_PROMPT {
        return Err(format!("text is over {MAX_PROMPT} bytes."));
    }
    let before = worker(dir, id)?;
    let info = before.worker.clone().unwrap_or_default();
    if info.released {
        return Err(format!(
            "Worker {id}'s work tree was merged or removed; start a new worker."
        ));
    }
    if before.state == TaskState::Rejected || before.chat.is_none() {
        return Err(format!("Worker {id} never started."));
    }
    worker_request(dir, |reply| crate::delegation::Request::Message {
        task: id.to_string(),
        text: text.to_string(),
        reply,
    })
}

fn diff(dir: &Path, args: &Value) -> Result<String, String> {
    let id = crate::delegation::string_arg(args, "id").ok_or("id is required")?;
    let status = worker(dir, id)?;
    let w = status.worker.as_ref().ok_or("not a worker")?;
    if w.released {
        return Err(format!(
            "Worker {id}'s changes were merged or removed already."
        ));
    }
    let base = w
        .base
        .as_deref()
        .ok_or_else(|| format!("Worker {id} has no starting point to compare with."))?;
    let stat_only = args["stat_only"].as_bool().unwrap_or(false);
    let cwd = Path::new(&w.cwd);
    let stat = crate::git::handoff::diff_since(cwd, base, true)?;
    if stat.trim().is_empty() {
        return Ok(format!("Worker {id} changed nothing."));
    }
    let shared = if w.cwd == w.root {
        "\n(It works in the workspace itself: this includes any other changes made there since it started.)"
    } else {
        ""
    };
    if stat_only {
        return Ok(format!("{}{shared}", stat.trim_end()));
    }
    let patch = crate::git::handoff::diff_since(cwd, base, false)?;
    Ok(format!(
        "{}{shared}\n\n{}",
        stat.trim_end(),
        cut(&patch, crate::git::handoff::MAX_DIFF_BYTES)
    ))
}

fn merge(dir: &Path, args: &Value) -> Result<String, String> {
    let id = crate::delegation::string_arg(args, "id").ok_or("id is required")?;
    worker(dir, id)?;
    worker_request(dir, |reply| crate::delegation::Request::Merge {
        task: id.into(),
        reply,
    })
}

/// Called off the UI thread while the app holds exclusive ownership of the ended worker.
pub fn merge_worktree(id: &str, w: &WorkerInfo) -> Result<String, String> {
    let base = w
        .base
        .as_deref()
        .ok_or("The worker has no starting snapshot.")?;
    let wt = Path::new(&w.cwd);
    let applied = crate::git::handoff::apply_onto(Path::new(&w.root), wt, base)
        .map_err(|e| format!("Worker {id}'s work tree was kept at {}: {e}", w.cwd))?;
    if !applied.unresolved.is_empty() {
        let mut report = format!(
            "Unresolved paths: {}. Worker {id}'s versions are still in {}. Resolve them before explicitly discarding the work tree.",
            applied.unresolved.join(", "),
            w.cwd
        );
        if !applied.conflicts.is_empty() {
            report.push_str(&format!(
                " Conflict markers: {}.",
                applied.conflicts.join(", ")
            ));
        }
        return Err(report);
    }
    remove_applied_tree(wt, &applied.tree)?;
    if applied.files.is_empty() {
        return Ok(format!(
            "Worker {id} changed nothing; its work tree was removed."
        ));
    }
    let mut text = format!(
        "Applied {} file(s) from {id} to the workspace as uncommitted changes: {}.",
        applied.files.len(),
        applied.files.join(", ")
    );
    if !applied.conflicts.is_empty() {
        text.push_str(&format!(
            "\nConflicts (conflict markers left in): {}. Resolve them in the workspace, or start a worker to do it.",
            applied.conflicts.join(", ")
        ));
    }
    Ok(text)
}

fn remove_applied_tree(wt: &Path, tree: &str) -> Result<(), String> {
    if crate::git::handoff::tree_of(wt).as_deref() != Ok(tree) {
        return Err(format!(
            "The patch was applied, but the worker tree changed or could not be verified. Its work tree was kept at {}.",
            wt.display()
        ));
    }
    remove_worker_tree(wt).map_err(|e| {
        format!(
            "Applied the patch, but cleanup failed for {}: {e}",
            wt.display()
        )
    })
}

pub fn remove_worker_tree(wt: &Path) -> Result<(), String> {
    crate::git::worktree::remove(
        wt,
        crate::git::worktree::RemoveOptions {
            keep_branch: false,
            force: true,
        },
    )?;
    if wt.try_exists().map_err(|e| e.to_string())? {
        return Err(format!("{} still exists after removal", wt.display()));
    }
    Ok(())
}

fn stop(dir: &Path, args: &Value) -> Result<String, String> {
    let id = crate::delegation::string_arg(args, "id").ok_or("id is required")?;
    let status = worker(dir, id)?;
    let discard = args["discard"].as_bool().unwrap_or(false);
    let mut done = Vec::new();
    if !status.state.ended() {
        crate::delegation::write_json(
            &crate::delegation::requests_dir(dir).join(format!(
                "{}-cancel-{id}.json",
                crate::delegation::now_millis()
            )),
            &crate::delegation::Request::Cancel {
                task: id.to_string(),
            },
        )?;
        done.push("asked oxi to stop it");
    }
    if discard {
        let w = status.worker.as_ref();
        if w.is_some_and(|w| w.cwd != w.root && !w.released) {
            crate::delegation::write_json(
                &crate::delegation::requests_dir(dir).join(format!(
                    "{}-discard-{id}.json",
                    crate::delegation::now_millis() + 1
                )),
                &crate::delegation::Request::Discard {
                    task: id.to_string(),
                },
            )?;
            done.push("its work tree and changes will be removed");
        } else if w.is_some_and(|w| w.cwd == w.root && w.access == Access::Edit) {
            done.push("it edited the workspace itself, so its changes stay there (revert them in the Changes panel if unwanted)");
        }
    }
    if done.is_empty() {
        return Ok(format!("Worker {id} already ended."));
    }
    Ok(format!("Worker {id}: {}.", done.join("; ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_ride_on_session_keys_and_mailbox_names() {
        let lead = Role::Lead.session_key("/chats/a.jsonl");
        assert_eq!(Role::of_session_key(&lead), Some(Role::Lead));
        assert_eq!(Role::of_session_key("/chats/a.jsonl"), None);
        let dir = crate::delegation::dir_for(&lead);
        assert_eq!(Role::of_dir(&dir), Some(Role::Lead));
        let worker = crate::delegation::dir_for(&Role::Worker.session_key("mem:0:1"));
        assert_eq!(Role::of_dir(&worker), Some(Role::Worker));
        assert_eq!(Role::of_dir(&crate::delegation::dir_for("mem:0:1")), None);
    }

    #[test]
    fn changed_worker_tree_survives_cleanup_after_apply() {
        let main = crate::fsutil::unique_temp_path(&std::env::temp_dir(), "oxi-orch-changed", "d");
        let repo = git2::Repository::init(&main).unwrap();
        let base = crate::git::handoff::snapshot(&main).unwrap();
        let commit = repo
            .find_commit(git2::Oid::from_str(&base).unwrap())
            .unwrap();
        repo.branch("main-fixture", &commit, false).unwrap();
        repo.set_head("refs/heads/main-fixture").unwrap();
        let wt = crate::git::worktree::create_at(&main, Some(&base)).unwrap();
        crate::fsutil::write_atomic(&wt.join("draft"), b"first").unwrap();
        let applied = crate::git::handoff::apply_onto(&main, &wt, &base).unwrap();
        crate::fsutil::write_atomic(&wt.join("draft"), b"later").unwrap();
        assert!(
            remove_applied_tree(&wt, &applied.tree)
                .unwrap_err()
                .contains("changed")
        );
        assert_eq!(std::fs::read(main.join("draft")).unwrap(), b"first");
        assert_eq!(std::fs::read(wt.join("draft")).unwrap(), b"later");
        let since_first = crate::git::handoff::snapshot(&main).unwrap();
        let w = WorkerInfo {
            root: main.display().to_string(),
            cwd: wt.display().to_string(),
            base: Some(since_first),
            ..Default::default()
        };
        let report = merge_worktree("w1", &w).unwrap();
        assert!(report.contains("draft"));
        assert!(!wt.exists());
        assert_eq!(std::fs::read(main.join("draft")).unwrap(), b"later");
        drop(commit);
        drop(repo);
        std::fs::remove_dir_all(main).unwrap();
    }

    #[test]
    fn only_a_lead_mailbox_runs_the_tools() {
        let dir = crate::fsutil::unique_temp_path(&std::env::temp_dir(), "oxi-orch", "d");
        assert!(call(Some(&dir), TEAM, &json!({})).is_err());
        assert!(call(None, TEAM, &json!({})).is_err());
    }

    fn lead_dir() -> PathBuf {
        let dir = crate::fsutil::unique_temp_path(&std::env::temp_dir(), "oxi-orch", "d");
        let dir = dir.with_file_name(format!(
            "{}-lead",
            dir.file_name().unwrap().to_string_lossy()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        write_team(
            &dir,
            &Team {
                members: vec![Member {
                    agent: "codex".into(),
                    provider: "codexacp".into(),
                    label: "Codex".into(),
                    model: "gpt-6.2-sol".into(),
                    any_model: false,
                    note: "backend".into(),
                    quota: Some("5h 20% used".into()),
                    exhausted: false,
                    used_pct: Some(20.0),
                }],
                max_workers: 3,
                remote: false,
            },
        );
        dir
    }

    #[test]
    fn spawning_checks_the_team_and_hands_the_batch_to_the_app() {
        let dir = lead_dir();
        let err = call(
            Some(&dir),
            SPAWN,
            &json!({"workers": [{"agent": "cursor", "prompt": "x"}]}),
        )
        .unwrap_err();
        assert!(err.contains("not on the team"), "{err}");
        let err = call(
            Some(&dir),
            SPAWN,
            &json!({"workers": [{"agent": "codex", "model": "gpt-4", "prompt": "x"}]}),
        )
        .unwrap_err();
        assert!(err.contains("may only use gpt-6.2-sol"), "{err}");

        for access in [json!("read_ony"), json!(null), json!(7), json!("")] {
            let err = call(
                Some(&dir),
                SPAWN,
                &json!({"workers": [{"agent":"codex","prompt":"review","access":access}]}),
            )
            .unwrap_err();
            assert!(err.contains("workers[0].access"), "{err}");
        }
        assert!(crate::delegation::take_requests(&dir).is_empty());

        let app_dir = dir.clone();
        let app = std::thread::spawn(move || {
            for _ in 0..200 {
                if let Some(crate::delegation::Request::Spawn { batch, workers }) =
                    crate::delegation::take_requests(&app_dir)
                        .into_iter()
                        .next()
                {
                    assert_eq!(workers.len(), 2);
                    assert_eq!(workers[0].model, "gpt-6.2-sol");
                    assert_eq!(workers[1].access, Access::ReadOnly);
                    for w in workers {
                        crate::delegation::write_status(
                            &app_dir,
                            &Status {
                                task: w.task,
                                state: TaskState::Running,
                                provider: w.provider,
                                model: w.model,
                                title: w.title,
                                chat: Some("c.jsonl".into()),
                                worktree: None,
                                result: None,
                                error: None,
                                worker: Some(WorkerInfo {
                                    batch: batch.clone(),
                                    access: w.access,
                                    root: "/repo".into(),
                                    cwd: "/repo".into(),
                                    turns: 1,
                                    ..Default::default()
                                }),
                            },
                        );
                    }
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            panic!("no request");
        });
        let out = call(
            Some(&dir),
            SPAWN,
            &json!({"workers": [
                {"agent": "codex", "title": "API", "prompt": "Build the API"},
                {"agent": "Codex", "prompt": "Review it", "access": "read_only"}
            ]}),
        )
        .unwrap();
        app.join().unwrap();
        assert!(
            out.contains("\"API\" · running · edits the workspace"),
            "{out}"
        );
        assert!(out.contains("read-only"), "{out}");
        let team = call(Some(&dir), TEAM, &json!({})).unwrap();
        assert!(
            team.contains("- codex: Codex · gpt-6.2-sol · 5h 20% used — backend"),
            "{team}"
        );
        assert!(team.contains("Workers:"), "{team}");
        let status = call(Some(&dir), STATUS, &json!({})).unwrap();
        assert_eq!(status.lines().filter(|l| l.starts_with("- w")).count(), 2);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn exhausted_member_reroutes_to_available_subscription_regardless_of_notes() {
        let dir = crate::fsutil::unique_temp_path(&std::env::temp_dir(), "oxi-orch-quota", "d");
        let dir = dir.with_file_name(format!(
            "{}-lead",
            dir.file_name().unwrap().to_string_lossy()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        write_team(
            &dir,
            &Team {
                members: vec![
                    Member {
                        agent: "claude".into(),
                        provider: "claudecodeacp".into(),
                        label: "Claude".into(),
                        model: "opus".into(),
                        any_model: true,
                        note: "frontend UI".into(),
                        quota: Some("limit reached, 5h 100% used".into()),
                        exhausted: true,
                        used_pct: Some(100.0),
                    },
                    Member {
                        agent: "codex".into(),
                        provider: "codexacp".into(),
                        label: "Codex".into(),
                        model: "gpt-6.2-astra".into(),
                        any_model: false,
                        note: "backend".into(),
                        quota: Some("5h 60% used".into()),
                        exhausted: false,
                        used_pct: Some(60.0),
                    },
                    Member {
                        agent: "antigravity".into(),
                        provider: "antigravityacp".into(),
                        label: "Antigravity".into(),
                        model: "gemini-4-neon".into(),
                        any_model: true,
                        note: "research only".into(),
                        quota: Some("5h 15% used".into()),
                        exhausted: false,
                        used_pct: Some(15.0),
                    },
                ],
                max_workers: 3,
                remote: false,
            },
        );
        let summary = call(Some(&dir), TEAM, &json!({})).unwrap();
        assert!(summary.contains("[out of quota]"), "{summary}");
        assert!(
            summary.contains("use codex, antigravity for any task regardless of their notes"),
            "{summary}"
        );

        let app_dir = dir.clone();
        let app = std::thread::spawn(move || {
            for _ in 0..200 {
                if let Some(crate::delegation::Request::Spawn { batch, workers }) =
                    crate::delegation::take_requests(&app_dir)
                        .into_iter()
                        .next()
                {
                    assert_eq!(workers.len(), 1);
                    // Rerouted from exhausted Claude to Antigravity (lowest usage: 15% vs 60%),
                    // using Antigravity's model even though its note says "research only".
                    assert_eq!(workers[0].provider, "antigravityacp");
                    assert_eq!(workers[0].model, "gemini-4-neon");
                    assert_eq!(workers[0].rerouted_from.as_deref(), Some("Claude"));
                    let w = &workers[0];
                    crate::delegation::write_status(
                        &app_dir,
                        &Status {
                            task: w.task.clone(),
                            state: TaskState::Running,
                            provider: w.provider.clone(),
                            model: w.model.clone(),
                            title: w.title.clone(),
                            chat: Some("c.jsonl".into()),
                            worktree: None,
                            result: None,
                            error: None,
                            worker: Some(WorkerInfo {
                                batch,
                                access: w.access,
                                root: "/repo".into(),
                                cwd: "/repo".into(),
                                turns: 1,
                                rerouted_from: w.rerouted_from.clone(),
                                ..Default::default()
                            }),
                        },
                    );
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            panic!("no request");
        });
        let out = call(
            Some(&dir),
            SPAWN,
            &json!({"workers": [{"agent": "claude", "title": "Fix CSS", "prompt": "Fix button padding"}]}),
        )
        .unwrap();
        app.join().unwrap();
        assert!(out.contains("Antigravity · gemini-4-neon"), "{out}");
        assert!(out.contains("rerouted from Claude (out of quota)"), "{out}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn multiple_rerouted_workers_in_one_batch_distribute_across_available_subscriptions() {
        let dir = crate::fsutil::unique_temp_path(&std::env::temp_dir(), "oxi-orch-dist", "d");
        let dir = dir.with_file_name(format!(
            "{}-lead",
            dir.file_name().unwrap().to_string_lossy()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let team = Team {
            members: vec![
                Member {
                    agent: "claude".into(),
                    provider: "claudecodeacp".into(),
                    label: "Claude".into(),
                    model: "opus".into(),
                    any_model: true,
                    note: "frontend UI".into(),
                    quota: Some("limit reached, 5h 100% used".into()),
                    exhausted: true,
                    used_pct: Some(100.0),
                },
                Member {
                    agent: "codex".into(),
                    provider: "codexacp".into(),
                    label: "Codex".into(),
                    model: "gpt-6.2-astra".into(),
                    any_model: false,
                    note: "backend".into(),
                    quota: Some("5h 30% used".into()),
                    exhausted: false,
                    used_pct: Some(30.0),
                },
                Member {
                    agent: "antigravity".into(),
                    provider: "antigravityacp".into(),
                    label: "Antigravity".into(),
                    model: "gemini-4-neon".into(),
                    any_model: true,
                    note: "research".into(),
                    quota: Some("5h 15% used".into()),
                    exhausted: false,
                    used_pct: Some(15.0),
                },
            ],
            max_workers: 3,
            remote: false,
        };
        assert_eq!(
            team.fallback_member_for(0).unwrap().provider,
            "antigravityacp"
        );
        assert_eq!(team.fallback_member_for(1).unwrap().provider, "codexacp");
        assert_eq!(
            team.fallback_member_for(2).unwrap().provider,
            "antigravityacp"
        );
        write_team(&dir, &team);

        let app_dir = dir.clone();
        let app = std::thread::spawn(move || {
            for _ in 0..200 {
                if let Some(crate::delegation::Request::Spawn { batch, workers }) =
                    crate::delegation::take_requests(&app_dir)
                        .into_iter()
                        .next()
                {
                    assert_eq!(workers.len(), 2);
                    assert_eq!(workers[0].provider, "antigravityacp");
                    assert_eq!(workers[0].model, "gemini-4-neon");
                    assert_eq!(workers[0].rerouted_from.as_deref(), Some("Claude"));
                    assert_eq!(workers[1].provider, "codexacp");
                    assert_eq!(workers[1].model, "gpt-6.2-astra");
                    assert_eq!(workers[1].rerouted_from.as_deref(), Some("Claude"));
                    for w in workers {
                        crate::delegation::write_status(
                            &app_dir,
                            &Status {
                                task: w.task,
                                state: TaskState::Running,
                                provider: w.provider,
                                model: w.model,
                                title: w.title,
                                chat: Some("c.jsonl".into()),
                                worktree: None,
                                result: None,
                                error: None,
                                worker: Some(WorkerInfo {
                                    batch: batch.clone(),
                                    access: w.access,
                                    root: "/repo".into(),
                                    cwd: "/repo".into(),
                                    turns: 1,
                                    rerouted_from: w.rerouted_from,
                                    ..Default::default()
                                }),
                            },
                        );
                    }
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            panic!("no request");
        });
        let out = call(
            Some(&dir),
            SPAWN,
            &json!({"workers": [
                {"agent": "claude", "title": "Task 1", "prompt": "First task"},
                {"agent": "claude", "title": "Task 2", "prompt": "Second task"}
            ]}),
        )
        .unwrap();
        app.join().unwrap();
        assert!(out.contains("Antigravity · gemini-4-neon"), "{out}");
        assert!(out.contains("Codex · gpt-6.2-astra"), "{out}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
