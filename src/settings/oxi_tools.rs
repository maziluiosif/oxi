//! Which of oxi's own tools (the ones it hands agents through its MCP server, and the same
//! built-in tools of oxi's own agent) ask the user before they run: Settings > Agent >
//! "Ask before oxi tools". Stored as `oxi_tool_confirmation` in `settings.json`, one entry
//! per tool name; a missing entry means the tool's default.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer};
use serde_json::Value;

/// Settings page groups, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OxiToolGroup {
    Orchestrator,
    Delegation,
    NotesAndChat,
    Other,
}

impl OxiToolGroup {
    pub const ALL: [Self; 4] = [
        Self::Orchestrator,
        Self::Delegation,
        Self::NotesAndChat,
        Self::Other,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Orchestrator => "Orchestrator",
            Self::Delegation => "Delegation",
            Self::NotesAndChat => "Notes & chat",
            Self::Other => "Other",
        }
    }
}

pub struct OxiToolInfo {
    pub name: &'static str,
    pub group: OxiToolGroup,
    pub description: &'static str,
    /// Asks by default. [`FOLLOWS_FILE_EDIT_APPROVAL`] tools default to the file-edit switch.
    pub asks_by_default: bool,
    /// Changes nothing, so plan mode lets it run.
    pub read_only: bool,
}

const fn tool(
    name: &'static str,
    group: OxiToolGroup,
    asks_by_default: bool,
    read_only: bool,
    description: &'static str,
) -> OxiToolInfo {
    OxiToolInfo {
        name,
        group,
        description,
        asks_by_default,
        read_only,
    }
}

use OxiToolGroup::{Delegation, NotesAndChat, Orchestrator, Other};

/// Every oxi tool the setting covers. Defaults keep what oxi did before the setting existed.
pub const OXI_TOOLS: &[OxiToolInfo] = &[
    tool(
        "team",
        Orchestrator,
        false,
        true,
        "Lists the agents the lead may start.",
    ),
    tool(
        "spawn_workers",
        Orchestrator,
        true,
        false,
        "Starts worker agents (in their own work trees when they edit).",
    ),
    tool(
        "worker_status",
        Orchestrator,
        false,
        true,
        "Reads what the workers are doing.",
    ),
    tool(
        "worker_diff",
        Orchestrator,
        false,
        true,
        "Reads a worker's changes.",
    ),
    tool(
        "message_worker",
        Orchestrator,
        true,
        false,
        "Sends a worker a follow-up.",
    ),
    tool("stop_worker", Orchestrator, true, false, "Stops a worker."),
    tool(
        "merge_worker",
        Orchestrator,
        true,
        false,
        "Brings a worker's changes into the workspace, uncommitted.",
    ),
    tool(
        "delegate_task",
        Delegation,
        true,
        false,
        "Hands a task to another agent, in a chat of its own.",
    ),
    tool(
        "task_status",
        Delegation,
        false,
        true,
        "Reads a delegated task's progress and report.",
    ),
    tool(
        "task_cancel",
        Delegation,
        false,
        false,
        "Stops a delegated task.",
    ),
    tool(
        "todo_write",
        NotesAndChat,
        false,
        true,
        "Updates the checklist above the composer.",
    ),
    tool(
        "scratchpad_read",
        NotesAndChat,
        false,
        true,
        "Reads your global scratchpad.",
    ),
    tool(
        "scratchpad",
        NotesAndChat,
        true,
        false,
        "Writes to your global scratchpad.",
    ),
    tool(
        "chat_read",
        NotesAndChat,
        false,
        true,
        "Reads earlier turns of the same chat.",
    ),
    tool(
        "chat_search",
        NotesAndChat,
        false,
        true,
        "Searches your other saved chats.",
    ),
    tool(
        "learn_rules",
        NotesAndChat,
        true,
        false,
        "Saves rules found by /learn.",
    ),
    tool(
        "evidence_capture",
        Other,
        false,
        true,
        "Takes screenshots or recordings as proof. A command it runs asks like bash.",
    ),
    tool(
        "evidence_run",
        Other,
        false,
        true,
        "Runs a check as proof; the command asks like bash.",
    ),
    tool(
        "evidence_add",
        Other,
        false,
        true,
        "Attaches a file as proof.",
    ),
    tool(
        "evidence_base_checkout",
        Other,
        false,
        true,
        "Checks out the pre-change code in a scratch copy for \u{201c}before\u{201d} captures.",
    ),
    tool(
        "evidence_submit",
        Other,
        false,
        true,
        "Shows the proof next to the reply.",
    ),
];

/// These used to ask only when "Ask before file changes" was on; until the user picks, they
/// still follow it.
pub const FOLLOWS_FILE_EDIT_APPROVAL: [&str; 2] = ["scratchpad", "learn_rules"];

const _: () = assert!(OXI_TOOLS.len() <= 64, "OxiToolConfirm is a u64 bit set");

pub fn oxi_tool_info(name: &str) -> Option<&'static OxiToolInfo> {
    OXI_TOOLS.iter().find(|t| t.name == name)
}

/// The tool's default when the user never picked: see [`OxiToolInfo::asks_by_default`].
pub fn oxi_tool_default(info: &OxiToolInfo, file_edit_approval: bool) -> bool {
    if FOLLOWS_FILE_EDIT_APPROVAL.contains(&info.name) {
        file_edit_approval
    } else {
        info.asks_by_default
    }
}

/// Whether oxi's own tool `name` changes nothing (so plan mode may run it).
pub fn oxi_tool_is_read_only(name: &str) -> bool {
    oxi_tool_info(name).is_some_and(|t| t.read_only)
}

/// The resolved per-tool switches, cheap to copy into every agent run's approval policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OxiToolConfirm {
    /// Bit `i` set: [`OXI_TOOLS`]`[i]` asks first.
    asks: u64,
}

impl OxiToolConfirm {
    /// Nothing asks (for runs with approvals off).
    pub const fn none() -> Self {
        Self { asks: 0 }
    }

    /// The defaults, with the file-edit switch at `file_edit_approval`.
    pub fn defaults(file_edit_approval: bool) -> Self {
        Self::resolve(&BTreeMap::new(), file_edit_approval)
    }

    /// The user's choices in `chosen`, defaults for the rest.
    pub fn resolve(chosen: &BTreeMap<String, bool>, file_edit_approval: bool) -> Self {
        let asks = OXI_TOOLS
            .iter()
            .enumerate()
            .filter(|(_, info)| {
                chosen
                    .get(info.name)
                    .copied()
                    .unwrap_or_else(|| oxi_tool_default(info, file_edit_approval))
            })
            .fold(0u64, |bits, (i, _)| bits | 1 << i);
        Self { asks }
    }

    /// `Some(asks first)` for oxi's own tools, `None` for any other tool.
    pub fn asks(self, name: &str) -> Option<bool> {
        OXI_TOOLS
            .iter()
            .position(|t| t.name == name)
            .map(|i| self.asks & (1 << i) != 0)
    }
}

/// Lenient reader for `oxi_tool_confirmation`: keeps every `"tool": bool` entry and drops
/// anything else, so one odd value never resets the user's other choices.
pub(crate) fn deserialize_choices<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, bool>, D::Error> {
    Ok(match Value::deserialize(deserializer)? {
        Value::Object(map) => map
            .into_iter()
            .filter_map(|(name, value)| value.as_bool().map(|b| (name, b)))
            .collect(),
        _ => BTreeMap::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_names_are_unique_and_known_to_oxi() {
        let mut names: Vec<&str> = OXI_TOOLS.iter().map(|t| t.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), OXI_TOOLS.len());
        for name in crate::orchestrator::NAMES
            .into_iter()
            .chain(crate::delegation::NAMES)
            .chain(crate::evidence::tools::NAMES)
        {
            assert!(oxi_tool_info(name).is_some(), "{name} missing");
        }
        for group in OxiToolGroup::ALL {
            assert!(OXI_TOOLS.iter().any(|t| t.group == group));
        }
    }

    #[test]
    fn defaults_keep_the_old_behavior() {
        let on = OxiToolConfirm::defaults(true);
        for name in [
            "spawn_workers",
            "message_worker",
            "stop_worker",
            "merge_worker",
            "delegate_task",
            "scratchpad",
            "learn_rules",
        ] {
            assert_eq!(on.asks(name), Some(true), "{name}");
        }
        for name in [
            "team",
            "worker_status",
            "worker_diff",
            "task_status",
            "task_cancel",
            "todo_write",
            "scratchpad_read",
            "chat_read",
            "chat_search",
            "evidence_submit",
        ] {
            assert_eq!(on.asks(name), Some(false), "{name}");
        }
        let edits_off = OxiToolConfirm::defaults(false);
        assert_eq!(edits_off.asks("scratchpad"), Some(false));
        assert_eq!(edits_off.asks("learn_rules"), Some(false));
        assert_eq!(edits_off.asks("delegate_task"), Some(true));
        assert_eq!(on.asks("bash"), None);
        assert_eq!(OxiToolConfirm::none().asks("merge_worker"), Some(false));
    }

    #[test]
    fn choices_override_defaults() {
        let chosen = BTreeMap::from([
            ("merge_worker".to_string(), false),
            ("todo_write".to_string(), true),
            ("not_a_tool".to_string(), true),
        ]);
        let c = OxiToolConfirm::resolve(&chosen, true);
        assert_eq!(c.asks("merge_worker"), Some(false));
        assert_eq!(c.asks("todo_write"), Some(true));
        assert_eq!(c.asks("spawn_workers"), Some(true));
    }
}
