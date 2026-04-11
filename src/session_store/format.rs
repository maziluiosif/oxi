use std::path::Path;

use base64::Engine;
use serde_json::{json, Value};

use crate::model::{AssistantBlock, ChatMessage, MsgRole, UserAttachment};

use super::paths::generate_session_id;

pub fn session_file_stem_or_generated(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.trim().is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(generate_session_id)
}

pub fn chat_message_to_json_entries(message: &ChatMessage) -> Vec<Value> {
    match message.role {
        MsgRole::User => vec![json!({
            "role": "user",
            "content": user_content_to_json(&message.text, &message.attachments),
        })],
        MsgRole::Assistant => assistant_message_to_json_entries(&message.blocks),
    }
}

fn user_content_to_json(text: &str, attachments: &[UserAttachment]) -> Value {
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
                "type": "image",
                "mimeType": mime,
                "data": base64::engine::general_purpose::STANDARD.encode(data),
            })),
        }
    }

    Value::Array(blocks)
}

fn assistant_message_to_json_entries(blocks: &[AssistantBlock]) -> Vec<Value> {
    let mut assistant_content = Vec::new();
    let mut trailing_entries = Vec::new();

    for block in blocks {
        match block {
            AssistantBlock::Thinking(s) => {
                if !s.is_empty() {
                    assistant_content.push(json!({
                        "type": "thinking",
                        "thinking": s,
                    }));
                }
            }
            AssistantBlock::Answer(s) => {
                if !s.is_empty() {
                    assistant_content.push(json!({
                        "type": "text",
                        "text": s,
                    }));
                }
            }
            AssistantBlock::Tool {
                tool_call_id,
                name,
                args_summary,
                output,
                diff,
                is_error,
                full_output_path,
                output_truncated,
            } => {
                assistant_content.push(json!({
                    "type": "toolCall",
                    "id": tool_call_id,
                    "name": name,
                    "arguments": args_summary
                        .as_deref()
                        .and_then(|s| serde_json::from_str::<Value>(s).ok())
                        .unwrap_or_else(|| Value::String(args_summary.clone().unwrap_or_default())),
                }));

                if !output.trim().is_empty()
                    || diff.as_deref().is_some_and(|d| !d.trim().is_empty())
                    || is_error.is_some()
                    || full_output_path.is_some()
                    || *output_truncated
                {
                    let mut result = json!({
                        "role": "toolResult",
                        "toolCallId": tool_call_id,
                        "toolName": name,
                        "content": [{ "type": "text", "text": output }],
                    });

                    if let Some(is_error) = is_error {
                        result["isError"] = json!(is_error);
                    }
                    if diff.as_deref().is_some_and(|d| !d.trim().is_empty())
                        || full_output_path.is_some()
                        || *output_truncated
                    {
                        let mut details = json!({});
                        if let Some(diff) = diff {
                            details["diff"] = json!(diff);
                        }
                        if let Some(path) = full_output_path {
                            details["fullOutputPath"] = json!(path);
                        }
                        if *output_truncated {
                            details["truncation"] = json!({ "truncated": true });
                        }
                        result["details"] = details;
                    }

                    trailing_entries.push(result);
                }
            }
        }
    }

    let mut entries = vec![json!({
        "role": "assistant",
        "content": Value::Array(assistant_content),
    })];
    entries.extend(trailing_entries);
    entries
}
