//! GitHub Copilot HTTP details shared across backends.
//!
//! Copilot bills premium quota per request when `X-Initiator` is `user`. Multi-turn agent
//! flows (tool results, assistant continuation) must send `X-Initiator: agent` so only the
//! top-level user message counts—matching VS Code / Copilot CLI behavior.

use serde_json::Value;

/// Infer [`X-Initiator`](https://github.com/BerriAI/litellm/pull/25278) from OpenAI-format
/// `messages` (including `role: "system"` / `"user"` / `"assistant"` / `"tool"`).
pub(crate) fn copilot_x_initiator_from_openai_messages(messages: &[Value]) -> &'static str {
    let Some(last) = messages.last() else {
        return "user";
    };
    match last.get("role").and_then(|x| x.as_str()) {
        Some("user") => "user",
        _ => "agent",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn last_user_message_is_user_initiator() {
        let m = vec![json!({"role": "system", "content": "s"}), json!({"role": "user", "content": "hi"})];
        assert_eq!(copilot_x_initiator_from_openai_messages(&m), "user");
    }

    #[test]
    fn tool_follow_up_is_agent_initiator() {
        let m = vec![
            json!({"role": "user", "content": "run"}),
            json!({"role": "assistant", "content": "", "tool_calls": []}),
            json!({"role": "tool", "tool_call_id": "x", "content": "out"}),
        ];
        assert_eq!(copilot_x_initiator_from_openai_messages(&m), "agent");
    }
}
