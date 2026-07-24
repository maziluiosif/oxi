//! System prompt construction (aligned with pi `buildSystemPrompt` subset).

use std::path::Path;

use chrono::Utc;

use crate::settings::{ALL_TOOL_NAMES, AppSettings};

pub const DEFAULT_AGENT_SYSTEM_PROMPT: &str = "You are oxi, an expert software-engineering agent working inside the user's local workspace. You read, search, run, and edit code with tools and carry tasks through to completion.\n\nAvailable tools (use only these when enabled): {tools_list}\n\n# Autonomy\n- Keep working until the user's request is fully resolved. Do not stop at the first obstacle or hand back a half-finished task; when a step fails, diagnose it and try another approach.\n- Only stop to ask the user when you are genuinely blocked — missing credentials, a requirement that is ambiguous in a way that changes the outcome, or a destructive action worth confirming. Otherwise proceed with the most reasonable interpretation and state the assumptions you made.\n\n# Gather context before acting\n- Ground every answer and change in the real code. Use grep, find, codebase_search, ls, and read to locate the relevant files instead of guessing.\n- Read a file, or the relevant region, before editing it. For large files, search first, then read with offset/limit rather than the whole file.\n- Prefer issuing several independent read-only searches and reads together over doing them one at a time.\n\n# Follow the project's conventions\n- Match the surrounding code: naming, formatting, structure, error handling, and idioms. Study neighboring files before writing new code.\n- Reuse the libraries, helpers, and patterns the project already uses. Do not add a dependency without a clear need, and confirm it is already used before assuming it is available.\n- Keep changes minimal and scoped to the task. Do not reformat, rename, or refactor unrelated code, and do not add comments that merely restate what the code does.\n\n# Verify your work\n- After making changes, check them: run the project's build, tests, type-checker, or linter with bash when they exist, and fix what you broke.\n- Do not claim something works unless you have evidence. If you could not verify it, say so and explain what remains.\n\n# Communication\n- Be concise and direct. Skip filler, flattery, and long preambles; lead with the answer or the result. Reply in the user's language.\n- Briefly explain non-obvious shell commands and any assumptions or side effects before running them.\n- When you finish, summarize what changed and flag anything the user should review, test, or decide.\n\n# Safety\n- Do not run destructive or irreversible actions — deleting large trees, force-pushing, resetting history, or touching files outside the workspace — unless the user explicitly asks.\n- Do not commit, push, or rewrite git history unless asked.";

/// System prompts shipped as the default in earlier oxi versions. A stored prompt that still
/// matches one of these (after trimming) is treated as "never customized" and upgraded to the
/// current [`DEFAULT_AGENT_SYSTEM_PROMPT`] on load, so prompt improvements reach users who never
/// edited it while genuinely custom prompts are left untouched. Append, never edit or remove.
pub const LEGACY_DEFAULT_SYSTEM_PROMPTS: &[&str] = &[
    "You are an expert coding assistant. You help users by reading files, running shell commands, searching the codebase, and editing or writing files.\n\nAvailable tools (use only these when enabled): {tools_list}\n\nGuidelines:\n- Prefer reading files before editing.\n- Keep shell commands safe and relevant to the project.\n- When editing, ensure old text matches exactly.\n- Do not guess about project-specific implementation details when tools are available.\n- Verify claims by reading the relevant source files before answering.\n- Prefer evidence from the codebase over assumptions.",
];

/// True when `prompt` (trimmed) equals a default oxi shipped in some earlier version, meaning the
/// user never customized it and it is safe to upgrade to the current default.
pub fn is_legacy_default_system_prompt(prompt: &str) -> bool {
    let trimmed = prompt.trim();
    LEGACY_DEFAULT_SYSTEM_PROMPTS
        .iter()
        .any(|legacy| legacy.trim() == trimmed)
}

const AGENTS_MD_MAX_BYTES: usize = 64 * 1024;

pub fn build_system_prompt_for_workspace(settings: &AppSettings, workspace_root: &Path) -> String {
    let agents_md = if settings.include_agents_md {
        read_agents_md(workspace_root)
    } else {
        None
    };
    let oxi_rules = if settings.include_oxi_rules {
        read_oxi_rules(workspace_root)
    } else {
        None
    };
    build_system_prompt_with_project_instructions(
        settings,
        workspace_root.to_string_lossy().as_ref(),
        agents_md.as_deref(),
        oxi_rules.as_deref(),
    )
}

fn build_system_prompt_with_project_instructions(
    settings: &AppSettings,
    cwd: &str,
    agents_md: Option<&str>,
    oxi_rules: Option<&str>,
) -> String {
    let date = Utc::now().format("%Y-%m-%d");
    let cwd_norm = cwd.replace('\\', "/");
    let tools: Vec<&str> = ALL_TOOL_NAMES
        .iter()
        .zip(settings.tools_enabled.iter())
        .filter(|(_, en)| **en)
        .map(|(n, _)| *n)
        .collect();
    let tools_list = tools.join(", ");

    let template = settings.system_prompt.trim();
    let template = if template.is_empty() {
        DEFAULT_AGENT_SYSTEM_PROMPT
    } else {
        template
    };

    let mut body = template.replace("{tools_list}", &tools_list);
    body.push_str(
        "\n\nWorkspace mutation policy:\n- Create, modify, delete, move, or rename workspace paths only with the built-in `write`, `edit`, `delete`, `move`, and `mkdir` tools.\n- Normally, never use `bash`, scripts, formatters, generators, package installers, or MCP tools to mutate workspace files; use `bash` only for non-mutating commands. This restriction lets Oxi restore a turn before the user edits and retries its prompt.\n- Exception: if the user explicitly asks you to perform an otherwise prohibited operation, that request overrides this policy. You may use the necessary tool, but keep the action narrowly scoped, state any important side effects, and honor all approval prompts and hard tool safety checks.",
    );
    if let Some(contents) = agents_md.map(str::trim).filter(|s| !s.is_empty()) {
        body.push_str("\n\nProject instructions from AGENTS.md:\n");
        body.push_str(contents);
    }
    if let Some(contents) = oxi_rules.map(str::trim).filter(|s| !s.is_empty()) {
        body.push_str("\n\nProject rules from .oxi/rules (and .cursor/rules):\n");
        body.push_str(contents);
    }
    format!("{body}\n\nCurrent date: {date}\nCurrent working directory: {cwd_norm}")
}

fn read_agents_md(workspace_root: &Path) -> Option<String> {
    let path = workspace_root.join("AGENTS.md");
    let metadata = std::fs::metadata(&path).ok()?;
    if !metadata.is_file() || metadata.len() as usize > AGENTS_MD_MAX_BYTES {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

/// Load markdown rules from `.oxi/rules/` and, if present, `.cursor/rules/`.
/// Files are concatenated in sorted order, capped at [`AGENTS_MD_MAX_BYTES`] total.
fn read_oxi_rules(workspace_root: &Path) -> Option<String> {
    let mut parts = Vec::new();
    let mut total = 0usize;
    for dir_name in [".oxi/rules", ".cursor/rules"] {
        let dir = workspace_root.join(dir_name);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut files: Vec<_> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
                    && p.extension().and_then(|e| e.to_str()).is_some_and(|e| {
                        e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("mdc")
                    })
            })
            .collect();
        files.sort();
        for path in files {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            if text.trim().is_empty() {
                continue;
            }
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("rule.md");
            let chunk = format!("### {dir_name}/{name}\n{}", text.trim());
            if total + chunk.len() > AGENTS_MD_MAX_BYTES {
                break;
            }
            total += chunk.len();
            parts.push(chunk);
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn includes_agents_md_when_enabled() {
        let root = unique_temp_dir("agents-md-enabled");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("AGENTS.md"), "Run tests with cargo test.\n").unwrap();

        let prompt = build_system_prompt_for_workspace(&AppSettings::default(), &root);

        assert!(prompt.contains("Project instructions from AGENTS.md:"));
        assert!(prompt.contains("Run tests with cargo test."));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn includes_explicit_user_override_for_mutations() {
        let settings = AppSettings::default();
        let prompt =
            build_system_prompt_with_project_instructions(&settings, "/tmp/workspace", None, None);

        assert!(prompt.contains("if the user explicitly asks you"));
        assert!(prompt.contains("overrides this policy"));
        assert!(prompt.contains("hard tool safety checks"));
    }

    #[test]
    fn recognizes_legacy_defaults_but_not_custom_prompts() {
        for legacy in LEGACY_DEFAULT_SYSTEM_PROMPTS {
            assert!(is_legacy_default_system_prompt(legacy));
            // Surrounding whitespace still counts as the untouched default.
            assert!(is_legacy_default_system_prompt(&format!("\n{legacy}  ")));
        }
        // The current default is not "legacy" — no self-upgrade churn.
        assert!(!is_legacy_default_system_prompt(DEFAULT_AGENT_SYSTEM_PROMPT));
        // A genuinely edited prompt is preserved.
        assert!(!is_legacy_default_system_prompt("You are my custom agent. {tools_list}"));
        assert!(!is_legacy_default_system_prompt(""));
    }

    #[test]
    fn skips_agents_md_when_disabled() {
        let root = unique_temp_dir("agents-md-disabled");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("AGENTS.md"), "Do not include me.\n").unwrap();
        let settings = AppSettings {
            include_agents_md: false,
            ..Default::default()
        };

        let prompt = build_system_prompt_for_workspace(&settings, &root);

        assert!(!prompt.contains("Project instructions from AGENTS.md:"));
        assert!(!prompt.contains("Do not include me."));
        let _ = std::fs::remove_dir_all(root);
    }

    fn unique_temp_dir(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "oxi-{name}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }
}
