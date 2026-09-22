//! Events from the background agent thread to the egui UI (RPC-free).

use serde_json::Value;

use crate::model::WireCache;

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
    /// Output tokens from rounds whose generation time was measured (see `generation_ms`).
    /// Kept separate from `output_tokens` so an untimed round cannot inflate the rate.
    pub timed_output_tokens: u64,
    /// Wall-clock time spent streaming model output, excluding tool runs and approvals.
    pub generation_ms: u64,
}

impl TokenUsage {
    pub fn add(&mut self, other: &TokenUsage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cache_read_input_tokens += other.cache_read_input_tokens;
        self.cache_creation_input_tokens += other.cache_creation_input_tokens;
        self.timed_output_tokens += other.timed_output_tokens;
        self.generation_ms += other.generation_ms;
    }

    /// Record how long this round spent streaming output. A server-reported duration
    /// (llama.cpp `timings.predicted_ms`) wins over the client-side measurement because it
    /// excludes network and prompt-processing time.
    pub fn record_generation(&mut self, measured: std::time::Duration) {
        if self.output_tokens == 0 {
            return;
        }
        if self.generation_ms == 0 {
            self.generation_ms = measured.as_millis() as u64;
        }
        if self.generation_ms > 0 {
            self.timed_output_tokens = self.output_tokens;
        }
    }

    /// Output tokens per second across the timed rounds, if enough was measured to be
    /// meaningful. Very short windows (a single buffered chunk) would report nonsense.
    pub fn output_tokens_per_sec(&self) -> Option<f64> {
        const MIN_WINDOW_MS: u64 = 200;
        if self.timed_output_tokens == 0 || self.generation_ms < MIN_WINDOW_MS {
            return None;
        }
        Some(self.timed_output_tokens as f64 * 1000.0 / self.generation_ms as f64)
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
pub enum AgentOutcome {
    Success { wire_cache: Option<WireCache> },
    Failed { error: String },
    Cancelled,
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
    /// The only terminal event for a run.
    Finished(AgentOutcome),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn rate_uses_only_timed_rounds() {
        let mut timed = TokenUsage {
            output_tokens: 100,
            ..Default::default()
        };
        timed.record_generation(Duration::from_secs(2));
        let untimed = TokenUsage {
            output_tokens: 900,
            ..Default::default()
        };
        let mut total = TokenUsage::default();
        total.add(&timed);
        total.add(&untimed);
        assert_eq!(total.output_tokens, 1000);
        assert_eq!(total.output_tokens_per_sec(), Some(50.0));
    }

    #[test]
    fn server_reported_duration_wins() {
        let mut u = TokenUsage {
            output_tokens: 300,
            generation_ms: 1000,
            ..Default::default()
        };
        u.record_generation(Duration::from_secs(10));
        assert_eq!(u.output_tokens_per_sec(), Some(300.0));
    }

    #[test]
    fn no_rate_without_output_or_for_tiny_windows() {
        let mut empty = TokenUsage::default();
        empty.record_generation(Duration::from_secs(5));
        assert_eq!(empty.output_tokens_per_sec(), None);

        let mut burst = TokenUsage {
            output_tokens: 50,
            ..Default::default()
        };
        burst.record_generation(Duration::from_millis(20));
        assert_eq!(burst.output_tokens_per_sec(), None);
    }
}
