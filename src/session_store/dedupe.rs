use crate::model::{AssistantBlock, ChatMessage, UserAttachment};

pub fn dedupe_trailing_duplicate_messages(messages: &mut Vec<ChatMessage>) {
    while let Some(new_len) = trailing_duplicate_pair_prefix_len(messages) {
        messages.truncate(new_len);
    }
}

fn trailing_duplicate_pair_prefix_len(messages: &[ChatMessage]) -> Option<usize> {
    if messages.len() < 4 || !messages.len().is_multiple_of(2) {
        return None;
    }

    let half = messages.len() / 2;
    if messages[..half]
        .iter()
        .zip(&messages[half..])
        .all(|(left, right)| chat_messages_equal(left, right))
    {
        Some(half)
    } else {
        None
    }
}

pub fn chat_messages_equal(left: &ChatMessage, right: &ChatMessage) -> bool {
    if left.role != right.role || left.text != right.text || left.streaming != right.streaming {
        return false;
    }
    user_attachments_equal(&left.attachments, &right.attachments)
        && assistant_blocks_equal(&left.blocks, &right.blocks)
}

fn user_attachments_equal(left: &[UserAttachment], right: &[UserAttachment]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| match (left, right) {
                (
                    UserAttachment::Image {
                        mime: left_mime,
                        data: left_data,
                    },
                    UserAttachment::Image {
                        mime: right_mime,
                        data: right_data,
                    },
                ) => left_mime == right_mime && left_data == right_data,
            })
}

fn assistant_blocks_equal(left: &[AssistantBlock], right: &[AssistantBlock]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| match (left, right) {
                (AssistantBlock::Thinking(left), AssistantBlock::Thinking(right)) => left == right,
                (AssistantBlock::Answer(left), AssistantBlock::Answer(right)) => left == right,
                (
                    AssistantBlock::Tool {
                        tool_call_id: left_tool_call_id,
                        name: left_name,
                        args_summary: left_args_summary,
                        output: left_output,
                        diff: left_diff,
                        is_error: left_is_error,
                        full_output_path: left_full_output_path,
                        output_truncated: left_output_truncated,
                    },
                    AssistantBlock::Tool {
                        tool_call_id: right_tool_call_id,
                        name: right_name,
                        args_summary: right_args_summary,
                        output: right_output,
                        diff: right_diff,
                        is_error: right_is_error,
                        full_output_path: right_full_output_path,
                        output_truncated: right_output_truncated,
                    },
                ) => {
                    left_tool_call_id == right_tool_call_id
                        && left_name == right_name
                        && left_args_summary == right_args_summary
                        && left_output == right_output
                        && left_diff == right_diff
                        && left_is_error == right_is_error
                        && left_full_output_path == right_full_output_path
                        && left_output_truncated == right_output_truncated
                }
                _ => false,
            })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::MsgRole;

    fn user(text: &str) -> ChatMessage {
        ChatMessage {
            role: MsgRole::User,
            text: text.into(),
            attachments: vec![],
            blocks: vec![],
            streaming: false,
        }
    }

    fn assistant(answer: &str) -> ChatMessage {
        ChatMessage {
            role: MsgRole::Assistant,
            text: String::new(),
            attachments: vec![],
            blocks: vec![AssistantBlock::Answer(answer.into())],
            streaming: false,
        }
    }

    #[test]
    fn dedupe_removes_exact_duplicate_halves() {
        let mut msgs = vec![
            user("hi"), assistant("hello"),
            user("hi"), assistant("hello"),
        ];
        dedupe_trailing_duplicate_messages(&mut msgs);
        assert_eq!(msgs.len(), 2);
    }

    #[test]
    fn dedupe_keeps_non_duplicates() {
        let mut msgs = vec![
            user("hi"), assistant("hello"),
            user("bye"), assistant("goodbye"),
        ];
        dedupe_trailing_duplicate_messages(&mut msgs);
        assert_eq!(msgs.len(), 4);
    }

    #[test]
    fn dedupe_handles_empty() {
        let mut msgs: Vec<ChatMessage> = vec![];
        dedupe_trailing_duplicate_messages(&mut msgs);
        assert!(msgs.is_empty());
    }

    #[test]
    fn dedupe_handles_odd_count() {
        let mut msgs = vec![user("hi"), assistant("hello"), user("extra")];
        dedupe_trailing_duplicate_messages(&mut msgs);
        assert_eq!(msgs.len(), 3);
    }

    #[test]
    fn dedupe_quadruple_reduces_to_single() {
        let mut msgs = vec![
            user("a"), assistant("b"),
            user("a"), assistant("b"),
            user("a"), assistant("b"),
            user("a"), assistant("b"),
        ];
        dedupe_trailing_duplicate_messages(&mut msgs);
        // 8 -> 4 -> 2
        assert_eq!(msgs.len(), 2);
    }

    #[test]
    fn chat_messages_equal_different_text() {
        let a = user("hello");
        let b = user("world");
        assert!(!chat_messages_equal(&a, &b));
    }

    #[test]
    fn chat_messages_equal_different_roles() {
        let a = user("hi");
        let b = assistant("hi");
        assert!(!chat_messages_equal(&a, &b));
    }

    #[test]
    fn chat_messages_equal_same_content() {
        let a = user("same");
        let b = user("same");
        assert!(chat_messages_equal(&a, &b));
    }

    #[test]
    fn chat_messages_equal_with_attachments() {
        let a = ChatMessage {
            role: MsgRole::User,
            text: "img".into(),
            attachments: vec![UserAttachment::Image {
                mime: "image/png".into(),
                data: vec![1, 2, 3],
            }],
            blocks: vec![],
            streaming: false,
        };
        let b = ChatMessage {
            role: MsgRole::User,
            text: "img".into(),
            attachments: vec![UserAttachment::Image {
                mime: "image/png".into(),
                data: vec![1, 2, 3],
            }],
            blocks: vec![],
            streaming: false,
        };
        assert!(chat_messages_equal(&a, &b));
    }

    #[test]
    fn chat_messages_equal_different_attachments() {
        let a = ChatMessage {
            role: MsgRole::User,
            text: "img".into(),
            attachments: vec![UserAttachment::Image {
                mime: "image/png".into(),
                data: vec![1, 2, 3],
            }],
            blocks: vec![],
            streaming: false,
        };
        let b = ChatMessage {
            role: MsgRole::User,
            text: "img".into(),
            attachments: vec![UserAttachment::Image {
                mime: "image/jpeg".into(),
                data: vec![4, 5, 6],
            }],
            blocks: vec![],
            streaming: false,
        };
        assert!(!chat_messages_equal(&a, &b));
    }
}
