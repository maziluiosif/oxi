//! Local agent (no pi RPC): LLM streaming + tools.

mod anthropic;
mod codex_responses;
mod copilot;
pub mod events;
mod history;
mod openai;
pub mod prompt;
pub mod runner;
pub mod tools;

pub use events::AgentEvent;
pub use runner::spawn_agent_run;
