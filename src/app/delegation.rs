//! The app's side of delegation between agents (see [`crate::delegation`]): requests from a
//! chat's agent become chats of their own on the requested provider/model, in the parent's
//! workspace or a new work tree; their state is kept in the parent's mailbox, and their report
//! goes back to the parent chat when they end.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText, Ui};

use super::compaction::QueuedSend;
use super::state::Workspace;
use super::{OxiApp, SessionKey};
use crate::delegation::{Request, Status, TaskState};
use crate::model::MsgRole;
use crate::theme::*;

/// A finished task's report goes to the parent this long after it ended, unless the parent's
/// agent read it with `task_status` meanwhile.
const DELIVERY_DELAY: Duration = Duration::from_secs(2);
/// How often the mailboxes are checked for requests while nothing else wakes the app.
const WATCH_EVERY: Duration = Duration::from_millis(300);

/// Set by the watcher when a mailbox has requests; the next frame reads them.
static REQUESTS_WAITING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// One delegated task.
pub(crate) struct DelegatedTask {
    /// The parent chat's mailbox.
    dir: PathBuf,
    /// The parent chat's agent session key (its file, or a `mem:` key before it has one).
    parent: String,
    pub status: Status,
    prompt: String,
    plan_mode: bool,
    /// Its work tree being created.
    worktree_rx: Option<Receiver<Result<PathBuf, String>>>,
    ended_at: Option<Instant>,
    delivered: bool,
}

impl DelegatedTask {
    fn save(&self) {
        crate::delegation::write_status(&self.dir, &self.status);
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

/// What goes back to the parent chat when a task ends.
fn report_message(status: &Status) -> String {
    format!("[Delegated task] {}", crate::delegation::describe(status))
}

impl OxiApp {
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
    pub(crate) fn poll_delegations_now(&mut self, ctx: &egui::Context) {
        self.take_delegation_requests();
        self.poll_delegations(ctx);
    }

    fn handle_delegation_request(&mut self, dir: &Path, request: Request) {
        match request {
            Request::Cancel { task } => {
                let Some(index) = self
                    .conv
                    .delegations
                    .iter()
                    .position(|t| t.status.task == task && t.dir == dir)
                else {
                    return;
                };
                self.cancel_delegation(index);
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
                if kind == crate::settings::LlmProviderKind::Router {
                    reject("Pick a provider, not the Router.".into());
                    return;
                }
                let mut config = self.conv.settings.provider(kind).clone();
                if let Some(model) = model.as_deref() {
                    config.model_id = model.to_string();
                }
                let title = title.unwrap_or_else(|| crate::model::make_session_title(&prompt));
                let parent_label = self
                    .session_by_key(parent_key)
                    .config
                    .as_ref()
                    .map(|c| crate::app::composer::composer_provider_label(c.provider))
                    .unwrap_or("an agent")
                    .to_string();
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
                };
                let plan_mode = self.run_state(parent_key).is_some_and(|r| r.plan_mode);
                let mut worktree_rx = None;
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
                    let (tx, rx) = std::sync::mpsc::channel();
                    let ctx = self.conv.git_ctx.clone();
                    std::thread::spawn(move || {
                        let _ = tx.send(crate::git::worktree::create(&root));
                        ctx.request_repaint();
                    });
                    worktree_rx = Some(rx);
                }
                let mut task = DelegatedTask {
                    dir: dir.to_path_buf(),
                    parent,
                    status,
                    prompt: task_prompt(&parent_label, &prompt),
                    plan_mode,
                    worktree_rx,
                    ended_at: None,
                    delivered: false,
                };
                task.save();
                if task.worktree_rx.is_none() {
                    self.start_delegated_chat(&mut task, parent_key.workspace_idx, &config);
                }
                self.conv.delegations.push(task);
            }
        }
    }

    /// Open the task's chat in workspace `wi` and send it the task.
    fn start_delegated_chat(
        &mut self,
        task: &mut DelegatedTask,
        wi: usize,
        config: &crate::settings::ProviderConfig,
    ) {
        let mut session = Self::blank_session(task.status.title.clone());
        session.config = Some(crate::model::SessionConfig::from_provider(config));
        // Appended, so no other chat's key moves.
        self.conv.workspaces[wi].sessions.push(session);
        let key = self.session_key(wi, self.conv.workspaces[wi].sessions.len() - 1);
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
        task.save();
    }

    /// Advance tasks: work trees that finished being created, chats that ended, reports due.
    fn follow_delegations(&mut self) {
        let mut tasks = std::mem::take(&mut self.conv.delegations);
        for task in &mut tasks {
            if let Some(rx) = task.worktree_rx.as_ref() {
                match rx.try_recv() {
                    Err(TryRecvError::Empty) => continue,
                    result => {
                        task.worktree_rx = None;
                        match result.unwrap_or_else(|_| Err("the work tree was not created".into()))
                        {
                            Ok(path) => {
                                let wi = self.add_background_worktree(&path);
                                task.status.worktree = Some(path.to_string_lossy().into_owned());
                                let kind = crate::delegation::parse_provider(&task.status.provider)
                                    .unwrap_or_default();
                                let mut config = self.conv.settings.provider(kind).clone();
                                config.model_id = task.status.model.clone();
                                self.start_delegated_chat(task, wi, &config);
                            }
                            Err(e) => {
                                task.status.state = TaskState::Rejected;
                                task.status.error =
                                    Some(format!("Could not create a work tree: {e}"));
                                task.save();
                                task.delivered = true;
                            }
                        }
                    }
                }
            }
            if task.status.state == TaskState::Running {
                self.check_delegated_chat(task);
            }
            if task.status.state.ended() && !task.delivered {
                let due = task
                    .ended_at
                    .is_none_or(|at| at.elapsed() >= DELIVERY_DELAY);
                if due {
                    task.delivered = true;
                    if !crate::delegation::fetched(&task.dir, &task.status.task) {
                        self.deliver_report(task);
                    }
                }
            }
        }
        // Ended and reported tasks stay listed with their parent until the app restarts.
        self.conv.delegations = tasks;
    }

    /// Whether the task's chat has answered; record how it ended.
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
        task.status.state = if crate::model::reply_failed(reply) {
            TaskState::Failed
        } else {
            TaskState::Completed
        };
        task.status.result = crate::model::last_answer(reply).map(str::to_owned);
        task.ended_at = Some(Instant::now());
        task.save();
    }

    /// Send a task's report into its parent chat: right away when it is idle, into the running
    /// turn when it can take it, else after the turn.
    fn deliver_report(&mut self, task: &DelegatedTask) {
        let Some(parent) = self.conv.workspaces.iter().enumerate().find_map(|(wi, w)| {
            (0..w.sessions.len())
                .map(|si| self.session_key(wi, si))
                .find(|key| self.acp_session_key(*key) == task.parent)
        }) else {
            return;
        };
        let text = report_message(&task.status);
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
        if let Some(key) = chat.as_deref().and_then(|f| self.chat_by_file(f))
            && self.chat_is_busy(key)
        {
            self.stop_agent_run(key);
        }
        let task = &mut self.conv.delegations[index];
        task.worktree_rx = None;
        task.status.state = TaskState::Cancelled;
        task.ended_at = Some(Instant::now());
        // The parent asked for it (or the user stopped it here): nothing to report back.
        task.delivered = true;
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
            folded_groups: Vec::new(),
            worktree: crate::git::worktree::info(path),
        });
        self.sync_workspaces_to_settings();
        self.start_worktree_setup(&root);
        self.conv.workspaces.len() - 1
    }

    /// The active chat's delegated tasks, above its composer: state, a way to open each task's
    /// chat, and to stop one still running.
    pub(super) fn render_delegations(&mut self, ui: &mut Ui) {
        let key = self.active_session_key();
        let parent = self.acp_session_key(key);
        let rows: Vec<(usize, String, TaskState, Option<String>)> = self
            .conv
            .delegations
            .iter()
            .enumerate()
            .filter(|(_, t)| t.parent == parent)
            .map(|(i, t)| {
                let who = crate::delegation::parse_provider(&t.status.provider)
                    .map(|k| crate::app::composer::composer_provider_label(k).to_string())
                    .unwrap_or_else(|| t.status.provider.clone());
                (
                    i,
                    format!("{who} · {}", t.status.title),
                    t.status.state,
                    t.status.chat.clone(),
                )
            })
            .collect();
        if rows.is_empty() {
            return;
        }
        let mut open = None;
        let mut stop = None;
        for (index, label, state, chat) in &rows {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let (text, color) = match state {
                    TaskState::Starting => ("Starting", c_text_muted()),
                    TaskState::Running => ("Delegated", c_accent()),
                    TaskState::Completed => ("Done", c_success()),
                    TaskState::Failed => ("Failed", c_error_fg()),
                    TaskState::Cancelled => ("Stopped", c_text_muted()),
                    TaskState::Rejected => ("Not started", c_error_fg()),
                };
                if matches!(state, TaskState::Starting | TaskState::Running) {
                    crate::theme::small_spinner(ui);
                }
                ui.label(RichText::new(text).size(FS_SMALL).color(color));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if !state.ended()
                        && ui
                            .small_button("Stop")
                            .on_hover_text("Stop this delegated task")
                            .clicked()
                    {
                        stop = Some(*index);
                    }
                    if let Some(chat) = chat
                        && ui
                            .small_button("Open")
                            .on_hover_text("Show the task's chat")
                            .clicked()
                    {
                        open = Some(chat.clone());
                    }
                    ui.add(
                        egui::Label::new(RichText::new(label).size(FS_SMALL))
                            .truncate()
                            .selectable(false),
                    )
                    .on_hover_text(label);
                });
            });
        }
        ui.add_space(4.0);
        if let Some(index) = stop {
            self.cancel_delegation(index);
        }
        if let Some(file) = open
            && let Some(key) = self.chat_by_file(&file)
        {
            self.select_session_in_workspace(key.workspace_idx, key.session_idx);
        }
    }
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
}
