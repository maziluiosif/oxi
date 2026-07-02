//! Local agent (no pi RPC): LLM streaming + tools.

mod anthropic;
mod approval;
mod codex_responses;
pub mod complete;
pub mod events;
mod history;
pub mod models;
mod net;
mod openai;
pub mod prompt;
pub mod runner;
pub mod tools;

pub use approval::ApprovalDecision;
pub use complete::{spawn_completion, CompleteEvent, CompleteRequest};
pub use events::AgentEvent;
pub use models::fetch_models;
pub use runner::spawn_agent_run;
