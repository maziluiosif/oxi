//! Events from the background agent thread to the egui UI (RPC-free).

use serde_json::Value;

/// Token usage reported by a provider for one request/round.
///
/// `input_tokens` counts only the uncached remainder; the full prompt size is
/// `input + cache_read + cache_creation`.
#[derive(Debug, Default, Clone, Copy)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
}

impl TokenUsage {
    pub fn add(&mut self, other: &TokenUsage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cache_read_input_tokens += other.cache_read_input_tokens;
        self.cache_creation_input_tokens += other.cache_creation_input_tokens;
    }

    pub fn is_zero(&self) -> bool {
        self.input_tokens == 0
            && self.output_tokens == 0
            && self.cache_read_input_tokens == 0
            && self.cache_creation_input_tokens == 0
    }

    /// Total prompt tokens processed (cached + uncached).
    pub fn total_input(&self) -> u64 {
        self.input_tokens + self.cache_read_input_tokens + self.cache_creation_input_tokens
    }

    /// Fraction of the prompt served from cache, in percent (0 when unknown).
    pub fn cache_hit_pct(&self) -> u64 {
        let total = self.total_input();
        if total == 0 {
            return 0;
        }
        self.cache_read_input_tokens * 100 / total
    }
}

#[derive(Debug)]
pub enum AgentEvent {
    AgentStart,
    TextStart,
    TextDelta(String),
    /// Extended reasoning / thinking content from models that support it.
    ThinkingDelta(String),
    ToolStart {
        name: String,
        tool_call_id: String,
        args: Option<Value>,
    },
    /// A shell or built-in filesystem mutation tool is waiting for approval.
    ApprovalRequest {
        name: String,
        args: Option<Value>,
    },
    ToolOutput {
        tool_call_id: String,
        text: String,
        truncated: bool,
    },
    ToolEnd {
        tool_call_id: String,
        is_error: Option<bool>,
        full_output_path: Option<String>,
        diff: Option<String>,
    },
    StreamError(String),
    /// The stream died mid-round and the round is being re-sent; the UI should
    /// discard the partial text/thinking of the current round to avoid duplicates.
    StreamRetry {
        attempt: u32,
        reason: String,
    },
    /// LLM finished one assistant message (may still have tool rounds).
    AssistantMessageDone,
    /// Token usage for one provider round; the UI accumulates per turn/session.
    Usage(TokenUsage),
    /// Canonical provider wire-format history to reuse on the next turn for cache hits.
    WireHistory(Vec<Value>),
    /// Provider loop finished successfully; runner will emit `WireHistory` and then
    /// `AgentEnd` so the UI cannot drop the canonical history by clearing the receiver first.
    ProviderDone,
    /// Entire turn finished (no more tool calls).
    AgentEnd,
}
