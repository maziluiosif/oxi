//! Convert UI [`ChatMessage`](crate::model::ChatMessage) list to OpenAI-style `messages`.
//!
//! ## Context window management
//! Long conversations can exceed the model's context window and cause HTTP 400 errors.
//! [`build_openai_messages`] applies a two-pass budget trim:
//!
//! 1. The system prompt and the **last `MIN_KEEP_TURNS` user+assistant pairs** are always kept.
//! 2. Older turns are counted character-by-character and dropped from the oldest end until the
//!    total estimated character count is below the caller-supplied context budget.
//!
//! A conservative `~4 chars per token` ratio is used; the budget is computed per run from the
//! active profile's effective context window (`~4 chars/token × context tokens`).

use base64::Engine;
use std::ops::Range;

use serde_json::{Value, json};

use crate::agent::tools::floor_char_boundary;
use crate::model::{AssistantBlock, ChatMessage, MsgRole, UserAttachment};

/// Approximate character budget for the default 100k-token fallback (~100k tokens × 4 chars/token).
const CONTEXT_CHAR_BUDGET: usize = 400_000;

/// Always keep this many most-recent user+assistant turn pairs regardless of budget.
const MIN_KEEP_TURNS: usize = 6;

/// Prefix that marks a compaction summary once it is rendered onto the wire. Written by
/// [`message_to_openai`] and detected by the trimmer so the summary — the LLM-produced,
/// compressed replacement for everything older — is pinned and never dropped by [`trim_to_budget`].
pub(crate) const SUMMARY_WIRE_PREFIX: &str = "[Summary of the earlier conversation]";

/// Conservative default characters-per-token estimate, used before a session has measured a
/// real ratio from a provider `Usage` event. Also the clamp midpoint.
pub const DEFAULT_CHARS_PER_TOKEN: f32 = 4.0;

/// Clamp bounds for the per-session calibrated chars-per-token ratio.
pub const MIN_CHARS_PER_TOKEN: f32 = 2.0;
pub const MAX_CHARS_PER_TOKEN: f32 = 6.0;

/// Derive a calibrated chars-per-token ratio from an estimated prompt size (chars) and the
/// provider-reported prompt token count, clamped to `[MIN, MAX]`.
pub fn calibrate_chars_per_token(estimated_chars: usize, prompt_tokens: u64) -> f32 {
    if prompt_tokens == 0 {
        return DEFAULT_CHARS_PER_TOKEN;
    }
    (estimated_chars as f32 / prompt_tokens as f32).clamp(MIN_CHARS_PER_TOKEN, MAX_CHARS_PER_TOKEN)
}

/// Graduated context-management ladder, expressed as fractions of the model context window.
/// The three bounds are read together and against the same measure — the whole prompt
/// (system + tool definitions + history), the same thing the compaction trigger and the
/// context meter use — so they form one coherent scale. Keep them monotonic
/// (`TRIM_TARGET_FRAC < AUTO_COMPACT_THRESHOLD < TRIM_CEILING_FRAC`; enforced by a test):
///
/// - Below [`AUTO_COMPACT_THRESHOLD`]: nothing happens.
/// - At/above it: compaction summarizes older turns — the primary, semantic strategy.
/// - At/above [`TRIM_CEILING_FRAC`]: the trimmer fires as a last-resort safety valve,
///   dropping whole turns until the prompt is back under [`TRIM_TARGET_FRAC`] (hysteresis,
///   and below the compaction line so a forced trim also clears the compaction condition).
pub const AUTO_COMPACT_THRESHOLD: f32 = 0.85;
pub const TRIM_CEILING_FRAC: f32 = 0.92;
pub const TRIM_TARGET_FRAC: f32 = 0.70;

// Enforce the ladder ordering at compile time: reordering these into an inconsistent scale
// (e.g. a trim target above the compaction line) fails the build instead of silently thrashing.
const _: () = assert!(
    TRIM_TARGET_FRAC < AUTO_COMPACT_THRESHOLD && AUTO_COMPACT_THRESHOLD < TRIM_CEILING_FRAC,
    "context ladder fractions must stay monotonic: target < compact < ceiling",
);

/// Compute the character trim *ceiling* from a context window measured in tokens, using the
/// (possibly calibrated) `chars_per_token` ratio: the prompt size above which the trimmer
/// starts dropping turns. [`TRIM_CEILING_FRAC`] leaves headroom for the response the model is
/// about to generate. Never drops below ~8k chars so the protected tail always has room.
pub fn context_char_budget_from_tokens(context_tokens: usize, chars_per_token: f32) -> usize {
    if context_tokens == 0 {
        return CONTEXT_CHAR_BUDGET;
    }
    let cpt = if chars_per_token.is_finite() && chars_per_token > 0.0 {
        chars_per_token
    } else {
        DEFAULT_CHARS_PER_TOKEN
    };
    let chars = context_tokens as f32 * TRIM_CEILING_FRAC * cpt;
    (chars as usize).max(8_192)
}

pub fn build_openai_messages(
    system: &str,
    chat: &[ChatMessage],
    tools_chars: usize,
    context_char_budget: usize,
) -> Vec<Value> {
    let system_msg = json!({ "role": "system", "content": system });

    // Build all candidate turn JSON values first.
    let mut turns: Vec<Value> = chat.iter().filter_map(message_to_openai).collect();

    // Apply context budget trimming. The fixed overhead (system prompt + tool definitions) is
    // counted against the budget too, so the ceiling matches the whole prompt sent on the wire.
    trim_to_budget(&mut turns, system.len() + tools_chars, context_char_budget);

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
            if m.is_summary {
                // Present the summary as context rather than a literal user request.
                return Some(json!({
                    "role": "user",
                    "content": format!("{SUMMARY_WIRE_PREFIX}\n\n{}", m.text)
                }));
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

pub(crate) fn user_content_to_openai(text: &str, attachments: &[UserAttachment]) -> Value {
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

pub(crate) fn flatten_assistant(m: &ChatMessage) -> String {
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
                // Keep both the head (command/args context) and the tail (final result, e.g. a
                // test summary), dropping only the middle, so neither end is lost.
                const HISTORY_TOOL_OUTPUT_CAP: usize = 8_000;
                const HISTORY_TOOL_OUTPUT_TAIL: usize = 2_000;
                if output.len() > HISTORY_TOOL_OUTPUT_CAP {
                    let head_end =
                        floor_char_boundary(output, HISTORY_TOOL_OUTPUT_CAP - HISTORY_TOOL_OUTPUT_TAIL);
                    let tail_start =
                        floor_char_boundary(output, output.len() - HISTORY_TOOL_OUTPUT_TAIL);
                    s.push_str(&output[..head_end]);
                    s.push_str("\n… [middle truncated in history] …\n");
                    s.push_str(&output[tail_start..]);
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

/// Hysteresis target: once the trimmer fires it drops turns until the prompt is back under
/// this many chars — [`TRIM_TARGET_FRAC`] of the window, expressed as a fraction of the
/// [`TRIM_CEILING_FRAC`] ceiling budget. Dropping well below the ceiling (rather than just to
/// it) avoids re-trimming on every subsequent turn.
fn trim_target(ceiling_budget: usize) -> usize {
    ((ceiling_budget as f32) * (TRIM_TARGET_FRAC / TRIM_CEILING_FRAC)) as usize
}

/// Return ranges of complete conversational turns. A turn starts at a normal user
/// message and includes following assistant/tool messages up to the next normal user.
/// This keeps `tool` messages attached to their `assistant.tool_calls` owner.
pub(crate) fn turn_boundaries(messages: &[Value]) -> Vec<Range<usize>> {
    let mut starts = Vec::new();
    for (i, m) in messages.iter().enumerate() {
        if m.get("role").and_then(|x| x.as_str()) == Some("user") {
            starts.push(i);
        }
    }
    starts
        .iter()
        .enumerate()
        .map(|(idx, start)| {
            let end = starts.get(idx + 1).copied().unwrap_or(messages.len());
            *start..end
        })
        .collect()
}

/// True when the turn at `range` is a compaction summary (a leading user message whose
/// content starts with [`SUMMARY_WIRE_PREFIX`]). The summary content is always a plain string.
fn is_summary_turn(messages: &[Value], range: &Range<usize>) -> bool {
    messages
        .get(range.start)
        .filter(|m| m.get("role").and_then(Value::as_str) == Some("user"))
        .and_then(|m| m.get("content").and_then(Value::as_str))
        .is_some_and(|c| c.starts_with(SUMMARY_WIRE_PREFIX))
}

fn trim_message_groups_to_budget(messages: &mut Vec<Value>, fixed_overhead: usize, budget: usize) {
    let lens: Vec<usize> = messages.iter().map(value_char_len).collect();
    let total: usize = fixed_overhead + lens.iter().sum::<usize>();
    if total <= budget {
        return;
    }

    let target = trim_target(budget);
    let groups = turn_boundaries(messages);
    let min_keep_groups = MIN_KEEP_TURNS.min(groups.len());
    // Pin a leading compaction summary: it is the compressed replacement for everything older,
    // so it must survive even though it is the oldest turn. Dropping then starts at turn 1 and
    // removes a middle slice, leaving both the summary and the most-recent turns intact.
    let first_droppable = usize::from(groups.first().is_some_and(|g| is_summary_turn(messages, g)));
    let droppable_end = groups
        .len()
        .saturating_sub(min_keep_groups)
        .max(first_droppable);

    let mut running = total;
    let mut drop_start: Option<usize> = None;
    let mut drop_end = 0usize;
    for range in &groups[first_droppable..droppable_end] {
        if running <= target {
            break;
        }
        let group_len: usize = lens[range.clone()].iter().sum();
        running = running.saturating_sub(group_len);
        drop_start.get_or_insert(range.start);
        drop_end = range.end;
    }

    if let Some(start) = drop_start {
        messages.drain(start..drop_end);
    }
}

/// Drop oldest complete turns until total character count fits the hysteresis
/// target, while always keeping at least `MIN_KEEP_TURNS` recent turns.
fn trim_to_budget(turns: &mut Vec<Value>, fixed_overhead: usize, budget: usize) {
    trim_message_groups_to_budget(turns, fixed_overhead, budget);
}

/// Trim a full wire-format OpenAI message list in-place. The leading system message is
/// preserved; remaining messages are dropped only on whole-turn boundaries so tool messages
/// are never orphaned. `tools_chars` is the size of the tool definitions sent alongside — part
/// of the fixed overhead counted against the budget.
pub(crate) fn trim_wire_history_to_budget(
    messages: &mut Vec<Value>,
    tools_chars: usize,
    budget: usize,
) {
    if messages.is_empty() {
        return;
    }
    let system_len = messages.first().map(value_char_len).unwrap_or_default();
    let mut rest: Vec<Value> = messages.drain(1..).collect();
    trim_message_groups_to_budget(&mut rest, system_len + tools_chars, budget);
    messages.extend(rest);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AssistantBlock, ChatMessage, MsgRole};

    fn user_msg(text: &str) -> ChatMessage {
        ChatMessage {
            role: MsgRole::User,
            text: text.to_string(),
            is_summary: false,
            attachments: vec![],
            blocks: vec![],
            streaming: false,
            started_at: None,
            worked_duration: None,
        }
    }

    fn assistant_msg(answer: &str) -> ChatMessage {
        ChatMessage {
            role: MsgRole::Assistant,
            text: String::new(),
            is_summary: false,
            attachments: vec![],
            blocks: vec![AssistantBlock::Answer(answer.to_string())],
            streaming: false,
            started_at: None,
            worked_duration: None,
        }
    }

    #[test]
    fn long_tool_output_keeps_head_and_tail_in_history() {
        let output = format!("{}{}{}", "H".repeat(6_000), "M".repeat(20_000), "T".repeat(3_000));
        let msg = ChatMessage {
            role: MsgRole::Assistant,
            text: String::new(),
            is_summary: false,
            attachments: vec![],
            blocks: vec![AssistantBlock::Tool {
                tool_call_id: String::new(),
                name: "bash".to_string(),
                args_summary: None,
                output,
                diff: None,
                is_error: None,
                full_output_path: None,
                output_truncated: false,
            }],
            streaming: false,
            started_at: None,
            worked_duration: None,
        };
        let flat = flatten_assistant(&msg);
        // Both the leading command context and the final result survive; the middle is dropped.
        assert!(flat.contains("HHHH"));
        assert!(flat.contains("TTTT"));
        assert!(flat.contains("middle truncated in history"));
        assert!(!flat.contains(&"M".repeat(3_000)));
    }

    #[test]
    fn summary_message_gets_preamble_on_the_wire() {
        let mut summary = user_msg("Goal: build X. Pending: finish Y.");
        summary.is_summary = true;
        let msgs = build_openai_messages("sys", &[summary, user_msg("continue")], 0, 1_000_000);
        let first_user = msgs
            .iter()
            .find(|m| m["role"] == "user")
            .expect("a user message");
        let content = first_user["content"].as_str().unwrap();
        assert!(content.starts_with("[Summary of the earlier conversation]"));
        assert!(content.contains("Goal: build X"));
    }

    #[test]
    fn budget_is_ceiling_fraction_of_window_at_default_ratio() {
        // 100k tokens × TRIM_CEILING_FRAC (0.92) × 4 chars = 368_000.
        assert_eq!(
            context_char_budget_from_tokens(100_000, DEFAULT_CHARS_PER_TOKEN),
            368_000
        );
    }

    #[test]
    fn budget_scales_with_calibrated_ratio() {
        let low = context_char_budget_from_tokens(100_000, 2.5);
        let high = context_char_budget_from_tokens(100_000, DEFAULT_CHARS_PER_TOKEN);
        assert!(low < high);
        assert_eq!(low, 230_000); // 100k × 0.92 × 2.5
    }

    #[test]
    fn calibrate_clamps_ratio() {
        // Very dense text (few chars/token) clamps up to the floor.
        assert_eq!(calibrate_chars_per_token(1_000, 1_000), MIN_CHARS_PER_TOKEN);
        // Very sparse text clamps down to the ceiling.
        assert_eq!(
            calibrate_chars_per_token(1_000_000, 1_000),
            MAX_CHARS_PER_TOKEN
        );
        // Zero tokens falls back to the default.
        assert_eq!(calibrate_chars_per_token(1_000, 0), DEFAULT_CHARS_PER_TOKEN);
    }

    #[test]
    fn short_conversation_untouched() {
        let chat = vec![user_msg("hello"), assistant_msg("hi")];
        let msgs = build_openai_messages("system", &chat, 0, CONTEXT_CHAR_BUDGET);
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
        let msgs = build_openai_messages("system", &chat, 0, CONTEXT_CHAR_BUDGET);
        // Total serialized size must be within budget.
        let total: usize = msgs.iter().map(|v| v.to_string().len()).sum();
        assert!(
            total <= CONTEXT_CHAR_BUDGET + 2048, // allow minor rounding
            "total {total} exceeds budget {CONTEXT_CHAR_BUDGET}"
        );
        // MIN_KEEP_TURNS pairs must always be present (+ system message).
        assert!(msgs.len() > MIN_KEEP_TURNS * 2);
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
        let msgs = build_openai_messages("system", &chat, 0, CONTEXT_CHAR_BUDGET);
        // Even if we can't fit everything, the last MIN_KEEP_TURNS pairs stay.
        // With only 2 pairs and MIN_KEEP_TURNS=6, all turns are kept.
        assert!(msgs.len() >= 3); // at minimum: system + 1 user + 1 assistant
    }

    #[test]
    fn trim_hysteresis_targets_below_ceiling_when_possible() {
        let budget = 10_000;
        let chat: Vec<ChatMessage> = (0..10)
            .flat_map(|i| {
                [
                    user_msg(&format!("u{i} {}", "x".repeat(500))),
                    assistant_msg(&format!("a{i} {}", "y".repeat(500))),
                ]
            })
            .collect();
        let msgs = build_openai_messages("system", &chat, 0, budget);
        let total: usize = msgs.iter().map(|v| v.to_string().len()).sum();
        assert!(total <= trim_target(budget) + 2048, "total={total}");
        assert!(msgs.len() > MIN_KEEP_TURNS * 2);
    }

    #[test]
    fn compaction_summary_is_pinned_through_trimming() {
        let budget = 10_000;
        let big = "x".repeat(800);
        let mut summary = user_msg("Goal: build X. Pending: finish Y.");
        summary.is_summary = true;
        let mut chat = vec![summary];
        // Enough oversized turns after the summary to force trimming past MIN_KEEP_TURNS.
        for i in 0..12 {
            chat.push(user_msg(&format!("u{i} {big}")));
            chat.push(assistant_msg(&format!("a{i} {big}")));
        }
        let msgs = build_openai_messages("system", &chat, 0, budget);
        let joined = msgs.iter().map(|v| v.to_string()).collect::<String>();

        // Trimming actually fired: the oldest post-summary turns were dropped …
        assert!(!joined.contains("u0 "), "oldest turn should be trimmed");
        // … while the most recent turns were kept …
        assert!(joined.contains("u11 "), "recent turn should be kept");
        // … and the summary survived as the pinned first user turn.
        let first_user = msgs
            .iter()
            .find(|m| m["role"] == "user")
            .expect("a user message");
        assert!(
            first_user["content"]
                .as_str()
                .unwrap()
                .starts_with(SUMMARY_WIRE_PREFIX),
            "summary must be pinned as the first turn"
        );
    }

    #[test]
    fn wire_trim_does_not_orphan_tool_messages() {
        let mut wire = vec![json!({"role":"system","content":"system"})];
        for i in 0..8 {
            wire.push(json!({"role":"user","content":format!("u{i} {}", "x".repeat(1200))}));
            wire.push(json!({"role":"assistant","content":"","tool_calls":[{"id":format!("call{i}"),"type":"function","function":{"name":"read","arguments":"{}"}}]}));
            wire.push(json!({"role":"tool","tool_call_id":format!("call{i}"),"content":format!("tool{i}")}));
        }
        trim_wire_history_to_budget(&mut wire, 0, 10_000);
        assert_eq!(wire[0]["role"], "system");
        for i in 1..wire.len() {
            if wire[i]["role"] == "tool" {
                assert_eq!(wire[i - 1]["role"], "assistant");
            }
        }
    }
}
