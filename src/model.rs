//! Domain types and pure helpers (no egui).

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MsgRole {
    User,
    Assistant,
}

/// User message attachment (image bytes; base64 only when converting for provider APIs).
#[derive(Clone)]
pub enum UserAttachment {
    Image { mime: String, data: Vec<u8> },
}

/// Segments streamed from the provider loop (`thinking_*` / `text_*` / tool events).
#[derive(Clone)]
pub enum AssistantBlock {
    /// Extended reasoning (`thinking_delta`); shown in a collapsible, distinct from the answer.
    Thinking(String),
    /// Main reply (`text_delta`); rendered as Markdown.
    Answer(String),
    /// Tool run keyed by provider/tool-call id.
    Tool {
        tool_call_id: String,
        name: String,
        /// JSON args from `tool_execution_start` (truncated for display).
        args_summary: Option<String>,
        /// Cumulative output from pi (partial updates replace this).
        output: String,
        /// Unified diff for edit-like tools when provided in `result.details.diff`.
        diff: Option<String>,
        is_error: Option<bool>,
        full_output_path: Option<String>,
        output_truncated: bool,
    },
}

#[derive(Clone)]
pub struct ChatMessage {
    pub role: MsgRole,
    /// User message only.
    pub text: String,
    /// User only: images and similar (empty for assistant).
    pub attachments: Vec<UserAttachment>,
    /// Assistant only: ordered thinking / answer / tool segments.
    pub blocks: Vec<AssistantBlock>,
    /// Assistant message still receiving stream.
    pub streaming: bool,
}

pub struct Session {
    pub title: String,
    pub messages: Vec<ChatMessage>,
    /// Local session file when persistence is enabled.
    pub session_file: Option<String>,
    /// `true` once this tab's transcript was loaded from disk or created locally in-memory.
    pub messages_loaded: bool,
}

pub fn make_session_title(text: &str) -> String {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut title = String::new();
    for ch in normalized.chars().take(36) {
        title.push(ch);
    }
    if normalized.chars().count() > 36 {
        title.push('…');
    }
    if title.is_empty() {
        "New chat".to_string()
    } else {
        title
    }
}

/// Assistant message has nothing to render yet (no text, no tool I/O).
/// While `streaming` is true, any tool call in flight counts as content so Worked / Explored can appear.
pub fn assistant_is_effectively_empty(blocks: &[AssistantBlock], streaming: bool) -> bool {
    if streaming
        && blocks
            .iter()
            .any(|b| matches!(b, AssistantBlock::Tool { .. }))
    {
        return false;
    }
    blocks.iter().all(|b| match b {
        AssistantBlock::Thinking(s) | AssistantBlock::Answer(s) => s.trim().is_empty(),
        AssistantBlock::Tool { output, diff, .. } => {
            output.trim().is_empty()
                && diff
                    .as_deref()
                    .is_none_or(|diff_text| diff_text.trim().is_empty())
        }
    })
}

/// Tools that produce edits/diffs or are the `edit` tool end an "explore" run (Cursor-style).
pub fn tool_breaks_explore_cluster(block: &AssistantBlock) -> bool {
    matches!(
        block,
        AssistantBlock::Tool { name, diff, .. }
            if name.eq_ignore_ascii_case("edit")
                || diff
                    .as_deref()
                    .is_some_and(|diff_text| !diff_text.trim().is_empty())
    )
}

pub fn is_explore_groupable_tool(block: &AssistantBlock) -> bool {
    matches!(block, AssistantBlock::Tool { .. } if !tool_breaks_explore_cluster(block))
}

#[derive(Debug, Clone)]
pub enum AssistantBlockGroup {
    Thinking(Vec<usize>),
    Answer(usize),
    ExploringTools {
        range_start: usize,
        range_end: usize,
        tool_indices: Vec<usize>,
    },
    Tool(usize),
}

/// Group assistant blocks for rendering: merges 3+ consecutive explore tools (possibly separated by
/// thinking segments or empty answers) into [`AssistantBlockGroup::ExploringTools`].
pub fn build_assistant_block_groups(blocks: &[AssistantBlock]) -> Vec<AssistantBlockGroup> {
    let mut i = 0;
    let mut out = Vec::new();
    while i < blocks.len() {
        if matches!(
            &blocks[i],
            AssistantBlock::Thinking(_) | AssistantBlock::Tool { .. }
        ) {
            let cluster_start = i;
            let mut j = i;
            let mut tool_indices = Vec::new();
            while j < blocks.len() {
                match &blocks[j] {
                    AssistantBlock::Thinking(_) => j += 1,
                    AssistantBlock::Tool { .. } if tool_breaks_explore_cluster(&blocks[j]) => break,
                    AssistantBlock::Tool { .. } if is_explore_groupable_tool(&blocks[j]) => {
                        tool_indices.push(j);
                        j += 1;
                    }
                    AssistantBlock::Answer(text) if text.trim().is_empty() => j += 1,
                    _ => break,
                }
            }
            if tool_indices.len() >= 3 {
                out.push(AssistantBlockGroup::ExploringTools {
                    range_start: cluster_start,
                    range_end: j,
                    tool_indices,
                });
                i = j;
                continue;
            }
        }

        match &blocks[i] {
            AssistantBlock::Thinking(_) => {
                let start = i;
                while i < blocks.len() && matches!(blocks[i], AssistantBlock::Thinking(_)) {
                    i += 1;
                }
                out.push(AssistantBlockGroup::Thinking((start..i).collect()));
            }
            AssistantBlock::Answer(_) => {
                out.push(AssistantBlockGroup::Answer(i));
                i += 1;
            }
            AssistantBlock::Tool { .. } => {
                out.push(AssistantBlockGroup::Tool(i));
                i += 1;
            }
        }
    }
    out
}

pub fn estimate_thought_seconds(total_chars: usize) -> u32 {
    if total_chars == 0 {
        return 1;
    }
    ((total_chars as f32 / 400.0).ceil() as u32).clamp(1, 999)
}

pub fn bash_command_tokens(blocks: &[AssistantBlock], indices: &[usize]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for &idx in indices.iter().take(4) {
        let AssistantBlock::Tool { args_summary, .. } = &blocks[idx] else {
            continue;
        };
        let Some(raw) = args_summary else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) else {
            continue;
        };
        let cmd = v
            .get("command")
            .and_then(|x| x.as_str())
            .or_else(|| v.as_str());
        let Some(cmd) = cmd else {
            continue;
        };
        let token = cmd
            .split_whitespace()
            .next()
            .unwrap_or(cmd)
            .chars()
            .take(16)
            .collect::<String>();
        if !token.is_empty() && !parts.contains(&token) {
            parts.push(token);
        }
    }
    parts.join(", ")
}

pub fn concat_thinking_blocks(blocks: &[AssistantBlock], indices: &[usize]) -> String {
    let mut s = String::new();
    for &i in indices {
        if let AssistantBlock::Thinking(t) = &blocks[i] {
            if !s.is_empty() && !t.is_empty() {
                s.push_str("\n\n");
            }
            s.push_str(t);
        }
    }
    s
}

/// Apply tool output to the last assistant message blocks (same routing as live RPC).
pub fn set_tool_output_on_blocks(
    blocks: &mut Vec<AssistantBlock>,
    tool_call_id: Option<&str>,
    text: &str,
    truncated: bool,
) {
    if let Some(id) = tool_call_id {
        if !id.is_empty() {
            for b in blocks.iter_mut() {
                if let AssistantBlock::Tool {
                    tool_call_id: tid,
                    output,
                    output_truncated,
                    ..
                } = b
                {
                    if tid == id {
                        *output = text.to_string();
                        *output_truncated = truncated;
                        return;
                    }
                }
            }
        }
    }
    if let Some(AssistantBlock::Tool {
        output,
        output_truncated,
        ..
    }) = blocks.last_mut()
    {
        *output = text.to_string();
        *output_truncated = truncated;
        return;
    }
    blocks.push(AssistantBlock::Tool {
        tool_call_id: tool_call_id.unwrap_or("").to_string(),
        name: "tool".to_string(),
        args_summary: None,
        output: text.to_string(),
        diff: None,
        is_error: None,
        full_output_path: None,
        output_truncated: truncated,
    });
}

pub fn tool_compact_header(name: &str, output: &str) -> String {
    let preview = output
        .lines()
        .find(|l| !l.trim().is_empty())
        .map(str::trim)
        .unwrap_or("");
    let mut p: String = preview.chars().take(56).collect();
    if preview.chars().count() > 56 {
        p.push('…');
    }
    if p.is_empty() {
        format!("{name} · …")
    } else {
        format!("{name} · {p}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(id: &str, name: &str) -> AssistantBlock {
        AssistantBlock::Tool {
            tool_call_id: id.to_string(),
            name: name.to_string(),
            args_summary: None,
            output: String::new(),
            diff: None,
            is_error: None,
            full_output_path: None,
            output_truncated: false,
        }
    }

    #[test]
    fn build_assistant_block_groups_empty() {
        assert!(build_assistant_block_groups(&[]).is_empty());
    }

    #[test]
    fn build_assistant_block_groups_two_tools_stay_individual() {
        let blocks = vec![tool("a", "read"), tool("b", "bash")];
        let g = build_assistant_block_groups(&blocks);
        assert_eq!(g.len(), 2);
        assert!(matches!(&g[0], AssistantBlockGroup::Tool(0)));
        assert!(matches!(&g[1], AssistantBlockGroup::Tool(1)));
    }

    #[test]
    fn build_assistant_block_groups_three_tools_become_exploring() {
        let blocks = vec![tool("a", "read"), tool("b", "bash"), tool("c", "grep")];
        let g = build_assistant_block_groups(&blocks);
        assert_eq!(g.len(), 1);
        match &g[0] {
            AssistantBlockGroup::ExploringTools {
                tool_indices,
                range_start,
                range_end,
            } => {
                assert_eq!(tool_indices.len(), 3);
                assert_eq!(*range_start, 0);
                assert_eq!(*range_end, 3);
            }
            _ => panic!("expected ExploringTools group"),
        }
    }

    #[test]
    fn build_assistant_block_groups_exploring_skips_thinking_between_tools() {
        let blocks = vec![
            tool("a", "read"),
            AssistantBlock::Thinking("reasoning".into()),
            tool("b", "bash"),
            tool("c", "grep"),
        ];
        let g = build_assistant_block_groups(&blocks);
        assert_eq!(g.len(), 1);
        match &g[0] {
            AssistantBlockGroup::ExploringTools {
                tool_indices,
                range_start,
                range_end,
            } => {
                assert_eq!(tool_indices, &vec![0, 2, 3]);
                assert_eq!(*range_start, 0);
                assert_eq!(*range_end, 4);
            }
            _ => panic!("expected ExploringTools group"),
        }
    }

    #[test]
    fn build_assistant_block_groups_diff_tool_breaks_exploring() {
        let mut edit = tool("c", "edit");
        if let AssistantBlock::Tool { diff, .. } = &mut edit {
            *diff = Some("+1 new".into());
        }
        let blocks = vec![tool("a", "read"), tool("b", "bash"), edit];
        let g = build_assistant_block_groups(&blocks);
        assert_eq!(g.len(), 3);
        assert!(matches!(&g[0], AssistantBlockGroup::Tool(0)));
        assert!(matches!(&g[1], AssistantBlockGroup::Tool(1)));
        assert!(matches!(&g[2], AssistantBlockGroup::Tool(2)));
    }

    #[test]
    fn set_tool_output_routes_by_id() {
        let mut blocks = vec![tool("call_1", "bash"), tool("call_2", "bash")];
        set_tool_output_on_blocks(&mut blocks, Some("call_2"), "out", false);
        match &blocks[1] {
            AssistantBlock::Tool { output, .. } => assert_eq!(output, "out"),
            _ => panic!(),
        }
    }
}
