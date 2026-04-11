//! System prompt construction (aligned with pi `buildSystemPrompt` subset).

use chrono::Utc;

use crate::settings::{AppSettings, ALL_TOOL_NAMES};

pub const DEFAULT_AGENT_SYSTEM_PROMPT: &str = "You are an expert coding assistant. You help users by reading files, running shell commands, searching the codebase, and editing or writing files.\n\nAvailable tools (use only these when enabled): {tools_list}\n\nGuidelines:\n- Prefer reading files before editing.\n- Keep shell commands safe and relevant to the project.\n- When editing, ensure old text matches exactly.\n- Do not guess about project-specific implementation details when tools are available.\n- Verify claims by reading the relevant source files before answering.\n- Prefer evidence from the codebase over assumptions.";

pub fn build_system_prompt(settings: &AppSettings, cwd: &str) -> String {
    let date = Utc::now().format("%Y-%m-%d");
    let cwd_norm = cwd.replace('\\', "/");
    let tools: Vec<&str> = ALL_TOOL_NAMES
        .iter()
        .zip(settings.tools_enabled.iter())
        .filter(|(_, en)| **en)
        .map(|(n, _)| *n)
        .collect();
    let tools_list = tools.join(", ");

    let custom = settings.system_prompt.trim();
    if !custom.is_empty() {
        return format!("{custom}\n\nCurrent date: {date}\nCurrent working directory: {cwd_norm}");
    }

    let template = settings.agent_system_prompt.trim();
    let template = if template.is_empty() {
        DEFAULT_AGENT_SYSTEM_PROMPT
    } else {
        template
    };

    let body = template.replace("{tools_list}", &tools_list);
    format!("{body}\n\nCurrent date: {date}\nCurrent working directory: {cwd_norm}")
}
