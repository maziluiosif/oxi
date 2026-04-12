//! Events from the background agent thread to the egui UI (RPC-free).

use serde_json::Value;

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
    /// LLM finished one assistant message (may still have tool rounds).
    AssistantMessageDone,
    /// Entire turn finished (no more tool calls).
    AgentEnd,
}
