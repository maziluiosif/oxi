//! Best-effort conversion of `get_messages` `data` JSON to [`ChatMessage`] list.

use base64::Engine;
use serde_json::Value;

use crate::model::{AssistantBlock, ChatMessage, MsgRole, UserAttachment};

/// Text extracted from `partialResult` / tool `result` `content` arrays (shared with live RPC).
pub fn tool_text_from_content_array(v: &Value) -> Option<String> {
    let arr = v.get("content")?.as_array()?;
    let mut out = String::new();
    for item in arr {
        if let Some(t) = item.get("text").and_then(|x| x.as_str()) {
            out.push_str(t);
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

pub fn tool_diff_from_result(v: &Value) -> Option<String> {
    v.get("details")
        .and_then(|d| d.get("diff"))
        .and_then(|x| x.as_str())
        .or_else(|| v.get("diff").and_then(|x| x.as_str()))
        .map(str::to_owned)
}

pub fn messages_from_get_messages(data: &Value) -> Vec<ChatMessage> {
    let Some(arr) = data.get("messages").and_then(|m| m.as_array()) else {
        return vec![];
    };
    let mut out = Vec::new();
    for m in arr {
        let Some(role) = m.get("role").and_then(|r| r.as_str()) else {
            continue;
        };
        match role {
            "user" => {
                let (text, attachments) = parse_user_message(m);
                out.push(ChatMessage {
                    role: MsgRole::User,
                    text,
                    attachments,
                    blocks: vec![],
                    streaming: false,
                });
            }
            "assistant" => {
                let blocks = assistant_blocks_from_content(m);
                if blocks.is_empty() {
                    continue;
                }
                out.push(ChatMessage {
                    role: MsgRole::Assistant,
                    text: String::new(),
                    attachments: vec![],
                    blocks,
                    streaming: false,
                });
            }
            "toolResult" => {
                merge_tool_result(&mut out, m);
            }
            _ => {}
        }
    }
    out
}

fn parse_user_message(m: &Value) -> (String, Vec<UserAttachment>) {
    let mut text = String::new();
    let mut attachments = Vec::new();
    let c = m.get("content");
    if let Some(s) = c.and_then(|x| x.as_str()) {
        return (s.to_string(), vec![]);
    }
    if let Some(parts) = c.and_then(|x| x.as_array()) {
        for p in parts {
            let ty = p.get("type").and_then(|x| x.as_str()).unwrap_or("");
            match ty {
                "text" => {
                    if let Some(t) = p.get("text").and_then(|x| x.as_str()) {
                        text.push_str(t);
                    }
                }
                "image" => {
                    let mime = p
                        .get("mimeType")
                        .and_then(|x| x.as_str())
                        .unwrap_or("image/png")
                        .to_string();
                    if let Some(b64) = p.get("data").and_then(|x| x.as_str()) {
                        if let Ok(bytes) =
                            base64::engine::general_purpose::STANDARD.decode(b64.as_bytes())
                        {
                            attachments.push(UserAttachment::Image { mime, data: bytes });
                        }
                    }
                }
                _ => {}
            }
        }
        return (text, attachments);
    }
    (String::new(), vec![])
}

fn assistant_blocks_from_content(m: &Value) -> Vec<AssistantBlock> {
    let mut blocks = Vec::new();
    let c = m.get("content");
    if let Some(s) = c.and_then(|x| x.as_str()) {
        if !s.is_empty() {
            blocks.push(AssistantBlock::Answer(s.to_string()));
        }
        return blocks;
    }
    let Some(parts) = c.and_then(|x| x.as_array()) else {
        return blocks;
    };
    for p in parts {
        let ty = p.get("type").and_then(|x| x.as_str()).unwrap_or("");
        match ty {
            "thinking" => {
                let th = p.get("thinking").and_then(|x| x.as_str()).unwrap_or("");
                if th.is_empty() {
                    continue;
                }
                match blocks.last_mut() {
                    Some(AssistantBlock::Thinking(prev)) => {
                        if !prev.is_empty() {
                            prev.push_str("\n\n");
                        }
                        prev.push_str(th);
                    }
                    _ => blocks.push(AssistantBlock::Thinking(th.to_string())),
                }
            }
            "text" => {
                let t = p.get("text").and_then(|x| x.as_str()).unwrap_or("");
                if t.is_empty() {
                    continue;
                }
                match blocks.last_mut() {
                    Some(AssistantBlock::Answer(prev)) => prev.push_str(t),
                    _ => blocks.push(AssistantBlock::Answer(t.to_string())),
                }
            }
            "toolCall" => {
                let id = p
                    .get("id")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                let name = p
                    .get("name")
                    .and_then(|x| x.as_str())
                    .unwrap_or("tool")
                    .to_string();
                let args_summary = p.get("arguments").map(|a| {
                    let s = a.to_string();
                    s.chars().take(800).collect::<String>()
                });
                blocks.push(AssistantBlock::Tool {
                    tool_call_id: id,
                    name,
                    args_summary,
                    output: String::new(),
                    diff: None,
                    is_error: None,
                    full_output_path: None,
                    output_truncated: false,
                });
            }
            _ => {}
        }
    }
    blocks
}

fn merge_tool_result(out: &mut Vec<ChatMessage>, m: &Value) {
    let id = m.get("toolCallId").and_then(|x| x.as_str()).unwrap_or("");
    let name = m.get("toolName").and_then(|x| x.as_str()).unwrap_or("tool");
    let output = extract_tool_result_text(m);
    let is_error = m.get("isError").and_then(|x| x.as_bool());
    let diff = tool_diff_from_result(m);
    let full_output_path = m
        .get("details")
        .and_then(|d| d.get("fullOutputPath"))
        .and_then(|x| x.as_str())
        .or_else(|| m.get("fullOutputPath").and_then(|x| x.as_str()))
        .map(String::from);
    let output_truncated = m.get("details").and_then(|d| d.get("truncation")).is_some()
        || m.get("truncated")
            .and_then(|x| x.as_bool())
            .unwrap_or(false);

    for msg in out.iter_mut().rev() {
        if msg.role != MsgRole::Assistant {
            continue;
        }
        for b in &mut msg.blocks {
            if let AssistantBlock::Tool {
                tool_call_id,
                output: o,
                diff: d,
                is_error: ie,
                full_output_path: fp,
                output_truncated: ot,
                ..
            } = b
            {
                if !id.is_empty() && tool_call_id == id {
                    *o = output.clone();
                    *d = diff.clone();
                    *ie = is_error;
                    *fp = full_output_path.clone();
                    *ot = output_truncated;
                    return;
                }
            }
        }
    }

    out.push(ChatMessage {
        role: MsgRole::Assistant,
        text: String::new(),
        attachments: vec![],
        blocks: vec![AssistantBlock::Tool {
            tool_call_id: id.to_string(),
            name: name.to_string(),
            args_summary: None,
            output,
            diff,
            is_error,
            full_output_path,
            output_truncated,
        }],
        streaming: false,
    });
}

fn extract_tool_result_text(m: &Value) -> String {
    tool_text_from_content_array(m).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AssistantBlock, MsgRole};
    use serde_json::json;

    #[test]
    fn messages_from_get_messages_empty_array() {
        let data = json!({ "messages": [] });
        assert!(messages_from_get_messages(&data).is_empty());
    }

    #[test]
    fn messages_from_get_messages_missing_messages() {
        let data = json!({});
        assert!(messages_from_get_messages(&data).is_empty());
    }

    #[test]
    fn messages_from_get_messages_user_and_assistant() {
        let data = json!({
            "messages": [
                {
                    "role": "user",
                    "content": "hello"
                },
                {
                    "role": "assistant",
                    "content": [
                        { "type": "text", "text": "hi there" }
                    ]
                }
            ]
        });
        let msgs = messages_from_get_messages(&data);
        assert_eq!(msgs.len(), 2);
        assert!(matches!(msgs[0].role, MsgRole::User));
        assert_eq!(msgs[0].text, "hello");
        assert!(msgs[0].attachments.is_empty());
        assert!(matches!(msgs[1].role, MsgRole::Assistant));
        match &msgs[1].blocks[..] {
            [AssistantBlock::Answer(s)] => assert_eq!(s.as_str(), "hi there"),
            _ => panic!("expected one answer block"),
        }
    }

    #[test]
    fn assistant_interleaved_thinking_and_text() {
        let data = json!({
            "messages": [{
                "role": "assistant",
                "content": [
                    { "type": "thinking", "thinking": "a" },
                    { "type": "text", "text": "b" },
                    { "type": "thinking", "thinking": "c" }
                ]
            }]
        });
        let msgs = messages_from_get_messages(&data);
        assert_eq!(msgs.len(), 1);
        match &msgs[0].blocks[..] {
            [AssistantBlock::Thinking(t1), AssistantBlock::Answer(ans), AssistantBlock::Thinking(t2)] =>
            {
                assert_eq!(t1, "a");
                assert_eq!(ans, "b");
                assert_eq!(t2, "c");
            }
            _ => panic!("unexpected blocks"),
        }
    }

    #[test]
    fn tool_result_merges_into_assistant_tool_call() {
        let data = json!({
            "messages": [
                {
                    "role": "assistant",
                    "content": [
                        { "type": "toolCall", "id": "call_1", "name": "bash", "arguments": { "command": "ls" } }
                    ]
                },
                {
                    "role": "toolResult",
                    "toolCallId": "call_1",
                    "toolName": "bash",
                    "content": [{ "type": "text", "text": "out\n" }]
                }
            ]
        });
        let msgs = messages_from_get_messages(&data);
        assert_eq!(msgs.len(), 1);
        match &msgs[0].blocks[..] {
            [AssistantBlock::Tool {
                tool_call_id,
                output,
                ..
            }] => {
                assert_eq!(tool_call_id, "call_1");
                assert_eq!(output, "out\n");
            }
            _ => panic!("unexpected blocks"),
        }
    }

    #[test]
    fn tool_result_picks_up_nested_diff_details() {
        let data = json!({
            "messages": [
                {
                    "role": "assistant",
                    "content": [
                        { "type": "toolCall", "id": "call_2", "name": "edit", "arguments": { "path": "src/main.rs" } }
                    ]
                },
                {
                    "role": "toolResult",
                    "toolCallId": "call_2",
                    "toolName": "edit",
                    "content": [{ "type": "text", "text": "edited" }],
                    "details": { "diff": "+1 new\n-1 old" }
                }
            ]
        });
        let msgs = messages_from_get_messages(&data);
        match &msgs[0].blocks[..] {
            [AssistantBlock::Tool { diff, .. }] => {
                assert_eq!(diff.as_deref(), Some("+1 new\n-1 old"));
            }
            _ => panic!("unexpected blocks"),
        }
    }
}
