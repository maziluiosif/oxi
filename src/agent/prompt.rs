//! System prompt construction (aligned with pi `buildSystemPrompt` subset).

use std::path::Path;

use chrono::Utc;

use crate::settings::{ALL_TOOL_NAMES, AppSettings};

pub const DEFAULT_AGENT_SYSTEM_PROMPT: &str = "You are an expert coding assistant. You help users by reading files, running shell commands, searching the codebase, and editing or writing files.\n\nAvailable tools (use only these when enabled): {tools_list}\n\nGuidelines:\n- Prefer reading files before editing.\n- Keep shell commands safe and relevant to the project.\n- When editing, ensure old text matches exactly.\n- Do not guess about project-specific implementation details when tools are available.\n- Verify claims by reading the relevant source files before answering.\n- Prefer evidence from the codebase over assumptions.";

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
