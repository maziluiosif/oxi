//! Convert UI [`ChatMessage`](crate::model::ChatMessage) list to OpenAI-style `messages`.
//!
//! ## Context window management
//! Long conversations can exceed the model's context window and cause HTTP 400 errors.
//! [`build_openai_messages`] applies a two-pass budget trim:
//!
//! 1. The system prompt and the **last `MIN_KEEP_TURNS` user+assistant pairs** are always kept.
//! 2. Older turns are counted character-by-character and dropped from the oldest end until the
//!    total estimated character count is below [`CONTEXT_CHAR_BUDGET`].
//!
//! A conservative `~4 chars per token` ratio is used; the budget targets ~100k tokens which is
//! safe for all current providers (GPT-4o context is 128k tokens).

use base64::Engine;
use serde_json::{json, Value};

use crate::model::{AssistantBlock, ChatMessage, MsgRole, UserAttachment};

/// Approximate character budget before trimming old turns (~100k tokens × 4 chars/token).
const CONTEXT_CHAR_BUDGET: usize = 400_000;

/// Always keep this many most-recent user+assistant turn pairs regardless of budget.
const MIN_KEEP_TURNS: usize = 6;

pub fn build_openai_messages(system: &str, chat: &[ChatMessage]) -> Vec<Value> {
    let system_msg = json!({ "role": "system", "content": system });

    // Build all candidate turn JSON values first.
    let mut turns: Vec<Value> = chat
        .iter()
        .filter_map(message_to_openai)
        .collect();

    // Apply context budget trimming.
    trim_to_budget(&mut turns, system.len());

    let mut out = Vec::with_capacity(1 + turns.len());
    out.push(system_msg);
    out.extend(turns);
    out
}

/// Convert a single [`ChatMessage`] to an OpenAI message JSON value.
/// Returns `None` for empty / still-streaming assistant messages.
fn message_to_openai(m: &ChatMessage) -> Option<Value> {
    match m.role {
        MsgRole::User => {
            if m.text.trim().is_empty() && m.attachments.is_empty() {
                return None;
            }
            Some(json!({
                "role": "user",
                "content": user_content_to_openai(&m.text, &m.attachments)
            }))
        }
        MsgRole::Assistant => {
            if m.streaming {
                return None;
            }
            let text = flatten_assistant(m);
            if text.is_empty() {
                return None;
            }
            Some(json!({ "role": "assistant", "content": text }))
        }
    }
}

fn user_content_to_openai(text: &str, attachments: &[UserAttachment]) -> Value {
    if attachments.is_empty() {
        return Value::String(text.to_string());
    }

    let mut blocks: Vec<Value> = Vec::new();
    if !text.trim().is_empty() {
        blocks.push(json!({ "type": "text", "text": text }));
    }

    for attachment in attachments {
        match attachment {
            UserAttachment::Image { mime, data } => {
                blocks.push(json!({
                    "type": "image_url",
                    "image_url": {
                        "url": format!(
                            "data:{};base64,{}",
                            mime,
                            base64::engine::general_purpose::STANDARD.encode(data)
                        ),
                        "detail": "auto"
                    }
                }));
            }
        }
    }

    Value::Array(blocks)
}

fn flatten_assistant(m: &ChatMessage) -> String {
    let mut s = String::new();
    for b in &m.blocks {
        match b {
            AssistantBlock::Thinking(t) => {
                if !t.is_empty() {
                    s.push_str("(thinking) ");
                    s.push_str(t);
                    s.push('\n');
                }
            }
            AssistantBlock::Answer(t) => {
                s.push_str(t);
            }
            AssistantBlock::Tool {
                name,
                output,
                args_summary,
                ..
            } => {
                s.push_str("\n[tool ");
                s.push_str(name);
                s.push(']');
                if let Some(a) = args_summary {
                    s.push_str(a);
                }
                s.push('\n');
                // Truncate tool output in history to avoid blowing up context with large reads.
                const HISTORY_TOOL_OUTPUT_CAP: usize = 8_000;
                if output.len() > HISTORY_TOOL_OUTPUT_CAP {
                    s.push_str(&output[..HISTORY_TOOL_OUTPUT_CAP]);
                    s.push_str("\n… [truncated in history]");
                } else {
                    s.push_str(output);
                }
                s.push('\n');
            }
        }
    }
    s
}

/// Estimate character count for a JSON value (rough proxy for token count).
fn value_char_len(v: &Value) -> usize {
    v.to_string().len()
}

/// Drop oldest turns until the total character count (system + turns) fits the budget,
/// while always keeping at least `MIN_KEEP_TURNS` pairs from the end.
fn trim_to_budget(turns: &mut Vec<Value>, system_len: usize) {
    let total: usize = system_len + turns.iter().map(value_char_len).sum::<usize>();
    if total <= CONTEXT_CHAR_BUDGET {
        return;
    }

    // Identify the minimum tail we must keep: last MIN_KEEP_TURNS pairs = 2×MIN_KEEP_TURNS messages.
    let min_keep_msgs = (MIN_KEEP_TURNS * 2).min(turns.len());
    let keep_from = turns.len().saturating_sub(min_keep_msgs);

    // Drop from the front until we're under budget or we've reached the protected tail.
    let mut drop_until = 0usize;
    let mut running = total;
    for (i, v) in turns.iter().enumerate() {
        if i >= keep_from {
            break;
        }
        if running <= CONTEXT_CHAR_BUDGET {
            break;
        }
        running = running.saturating_sub(value_char_len(v));
        drop_until = i + 1;
    }

    if drop_until > 0 {
        turns.drain(0..drop_until);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AssistantBlock, ChatMessage, MsgRole};

    fn user_msg(text: &str) -> ChatMessage {
        ChatMessage {
            role: MsgRole::User,
            text: text.to_string(),
            attachments: vec![],
            blocks: vec![],
            streaming: false,
        }
    }

    fn assistant_msg(answer: &str) -> ChatMessage {
        ChatMessage {
            role: MsgRole::Assistant,
            text: String::new(),
            attachments: vec![],
            blocks: vec![AssistantBlock::Answer(answer.to_string())],
            streaming: false,
        }
    }

    #[test]
    fn short_conversation_untouched() {
        let chat = vec![user_msg("hello"), assistant_msg("hi")];
        let msgs = build_openai_messages("system", &chat);
        // system + user + assistant
        assert_eq!(msgs.len(), 3);
    }

    #[test]
    fn long_conversation_trimmed_to_budget() {
        // Each message ~40k chars in serialized JSON (big enough to force trimming across 20 pairs).
        let big = "x".repeat(10_000);
        let mut chat: Vec<ChatMessage> = Vec::new();
        for _ in 0..20 {
            chat.push(user_msg(&big));
            chat.push(assistant_msg(&big));
        }
        let msgs = build_openai_messages("system", &chat);
        // Total serialized size must be within budget.
        let total: usize = msgs.iter().map(|v| v.to_string().len()).sum();
        assert!(
            total <= CONTEXT_CHAR_BUDGET + 2048, // allow minor rounding
            "total {total} exceeds budget {CONTEXT_CHAR_BUDGET}"
        );
        // MIN_KEEP_TURNS pairs must always be present (+ system message).
        assert!(msgs.len() >= MIN_KEEP_TURNS * 2 + 1);
    }

    #[test]
    fn min_keep_turns_preserved_even_over_budget() {
        let huge = "y".repeat(CONTEXT_CHAR_BUDGET / 2);
        let chat = vec![
            user_msg(&huge),
            assistant_msg(&huge),
            user_msg(&huge),
            assistant_msg(&huge),
        ];
        let msgs = build_openai_messages("system", &chat);
        // Even if we can't fit everything, the last MIN_KEEP_TURNS pairs stay.
        // With only 2 pairs and MIN_KEEP_TURNS=6, all turns are kept.
        assert!(msgs.len() >= 3); // at minimum: system + 1 user + 1 assistant
    }
}
