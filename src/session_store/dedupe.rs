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
