//! Transcript grouping and cache fingerprint helpers.

use crate::model::MsgRole;

/// Cheap content fingerprint for one transcript unit (a user message or a contiguous assistant
/// run). Built from lengths and flags rather than full text so revalidating every unit each
/// frame stays O(blocks), not O(bytes). Any append/edit/tool update changes a length and
/// invalidates the cached height; the unit is then re-rendered and re-measured.
pub(super) fn transcript_unit_fingerprint(messages: &[crate::model::ChatMessage]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    messages.len().hash(&mut hasher);
    for message in messages {
        (message.role == MsgRole::User).hash(&mut hasher);
        message.is_summary.hash(&mut hasher);
        message.streaming.hash(&mut hasher);
        message.text.len().hash(&mut hasher);
        message.attachments.len().hash(&mut hasher);
        message.worked_duration.is_some().hash(&mut hasher);
        message.blocks.len().hash(&mut hasher);
        for block in &message.blocks {
            match block {
                crate::model::AssistantBlock::Thinking(text) => {
                    (0u8, text.len()).hash(&mut hasher);
                }
                crate::model::AssistantBlock::Answer(text) => {
                    (1u8, text.len()).hash(&mut hasher);
                }
                crate::model::AssistantBlock::Tool {
                    args_summary,
                    output,
                    diff,
                    is_error,
                    output_truncated,
                    ..
                } => {
                    (
                        2u8,
                        args_summary.as_deref().map_or(0, str::len),
                        output.len(),
                        diff.as_deref().map_or(0, str::len),
                        *is_error,
                        *output_truncated,
                    )
                        .hash(&mut hasher);
                }
            }
        }
    }
    hasher.finish()
}

/// Split the transcript into render units: one per user message, one per contiguous
/// assistant run (rendered together by [`render_assistant_message_run`]).
pub(super) fn transcript_units(messages: &[crate::model::ChatMessage]) -> Vec<(usize, usize)> {
    let mut units = Vec::new();
    let mut index = 0;
    while index < messages.len() {
        if messages[index].role == MsgRole::Assistant {
            let start = index;
            while index < messages.len() && messages[index].role == MsgRole::Assistant {
                index += 1;
            }
            units.push((start, index));
        } else {
            units.push((index, index + 1));
            index += 1;
        }
    }
    units
}
