mod dedupe;
mod format;
mod io;
mod paths;

use std::cmp::Reverse;
use std::fs;
use std::path::Path;
use std::time::SystemTime;

use crate::model::{make_session_title, Session};

#[cfg(test)]
pub use dedupe::chat_messages_equal;
use io::parse_session_header_and_messages;
pub use io::{load_session_messages, save_session_messages};
use paths::{agent_dir, configured_session_dir, default_session_dir};

pub fn load_workspace_sessions(root_path: &str) -> Vec<Session> {
    let root = Path::new(root_path);
    load_workspace_sessions_from(root, &agent_dir())
}

fn load_workspace_sessions_from(root_path: &Path, agent_dir: &Path) -> Vec<Session> {
    let session_dir = configured_session_dir(root_path, agent_dir)
        .unwrap_or_else(|| default_session_dir(root_path, agent_dir));
    let Ok(entries) = fs::read_dir(&session_dir) else {
        return Vec::new();
    };

    let mut sessions: Vec<LoadedSession> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("jsonl"))
        .filter_map(|path| parse_session_file(&path))
        .collect();

    sessions.sort_by_key(|session| Reverse(session.modified));

    sessions
        .into_iter()
        .map(|session| Session {
            title: session.title,
            messages: Vec::new(),
            session_file: Some(session.path),
            messages_loaded: false,
            input_text: String::new(),
            pending_images: Vec::new(),
        })
        .collect()
}

struct LoadedSession {
    path: String,
    title: String,
    modified: SystemTime,
}

fn parse_session_file(path: &Path) -> Option<LoadedSession> {
    let (session_name, first_user_message) = parse_session_header_and_messages(path)?;
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    let title = session_name
        .or_else(|| first_user_message.map(|text| make_session_title(&text)))
        .unwrap_or_else(|| "New chat".to_string());

    Some(LoadedSession {
        path: path.to_string_lossy().to_string(),
        title,
        modified,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hydrate;
    use crate::model::{AssistantBlock, ChatMessage, MsgRole, Session, UserAttachment};
    use serde_json::json;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn temp_root(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_else(|_| Duration::from_secs(0))
            .as_nanos();
        let path = std::env::temp_dir().join(format!("oxi-{name}-{nanos}"));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn dedupe_trailing_duplicate_messages_removes_repeated_history() {
        let user = ChatMessage {
            role: MsgRole::User,
            text: "hello".into(),
            attachments: vec![UserAttachment::Image {
                mime: "image/png".into(),
                data: vec![1, 2, 3],
            }],
            blocks: vec![],
            streaming: false,
        };
        let assistant = ChatMessage {
            role: MsgRole::Assistant,
            text: String::new(),
            attachments: vec![],
            blocks: vec![
                AssistantBlock::Thinking("plan".into()),
                AssistantBlock::Tool {
                    tool_call_id: "call_1".into(),
                    name: "bash".into(),
                    args_summary: Some("{\"command\":\"pwd\"}".into()),
                    output: "/tmp\n".into(),
                    diff: None,
                    is_error: Some(false),
                    full_output_path: Some("/tmp/out.txt".into()),
                    output_truncated: false,
                },
                AssistantBlock::Answer("done".into()),
            ],
            streaming: false,
        };
        let mut messages = vec![user.clone(), assistant.clone(), user, assistant];

        crate::session_store::dedupe::dedupe_trailing_duplicate_messages(&mut messages);

        assert_eq!(messages.len(), 2);
    }

    #[test]
    fn dedupe_trailing_duplicate_messages_keeps_distinct_history() {
        let mut messages = vec![
            ChatMessage {
                role: MsgRole::User,
                text: "hello".into(),
                attachments: vec![],
                blocks: vec![],
                streaming: false,
            },
            ChatMessage {
                role: MsgRole::Assistant,
                text: String::new(),
                attachments: vec![],
                blocks: vec![AssistantBlock::Answer("first".into())],
                streaming: false,
            },
            ChatMessage {
                role: MsgRole::User,
                text: "hello".into(),
                attachments: vec![],
                blocks: vec![],
                streaming: false,
            },
            ChatMessage {
                role: MsgRole::Assistant,
                text: String::new(),
                attachments: vec![],
                blocks: vec![AssistantBlock::Answer("second".into())],
                streaming: false,
            },
        ];

        crate::session_store::dedupe::dedupe_trailing_duplicate_messages(&mut messages);

        assert_eq!(messages.len(), 4);
    }

    #[test]
    fn save_session_messages_dedupes_duplicate_full_history_before_writing() {
        let root = temp_root("save-dedupe");
        let mut session = Session {
            title: "Chat".into(),
            messages: hydrate::messages_from_get_messages(&json!({
                "messages": [
                    { "role": "user", "content": "hello" },
                    { "role": "assistant", "content": [{ "type": "text", "text": "hi" }] },
                    { "role": "user", "content": "hello" },
                    { "role": "assistant", "content": [{ "type": "text", "text": "hi" }] }
                ]
            })),
            session_file: None,
            messages_loaded: true,
            input_text: String::new(),
            pending_images: Vec::new(),
        };

        save_session_messages(root.to_str().unwrap(), &mut session).unwrap();

        assert_eq!(session.messages.len(), 2);
        let loaded = load_session_messages(session.session_file.as_deref().unwrap()).unwrap();
        assert_eq!(loaded.len(), 2);
        assert!(chat_messages_equal(&loaded[0], &session.messages[0]));
        assert!(chat_messages_equal(&loaded[1], &session.messages[1]));
    }

    #[test]
    fn save_and_reload_preserves_assistant_block_formatting() {
        let root = temp_root("formatting-roundtrip");
        let mut session = Session {
            title: "Chat".into(),
            messages: vec![
                ChatMessage {
                    role: MsgRole::User,
                    text: "look".into(),
                    attachments: vec![UserAttachment::Image {
                        mime: "image/png".into(),
                        data: vec![1, 2, 3, 4],
                    }],
                    blocks: vec![],
                    streaming: false,
                },
                ChatMessage {
                    role: MsgRole::Assistant,
                    text: String::new(),
                    attachments: vec![],
                    blocks: vec![
                        AssistantBlock::Thinking("thinking".into()),
                        AssistantBlock::Tool {
                            tool_call_id: "call_123".into(),
                            name: "edit".into(),
                            args_summary: Some("{\"path\":\"src/main.rs\"}".into()),
                            output: "changed file".into(),
                            diff: Some("+new\n-old".into()),
                            is_error: Some(false),
                            full_output_path: Some("/tmp/tool-output.txt".into()),
                            output_truncated: true,
                        },
                        AssistantBlock::Answer("done".into()),
                    ],
                    streaming: false,
                },
            ],
            session_file: None,
            messages_loaded: true,
            input_text: String::new(),
            pending_images: Vec::new(),
        };

        save_session_messages(root.to_str().unwrap(), &mut session).unwrap();
        let loaded = load_session_messages(session.session_file.as_deref().unwrap()).unwrap();

        assert_eq!(loaded.len(), session.messages.len());
        assert!(chat_messages_equal(&loaded[0], &session.messages[0]));
        assert!(chat_messages_equal(&loaded[1], &session.messages[1]));
    }
}
