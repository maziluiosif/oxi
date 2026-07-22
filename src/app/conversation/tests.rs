use super::*;
use crate::model::{AssistantBlock, ChatMessage};

fn answer(size: usize) -> ChatMessage {
    ChatMessage {
        role: MsgRole::Assistant,
        text: String::new(),
        is_summary: false,
        attachments: Vec::new(),
        blocks: vec![AssistantBlock::Answer("x".repeat(size))],
        streaming: false,
        started_at: None,
        worked_duration: None,
    }
}

fn user(text: &str) -> ChatMessage {
    ChatMessage {
        role: MsgRole::User,
        text: text.to_owned(),
        is_summary: false,
        attachments: Vec::new(),
        blocks: Vec::new(),
        streaming: false,
        started_at: None,
        worked_duration: None,
    }
}

#[test]
fn units_group_contiguous_assistant_messages() {
    let messages = vec![user("a"), answer(10), answer(10), user("b"), answer(10)];
    assert_eq!(
        transcript_units(&messages),
        vec![(0, 1), (1, 3), (3, 4), (4, 5)]
    );
}

#[test]
fn fingerprint_changes_when_content_grows() {
    let before = vec![answer(100)];
    let after = vec![answer(101)];
    assert_ne!(
        transcript_unit_fingerprint(&before),
        transcript_unit_fingerprint(&after)
    );
}

#[test]
fn fingerprint_changes_when_streaming_ends() {
    let mut streaming = answer(100);
    streaming.streaming = true;
    let done = answer(100);
    assert_ne!(
        transcript_unit_fingerprint(std::slice::from_ref(&streaming)),
        transcript_unit_fingerprint(std::slice::from_ref(&done))
    );
}
