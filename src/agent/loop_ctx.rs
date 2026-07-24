//! Shared parameters for the provider-specific `run_*_loop` functions (openai/anthropic/codex).

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Sender;

use super::approval::ApprovalGate;
use super::events::AgentEvent;
use super::tools::ToolEnv;

pub struct LoopCtx<'a> {
    pub client: &'a reqwest::Client,
    pub base_url: &'a str,
    pub model: &'a str,
    pub cwd: &'a Path,
    pub env: &'a ToolEnv,
    pub tx: &'a Sender<AgentEvent>,
    pub cancel: &'a Arc<AtomicBool>,
    pub gate: &'a mut ApprovalGate,
    pub max_rounds: u32,
    pub effort_override: Option<&'a str>,
    /// Char ceiling above which history is trimmed before each round, so a long multi-round
    /// run cannot grow the prompt past the context window mid-flight. See the ladder in
    /// [`crate::agent::history`]. `usize::MAX` disables mid-run trimming (single-round helpers).
    pub context_char_budget: usize,
    /// Size (chars) of the tool definitions sent every round, counted as fixed overhead when
    /// measuring history against [`Self::context_char_budget`].
    pub tools_chars: usize,
}
