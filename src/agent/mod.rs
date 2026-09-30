//! Local agent (no pi RPC): LLM streaming + tools.

pub mod acp;
pub mod activity_log;
mod anthropic;
mod approval;
mod codex_responses;
pub mod complete;
mod dispatch;
pub mod events;
mod history;
mod loop_ctx;
pub mod mcp;
pub mod models;
mod net;
mod openai;
pub mod prompt;
pub mod runner;
pub mod subagent;
pub mod tools;

/// Resolve the fixture interpreter before invoking cmd, which discards inherited environment
/// variables longer than 8191 characters (Cargo can expand PATH with native library directories).
#[cfg(test)]
pub(crate) fn test_python_executable() -> std::path::PathBuf {
    let name = if cfg!(windows) {
        "python.exe"
    } else {
        "python3"
    };
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
        .unwrap_or_else(|| {
            panic!("Python 3 is required for subprocess tests; {name} was not found on PATH")
        })
}

pub use approval::{ApprovalDecision, suggest_bash_allow_prefix};
pub use complete::{CompleteEvent, CompleteRequest, spawn_completion};
pub use events::{AgentEvent, AgentOutcome, TokenUsage};
pub(crate) use history::flatten_assistant;
pub use history::{AUTO_COMPACT_THRESHOLD, DEFAULT_CHARS_PER_TOKEN, calibrate_chars_per_token};
pub use models::fetch_models;
pub use runner::spawn_agent_run;
