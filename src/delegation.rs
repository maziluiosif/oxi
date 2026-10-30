//! Delegation between agents, coordinated by the oxi app: an agent hands a task to another
//! provider/model (Claude asks Codex to review its change, say), checks on it, and gets the
//! result back in its own chat.
//!
//! The agent's tools (`delegate_task`, `task_status`, `task_cancel`) run outside the app: in
//! oxi's MCP server for ACP agents, in the agent loop for oxi's own agent. They talk to the app
//! through a mailbox folder per parent chat, named after the app's process and the chat: the
//! tools drop requests there, the app picks them up, runs each task as a chat of its own (in a
//! new work tree when asked) and keeps a status file per task. When a task ends, the app sends
//! its result to the parent chat, unless the parent already read it with `task_status`.
//!
//! The delegated chat has the same approval policy and plan mode as its parent; its approval
//! prompts show in its own chat.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::settings::LlmProviderKind;

/// Environment variable naming the mailbox for oxi's MCP server.
pub const DIR_ENV: &str = "OXI_DELEGATION_DIR";
pub const DELEGATE: &str = "delegate_task";
pub const STATUS: &str = "task_status";
pub const CANCEL: &str = "task_cancel";
pub const NAMES: [&str; 3] = [DELEGATE, STATUS, CANCEL];
/// How long `delegate_task` waits for the app to take the request.
const ACCEPT_TIMEOUT: Duration = Duration::from_secs(15);
/// Longest `task_status` may wait for a task to end.
pub const MAX_WAIT: Duration = Duration::from_secs(60);
/// Longest prompt handed to a delegated task.
const MAX_PROMPT: usize = 100_000;

static AVAILABLE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// This process is the oxi app, which runs delegated tasks: agents get the tools.
pub fn set_available() {
    AVAILABLE.store(true, std::sync::atomic::Ordering::Relaxed);
}

pub fn available() -> bool {
    AVAILABLE.load(std::sync::atomic::Ordering::Relaxed)
}

/// Where every process's mailboxes live.
pub fn root() -> PathBuf {
    crate::app_dirs::data_dir().join("delegation")
}

/// The mailbox of the chat whose agent session key is `session_key`, owned by this process.
pub fn dir_for(session_key: &str) -> PathBuf {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(session_key.as_bytes());
    let hash: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    root().join(format!("{}-{hash}", std::process::id()))
}

/// Mailboxes of processes that no longer run (their folder is named after the process id).
pub fn prune_stale() {
    let own = format!("{}-", std::process::id());
    for entry in std::fs::read_dir(root()).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with(&own)
            && entry
                .metadata()
                .and_then(|m| m.modified())
                .is_ok_and(|t| t.elapsed().unwrap_or_default() > Duration::from_secs(24 * 3600))
        {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Request {
    Delegate {
        task: String,
        provider: String,
        #[serde(default)]
        model: Option<String>,
        prompt: String,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        worktree: bool,
    },
    Cancel {
        task: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    /// Accepted; its chat (or work tree) is being prepared.
    Starting,
    Running,
    Completed,
    Failed,
    Cancelled,
    /// Not started: the reason is in `error`.
    Rejected,
}

impl TaskState {
    pub fn ended(self) -> bool {
        !matches!(self, Self::Starting | Self::Running)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub task: String,
    pub state: TaskState,
    pub provider: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub title: String,
    /// The delegated chat's file, once it has one.
    #[serde(default)]
    pub chat: Option<String>,
    /// Its work tree, when it runs in one.
    #[serde(default)]
    pub worktree: Option<String>,
    /// The task's final answer.
    #[serde(default)]
    pub result: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

fn requests_dir(dir: &Path) -> PathBuf {
    dir.join("requests")
}

fn status_path(dir: &Path, task: &str) -> PathBuf {
    dir.join("status").join(format!("{task}.json"))
}

/// Marks that the parent read a finished task's result itself.
fn fetched_path(dir: &Path, task: &str) -> PathBuf {
    dir.join("status").join(format!("{task}.fetched"))
}

fn safe_task_id(task: &str) -> bool {
    !task.is_empty()
        && task.len() <= 40
        && task.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    crate::fsutil::write_atomic(path, &bytes).map_err(|e| e.to_string())
}

pub fn read_status(dir: &Path, task: &str) -> Option<Status> {
    if !safe_task_id(task) {
        return None;
    }
    let bytes = std::fs::read(status_path(dir, task)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// The app's side: record `status`.
pub fn write_status(dir: &Path, status: &Status) {
    if let Err(e) = write_json(&status_path(dir, &status.task), status) {
        log::warn!("delegation status {}: {e}", status.task);
    }
}

/// The app's side: whether the parent already read task `task`'s result.
pub fn fetched(dir: &Path, task: &str) -> bool {
    fetched_path(dir, task).exists()
}

/// The app's side: take the requests waiting in `dir`, oldest first.
pub fn take_requests(dir: &Path) -> Vec<Request> {
    let mut files: Vec<_> = std::fs::read_dir(requests_dir(dir))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    files.sort();
    files
        .into_iter()
        .filter_map(|path| {
            let request = std::fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok());
            let _ = std::fs::remove_file(&path);
            request
        })
        .collect()
}

/// The provider a name given by an agent means: a slug (`codexacp`), or a plain name such as
/// "codex", "claude", "cursor", "copilot", "gemini", "openai".
pub fn parse_provider(name: &str) -> Option<LlmProviderKind> {
    let name = name.trim().to_ascii_lowercase();
    let alias = match name.as_str() {
        "claude" | "claude code" | "claude-code" | "anthropic" => {
            Some(LlmProviderKind::ClaudeCodeAcp)
        }
        "codex" | "openai codex" | "codex cli" => Some(LlmProviderKind::CodexAcp),
        "cursor" => Some(LlmProviderKind::CursorAcp),
        "copilot" | "github copilot" => Some(LlmProviderKind::CopilotAcp),
        "gemini" | "antigravity" => Some(LlmProviderKind::AntigravityAcp),
        "muse" | "muse code" => Some(LlmProviderKind::MuseCodeAcp),
        "opencode" | "open code" => Some(LlmProviderKind::OpenCodeAcp),
        _ => None,
    };
    alias.or_else(|| {
        LlmProviderKind::ALL
            .into_iter()
            .find(|kind| kind.slug() == name || kind.label().to_ascii_lowercase() == name)
    })
}

/// Definitions of the three tools, in the OpenAI function format.
pub fn tool_definitions() -> Vec<Value> {
    let providers = "claude, codex, cursor, copilot, gemini, muse, or an API provider such as openai, openrouter, ollama";
    vec![
        json!({
            "type": "function",
            "function": {
                "name": DELEGATE,
                "description": format!("Hand a self-contained task to another agent (provider/model) through oxi, e.g. to have a different model review or verify your change. It runs as a separate chat the user can watch, with the same approval rules as yours. Returns a task id at once; the task's final report is sent back to this chat as a message when it ends, or read it with {STATUS}. Use worktree for tasks that change files, so they cannot clash with yours. Providers: {providers}."),
                "parameters": {
                    "type": "object",
                    "properties": {
                        "provider": { "type": "string", "description": "Agent to run the task, e.g. codex" },
                        "model": { "type": "string", "description": "Model id (optional: the provider's default)" },
                        "prompt": { "type": "string", "description": "The complete task: the other agent sees nothing else of this conversation" },
                        "title": { "type": "string", "description": "Short title for the task's chat (optional)" },
                        "worktree": { "type": "boolean", "description": "Run it in a new git work tree (for tasks that change files)" }
                    },
                    "required": ["provider", "prompt"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": STATUS,
                "description": format!("State of a task started with {DELEGATE}, with its final report once it ended. wait_seconds (up to {}) waits for it to end.", MAX_WAIT.as_secs()),
                "parameters": {
                    "type": "object",
                    "properties": {
                        "task_id": { "type": "string" },
                        "wait_seconds": { "type": "integer" }
                    },
                    "required": ["task_id"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": CANCEL,
                "description": format!("Stop a task started with {DELEGATE}."),
                "parameters": {
                    "type": "object",
                    "properties": { "task_id": { "type": "string" } },
                    "required": ["task_id"]
                }
            }
        }),
    ]
}

/// Run one of the tools against mailbox `dir`.
pub fn call(dir: Option<&Path>, name: &str, args: &Value) -> Result<String, String> {
    let dir = dir.ok_or(
        "Delegation needs the oxi desktop app: this agent does not run under it (an editor or \
         a remote host).",
    )?;
    match name {
        DELEGATE => delegate(dir, args),
        STATUS => status(dir, args),
        CANCEL => cancel(dir, args),
        other => Err(format!("unknown tool {other}")),
    }
}

fn string_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args[key].as_str().map(str::trim).filter(|s| !s.is_empty())
}

fn delegate(dir: &Path, args: &Value) -> Result<String, String> {
    let provider = string_arg(args, "provider").ok_or("provider is required")?;
    let kind = parse_provider(provider).ok_or_else(|| format!("Unknown provider `{provider}`."))?;
    let prompt = string_arg(args, "prompt").ok_or("prompt is required")?;
    if prompt.len() > MAX_PROMPT {
        return Err(format!(
            "The prompt is over {MAX_PROMPT} bytes; shorten it."
        ));
    }
    let task = format!("d{:08x}", rand::random::<u32>());
    let request = Request::Delegate {
        task: task.clone(),
        provider: kind.slug().to_string(),
        model: string_arg(args, "model").map(str::to_owned),
        prompt: prompt.to_string(),
        title: string_arg(args, "title").map(str::to_owned),
        worktree: args["worktree"].as_bool().unwrap_or(false),
    };
    let request_file = requests_dir(dir).join(format!("{}-{task}.json", now_millis()));
    write_json(&request_file, &request)?;
    let started = Instant::now();
    while started.elapsed() < ACCEPT_TIMEOUT {
        if let Some(status) = read_status(dir, &task) {
            if status.state == TaskState::Rejected {
                return Err(status
                    .error
                    .unwrap_or_else(|| "The task was not started.".into()));
            }
            return Ok(format!(
                "Started task {task} on {}{}. Its report comes back to this chat as a message \
                 when it ends; {STATUS} with task_id \"{task}\" shows its state meanwhile.",
                kind.label(),
                status
                    .worktree
                    .as_deref()
                    .map(|w| format!(" in work tree {w}"))
                    .unwrap_or_default(),
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    // Nobody took it: leave nothing behind that a later app would run unexpectedly.
    let _ = std::fs::remove_file(&request_file);
    Err("The oxi app did not take the task (delegation runs in the oxi desktop app).".into())
}

fn status(dir: &Path, args: &Value) -> Result<String, String> {
    let task = string_arg(args, "task_id").ok_or("task_id is required")?;
    let wait = Duration::from_secs(args["wait_seconds"].as_u64().unwrap_or(0)).min(MAX_WAIT);
    let started = Instant::now();
    loop {
        let status = read_status(dir, task).ok_or_else(|| format!("No task `{task}`."))?;
        if status.state.ended() || started.elapsed() >= wait {
            if status.state.ended() {
                // The parent has the result now; the app need not send it again.
                let _ = std::fs::write(fetched_path(dir, task), b"");
            }
            return Ok(describe(&status));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn cancel(dir: &Path, args: &Value) -> Result<String, String> {
    let task = string_arg(args, "task_id").ok_or("task_id is required")?;
    let status = read_status(dir, task).ok_or_else(|| format!("No task `{task}`."))?;
    if status.state.ended() {
        return Ok(describe(&status));
    }
    write_json(
        &requests_dir(dir).join(format!("{}-cancel-{task}.json", now_millis())),
        &Request::Cancel {
            task: task.to_string(),
        },
    )?;
    Ok(format!("Asked oxi to stop task {task}."))
}

fn now_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// A task's status for the agent, with its report when it ended.
pub fn describe(status: &Status) -> String {
    let who = match LlmProviderKind::ALL
        .into_iter()
        .find(|k| k.slug() == status.provider)
    {
        Some(kind) if status.model.is_empty() => kind.label().to_string(),
        Some(kind) => format!("{} · {}", kind.label(), status.model),
        None => status.provider.clone(),
    };
    let state = match status.state {
        TaskState::Starting => "starting",
        TaskState::Running => "running",
        TaskState::Completed => "completed",
        TaskState::Failed => "failed",
        TaskState::Cancelled => "cancelled",
        TaskState::Rejected => "not started",
    };
    let mut text = format!("Task {} ({who}): {state}.", status.task);
    if let Some(error) = &status.error {
        text.push_str(&format!(" {error}"));
    }
    if let Some(result) = status.result.as_deref().filter(|r| !r.trim().is_empty()) {
        text.push_str(&format!("\n\nReport:\n{}", result.trim()));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_is_taken_by_the_app_and_its_status_read_back() {
        let dir = crate::fsutil::unique_temp_path(&std::env::temp_dir(), "oxi-delegation", "d");
        let app_dir = dir.clone();
        // The app: answer the first request it finds.
        let app = std::thread::spawn(move || {
            for _ in 0..200 {
                if let Some(Request::Delegate { task, provider, .. }) =
                    take_requests(&app_dir).into_iter().next()
                {
                    assert_eq!(provider, "codexacp");
                    write_status(
                        &app_dir,
                        &Status {
                            task,
                            state: TaskState::Running,
                            provider,
                            model: String::new(),
                            title: "Review".into(),
                            chat: None,
                            worktree: None,
                            result: None,
                            error: None,
                        },
                    );
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        let started = call(
            Some(&dir),
            DELEGATE,
            &json!({"provider": "codex", "prompt": "Review the diff"}),
        )
        .unwrap();
        app.join().unwrap();
        let task = started.split_whitespace().nth(2).unwrap().to_string();
        assert!(started.contains("on Codex (ACP)"), "{started}");

        let mut status = read_status(&dir, &task).unwrap();
        assert!(
            call(Some(&dir), STATUS, &json!({"task_id": task}))
                .unwrap()
                .contains("running")
        );
        status.state = TaskState::Completed;
        status.result = Some("No issues found.".into());
        write_status(&dir, &status);
        let done = call(
            Some(&dir),
            STATUS,
            &json!({"task_id": task, "wait_seconds": 5}),
        )
        .unwrap();
        assert!(
            done.contains("completed") && done.ends_with("No issues found."),
            "{done}"
        );
        assert!(fetched(&dir, &task), "the parent read the report itself");
        assert!(call(Some(&dir), STATUS, &json!({"task_id": "../x"})).is_err());
        assert!(call(None, DELEGATE, &json!({})).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn providers_are_found_by_plain_names_and_slugs() {
        assert_eq!(parse_provider("Codex"), Some(LlmProviderKind::CodexAcp));
        assert_eq!(
            parse_provider("claude"),
            Some(LlmProviderKind::ClaudeCodeAcp)
        );
        assert_eq!(
            parse_provider("openrouter"),
            Some(LlmProviderKind::OpenRouter)
        );
        assert_eq!(parse_provider("nope"), None);
    }
}
