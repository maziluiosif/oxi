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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AssistantBlock, MsgRole, UserAttachment};
    use std::path::Path;

    #[test]
    fn session_file_stem_from_path() {
        let p = Path::new("/tmp/sessions/my-session.jsonl");
        assert_eq!(session_file_stem_or_generated(p), "my-session");
    }

    #[test]
    fn session_file_stem_uses_actual_stem() {
        // A `.jsonl` hidden file should use the file stem (which is ".jsonl" itself)
        let p = Path::new("/tmp/sessions/.jsonl");
        let stem = session_file_stem_or_generated(p);
        // .jsonl is a dotfile, file_stem = ".jsonl" which is non-empty, so it uses that
        assert_eq!(stem, ".jsonl");
    }

    #[test]
    fn user_message_to_json_plain_text() {
        let msg = ChatMessage {
            role: MsgRole::User,
            text: "hello".into(),
            attachments: vec![],
            blocks: vec![],
            streaming: false,
        };
        let entries = chat_message_to_json_entries(&msg);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["role"], "user");
        assert_eq!(entries[0]["content"], "hello");
    }

    #[test]
    fn user_message_with_image_produces_array_content() {
        let msg = ChatMessage {
            role: MsgRole::User,
            text: "look".into(),
            attachments: vec![UserAttachment::Image {
                mime: "image/png".into(),
                data: vec![1, 2, 3],
            }],
            blocks: vec![],
            streaming: false,
        };
        let entries = chat_message_to_json_entries(&msg);
        assert_eq!(entries.len(), 1);
        let content = entries[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2); // text + image
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "image");
    }

    #[test]
    fn assistant_message_with_thinking_and_answer() {
        let msg = ChatMessage {
            role: MsgRole::Assistant,
            text: String::new(),
            attachments: vec![],
            blocks: vec![
                AssistantBlock::Thinking("reason".into()),
                AssistantBlock::Answer("result".into()),
            ],
            streaming: false,
        };
        let entries = chat_message_to_json_entries(&msg);
        assert_eq!(entries.len(), 1);
        let content = entries[0]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "thinking");
        assert_eq!(content[1]["type"], "text");
    }

    #[test]
    fn assistant_message_with_tool_produces_tool_result() {
        let msg = ChatMessage {
            role: MsgRole::Assistant,
            text: String::new(),
            attachments: vec![],
            blocks: vec![AssistantBlock::Tool {
                tool_call_id: "call_1".into(),
                name: "bash".into(),
                args_summary: Some(r#"{"command":"ls"}"#.into()),
                output: "file.txt\n".into(),
                diff: None,
                is_error: Some(false),
                full_output_path: None,
                output_truncated: false,
            }],
            streaming: false,
        };
        let entries = chat_message_to_json_entries(&msg);
        assert_eq!(entries.len(), 2); // assistant + toolResult
        assert_eq!(entries[0]["role"], "assistant");
        assert_eq!(entries[1]["role"], "toolResult");
        assert_eq!(entries[1]["toolCallId"], "call_1");
    }

    #[test]
    fn assistant_message_with_tool_diff_includes_details() {
        let msg = ChatMessage {
            role: MsgRole::Assistant,
            text: String::new(),
            attachments: vec![],
            blocks: vec![AssistantBlock::Tool {
                tool_call_id: "call_2".into(),
                name: "edit".into(),
                args_summary: None,
                output: "edited".into(),
                diff: Some("+line\n-old".into()),
                is_error: None,
                full_output_path: Some("/tmp/out.txt".into()),
                output_truncated: true,
            }],
            streaming: false,
        };
        let entries = chat_message_to_json_entries(&msg);
        let result = &entries[1];
        assert!(result.get("details").is_some());
        assert_eq!(result["details"]["diff"], "+line\n-old");
        assert_eq!(result["details"]["fullOutputPath"], "/tmp/out.txt");
        assert!(result["details"]["truncation"]["truncated"]
            .as_bool()
            .unwrap());
    }

    #[test]
    fn empty_blocks_skipped() {
        let msg = ChatMessage {
            role: MsgRole::Assistant,
            text: String::new(),
            attachments: vec![],
            blocks: vec![
                AssistantBlock::Thinking("".into()),
                AssistantBlock::Answer("".into()),
            ],
            streaming: false,
        };
        let entries = chat_message_to_json_entries(&msg);
        let content = entries[0]["content"].as_array().unwrap();
        assert!(content.is_empty());
    }
}
