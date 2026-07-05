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
}
