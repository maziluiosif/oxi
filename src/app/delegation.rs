//! The app's side of delegation between agents (see [`crate::delegation`]): requests from a
//! chat's agent become chats of their own on the requested provider/model, in the parent's
//! workspace or a new work tree; their state is kept in the parent's mailbox, and their report
//! goes back to the parent chat when they end.
//!
//! Orchestrator workers (see [`crate::orchestrator`]) are delegated tasks too, started in
//! batches by a lead chat: oxi places them (workspace or own work tree), keeps them off the
//! sidebar while they belong to their lead, takes follow-ups to them and removes their work
//! trees once merged or dropped.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText, Ui};

use super::compaction::QueuedSend;
use super::state::Workspace;
use super::{OxiApp, SessionKey};
use crate::delegation::{Request, Status, TaskState};
use crate::model::{AssistantBlock, MsgRole};
use crate::orchestrator::{Access, WorkerInfo, WorkerRequest};
use crate::theme::*;

/// A finished task's report goes to the parent this long after it ended, unless the parent's
/// agent read it with `task_status` meanwhile.
const DELIVERY_DELAY: Duration = Duration::from_secs(2);
/// How often the mailboxes are checked for requests while nothing else wakes the app.
const WATCH_EVERY: Duration = Duration::from_millis(300);

/// Set by the watcher when a mailbox has requests; the next frame reads them.
static REQUESTS_WAITING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Where a task runs, once worked out off the UI thread.
struct Prepared {
    worktree: Option<PathBuf>,
    /// Orchestrator workers: the snapshot their changes are counted from.
    base: Option<String>,
}

/// One delegated task.
pub(crate) struct DelegatedTask {
    /// The parent chat's mailbox.
    dir: PathBuf,
    /// The parent chat's agent session key (its file, or a `mem:` key before it has one).
    parent: String,
    pub status: Status,
    prompt: String,
    plan_mode: bool,
    /// Its work tree (and, for a worker, its starting snapshot) being made.
    prep_rx: Option<Receiver<Result<Prepared, String>>>,
    ended_at: Option<Instant>,
    delivered: bool,
    /// Remove its work tree once it stopped running.
    discard_pending: bool,
    /// A running handoff owns the worker until its filesystem result returns.
    cleanup: Option<Cleanup>,
    checkout_lease: Option<std::fs::File>,
    /// The lead chat was deleted: keep driving any in-flight worktree cleanup, then drop without
    /// rewriting persisted worker records.
    parent_deleted: bool,
}

struct Cleanup {
    rx: Receiver<Result<String, String>>,
    reply: Option<u64>,
}

impl DelegatedTask {
    fn save(&self) {
        crate::delegation::write_status(&self.dir, &self.status);
        if self.is_worker() && !self.parent.starts_with("mem:") && !self.parent_deleted {
            let path = crate::delegation::status_path(
                &crate::delegation::worker_records(&self.parent),
                &self.status.task,
            );
            if let Err(e) = crate::delegation::write_json(
                &path,
                &crate::delegation::WorkerRecord {
                    status: self.status.clone(),
                    delivered: self.delivered,
                },
            ) {
                log::warn!("saving worker {}: {e}", self.status.task);
            }
        }
    }

    fn is_worker(&self) -> bool {
        self.status.worker.is_some()
    }

    /// A worker still part of its lead's work: running, or its work tree not merged/removed.
    fn worker_in_use(&self) -> bool {
        self.status
            .worker
            .as_ref()
            .is_some_and(|w| !self.status.state.ended() || (!w.released && w.cwd != w.root))
    }
}

/// Wake the UI when a mailbox of this process has a request, so a task starts while the app is
/// idle (the agent's tool waits a few seconds for it).
pub(crate) fn watch(ctx: &egui::Context) {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| start_watching(ctx.clone()));
}

fn start_watching(ctx: egui::Context) {
    crate::delegation::set_available();
    crate::delegation::prune_stale();
    let own = format!("{}-", std::process::id());
    let _ = std::thread::Builder::new()
        .name("oxi-delegation".into())
        .spawn(move || {
            loop {
                std::thread::sleep(WATCH_EVERY);
                let waiting = std::fs::read_dir(crate::delegation::root())
                    .into_iter()
                    .flatten()
                    .flatten()
                    .filter(|e| e.file_name().to_string_lossy().starts_with(&own))
                    .any(|e| {
                        std::fs::read_dir(e.path().join("requests"))
                            .is_ok_and(|mut entries| entries.next().is_some())
                    });
                if waiting {
                    REQUESTS_WAITING.store(true, std::sync::atomic::Ordering::SeqCst);
                    ctx.request_repaint();
                }
            }
        });
}

/// The preamble of a delegated task's prompt.
fn task_prompt(parent: &str, prompt: &str) -> String {
    format!(
        "Another agent ({parent}) working in oxi delegated this task to you. Work on it, then end \
         with a concise report of what you did and found: it is passed back to that agent.\n\n\
         Task:\n{prompt}"
    )
}

/// The preamble of an orchestrator worker's first prompt.
fn worker_prompt(lead: &str, access: Access, own_tree: bool, prompt: &str) -> String {
    let place = match (access, own_tree) {
        (Access::ReadOnly, _) => {
            "This is a read-only task: do not change, create or delete files. Read, run read-only \
             commands, and report."
        }
        (Access::Edit, true) => {
            "You work in your own git work tree (the current folder), a copy of the user's \
             workspace; other agents work in parallel elsewhere. Change files only here. Do not \
             commit, push, or switch branches: oxi brings your uncommitted changes back."
        }
        (Access::Edit, false) => {
            "You work directly in the user's workspace (the current folder). Do not commit, push, \
             or switch branches."
        }
    };
    format!(
        "You are a worker on a team led by another agent ({lead}) in oxi. {place}\n\
         Work on the task below, verify what you can (build, tests), then end with a concise \
         report for the lead: what you did, files changed, how you verified it, and anything \
         left open. You may get follow-ups in this chat.\n\nTask:\n{prompt}"
    )
}

/// What goes back to the parent chat when a task ends.
fn report_message(status: &Status) -> String {
    format!("[Delegated task] {}", crate::delegation::describe(status))
}

/// What goes back to a lead when the workers of a batch ended.
fn workers_report(statuses: &[&Status]) -> String {
    let mut text = String::from(if statuses.len() == 1 {
        "[Worker finished]"
    } else {
        "[Workers finished]"
    });
    text.push_str(
        "\nWorker output for the lead to review. Each quoted report comes from the named worker.",
    );
    for status in statuses {
        text.push_str("\n\n");
        text.push_str(
            &crate::delegation::describe(status)
                .lines()
                .map(|l| format!("> {l}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        if let Some(w) = &status.worker
            && w.access == Access::Edit
            && w.turns > 0
        {
            text.push_str(if w.cwd != w.root {
                "\n(Changes are in its own work tree: check them with worker_diff, then merge_worker or stop_worker with discard.)"
            } else {
                "\n(It edited the workspace directly.)"
            });
        }
    }
    text
}

/// What a worker chat is doing, from its last reply: its latest tool call.
fn activity_of(messages: &[crate::model::ChatMessage]) -> Option<String> {
    let reply = messages.last().filter(|m| m.role == MsgRole::Assistant)?;
    reply.blocks.iter().rev().find_map(|b| match b {
        AssistantBlock::Tool {
            name,
            metadata,
            args_summary,
            ..
        } => {
            let title = metadata
                .as_ref()
                .map(|m| m.title.trim().to_string())
                .filter(|t| !t.is_empty())
                .or_else(|| {
                    args_summary
                        .as_deref()
                        .map(|a| format!("{name} {}", one_line(a)))
                })
                .unwrap_or_else(|| name.clone());
            Some(cut_chars(&title, 120))
        }
        AssistantBlock::Answer(t) if !t.trim().is_empty() => Some("writing its report".into()),
        _ => None,
    })
}

fn cut_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        format!("{}…", text.chars().take(max).collect::<String>())
    }
}

impl OxiApp {
    pub(super) fn restore_worker_records(&mut self) {
        let parents: Vec<_> = self
            .conv
            .workspaces
            .iter()
            .flat_map(|ws| &ws.sessions)
            .filter_map(|s| s.session_file.as_ref())
            .map(|file| crate::orchestrator::Role::Lead.session_key(file))
            .collect();
        for parent in parents {
            if crate::delegation::worker_records(&parent)
                .join("status")
                .is_dir()
            {
                let _ = self.claim_lead(&parent);
            }
        }
    }

    fn claim_lead(&mut self, parent: &str) -> Result<(), String> {
        if parent.starts_with("mem:") || self.conv.worker_owners.contains_key(parent) {
            return Ok(());
        }
        let lease = crate::delegation::reserve_lead(parent)?
            .ok_or("Another oxi window owns the workers of this lead chat.")?;
        self.conv.worker_owners.insert(parent.to_owned(), lease);
        let dir = crate::delegation::dir_for(parent);
        let records = crate::delegation::worker_records(parent).join("status");
        for entry in std::fs::read_dir(records).into_iter().flatten().flatten() {
            let Some(mut record) = std::fs::read(entry.path())
                .ok()
                .and_then(|b| serde_json::from_slice::<crate::delegation::WorkerRecord>(&b).ok())
            else {
                continue;
            };
            if !crate::delegation::safe_task_id(&record.status.task)
                || self.task_index(&dir, &record.status.task).is_some()
                || record.status.worker.is_none()
            {
                continue;
            }
            if !record.status.state.ended() {
                record.status.state = TaskState::Failed;
                record.status.error = Some("The worker was interrupted by an app restart.".into());
                record.delivered = false;
            }
            if let Some(w) = record.status.worker.as_mut() {
                w.activity = None;
                w.needs_approval = false;
                if !w.released
                    && record
                        .status
                        .worktree
                        .as_ref()
                        .is_some_and(|p| matches!(Path::new(p).try_exists(), Ok(false)))
                {
                    record.status.error = Some(format!("Its work tree is missing at {}.", w.cwd));
                }
            }
            let task = DelegatedTask {
                dir: dir.clone(),
                parent: parent.to_string(),
                status: record.status,
                prompt: String::new(),
                plan_mode: false,
                prep_rx: None,
                ended_at: None,
                delivered: record.delivered,
                discard_pending: false,
                cleanup: None,
                checkout_lease: None,
                parent_deleted: false,
            };
            task.save();
            self.conv.delegations.push(task);
        }
        Ok(())
    }

    /// The chat whose agent writes to mailbox `dir`.
    fn delegation_parent(&self, dir: &Path) -> Option<(SessionKey, String)> {
        self.conv.workspaces.iter().enumerate().find_map(|(wi, w)| {
            (0..w.sessions.len()).find_map(|si| {
                let key = self.session_key(wi, si);
                let acp_key = self.acp_session_key(key);
                (crate::delegation::dir_for(&acp_key) == dir).then_some((key, acp_key))
            })
        })
    }

    /// The chat with session file `file`.
    fn chat_by_file(&self, file: &str) -> Option<SessionKey> {
        self.conv.workspaces.iter().enumerate().find_map(|(wi, w)| {
            w.sessions
                .iter()
                .position(|s| s.session_file.as_deref() == Some(file))
                .map(|si| self.session_key(wi, si))
        })
    }

    /// A worker's chat or work tree workspace, kept off the sidebar while it is listed above
    /// its lead's composer (until dismissed there, or the app restarts).
    pub(crate) fn hidden_worker_chat(&self, file: Option<&str>) -> bool {
        let Some(file) = file else {
            return false;
        };
        self.conv
            .delegations
            .iter()
            .any(|t| t.is_worker() && t.status.chat.as_deref() == Some(file))
    }

    pub(crate) fn hidden_worker_workspace(&self, root: &str) -> bool {
        self.conv
            .delegations
            .iter()
            .any(|t| t.is_worker() && t.status.worktree.as_deref() == Some(root))
    }

    /// Before each of a lead's turns: who it may hand work to, with quota left, in its mailbox.
    pub(crate) fn write_orchestrator_team(
        &mut self,
        key: SessionKey,
        settings: &crate::settings::AppSettings,
    ) {
        if !crate::delegation::available() {
            return;
        }
        let parent = self.acp_session_key(key);
        let dir = crate::delegation::dir_for(&parent);
        if crate::delegation::worker_records(&parent)
            .join("status")
            .is_dir()
        {
            let _ = self.claim_lead(&parent);
        }
        let configured = self.cached_configured_providers(&self.conv.git_ctx);
        let team = orchestrator_team(settings, &configured, self.is_remote_key(key));
        crate::orchestrator::write_team(&dir, &team);
    }

    /// Pick up requests, follow running tasks and hand finished reports to their parents.
    pub(crate) fn poll_delegations(&mut self, ctx: &egui::Context) {
        if REQUESTS_WAITING.swap(false, std::sync::atomic::Ordering::SeqCst) {
            self.take_delegation_requests();
        }
        if self.conv.delegations.is_empty() {
            return;
        }
        self.follow_delegations();
        ctx.request_repaint_after(Duration::from_millis(500));
    }

    /// Handle the requests waiting in this process's mailboxes.
    fn take_delegation_requests(&mut self) {
        let own = format!("{}-", std::process::id());
        let dirs: Vec<PathBuf> = std::fs::read_dir(crate::delegation::root())
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(&own))
            .map(|e| e.path())
            .collect();
        for dir in dirs {
            for request in crate::delegation::take_requests(&dir) {
                self.handle_delegation_request(&dir, request);
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn poll_delegations_now(&mut self, _ctx: &egui::Context) {
        // Other test apps share this process. Only take this fixture's requests.
        let dir = crate::delegation::dir_for(&self.acp_session_key(self.active_session_key()));
        for request in crate::delegation::take_requests(&dir) {
            self.handle_delegation_request(&dir, request);
        }
        self.follow_delegations();
    }

    fn task_index(&self, dir: &Path, task: &str) -> Option<usize> {
        self.conv
            .delegations
            .iter()
            .position(|t| t.status.task == task && t.dir == dir)
    }

    fn handle_delegation_request(&mut self, dir: &Path, request: Request) {
        match request {
            Request::Cancel { task } => {
                if let Some(index) = self.task_index(dir, &task) {
                    self.cancel_delegation(index);
                }
            }
            Request::Spawn { batch, workers } => self.spawn_workers(dir, &batch, workers),
            Request::Message { task, text, reply } => {
                let result = self.message_worker(dir, &task, text);
                crate::orchestrator::reply(dir, reply, &result);
            }
            Request::Merge { task, reply } => {
                if let Err(e) = self.begin_merge(dir, &task, Some(reply)) {
                    crate::orchestrator::reply(dir, reply, &Err(e));
                }
            }
            Request::Discard { task } => {
                if let Some(index) = self.task_index(dir, &task) {
                    self.discard_worker(index);
                }
            }
            Request::Delegate {
                task,
                provider,
                model,
                prompt,
                title,
                worktree,
            } => {
                let reject = |error: String| {
                    crate::delegation::write_status(
                        dir,
                        &Status {
                            task: task.clone(),
                            state: TaskState::Rejected,
                            provider: provider.clone(),
                            model: model.clone().unwrap_or_default(),
                            title: title.clone().unwrap_or_default(),
                            chat: None,
                            worktree: None,
                            result: None,
                            error: Some(error),
                            worker: None,
                        },
                    );
                };
                let Some((parent_key, parent)) = self.delegation_parent(dir) else {
                    reject("The chat that asked is not open in oxi.".into());
                    return;
                };
                let Some(kind) = crate::delegation::parse_provider(&provider) else {
                    reject(format!("Unknown provider `{provider}`."));
                    return;
                };
                if kind.is_pseudo() {
                    reject("Pick a provider, not the Router or the Orchestrator.".into());
                    return;
                }
                let mut config = self.conv.settings.provider(kind).clone();
                if let Some(model) = model.as_deref() {
                    config.model_id = model.to_string();
                }
                let title = title.unwrap_or_else(|| crate::model::make_session_title(&prompt));
                let parent_label = self.parent_label(parent_key);
                let mut status = Status {
                    task,
                    state: TaskState::Starting,
                    provider: kind.slug().to_string(),
                    model: config.model_id.clone(),
                    title,
                    chat: None,
                    worktree: None,
                    result: None,
                    error: None,
                    worker: None,
                };
                let plan_mode = self.run_state(parent_key).is_some_and(|r| r.plan_mode);
                let mut prep_rx = None;
                if worktree {
                    let root = &self.conv.workspaces[parent_key.workspace_idx].root_path;
                    if self.is_remote_key(parent_key) {
                        status.state = TaskState::Rejected;
                        status.error =
                            Some("Work trees are not available in remote workspaces.".into());
                        crate::delegation::write_status(dir, &status);
                        return;
                    }
                    let root = PathBuf::from(root);
                    prep_rx = Some(self.prepare_in_background(move || {
                        Ok(Prepared {
                            worktree: Some(crate::git::worktree::create(&root)?),
                            base: None,
                        })
                    }));
                }
                let mut task = DelegatedTask {
                    dir: dir.to_path_buf(),
                    parent,
                    status,
                    prompt: task_prompt(&parent_label, &prompt),
                    plan_mode,
                    prep_rx,
                    ended_at: None,
                    delivered: false,
                    discard_pending: false,
                    cleanup: None,
                    checkout_lease: None,
                    parent_deleted: false,
                };
                task.save();
                if task.prep_rx.is_none() {
                    self.start_delegated_chat(&mut task, parent_key.workspace_idx, &config);
                }
                self.conv.delegations.push(task);
            }
        }
    }

    /// Short name of the agent of chat `key`, for a delegated task's preamble.
    fn parent_label(&self, key: SessionKey) -> String {
        self.session_by_key(key)
            .config
            .as_ref()
            .map(|c| {
                if c.provider == crate::settings::LlmProviderKind::Orchestrator {
                    crate::app::composer::composer_provider_label(
                        self.conv.settings.orchestrator.lead_provider,
                    )
                } else {
                    crate::app::composer::composer_provider_label(c.provider)
                }
            })
            .unwrap_or("an agent")
            .to_string()
    }

    fn prepare_in_background(
        &self,
        work: impl FnOnce() -> Result<Prepared, String> + Send + 'static,
    ) -> Receiver<Result<Prepared, String>> {
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = self.conv.git_ctx.clone();
        std::thread::spawn(move || {
            if let Err(std::sync::mpsc::SendError(Ok(Prepared {
                worktree: Some(path),
                ..
            }))) = tx.send(work())
                && let Err(e) = crate::orchestrator::remove_worker_tree(&path)
            {
                log::warn!("cleaning up abandoned worker preparation: {e}");
            }
            ctx.request_repaint();
        });
        rx
    }

    fn active_worker(&self, t: &DelegatedTask) -> bool {
        t.is_worker()
            && (!t.status.state.ended()
                || t.status
                    .chat
                    .as_deref()
                    .and_then(|f| self.chat_by_file(f))
                    .is_some_and(|k| self.chat_is_busy(k)))
    }

    fn running_workers(&self, dir: &Path, except: Option<usize>) -> usize {
        self.conv
            .delegations
            .iter()
            .enumerate()
            .filter(|(i, t)| Some(*i) != except && t.dir == dir && self.active_worker(t))
            .count()
    }

    /// A lead's `spawn_workers`: check the team's limits, decide where each worker runs, and
    /// start them. Several editing workers at once each get a work tree; a single one edits the
    /// workspace.
    fn spawn_workers(&mut self, dir: &Path, batch: &str, workers: Vec<WorkerRequest>) {
        let parent = self.delegation_parent(dir);
        let ownership_error = parent
            .as_ref()
            .and_then(|(_, id)| self.claim_lead(id).err());
        let remote = parent
            .as_ref()
            .is_some_and(|(key, _)| self.is_remote_key(*key));
        let max = self.conv.settings.orchestrator.max_workers();
        let running = self.running_workers(dir, None);
        let editing_now = self
            .conv
            .delegations
            .iter()
            .filter(|t| {
                t.dir == dir
                    && !t.status.state.ended()
                    && t.status
                        .worker
                        .as_ref()
                        .is_some_and(|w| w.access == Access::Edit)
            })
            .count();
        let new_edits = workers.iter().filter(|w| w.access == Access::Edit).count();
        let isolate_all = !remote && editing_now + new_edits >= 2;
        let mut accepted = 0;
        let mut edits_accepted = 0;
        let mut live_team: Option<crate::orchestrator::Team> = None;
        let mut reroute_index = 0usize;
        for mut w in workers {
            let mut kind =
                crate::delegation::parse_provider(&w.provider).filter(|k| !k.is_pseudo());
            // Live quota check: if the chosen subscription hit 0% quota since team.json was
            // written (or on a custom model override), fall back to another team subscription
            // with quota left, regardless of its specialization note in Settings.
            if let Some(k) = kind {
                let effective_model = if w.model.trim().is_empty() {
                    self.conv.settings.provider(k).model_id.trim().to_string()
                } else {
                    w.model.trim().to_string()
                };
                if crate::router::quota::is_exhausted(k, &effective_model) {
                    let team = live_team.get_or_insert_with(|| {
                        let configured = self.cached_configured_providers(&self.conv.git_ctx);
                        let t = orchestrator_team(&self.conv.settings, &configured, remote);
                        crate::orchestrator::write_team(dir, &t);
                        t
                    });
                    if let Some(fallback) = team.fallback_member_for(reroute_index)
                        && (fallback.provider != w.provider
                            || !fallback.model.eq_ignore_ascii_case(&w.model))
                    {
                        reroute_index += 1;
                        if w.rerouted_from.is_none() {
                            w.rerouted_from =
                                Some(crate::app::composer::composer_provider_label(k).to_string());
                        }
                        w.provider = fallback.provider.clone();
                        w.model = fallback.model.clone();
                        kind = crate::delegation::parse_provider(&w.provider)
                            .filter(|k| !k.is_pseudo());
                    }
                }
            }
            let mut status = Status {
                task: w.task.clone(),
                state: TaskState::Starting,
                provider: w.provider.clone(),
                model: w.model.clone(),
                title: w.title.clone(),
                chat: None,
                worktree: None,
                result: None,
                error: None,
                worker: Some(WorkerInfo {
                    batch: batch.to_string(),
                    access: w.access,
                    rerouted_from: w.rerouted_from.clone(),
                    ..Default::default()
                }),
            };
            let problem = match (&parent, kind) {
                _ if ownership_error.is_some() => ownership_error.clone(),
                (None, _) => Some("The lead's chat is not open in oxi.".to_string()),
                (_, None) => Some(format!("Unknown agent `{}`.", w.provider)),
                _ if running + accepted >= max => Some(format!(
                    "At most {max} workers run at once (Settings > Orchestrator); wait for one to end."
                )),
                _ if remote && w.access == Access::Edit && editing_now + edits_accepted >= 1 => {
                    Some("Remote workspace: one editing worker at a time.".into())
                }
                _ => None,
            };
            if let Some(problem) = problem {
                status.state = TaskState::Rejected;
                status.error = Some(problem);
                crate::delegation::write_status(dir, &status);
                continue;
            }
            let (Some((parent_key, parent_acp)), Some(kind)) = (parent.clone(), kind) else {
                continue;
            };
            let mut own_tree = w.access == Access::Edit && !remote && (isolate_all || w.isolate);
            let root = self.conv.workspaces[parent_key.workspace_idx]
                .root_path
                .clone();
            let mut checkout_lease = None;
            if w.access == Access::Edit && !own_tree {
                match crate::delegation::reserve_checkout(&checkout_path(&root)) {
                    Ok(Some(lease)) => checkout_lease = Some(lease),
                    Ok(None) if !remote => own_tree = true,
                    result => {
                        status.state = TaskState::Rejected;
                        status.error = Some(result.err().unwrap_or_else(|| {
                            "Another worker is directly editing this checkout.".into()
                        }));
                        crate::delegation::write_status(dir, &status);
                        continue;
                    }
                }
            }
            accepted += 1;
            if w.access == Access::Edit {
                edits_accepted += 1;
            }
            if let Some(info) = status.worker.as_mut() {
                info.root = root.clone();
                info.cwd = root.clone();
            }
            let lead = self.parent_label(parent_key);
            let plan_mode = w.access == Access::ReadOnly
                || self.run_state(parent_key).is_some_and(|r| r.plan_mode);
            let mut task = DelegatedTask {
                dir: dir.to_path_buf(),
                parent: parent_acp,
                status,
                prompt: worker_prompt(&lead, w.access, own_tree, &w.prompt),
                plan_mode,
                prep_rx: None,
                ended_at: None,
                delivered: false,
                discard_pending: false,
                cleanup: None,
                checkout_lease,
                parent_deleted: false,
            };
            if remote {
                // Git runs on the other machine: no snapshot, no change summary.
                task.save();
                let mut config = self.conv.settings.provider(kind).clone();
                if !task.status.model.is_empty() {
                    config.model_id = task.status.model.clone();
                }
                self.start_delegated_chat(&mut task, parent_key.workspace_idx, &config);
            } else {
                let root = PathBuf::from(root);
                task.prep_rx = Some(self.prepare_in_background(move || {
                    // Not a git repository: the worker still runs, without a change summary.
                    let base = crate::git::handoff::snapshot(&root).ok();
                    let worktree = if own_tree {
                        let base = base.as_deref().ok_or(
                            "The workspace is not a git repository with commits, so workers cannot get work trees",
                        )?;
                        Some(crate::git::worktree::create_at(&root, Some(base))?)
                    } else {
                        None
                    };
                    Ok(Prepared { worktree, base })
                }));
                task.save();
            }
            self.conv.delegations.push(task);
        }
    }

    fn reserve_followup(&mut self, index: usize) -> Result<(), String> {
        let t = &self.conv.delegations[index];
        let w = t.status.worker.as_ref().ok_or("Not a worker.")?;
        let max = self.conv.settings.orchestrator.max_workers();
        if self.running_workers(&t.dir, Some(index)) >= max {
            return Err(format!(
                "At most {max} workers run at once. Wait for one to end."
            ));
        }
        if w.access == Access::Edit && w.root == w.cwd && t.checkout_lease.is_none() {
            let lease = crate::delegation::reserve_checkout(&checkout_path(&w.root))?
                .ok_or("Another worker is directly editing this checkout. Wait for it to end.")?;
            self.conv.delegations[index].checkout_lease = Some(lease);
        }
        Ok(())
    }

    /// The worker chat can also be opened directly in the UI. Check the same reservation at
    /// dispatch, including retries and manually submitted turns.
    pub(super) fn admit_worker_turn(&mut self, key: SessionKey) -> Result<(), String> {
        if let Some(parent) = self
            .session_by_key(key)
            .config
            .as_ref()
            .and_then(|c| c.lead.clone())
        {
            self.claim_lead(&parent)?;
        }
        let Some(file) = self.session_by_key(key).session_file.as_deref() else {
            return Ok(());
        };
        let Some(index) = self
            .conv
            .delegations
            .iter()
            .position(|t| t.is_worker() && t.status.chat.as_deref() == Some(file))
        else {
            return Ok(());
        };
        let t = &self.conv.delegations[index];
        if t.cleanup.is_some()
            || t.discard_pending
            || t.status.worker.as_ref().is_some_and(|w| w.released)
        {
            return Err("This worker is reserved for cleanup or has been released.".into());
        }
        if t.status.state.ended() {
            self.reserve_followup(index)?;
            self.resume_worker(index);
        }
        Ok(())
    }

    fn resume_worker(&mut self, index: usize) {
        let t = &mut self.conv.delegations[index];
        let _ = std::fs::remove_file(crate::delegation::fetched_path(&t.dir, &t.status.task));
        t.status.state = TaskState::Running;
        t.status.result = None;
        t.status.error = None;
        t.ended_at = None;
        t.delivered = false;
        if let Some(w) = t.status.worker.as_mut() {
            w.turns += 1;
            w.activity = None;
        }
        t.save();
    }

    /// A lead's follow-up for one of its workers, sent into the worker's chat.
    fn message_worker(&mut self, dir: &Path, task: &str, text: String) -> Result<String, String> {
        let index = self.task_index(dir, task).ok_or("Unknown worker.")?;
        let t = &self.conv.delegations[index];
        let w = t.status.worker.as_ref().ok_or("Not a worker.")?;
        if w.released || t.discard_pending || t.cleanup.is_some() {
            return Err(
                "Its work tree is being merged, discarded, or has already been released.".into(),
            );
        }
        let key = t
            .status
            .chat
            .as_deref()
            .and_then(|f| self.chat_by_file(f))
            .ok_or("Its chat is gone.")?;
        if !t.status.state.ended() || self.chat_is_busy(key) {
            return Err("The worker is still running.".into());
        }
        if self.setup_holding_agent(key.workspace_idx).is_some() {
            return Err("Work tree setup is holding the worker.".into());
        }
        let access = w.access;
        self.reserve_followup(index)?;
        if access == Access::ReadOnly {
            self.run_state_mut(key).plan_mode = true;
        }
        self.resume_worker(index);
        self.start_delegated_prompt(key, &text);
        Ok(format!(
            "Sent to {task}. Its new report comes back when it ends."
        ))
    }

    /// Cancel running workers, trigger discard/cleanup for unreleased work trees, and remove
    /// persisted worker records when an orchestrator lead chat is deleted. Entries with an active
    /// background cleanup, pending discard, or worktree preparation stay in `self.conv.delegations`
    /// until removal finishes cleanly.
    pub(super) fn cleanup_deleted_lead(&mut self, parent_acp_key: &str) {
        let indices: Vec<usize> = self
            .conv
            .delegations
            .iter()
            .enumerate()
            .filter(|(_, t)| t.parent == parent_acp_key)
            .map(|(i, _)| i)
            .collect();
        for i in indices {
            self.conv.delegations[i].parent_deleted = true;
            self.conv.delegations[i].delivered = true;
            self.cancel_delegation(i);
            let chat = self.conv.delegations[i].status.chat.clone();
            if let Some(key) = chat.as_deref().and_then(|f| self.chat_by_file(f)) {
                let worker_acp = self.acp_session_key(key);
                self.acp.close(&worker_acp);
            }
            let has_unreleased_tree = {
                let t = &self.conv.delegations[i];
                t.status
                    .worker
                    .as_ref()
                    .is_some_and(|w| !w.released && w.cwd != w.root)
                    || (t.status.worktree.is_some()
                        && t.status.worker.as_ref().is_none_or(|w| !w.released))
            };
            if has_unreleased_tree {
                self.discard_worker(i);
            }
        }
        self.conv.worker_owners.remove(parent_acp_key);
        let _ = std::fs::remove_dir_all(crate::delegation::worker_records(parent_acp_key));
        self.conv.delegations.retain(|t| {
            t.parent != parent_acp_key
                || t.cleanup.is_some()
                || t.discard_pending
                || t.prep_rx.is_some()
        });
    }

    /// Remove worker `index`'s work tree (its changes were merged, or are dropped); a running
    /// worker is stopped first.
    fn discard_worker(&mut self, index: usize) {
        if self.conv.delegations[index].cleanup.is_some() {
            return;
        }
        self.cancel_delegation(index);
        let t = &mut self.conv.delegations[index];
        if t.status.worker.as_ref().is_some_and(|w| w.released) {
            return;
        }
        t.discard_pending = true;
        self.finish_discards();
    }

    fn begin_merge(&mut self, dir: &Path, id: &str, reply: Option<u64>) -> Result<(), String> {
        let i = self.task_index(dir, id).ok_or("Unknown worker.")?;
        let t = &self.conv.delegations[i];
        let w = t.status.worker.as_ref().ok_or("Not a worker.")?.clone();
        if self.active_worker(t)
            || t.discard_pending
            || t.cleanup.is_some()
            || w.released
            || t.prep_rx.is_some()
        {
            return Err(format!(
                "Worker {id} is active, being cleaned up, or already released."
            ));
        }
        if self
            .conv
            .worktree_setups
            .iter()
            .any(|s| s.root == w.cwd && s.state == super::worktree_setup::SetupState::Running)
        {
            return Err("Work tree setup is still running.".into());
        }
        if w.cwd == w.root {
            let msg = format!(
                "Worker {id} worked in the workspace itself. Its changes are already there."
            );
            if let Some(reply) = reply {
                crate::orchestrator::reply(dir, reply, &Ok(msg));
            }
            return Ok(());
        }
        let id = id.to_string();
        self.start_cleanup(i, reply, move || {
            crate::orchestrator::merge_worktree(&id, &w)
        });
        Ok(())
    }

    fn start_cleanup(
        &mut self,
        index: usize,
        reply: Option<u64>,
        work: impl FnOnce() -> Result<String, String> + Send + 'static,
    ) {
        let (tx, rx) = std::sync::mpsc::channel();
        self.conv.delegations[index].cleanup = Some(Cleanup { rx, reply });
        let ctx = self.conv.git_ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(work());
            ctx.request_repaint();
        });
    }

    fn finish_discards(&mut self) {
        for i in 0..self.conv.delegations.len() {
            let t = &self.conv.delegations[i];
            if let Some(op) = &t.cleanup {
                let result = match op.rx.try_recv() {
                    Ok(r) => r,
                    Err(TryRecvError::Empty) => continue,
                    Err(TryRecvError::Disconnected) => {
                        Err("Worker cleanup was interrupted.".into())
                    }
                };
                let reply = op.reply;
                let root = t.status.worktree.clone();
                let parent_deleted = t.parent_deleted;
                let t = &mut self.conv.delegations[i];
                t.cleanup = None;
                t.discard_pending = false;
                if let Some(w) = t.status.worker.as_mut() {
                    w.released = result.is_ok();
                }
                if let Err(e) = &result {
                    t.status.error = Some(e.clone());
                }
                t.save();
                if let Some(reply) = reply {
                    crate::orchestrator::reply(&t.dir, reply, &result);
                }
                if result.is_ok()
                    && let Some(wi) = self
                        .conv
                        .workspaces
                        .iter()
                        .position(|w| Some(&w.root_path) == root.as_ref())
                {
                    for si in 0..self.conv.workspaces[wi].sessions.len() {
                        self.acp
                            .close(&self.acp_session_key(self.session_key(wi, si)));
                    }
                    self.delete_workspace(wi);
                }
                self.request(crate::git::GitOp::Refresh);
                self.conv.explorer.cache.invalidate();
                if !parent_deleted || result.is_err() {
                    self.notify_composer(result.unwrap_or_else(|e| e));
                }
            } else if t.discard_pending
                && t.prep_rx.is_none()
                && !self.conv.worktree_setups.iter().any(|s| {
                    Some(&s.root) == t.status.worktree.as_ref()
                        && s.state == super::worktree_setup::SetupState::Running
                })
                && t.status
                    .chat
                    .as_deref()
                    .and_then(|f| self.chat_by_file(f))
                    .is_none_or(|k| !self.chat_is_busy(k))
            {
                let path = t.status.worktree.clone();
                self.start_cleanup(i, None, move || {
                    if let Some(path) = path {
                        crate::orchestrator::remove_worker_tree(Path::new(&path))?;
                    }
                    Ok("Worker released.".into())
                });
            }
        }
        self.conv.delegations.retain(|t| {
            !t.parent_deleted || t.cleanup.is_some() || t.discard_pending || t.prep_rx.is_some()
        });
    }

    /// Open the task's chat in workspace `wi` and send it the task.
    fn start_delegated_chat(
        &mut self,
        task: &mut DelegatedTask,
        wi: usize,
        config: &crate::settings::ProviderConfig,
    ) {
        let mut session = Self::blank_session(task.status.title.clone());
        let mut session_config = crate::model::SessionConfig::from_provider(config);
        if task.is_worker() {
            session_config.lead = Some(task.parent.clone());
            session_config.worker_access = task.status.worker.as_ref().expect("worker").access;
        }
        session.config = Some(session_config);
        // Appended, so no other chat's key moves.
        self.conv.workspaces[wi].sessions.push(session);
        let key = self.session_key(wi, self.conv.workspaces[wi].sessions.len() - 1);
        // A held prompt has not materialized a transcript yet. Give it an identity before
        // queueing it so cancellation and restart recovery own the same chat and queue.
        if self.setup_holding_agent(wi).is_some() {
            let root = self.conv.workspaces[wi].root_path.clone();
            if let Err(e) =
                crate::session_store::save_session_messages(&root, self.session_mut_by_key(key))
            {
                task.status.state = TaskState::Rejected;
                task.status.error =
                    Some(format!("Could not save the worker chat before setup: {e}"));
                task.ended_at = Some(Instant::now());
                task.save();
                return;
            }
        }
        self.run_state_mut(key).plan_mode = task.plan_mode;
        self.send_queued(
            key,
            QueuedSend {
                text: task.prompt.clone(),
                images: Vec::new(),
                texts: Vec::new(),
            },
            true,
        );
        // The first prompt names a new chat; the task's own title is clearer.
        self.session_mut_by_key(key).title = task.status.title.clone();
        if !self.conn.no_session && !self.is_remote_key(key) {
            let root = self.conv.workspaces[wi].root_path.clone();
            if let Err(e) =
                crate::session_store::save_session_messages(&root, self.session_mut_by_key(key))
            {
                log::warn!("saving a delegated chat: {e}");
            }
        }
        task.status.chat = self.session_by_key(key).session_file.clone();
        task.status.state = TaskState::Running;
        if let Some(w) = task.status.worker.as_mut() {
            w.turns = 1;
        }
        task.save();
    }

    /// Advance tasks: work trees that finished being created, chats that ended, reports due.
    fn follow_delegations(&mut self) {
        let mut tasks = std::mem::take(&mut self.conv.delegations);
        for task in &mut tasks {
            if let Some(rx) = task.prep_rx.as_ref() {
                match rx.try_recv() {
                    Err(TryRecvError::Empty) => continue,
                    result => {
                        task.prep_rx = None;
                        match result.unwrap_or_else(|_| Err("the work tree was not created".into()))
                        {
                            Ok(prepared) if task.status.state == TaskState::Cancelled => {
                                if let Some(path) = prepared.worktree {
                                    let path = path.to_string_lossy().into_owned();
                                    task.status.worktree = Some(path.clone());
                                    if let Some(w) = task.status.worker.as_mut() {
                                        w.cwd = path;
                                        w.base = prepared.base;
                                    }
                                    task.discard_pending = true;
                                    task.save();
                                }
                            }
                            Err(_) if task.status.state == TaskState::Cancelled => {}
                            Ok(prepared) => self.start_prepared(task, prepared),
                            Err(e) => {
                                task.status.state = TaskState::Rejected;
                                task.status.error = Some(if task.is_worker() {
                                    e
                                } else {
                                    format!("Could not create a work tree: {e}")
                                });
                                task.save();
                                task.ended_at = Some(Instant::now());
                            }
                        }
                    }
                }
            }
            if task.status.state == TaskState::Running {
                self.check_delegated_chat(task);
            }
            if task.checkout_lease.is_some() && !self.active_worker(task) {
                task.checkout_lease = None;
            }
        }
        // Reports: a plain task's as soon as it ended; a worker's once its batch ended.
        for i in 0..tasks.len() {
            let task = &tasks[i];
            if !task.status.state.ended() || task.delivered {
                continue;
            }
            if task
                .ended_at
                .is_some_and(|at| at.elapsed() < DELIVERY_DELAY)
            {
                continue;
            }
            let Some(batch) = task.status.worker.as_ref().map(|w| w.batch.clone()) else {
                tasks[i].delivered = true;
                if !crate::delegation::fetched(&tasks[i].dir, &tasks[i].status.task) {
                    self.deliver_text(&tasks[i].parent, report_message(&tasks[i].status));
                }
                continue;
            };
            let dir = task.dir.clone();
            let mates: Vec<usize> = (0..tasks.len())
                .filter(|&j| {
                    tasks[j].dir == dir
                        && tasks[j]
                            .status
                            .worker
                            .as_ref()
                            .is_some_and(|w| w.batch == batch)
                        && !tasks[j].delivered
                })
                .collect();
            let waiting = mates.iter().any(|&j| {
                !tasks[j].status.state.ended()
                    || tasks[j]
                        .ended_at
                        .is_some_and(|at| at.elapsed() < DELIVERY_DELAY)
            });
            if waiting {
                continue;
            }
            let unread: Vec<&Status> = mates
                .iter()
                .map(|&j| &tasks[j])
                .filter(|t| !crate::delegation::fetched(&t.dir, &t.status.task))
                // Stopped by the lead itself: nothing to tell it.
                .filter(|t| t.status.state != TaskState::Cancelled)
                .map(|t| &t.status)
                .collect();
            if !unread.is_empty() {
                let text = workers_report(&unread);
                let parent = tasks[i].parent.clone();
                let Some(key) = self.session_key_for_acp(&parent) else {
                    continue;
                };
                self.deliver_to_chat(key, text);
            }
            for j in mates {
                tasks[j].delivered = true;
                tasks[j].save();
            }
        }
        // Ended and reported tasks stay listed with their parent until the app restarts.
        self.conv.delegations = tasks;
        self.finish_discards();
    }

    /// Start a task whose work tree / snapshot is ready.
    fn start_prepared(&mut self, task: &mut DelegatedTask, prepared: Prepared) {
        let wi = match &prepared.worktree {
            Some(path) => {
                let wi = self.add_background_worktree(path);
                task.status.worktree = Some(path.to_string_lossy().into_owned());
                wi
            }
            None => {
                let root = task.status.worker.as_ref().map(|w| w.root.clone());
                match root.and_then(|r| self.conv.workspaces.iter().position(|w| w.root_path == r))
                {
                    Some(wi) => wi,
                    None => {
                        task.status.state = TaskState::Rejected;
                        task.status.error = Some("Its workspace is no longer open.".into());
                        task.save();
                        task.ended_at = Some(Instant::now());
                        return;
                    }
                }
            }
        };
        if let Some(w) = task.status.worker.as_mut() {
            w.base = prepared.base;
            if let Some(path) = &prepared.worktree {
                w.cwd = path.to_string_lossy().into_owned();
            }
        }
        let kind = crate::delegation::parse_provider(&task.status.provider).unwrap_or_default();
        let mut config = self.conv.settings.provider(kind).clone();
        if !task.status.model.is_empty() {
            config.model_id = task.status.model.clone();
        }
        self.start_delegated_chat(task, wi, &config);
    }

    /// Whether the task's chat has answered; record how it ended. A running worker's current
    /// activity is recorded for its lead.
    fn check_delegated_chat(&mut self, task: &mut DelegatedTask) {
        let Some(file) = task.status.chat.clone() else {
            return;
        };
        let Some(key) = self.chat_by_file(&file) else {
            task.status.state = TaskState::Failed;
            task.status.error = Some("Its chat was deleted.".into());
            task.ended_at = Some(Instant::now());
            task.save();
            return;
        };
        if self.chat_is_busy(key) || self.setup_holding_agent(key.workspace_idx).is_some() {
            if task.is_worker() {
                let messages = &self.session_by_key(key).messages;
                let activity = activity_of(messages);
                let routed = messages
                    .last()
                    .filter(|m| m.role == MsgRole::Assistant)
                    .and_then(|m| m.route.as_deref())
                    .map(|r| (r.provider, r.model.clone()));
                let needs_approval = self
                    .run_state(key)
                    .is_some_and(|r| r.pending_approval.is_some());
                let mut changed = sync_worker_route(task, routed);
                let w = task.status.worker.as_mut().expect("a worker");
                if w.activity != activity || w.needs_approval != needs_approval {
                    w.activity = activity;
                    w.needs_approval = needs_approval;
                    changed = true;
                }
                if changed {
                    task.save();
                }
            }
            return;
        }
        let messages = &self.session_by_key(key).messages;
        let Some(start) = messages.iter().rposition(|m| m.role == MsgRole::User) else {
            return;
        };
        let reply = &messages[start + 1..];
        if reply.is_empty() || reply.iter().any(|m| m.streaming) {
            return;
        }
        let routed = reply
            .iter()
            .rev()
            .find_map(|m| m.route.as_deref().map(|r| (r.provider, r.model.clone())));
        let rerouted = sync_worker_route(task, routed);
        let answer = crate::model::last_answer(reply).map(str::to_owned);
        let any_tool_failed = reply.iter().any(|m| {
            m.blocks.iter().any(|b| match b {
                AssistantBlock::Tool {
                    is_error, metadata, ..
                } => {
                    *is_error == Some(true)
                        || metadata
                            .as_deref()
                            .is_some_and(|meta| meta.status == crate::model::ToolStatus::Failed)
                }
                _ => false,
            })
        });
        let failed = crate::model::reply_failed(reply) || answer.is_none();
        task.status.state = if failed {
            if task.status.error.is_none() {
                task.status.error = Some(if answer.is_none() && any_tool_failed {
                    "Ended without a report after a tool call failed; open its chat for details."
                        .into()
                } else if answer.is_none() {
                    "Ended without a report; open its chat for details.".into()
                } else {
                    "Its response failed; open its chat for details.".into()
                });
            }
            TaskState::Failed
        } else {
            TaskState::Completed
        };
        task.status.result = answer;
        if let Some(w) = task.status.worker.as_mut() {
            w.activity = None;
            w.needs_approval = false;
        }
        task.ended_at = Some(Instant::now());
        task.save();
        if task.is_worker() && (rerouted || failed) {
            let remote = self.is_remote_key(key);
            let configured = self.cached_configured_providers(&self.conv.git_ctx);
            let team = orchestrator_team(&self.conv.settings, &configured, remote);
            crate::orchestrator::write_team(&task.dir, &team);
        }
    }

    /// Send `text` into the chat whose agent session key is `parent`: right away when it is
    /// idle, into the running turn when it can take it, else after the turn.
    fn deliver_text(&mut self, parent: &str, text: String) {
        let Some(parent) = self.conv.workspaces.iter().enumerate().find_map(|(wi, w)| {
            (0..w.sessions.len())
                .map(|si| self.session_key(wi, si))
                .find(|key| self.acp_session_key(*key) == parent)
        }) else {
            return;
        };
        self.deliver_to_chat(parent, text);
    }

    fn deliver_to_chat(&mut self, parent: SessionKey, text: String) {
        if !self.chat_is_busy(parent) {
            self.send_queued(
                parent,
                QueuedSend {
                    text,
                    images: Vec::new(),
                    texts: Vec::new(),
                },
                false,
            );
            return;
        }
        let run = self.run_state_mut(parent);
        if run.waiting_response
            && let Some(inbox) = run.steer.clone()
        {
            run.steer_pending.push(text.clone());
            inbox.push(text);
            return;
        }
        run.queued.push_back(QueuedSend {
            text,
            images: Vec::new(),
            texts: Vec::new(),
        });
        self.persist_queue(parent);
    }

    fn cancel_delegation(&mut self, index: usize) {
        let task = &self.conv.delegations[index];
        if task.status.state.ended() {
            return;
        }
        let chat = task.status.chat.clone();
        if let Some(key) = chat.as_deref().and_then(|f| self.chat_by_file(f)) {
            if let Some(root) = task.status.worktree.as_deref() {
                self.cancel_worker_setup(root);
            }
            if self.chat_is_busy(key) {
                self.stop_agent_run(key);
            }
            self.run_state_mut(key).queued.clear();
            self.persist_queue(key);
        }
        let task = &mut self.conv.delegations[index];
        task.status.state = TaskState::Cancelled;
        task.ended_at = Some(Instant::now());
        // The parent asked for it (or the user stopped it here): nothing to report back.
        task.delivered = true;
        if let Some(w) = task.status.worker.as_mut() {
            w.activity = None;
            w.needs_approval = false;
        }
        task.save();
    }

    /// Add the work tree at `path` to the sidebar without switching to it, and run the
    /// repository's setup script there. Returns its workspace index.
    fn add_background_worktree(&mut self, path: &Path) -> usize {
        let root = path.to_string_lossy().into_owned();
        self.conv.workspaces.push(Workspace {
            root_path: root.clone(),
            sessions: Vec::new(),
            active: 0,
            sidebar_folded: false,
            pinned: Vec::new(),
            archived: Vec::new(),
            folded_groups: Vec::new(),
            worktree: crate::git::worktree::info(path),
        });
        self.sync_workspaces_to_settings();
        self.start_worktree_setup(&root);
        self.conv.workspaces.len() - 1
    }

    /// Compact bar above the composer when viewing an orchestrator worker chat, with a quick
    /// button to jump back to its lead chat.
    pub(super) fn render_worker_lead_bar(&mut self, ui: &mut Ui) {
        let key = self.active_session_key();
        let (lead_acp_key, access, session_file) = {
            let session = self.session_by_key(key);
            let Some(cfg) = session.config.as_ref() else {
                return;
            };
            let Some(lead) = cfg.lead.clone() else {
                return;
            };
            (lead, cfg.worker_access, session.session_file.clone())
        };
        let lead_key = self.session_key_for_acp(&lead_acp_key).or_else(|| {
            lead_acp_key
                .strip_suffix("#lead")
                .and_then(|file| self.chat_by_file(file))
        });
        let worker_info = session_file.as_deref().and_then(|file| {
            self.conv
                .delegations
                .iter()
                .find(|t| t.is_worker() && t.status.chat.as_deref() == Some(file))
                .and_then(|t| t.status.worker.as_ref())
        });
        let summary = match worker_info {
            Some(w) if w.access == Access::ReadOnly => "read-only",
            Some(w) if w.cwd != w.root && w.released => "work tree merged / removed",
            Some(w) if w.cwd != w.root => "own work tree",
            Some(_) => "edits workspace",
            None if access == Access::ReadOnly => "read-only",
            None if self.conv.workspaces[key.workspace_idx].worktree.is_some() => "own work tree",
            None => "edits workspace",
        };
        let mut label = format!("Worker · {summary}");
        if let Some(from) = worker_info.and_then(|w| w.rerouted_from.as_deref()) {
            label.push_str(&format!(" · rerouted from {from} (out of quota)"));
        }
        let lead_hover = lead_key
            .map(|k| format!("Return to lead chat: {}", self.session_by_key(k).title))
            .unwrap_or_else(|| "Return to the lead chat".into());
        let mut go_back = false;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if lead_key.is_some()
                    && crate::ui::chrome::mini_button(ui, "← Back to lead")
                        .on_hover_text(&lead_hover)
                        .clicked()
                {
                    go_back = true;
                }
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.add(
                        egui::Label::new(
                            RichText::new(&label).size(FS_SMALL).color(c_text_muted()),
                        )
                        .truncate()
                        .selectable(false),
                    )
                    .on_hover_text(&label);
                });
            });
        });
        ui.add_space(4.0);
        if go_back && let Some(target) = lead_key {
            self.select_session_in_workspace(target.workspace_idx, target.session_idx);
        }
    }

    /// The active chat's delegated tasks and workers, above its composer: state, what each is
    /// doing, a way to open each one's chat, and to stop one still running.
    pub(super) fn render_delegations(&mut self, ui: &mut Ui) {
        let key = self.active_session_key();
        let parent = self.acp_session_key(key);
        struct Row {
            index: usize,
            label: String,
            state: TaskState,
            chat: Option<String>,
            error: Option<String>,
            detail: Option<String>,
            rerouted_from: Option<String>,
            needs_approval: bool,
            discardable: bool,
        }
        let rows: Vec<Row> = self
            .conv
            .delegations
            .iter()
            .enumerate()
            .filter(|(_, t)| t.parent == parent)
            .map(|(i, t)| {
                let who = crate::delegation::parse_provider(&t.status.provider)
                    .map(|k| crate::app::composer::composer_provider_label(k).to_string())
                    .unwrap_or_else(|| t.status.provider.clone());
                let who = if t.is_worker() && !t.status.model.is_empty() {
                    format!("{who} · {}", t.status.model)
                } else {
                    who
                };
                let w = t.status.worker.as_ref();
                let rerouted_from = w.and_then(|w| w.rerouted_from.clone());
                let detail = w.and_then(|w| {
                    if !t.status.state.ended() {
                        w.activity.clone()
                    } else if w.released {
                        Some("merged / removed".into())
                    } else if w.access == Access::Edit && w.cwd != w.root {
                        Some("changes in its work tree".into())
                    } else {
                        None
                    }
                });
                Row {
                    index: i,
                    label: format!("{who} · {}", t.status.title),
                    state: t.status.state,
                    chat: t.status.chat.clone(),
                    error: t.status.error.clone(),
                    detail,
                    rerouted_from,
                    needs_approval: w.is_some_and(|w| w.needs_approval),
                    discardable: w.is_some_and(|w| !w.released && w.cwd != w.root)
                        && !t.discard_pending
                        && t.cleanup.is_none(),
                }
            })
            .collect();
        if rows.is_empty() {
            return;
        }
        let mut open = None;
        let mut stop = None;
        let mut dismiss = None;
        let mut discard = None;
        let mut merge = None;
        for row in &rows {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let (text, color) = match row.state {
                    _ if row.needs_approval => ("Needs approval", c_warning_fg()),
                    TaskState::Starting => ("Starting", c_text_muted()),
                    TaskState::Running if self.conv.delegations[row.index].is_worker() => {
                        ("Working", c_accent())
                    }
                    TaskState::Running => ("Delegated", c_accent()),
                    TaskState::Completed => ("Done", c_success()),
                    TaskState::Failed => ("Failed", c_error_fg()),
                    TaskState::Cancelled => ("Stopped", c_text_muted()),
                    TaskState::Rejected => ("Not started", c_error_fg()),
                };
                if matches!(row.state, TaskState::Starting | TaskState::Running) {
                    crate::theme::small_spinner(ui);
                }
                let reroute_note = row
                    .rerouted_from
                    .as_deref()
                    .map(|from| format!("rerouted from {from} (out of quota)"));
                let state_label = ui.label(RichText::new(text).size(FS_SMALL).color(color));
                if let Some(error) = &row.error {
                    state_label.on_hover_text(error);
                } else if let Some(note) = &reroute_note {
                    state_label.on_hover_text(note);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let in_use = self.conv.delegations[row.index].worker_in_use();
                    if row.state.ended()
                        && !in_use && self.conv.delegations[row.index].prep_rx.is_none() && self.conv.delegations[row.index].cleanup.is_none()
                        && crate::ui::chrome::dismiss_button(ui)
                            .on_hover_text("Dismiss")
                            .clicked()
                    {
                        dismiss = Some(row.index);
                    }
                    if row.state.ended()
                        && row.discardable
                        && crate::ui::chrome::mini_button(ui, "Discard")
                            .on_hover_text("Remove its work tree and its changes")
                            .clicked()
                    {
                        discard = Some(row.index);
                    }
                    if row.state.ended()
                        && row.discardable
                        && crate::ui::chrome::mini_button(ui, "Merge")
                            .on_hover_text(
                                "Apply its changes to the workspace as uncommitted changes, then remove its work tree",
                            )
                            .clicked()
                    {
                        merge = Some(row.index);
                    }
                    if !row.state.ended()
                        && crate::ui::chrome::mini_button(ui, "Stop")
                            .on_hover_text("Stop this task")
                            .clicked()
                    {
                        stop = Some(row.index);
                    }
                    if let Some(chat) = &row.chat
                        && crate::ui::chrome::mini_button(ui, "Open")
                            .on_hover_text("Show the task's chat")
                            .clicked()
                    {
                        open = Some(chat.clone());
                    }
                    // Why it failed, after its title: the report only reaches the parent agent.
                    let detail_text = match (&reroute_note, &row.detail) {
                        (Some(note), Some(detail)) => Some(format!("{note} · {detail}")),
                        (Some(note), None) => Some(note.clone()),
                        (None, Some(detail)) => Some(detail.clone()),
                        (None, None) => None,
                    };
                    let text = match (&row.error, &reroute_note, &detail_text) {
                        (Some(error), Some(note), _) => {
                            format!("{} · {note} — {}", row.label, one_line(error))
                        }
                        (Some(error), None, _) => format!("{} — {}", row.label, one_line(error)),
                        (None, _, Some(detail)) => format!("{} — {detail}", row.label),
                        (None, _, None) => row.label.clone(),
                    };
                    let hover = match (&row.error, &reroute_note) {
                        (Some(error), Some(note)) => format!("{note}\n{error}"),
                        (Some(error), None) => error.clone(),
                        (None, _) => text.clone(),
                    };
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.add(
                            egui::Label::new(RichText::new(&text).size(FS_SMALL))
                                .truncate()
                                .selectable(false),
                        )
                        .on_hover_text(hover);
                    });
                });
            });
        }
        ui.add_space(4.0);
        if let Some(index) = merge {
            let t = &self.conv.delegations[index];
            let (dir, id) = (t.dir.clone(), t.status.task.clone());
            if let Err(e) = self.begin_merge(&dir, &id, None) {
                self.notify_composer(e);
            }
        }
        if let Some(index) = stop {
            self.cancel_delegation(index);
        } else if let Some(index) = discard {
            self.discard_worker(index);
        } else if let Some(index) = dismiss
            && self.conv.delegations[index].delivered
        {
            let t = self.conv.delegations.remove(index);
            let _ = std::fs::remove_file(crate::delegation::status_path(
                &crate::delegation::worker_records(&t.parent),
                &t.status.task,
            ));
            let _ = std::fs::remove_file(crate::delegation::status_path(&t.dir, &t.status.task));
        }
        if let Some(file) = open
            && let Some(key) = self.chat_by_file(&file)
        {
            self.select_session_in_workspace(key.workspace_idx, key.session_idx);
        }
    }
}

#[cfg(test)]
impl OxiApp {
    /// A worker listed under the chat `parent` (gallery stills), without running anything.
    pub(crate) fn push_demo_worker(&mut self, parent: SessionKey, status: Status) {
        let parent = self.acp_session_key(parent);
        let dir = crate::delegation::dir_for(&parent);
        self.conv.delegations.push(DelegatedTask {
            dir,
            parent,
            status,
            prompt: String::new(),
            plan_mode: false,
            prep_rx: None,
            ended_at: None,
            delivered: true,
            discard_pending: false,
            cleanup: None,
            checkout_lease: None,
            parent_deleted: false,
        });
    }
}

/// The lead's team: the worker slots from Settings > Orchestrator, or, when there are none,
/// every configured agent with its selected model. `configured` lists the configured providers.
pub(crate) fn orchestrator_team(
    settings: &crate::settings::AppSettings,
    configured: &[crate::settings::LlmProviderKind],
    remote: bool,
) -> crate::orchestrator::Team {
    use crate::settings::WorkerSlot;
    let orch = &settings.orchestrator;
    let slots: Vec<WorkerSlot> = if !orch.workers.is_empty() {
        orch.workers
            .iter()
            .filter(|s| s.enabled && !s.provider.is_pseudo())
            .cloned()
            .collect()
    } else {
        let mut kinds: Vec<_> = configured
            .iter()
            .copied()
            .filter(|k| k.is_acp() && !k.is_pseudo())
            .collect();
        if !kinds.contains(&orch.lead_provider) && !orch.lead_provider.is_pseudo() {
            kinds.insert(0, orch.lead_provider);
        }
        kinds
            .into_iter()
            .map(|provider| WorkerSlot {
                provider,
                model: String::new(),
                note: String::new(),
                enabled: true,
            })
            .collect()
    };
    let now = crate::router::quota::now_secs();
    let members = slots
        .into_iter()
        .map(|slot| {
            let kind = slot.provider;
            let selected = settings.provider(kind).model_id.trim().to_string();
            let model = if slot.model.trim().is_empty() {
                selected
            } else {
                slot.model.trim().to_string()
            };
            let effective_model = if model == "default" {
                String::new()
            } else {
                model
            };
            let snap = crate::router::quota::snapshot(kind);
            let cooldown = crate::router::quota::cooldown_left(kind);
            let snap_exhausted = snap
                .as_ref()
                .is_some_and(|s| s.is_exhausted_for(&effective_model, now));
            let exhausted = cooldown.is_some() || snap_exhausted;
            let used_pct = snap.as_ref().and_then(|s| s.used_pct_for(&effective_model));
            let quota = snap.and_then(|snap| {
                let mut parts: Vec<String> = snap.windows.iter().map(|w| w.summary(now)).collect();
                if snap_exhausted {
                    parts.insert(0, "limit reached".into());
                }
                (!parts.is_empty()).then(|| parts.join(", "))
            });
            let quota = match cooldown {
                Some(secs) => Some(format!(
                    "rate limited for {}{}",
                    crate::router::decide::short_duration(secs),
                    quota.map(|q| format!(", {q}")).unwrap_or_default()
                )),
                None => quota,
            };
            crate::orchestrator::Member {
                agent: crate::orchestrator::agent_name(kind),
                provider: kind.slug().to_string(),
                label: crate::app::composer::composer_provider_label(kind).to_string(),
                model: effective_model,
                any_model: slot.model.trim().is_empty(),
                note: slot.note,
                quota,
                exhausted,
                used_pct,
            }
        })
        .collect();
    crate::orchestrator::Team {
        members,
        max_workers: orch.max_workers(),
        remote,
    }
}

/// If a worker failed over mid-turn to another team subscription, reflect its new provider/model
/// on `task.status` so the UI and lead report match who actually did the work.
fn sync_worker_route(
    task: &mut DelegatedTask,
    routed: Option<(crate::settings::LlmProviderKind, String)>,
) -> bool {
    let Some((provider, model)) = routed else {
        return false;
    };
    let slug = provider.slug();
    if task.status.provider == slug {
        return false;
    }
    let previous = crate::delegation::parse_provider(&task.status.provider)
        .map(|k| crate::app::composer::composer_provider_label(k).to_string())
        .unwrap_or_else(|| task.status.provider.clone());
    task.status.provider = slug.to_string();
    task.status.model = if model == "default" {
        String::new()
    } else {
        model
    };
    if let Some(w) = task.status.worker.as_mut()
        && w.rerouted_from.is_none()
    {
        w.rerouted_from = Some(previous);
    }
    true
}

/// The checkout identity shared by every lead, including roots opened via a subdirectory or alias.
fn checkout_path(root: &str) -> PathBuf {
    let root = Path::new(root);
    let checkout = git2::Repository::discover(root)
        .ok()
        .and_then(|r| r.workdir().map(Path::to_owned))
        .unwrap_or_else(|| root.to_owned());
    checkout.canonicalize().unwrap_or(checkout)
}

/// First non-blank line of `text`.
fn one_line(text: &str) -> &str {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_delegated_task_runs_as_its_own_chat_and_reports_back_to_its_parent() {
        crate::secrets::use_mock_store();
        let root = crate::fsutil::unique_temp_path(&std::env::temp_dir(), "oxi-delegate", "d");
        std::fs::create_dir_all(&root).unwrap();
        let mut app = OxiApp::new();
        app.conn.no_session = true;
        let parent = app.active_session_key();
        app.session_mut_by_key(parent).session_file =
            Some(root.join("parent.jsonl").display().to_string());
        app.conv.workspaces[parent.workspace_idx].root_path = root.to_string_lossy().into_owned();
        let dir = crate::delegation::dir_for(&app.acp_session_key(parent));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("requests")).unwrap();
        // What the parent's `delegate_task` tool drops in its mailbox.
        let request = Request::Delegate {
            task: "dtest".into(),
            provider: "openai".into(),
            model: Some("gpt-test".into()),
            prompt: "Review the parser".into(),
            title: Some("Review".into()),
            worktree: false,
        };
        std::fs::write(
            dir.join("requests/1-dtest.json"),
            serde_json::to_vec(&request).unwrap(),
        )
        .unwrap();
        let sessions_before = app.conv.workspaces[parent.workspace_idx].sessions.len();
        let ctx = egui::Context::default();
        app.poll_delegations_now(&ctx);

        let status = crate::delegation::read_status(&dir, "dtest").unwrap();
        assert_eq!(status.state, TaskState::Running);
        assert_eq!(status.model, "gpt-test");
        let sessions = &app.conv.workspaces[parent.workspace_idx].sessions;
        assert_eq!(
            sessions.len(),
            sessions_before + 1,
            "the task got a chat of its own"
        );
        let child = sessions.last().unwrap();
        assert_eq!(child.title, "Review");
        assert_eq!(
            child
                .config
                .as_ref()
                .map(|c| (c.provider, c.model_id.as_str())),
            Some((crate::settings::LlmProviderKind::OpenAi, "gpt-test"))
        );
        assert!(
            child.messages[0]
                .text
                .contains("delegated this task to you")
        );
        assert!(child.messages[0].text.ends_with("Review the parser"));

        // The child's agent fails (no API key here); the parent gets the report.
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(20) {
            app.drain_agent(&ctx);
            app.poll_delegations(&ctx);
            let parent_got_it = app
                .session_by_key(parent)
                .messages
                .iter()
                .any(|m| m.text.starts_with("[Delegated task]"));
            if parent_got_it {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let status = crate::delegation::read_status(&dir, "dtest").unwrap();
        assert_eq!(status.state, TaskState::Failed);
        let report = app
            .session_by_key(parent)
            .messages
            .iter()
            .find(|m| m.text.starts_with("[Delegated task]"))
            .expect("the report went back to the parent")
            .text
            .clone();
        assert!(
            report.contains("Task dtest (OpenAI Compatible · gpt-test): failed"),
            "{report}"
        );
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(root);
    }

    fn git(dir: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    }

    fn test_lead(root: &Path) -> (OxiApp, SessionKey, PathBuf) {
        crate::secrets::use_mock_store();
        std::fs::create_dir_all(root).unwrap();
        let (mut app, lead) = lead_app(root);
        app.conv.settings.orchestrator.lead_provider = crate::settings::LlmProviderKind::OpenAi;
        app.set_active_session_provider(crate::settings::LlmProviderKind::Orchestrator);
        app.session_mut_by_key(lead).session_file =
            Some(root.join("lead.jsonl").display().to_string());
        let dir = crate::delegation::dir_for(&app.acp_session_key(lead));
        (app, lead, dir)
    }

    /// An app whose only workspace is `root`, with one blank chat (the future lead) active,
    /// whose workers stay on the fixture provider.
    ///
    /// Tests in this process share one config dir, and `OxiApp::new` restores the workspaces
    /// and chats other tests saved there: the active chat could be another test's worker. The
    /// quota board is process-wide too: another test's OpenAI cooldown would reroute
    /// `spawn_workers` to whichever ACP agents are installed, and a real agent would run.
    fn lead_app(root: &Path) -> (OxiApp, SessionKey) {
        let mut app = OxiApp::new();
        app.conn.no_session = true;
        app.conv.workspaces = vec![Workspace {
            root_path: root.to_string_lossy().into_owned(),
            sessions: vec![OxiApp::blank_session("lead")],
            active: 0,
            sidebar_folded: false,
            pinned: Vec::new(),
            archived: Vec::new(),
            folded_groups: Vec::new(),
            worktree: None,
        }];
        app.conv.active_workspace = 0;
        app.flow.sessions.clear();
        app.conv.settings.orchestrator.workers = vec![crate::settings::WorkerSlot {
            provider: crate::settings::LlmProviderKind::OpenAi,
            model: String::new(),
            note: String::new(),
            enabled: true,
        }];
        let lead = app.active_session_key();
        (app, lead)
    }

    fn request(task: &str, access: Access) -> WorkerRequest {
        WorkerRequest {
            task: task.into(),
            provider: "openai".into(),
            model: "fixture".into(),
            title: task.into(),
            prompt: "Review the changes".into(),
            access,
            isolate: false,
            rerouted_from: None,
        }
    }

    fn wait_for(app: &mut OxiApp, condition: impl Fn(&OxiApp) -> bool) {
        let start = Instant::now();
        while !condition(app) {
            assert!(
                start.elapsed() < Duration::from_secs(15),
                "worker did not reach expected state"
            );
            app.drain_agent(&egui::Context::default());
            app.follow_delegations();
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn disabled_worker_slots_stay_disabled() {
        use crate::settings::{AppSettings, LlmProviderKind, WorkerSlot};
        let mut settings = AppSettings::default();
        let available = [LlmProviderKind::CodexAcp];
        assert!(
            !orchestrator_team(&settings, &available, false)
                .members
                .is_empty()
        );
        settings.orchestrator.workers.push(WorkerSlot {
            provider: available[0],
            model: String::new(),
            note: String::new(),
            enabled: false,
        });
        assert!(
            orchestrator_team(&settings, &available, false)
                .members
                .is_empty()
        );
    }

    #[test]
    fn spawn_workers_reroutes_exhausted_subscription_to_other_enabled_team_member() {
        use crate::router::quota::{QuotaSnapshot, UsageWindow};
        use crate::settings::{LlmProviderKind, WorkerSlot};

        let root =
            crate::fsutil::unique_temp_path(&std::env::temp_dir(), "oxi-quota-fallback", "d");
        let (mut app, _, dir) = test_lead(&root);
        let now = crate::router::quota::now_secs();
        // Claude is at 0% quota left (5h 100% used).
        crate::router::quota::set_snapshot_for_tests(
            LlmProviderKind::ClaudeCodeAcp,
            QuotaSnapshot {
                windows: vec![UsageWindow {
                    label: "5h".into(),
                    used_pct: 100.0,
                    resets_at: Some(now + 3600),
                    model_scope: None,
                }],
                limit_reached: true,
                ..Default::default()
            },
        );
        // OpenAi (representing another enabled slot with quota) has plenty left;
        // Codex is explicitly disabled so it must never be picked as a fallback.
        crate::router::quota::clear_snapshot_for_tests(LlmProviderKind::OpenAi);
        crate::router::quota::clear_cooldown(LlmProviderKind::OpenAi);
        crate::router::quota::clear_snapshot_for_tests(LlmProviderKind::CodexAcp);
        crate::router::quota::clear_cooldown(LlmProviderKind::CodexAcp);
        app.conv.settings.orchestrator.workers = vec![
            WorkerSlot {
                provider: LlmProviderKind::ClaudeCodeAcp,
                model: "opus".into(),
                note: "frontend UI".into(),
                enabled: true,
            },
            WorkerSlot {
                provider: LlmProviderKind::CodexAcp,
                model: "gpt-6.2-astra".into(),
                note: "backend".into(),
                enabled: false,
            },
            WorkerSlot {
                provider: LlmProviderKind::OpenAi,
                model: "fallback-model".into(),
                note: "docs only".into(),
                enabled: true,
            },
        ];

        let team = orchestrator_team(
            &app.conv.settings,
            &[
                LlmProviderKind::ClaudeCodeAcp,
                LlmProviderKind::CodexAcp,
                LlmProviderKind::OpenAi,
            ],
            false,
        );
        assert_eq!(team.members.len(), 2);
        assert!(team.members[0].exhausted);
        assert!(!team.members[1].exhausted);
        assert_eq!(team.fallback_member().unwrap().provider, "openai");

        let mut req = request("rerouted", Access::ReadOnly);
        req.provider = "claudecodeacp".into();
        req.model = "opus".into();
        app.spawn_workers(&dir, "b1", vec![req]);

        let status = crate::delegation::read_status(&dir, "rerouted").unwrap();
        assert_eq!(status.provider, "openai");
        assert_eq!(status.model, "fallback-model");
        assert_eq!(
            status
                .worker
                .as_ref()
                .and_then(|w| w.rerouted_from.as_deref()),
            Some("Claude")
        );

        crate::router::quota::clear_snapshot_for_tests(LlmProviderKind::ClaudeCodeAcp);
        crate::router::quota::clear_cooldown(LlmProviderKind::ClaudeCodeAcp);
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn preparation_failures_wake_the_lead_with_one_literal_report() {
        let root = crate::fsutil::unique_temp_path(&std::env::temp_dir(), "oxi-prep-report", "d");
        let (mut app, lead, dir) = test_lead(&root);
        crate::fsutil::write_atomic(&root.join("note.txt"), b"file contents stay private").unwrap();
        // Both editors require work trees, but this workspace is an ordinary folder.
        app.spawn_workers(
            &dir,
            "batch",
            vec![request("wa", Access::Edit), request("wb", Access::Edit)],
        );
        wait_for(&mut app, |a| {
            a.conv.delegations.iter().all(|t| t.status.state.ended())
        });
        assert!(
            app.conv
                .delegations
                .iter()
                .all(|t| t.status.state == TaskState::Rejected && !t.delivered)
        );
        app.conv.delegations[0].status.result = Some("Inspect @note.txt".into());
        app.conv.composer.input = "unfinished draft".into();
        wait_for(&mut app, |a| a.conv.delegations.iter().all(|t| t.delivered));
        let reports: Vec<_> = app
            .session_by_key(lead)
            .messages
            .iter()
            .filter(|m| crate::orchestrator::is_worker_report(&m.text))
            .collect();
        assert_eq!(reports.len(), 1);
        let report = &reports[0].text;
        assert!(
            report.contains("Task wa")
                && report.contains("Task wb")
                && report.contains("not a git repository")
        );
        assert!(
            report.contains("> Inspect @note.txt")
                && !report.contains("file contents stay private")
        );
        assert_eq!(app.conv.composer.input, "unfinished draft");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn followup_reserves_capacity_and_the_checkout_across_leads() {
        let root =
            crate::fsutil::unique_temp_path(&std::env::temp_dir(), "oxi-worker-admission", "d");
        let (mut app, lead, dir) = test_lead(&root);
        git(&root, &["init", "-q"]);
        std::fs::create_dir_all(root.join("nested")).unwrap();
        app.spawn_workers(
            &dir,
            "first",
            vec![
                request("editor", Access::Edit),
                request("reader", Access::ReadOnly),
            ],
        );
        wait_for(&mut app, |a| {
            a.conv.delegations.iter().all(|t| t.status.state.ended())
        });
        let reader = app.task_index(&dir, "reader").unwrap();
        let key = app
            .chat_by_file(app.conv.delegations[reader].status.chat.as_deref().unwrap())
            .unwrap();
        assert!(app.run_state(key).unwrap().last_turn_planned);
        app.select_session_in_workspace(key.workspace_idx, key.session_idx);
        app.set_active_session_model("changed".into());
        app.set_active_session_effort("high".into());
        app.select_session_in_workspace(lead.workspace_idx, lead.session_idx);
        assert!(app.acp_session_key(key).ends_with("#worker"));
        app.run_state_mut(key).plan_mode = false;
        app.message_worker(&dir, "reader", "review again".into())
            .unwrap();
        assert!(app.run_state(key).unwrap().last_turn_planned);
        app.conv.settings.orchestrator.max_workers = 1;
        assert!(
            app.message_worker(&dir, "editor", "edit".into())
                .unwrap_err()
                .contains("At most 1")
        );
        // A second lead in a different window opens a subdirectory of the same checkout.
        let (mut other, _, other_dir) = test_lead(&root.join("nested"));
        other.spawn_workers(&other_dir, "second", vec![request("other", Access::Edit)]);
        app.conv.settings.orchestrator.max_workers = 5;
        assert!(
            app.message_worker(&dir, "editor", "edit".into())
                .unwrap_err()
                .contains("directly editing")
        );
        other.cancel_delegation(other.task_index(&other_dir, "other").unwrap());
        wait_for(&mut other, |a| {
            a.conv
                .delegations
                .iter()
                .all(|t| t.prep_rx.is_none() && t.checkout_lease.is_none())
        });
        app.message_worker(&dir, "editor", "edit again".into())
            .unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn restart_restores_handoff_and_a_failed_merge_preserves_its_tree() {
        let root =
            crate::fsutil::unique_temp_path(&std::env::temp_dir(), "oxi-restart-worker", "d");
        let (mut app, _, dir) = test_lead(&root);
        git(&root, &["init", "-q"]);
        git(&root, &["config", "user.name", "Oxi Test"]);
        git(&root, &["config", "user.email", "oxi@example.com"]);
        crate::fsutil::write_atomic(&root.join("image.bin"), &[0, 1, 2]).unwrap();
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-qm", "base"]);
        let mut editor = request("isolated", Access::Edit);
        editor.isolate = true;
        app.spawn_workers(&dir, "batch", vec![editor]);
        wait_for(&mut app, |a| {
            a.conv
                .delegations
                .iter()
                .any(|t| t.status.task == "isolated" && t.status.state.ended())
        });
        let saved = crate::delegation::read_status(&dir, "isolated").unwrap();
        let wt = PathBuf::from(saved.worktree.as_ref().unwrap());
        assert!(saved.worker.as_ref().unwrap().base.is_some());
        drop(app);
        std::fs::remove_dir_all(&dir).unwrap();
        let (mut app, _, _) = test_lead(&root);
        app.restore_worker_records();
        assert_eq!(
            crate::delegation::read_status(&dir, "isolated").unwrap(),
            saved
        );
        crate::fsutil::write_atomic(&wt.join("image.bin"), &[0, 3, 4]).unwrap();
        crate::fsutil::write_atomic(&root.join("image.bin"), &[0, 5, 6]).unwrap();
        app.begin_merge(&dir, "isolated", None).unwrap();
        assert!(app.message_worker(&dir, "isolated", "run".into()).is_err());
        assert!(app.begin_merge(&dir, "isolated", None).is_err());
        let i = app.task_index(&dir, "isolated").unwrap();
        wait_for(&mut app, |a| a.conv.delegations[i].cleanup.is_none());
        let status = crate::delegation::read_status(&dir, "isolated").unwrap();
        assert!(!status.worker.unwrap().released);
        assert!(
            status.error.as_deref().unwrap().contains("image.bin"),
            "{:?}",
            status.error
        );
        assert_eq!(std::fs::read(wt.join("image.bin")).unwrap(), [0, 3, 4]);
        assert_eq!(std::fs::read(root.join("image.bin")).unwrap(), [0, 5, 6]);
        app.handle_delegation_request(
            &dir,
            Request::Discard {
                task: "isolated".into(),
            },
        );
        wait_for(&mut app, |a| a.conv.delegations[i].cleanup.is_none());
        assert!(!wt.exists());
        assert!(
            app.conv.delegations[i]
                .status
                .worker
                .as_ref()
                .unwrap()
                .released
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn cancellation_owns_the_preparation_until_the_new_tree_and_branch_are_removed() {
        let root = crate::fsutil::unique_temp_path(&std::env::temp_dir(), "oxi-cancel-prep", "d");
        let (mut app, _, dir) = test_lead(&root);
        git(&root, &["init", "-q"]);
        git(&root, &["config", "user.name", "Oxi Test"]);
        git(&root, &["config", "user.email", "oxi@example.com"]);
        git(&root, &["commit", "-q", "--allow-empty", "-m", "base"]);
        let mut worker = request("cancelprep", Access::Edit);
        worker.isolate = true;
        app.spawn_workers(&dir, "b", vec![worker]);
        let i = app.task_index(&dir, "cancelprep").unwrap();
        app.cancel_delegation(i);
        wait_for(&mut app, |a| {
            a.conv.delegations[i]
                .status
                .worker
                .as_ref()
                .unwrap()
                .released
        });
        let status = &app.conv.delegations[i].status;
        assert_eq!(status.state, TaskState::Cancelled);
        assert!(status.chat.is_none());
        assert!(!Path::new(status.worktree.as_ref().unwrap()).exists());
        let repo = git2::Repository::open(&root).unwrap();
        assert_eq!(
            repo.branches(Some(git2::BranchType::Local))
                .unwrap()
                .count(),
            1
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn cancellation_during_setup_invalidates_the_initial_prompt() {
        let root = crate::fsutil::unique_temp_path(&std::env::temp_dir(), "oxi-cancel-setup", "d");
        let (mut app, _, dir) = test_lead(&root);
        git(&root, &["init", "-q"]);
        git(&root, &["config", "user.name", "Oxi Test"]);
        git(&root, &["config", "user.email", "oxi@example.com"]);
        git(&root, &["commit", "-q", "--allow-empty", "-m", "base"]);
        app.conv
            .settings
            .worktree_setup_scripts
            .insert(root.display().to_string(), "sleep 30".into());
        let mut worker = request("cancelsetup", Access::Edit);
        worker.isolate = true;
        app.spawn_workers(&dir, "b", vec![worker]);
        let i = app.task_index(&dir, "cancelsetup").unwrap();
        wait_for(&mut app, |a| a.conv.delegations[i].status.chat.is_some());
        let key = app
            .chat_by_file(app.conv.delegations[i].status.chat.as_deref().unwrap())
            .unwrap();
        assert_eq!(app.run_state(key).unwrap().queued.len(), 1);
        app.cancel_delegation(i);
        app.send_next_queued(key);
        assert!(app.run_state(key).unwrap().queued.is_empty());
        assert!(app.session_by_key(key).messages.is_empty());
        app.discard_worker(i);
        let start = Instant::now();
        while !app.conv.delegations[i]
            .status
            .worker
            .as_ref()
            .unwrap()
            .released
        {
            assert!(start.elapsed() < Duration::from_secs(10));
            app.drain_worktree_setups(&egui::Context::default());
            app.finish_discards();
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = std::fs::remove_dir_all(root);
    }

    /// Two editing workers from one `spawn_workers` call get work trees of their own, a
    /// read-only one runs in the workspace, all stay off the sidebar, and their reports come
    /// back to the lead as one message.
    #[test]
    fn a_lead_s_editing_workers_get_their_own_work_trees_and_report_together() {
        crate::secrets::use_mock_store();
        let root = crate::fsutil::unique_temp_path(&std::env::temp_dir(), "oxi-orch-app", "d");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q"]);
        git(&root, &["config", "user.name", "Oxi Test"]);
        git(&root, &["config", "user.email", "oxi@example.com"]);
        std::fs::write(root.join("a.txt"), "a\n").unwrap();
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-q", "-m", "init"]);
        // Uncommitted work the workers should see.
        std::fs::write(root.join("draft.txt"), "wip\n").unwrap();

        let (mut app, lead) = lead_app(&root);
        app.session_mut_by_key(lead).session_file =
            Some(root.join("lead.jsonl").display().to_string());
        let mut config = crate::model::SessionConfig::from_provider(
            app.conv
                .settings
                .provider(crate::settings::LlmProviderKind::Orchestrator),
        );
        config.provider = crate::settings::LlmProviderKind::Orchestrator;
        app.session_mut_by_key(lead).config = Some(config);
        let lead_key = app.acp_session_key(lead);
        assert!(lead_key.ends_with("#lead"));
        let dir = crate::delegation::dir_for(&lead_key);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("requests")).unwrap();
        let worker = |task: &str, access| WorkerRequest {
            task: task.into(),
            provider: "openai".into(),
            model: "gpt-test".into(),
            title: task.into(),
            prompt: format!("do {task}"),
            access,
            isolate: false,
            rerouted_from: None,
        };
        let request = Request::Spawn {
            batch: "b1".into(),
            workers: vec![
                worker("wa", Access::Edit),
                worker("wb", Access::Edit),
                worker("wc", Access::ReadOnly),
            ],
        };
        std::fs::write(
            dir.join("requests/1-b1.json"),
            serde_json::to_vec(&request).unwrap(),
        )
        .unwrap();
        let ctx = egui::Context::default();
        app.poll_delegations_now(&ctx);
        let started = Instant::now();
        let mut lead_report = None;
        while started.elapsed() < Duration::from_secs(30) {
            app.drain_agent(&ctx);
            app.poll_delegations(&ctx);
            lead_report = app
                .session_by_key(lead)
                .messages
                .iter()
                .find(|m| m.text.starts_with("[Workers finished]"))
                .map(|m| m.text.clone());
            if lead_report.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let status = |task| crate::delegation::read_status(&dir, task).unwrap();
        let (a, b, c) = (status("wa"), status("wb"), status("wc"));
        let (wa, wb, wc) = (
            a.worker.clone().unwrap(),
            b.worker.clone().unwrap(),
            c.worker.clone().unwrap(),
        );
        assert_ne!(wa.cwd, wa.root, "two editors: each in a work tree: {a:?}");
        assert_ne!(wb.cwd, wb.root);
        assert_ne!(wa.cwd, wb.cwd);
        assert_eq!(wc.cwd, wc.root, "a reader stays in the workspace");
        assert!(
            Path::new(&wa.cwd).join("draft.txt").is_file(),
            "work trees start from the uncommitted workspace"
        );
        assert!(wa.base.is_some());
        let report = lead_report.expect("one report for the whole batch");
        assert!(
            report.contains("Task wa") && report.contains("Task wb") && report.contains("Task wc"),
            "{report}"
        );
        assert_eq!(
            app.session_by_key(lead)
                .messages
                .iter()
                .filter(|m| m.role == MsgRole::User && m.text.contains("Task w"))
                .count(),
            1
        );
        // Worker chats know their lead, and are kept off the sidebar while it uses them.
        assert!(app.hidden_worker_chat(c.chat.as_deref()), "{c:?}");
        let worktree = a.worktree.clone().unwrap();
        assert!(app.hidden_worker_workspace(&worktree));
        let wc_key = app
            .conv
            .workspaces
            .iter()
            .enumerate()
            .find_map(|(wi, w)| {
                w.sessions
                    .iter()
                    .position(|s| s.title == "wc")
                    .map(|si| app.session_key(wi, si))
            })
            .unwrap();
        assert!(app.acp_session_key(wc_key).ends_with("#worker"));
        assert!(app.run_state(wc_key).unwrap().last_turn_planned);

        // Dropping a worker removes its work tree.
        let index = app.task_index(&dir, "wa").unwrap();
        app.discard_worker(index);
        assert!(!status("wa").worker.unwrap().released);
        let started = Instant::now();
        while !status("wa").worker.unwrap().released && started.elapsed() < Duration::from_secs(10)
        {
            app.finish_discards();
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(status("wa").worker.unwrap().released);
        assert!(!Path::new(&worktree).exists(), "the work tree is gone");
        if let Some(wt) = status("wb").worktree {
            let _ = crate::git::worktree::remove(
                Path::new(&wt),
                crate::git::worktree::RemoveOptions {
                    keep_branch: false,
                    force: true,
                },
            );
        }
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn deleting_lead_chat_cancels_running_worker_and_cleans_up_worktree_and_records() {
        let root = crate::fsutil::unique_temp_path(&std::env::temp_dir(), "oxi-delete-lead", "d");
        let (mut app, lead, dir) = test_lead(&root);
        git(&root, &["init", "-q"]);
        git(&root, &["config", "user.name", "Oxi Test"]);
        git(&root, &["config", "user.email", "oxi@example.com"]);
        git(&root, &["commit", "-q", "--allow-empty", "-m", "base"]);

        let lead_acp = app.acp_session_key(lead);
        let records_dir = crate::delegation::worker_records(&lead_acp);
        let _ = std::fs::remove_dir_all(&records_dir);

        let mut worker = request("orphan", Access::Edit);
        worker.isolate = true;
        app.spawn_workers(&dir, "b1", vec![worker]);

        wait_for(&mut app, |a| {
            a.conv
                .delegations
                .iter()
                .any(|t| t.status.task == "orphan" && t.status.worktree.is_some())
        });
        let idx = app.task_index(&dir, "orphan").unwrap();
        let wt_str = app.conv.delegations[idx].status.worktree.clone().unwrap();
        let wt = PathBuf::from(&wt_str);
        assert!(wt.exists(), "worker worktree should exist before delete");
        assert!(
            crate::delegation::status_path(&records_dir, "orphan").is_file(),
            "persisted worker record should exist before delete"
        );
        assert!(app.conv.worker_owners.contains_key(&lead_acp));

        // Simulate the worker still actively running when the user deletes its lead chat.
        app.conv.delegations[idx].status.state = TaskState::Running;
        if let Some(worker_key) = app.conv.delegations[idx]
            .status
            .chat
            .as_deref()
            .and_then(|f| app.chat_by_file(f))
        {
            app.run_state_mut(worker_key).waiting_response = true;
        }

        app.delete_session(lead.session_idx);

        assert!(
            !app.conv.worker_owners.contains_key(&lead_acp),
            "lead ownership lease should be dropped"
        );
        assert!(
            !records_dir.exists(),
            "persisted worker records should be removed on lead deletion"
        );
        assert_eq!(
            crate::delegation::read_status(&dir, "orphan")
                .unwrap()
                .state,
            TaskState::Cancelled,
            "running worker should be cancelled on lead deletion"
        );

        // Background worktree cleanup should complete cleanly and drop the orphaned entry without
        // recreating persisted worker records.
        wait_for(&mut app, |a| a.conv.delegations.is_empty());
        assert!(!wt.exists(), "worker worktree should be removed");
        assert!(
            !records_dir.exists(),
            "worker records should not be recreated after cleanup finishes"
        );
        assert!(
            app.conv.workspaces.iter().all(|w| w.root_path != wt_str),
            "worktree workspace should be removed"
        );

        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn worker_ending_without_answer_is_marked_failed() {
        let root = crate::fsutil::unique_temp_path(&std::env::temp_dir(), "oxi-empty-worker", "d");
        let (mut app, lead, dir) = test_lead(&root);
        let parent = app.acp_session_key(lead);

        // Create a worker chat whose turn ended with only a failed tool call and no Answer text.
        let mut session = OxiApp::blank_session("Read-only probe");
        session
            .messages
            .push(crate::model::ChatMessage::user("inspect", &[]));
        let mut reply = crate::model::ChatMessage::streaming_assistant(None);
        reply.blocks.push(AssistantBlock::Tool {
            tool_call_id: "t1".into(),
            name: "bash".into(),
            args_summary: Some("{\"CommandLine\":\"rg foo\"}".into()),
            output: String::new(),
            diff: None,
            is_error: Some(true),
            full_output_path: None,
            output_truncated: false,
            metadata: Some(Box::new(crate::model::ToolMetadata {
                title: "bash".into(),
                kind: "execute".into(),
                name: Some("bash".into()),
                locations: Vec::new(),
                status: crate::model::ToolStatus::Failed,
            })),
        });
        reply.finish_streaming();
        session.messages.push(reply);
        session.session_file = Some("worker-empty.jsonl".into());
        app.conv.workspaces[lead.workspace_idx]
            .sessions
            .push(session);

        let mut task = DelegatedTask {
            dir: dir.clone(),
            parent,
            status: Status {
                task: "wempty".into(),
                state: TaskState::Running,
                provider: "antigravityacp".into(),
                model: "gemini-4-neon".into(),
                title: "Read-only probe".into(),
                chat: Some("worker-empty.jsonl".into()),
                worktree: None,
                result: None,
                error: None,
                worker: Some(WorkerInfo {
                    batch: "b1".into(),
                    access: Access::ReadOnly,
                    root: root.to_string_lossy().into_owned(),
                    cwd: root.to_string_lossy().into_owned(),
                    ..Default::default()
                }),
            },
            prompt: String::new(),
            plan_mode: true,
            prep_rx: None,
            ended_at: None,
            delivered: false,
            discard_pending: false,
            cleanup: None,
            checkout_lease: None,
            parent_deleted: false,
        };

        app.check_delegated_chat(&mut task);
        assert_eq!(task.status.state, TaskState::Failed);
        assert!(
            task.status
                .error
                .as_deref()
                .is_some_and(|e| e.contains("tool call failed")),
            "{:?}",
            task.status.error
        );
        assert!(task.status.result.is_none());

        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(root);
    }
}
