//! Convert UI [`ChatMessage`](crate::model::ChatMessage) list to OpenAI-style `messages` (lossy for old tool turns).

use base64::Engine;
use serde_json::{json, Value};

use crate::model::{AssistantBlock, ChatMessage, MsgRole, UserAttachment};

pub fn build_openai_messages(system: &str, chat: &[ChatMessage]) -> Vec<Value> {
    let mut out = vec![json!({
        "role": "system",
        "content": system
    })];
    for m in chat {
        match m.role {
            MsgRole::User => {
                if !m.text.trim().is_empty() || !m.attachments.is_empty() {
                    out.push(json!({
                        "role": "user",
                        "content": user_content_to_openai(&m.text, &m.attachments)
                    }));
                }
            }
            MsgRole::Assistant => {
                if !m.streaming {
                    let text = flatten_assistant(m);
                    if !text.is_empty() {
                        out.push(json!({ "role": "assistant", "content": text }));
                    }
                }
            }
        }
    }
    out
}

fn user_content_to_openai(text: &str, attachments: &[UserAttachment]) -> Value {
    if attachments.is_empty() {
        return Value::String(text.to_string());
    }

    let mut blocks = Vec::new();
    if !text.trim().is_empty() {
        blocks.push(json!({
            "type": "text",
            "text": text,
        }));
    }

    for attachment in attachments {
        match attachment {
            UserAttachment::Image { mime, data } => blocks.push(json!({
                "type": "image_url",
                "image_url": {
                    "url": format!(
                        "data:{};base64,{}",
                        mime,
                        base64::engine::general_purpose::STANDARD.encode(data)
                    )
                }
            })),
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
                s.push_str(output);
                s.push('\n');
            }
        }
    }
    s
}
