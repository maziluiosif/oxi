//! Presentation state for externally executed tools. Native tools keep their existing events.

use serde::{Deserialize, Serialize};

use super::AssistantBlock;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    #[default]
    Pending,
    InProgress,
    Completed,
    Failed,
    Interrupted,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolMetadata {
    pub title: String,
    pub kind: String,
    pub name: Option<String>,
    pub locations: Vec<ToolLocation>,
    pub status: ToolStatus,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolLocation {
    pub path: String,
    #[serde(default)]
    pub line: Option<u64>,
}

impl ToolMetadata {
    pub fn interrupt_if_unfinished(&mut self) {
        if matches!(self.status, ToolStatus::Pending | ToolStatus::InProgress) {
            self.status = ToolStatus::Interrupted;
        }
    }
}

#[derive(Clone, Debug)]
pub struct ToolUpdate {
    pub tool_call_id: String,
    pub name: String,
    pub args: Option<serde_json::Value>,
    pub output: String,
    pub diff: Option<String>,
    pub output_truncated: bool,
    pub metadata: ToolMetadata,
}

/// Replace only the matching tool, retaining its chronological position in the transcript.
/// An update can precede the initial call; a later call then fills the same block.
pub fn apply_tool_update(blocks: &mut Vec<AssistantBlock>, update: ToolUpdate) {
    if update.tool_call_id.is_empty() {
        return;
    }
    let is_error = match update.metadata.status {
        ToolStatus::Completed => Some(false),
        ToolStatus::Failed => Some(true),
        _ => None,
    };
    let position = blocks.iter().position(|block| {
        matches!(block, AssistantBlock::Tool { tool_call_id, .. } if *tool_call_id == update.tool_call_id)
    });
    let block = AssistantBlock::Tool {
        tool_call_id: update.tool_call_id,
        name: update.name,
        args_summary: update.args.map(|args| args.to_string()),
        output: update.output,
        diff: update.diff,
        is_error,
        full_output_path: None,
        output_truncated: update.output_truncated,
        metadata: Some(Box::new(update.metadata)),
    };
    if let Some(position) = position {
        blocks[position] = block;
    } else {
        blocks.push(block);
    }
}
