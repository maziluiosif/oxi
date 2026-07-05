//! Local agent (no pi RPC): LLM streaming + tools.

mod anthropic;
mod approval;
mod codex_responses;
pub mod complete;
pub mod events;
mod history;
mod loop_ctx;
pub mod models;
mod net;
mod openai;
pub mod prompt;
pub mod runner;
pub mod tools;

pub use approval::ApprovalDecision;
pub use complete::{CompleteEvent, CompleteRequest, spawn_completion};
pub use events::AgentEvent;
pub use models::fetch_models;
pub use runner::spawn_agent_run;
