//! Slash commands advertised by ACP agents through `available_commands_update`.
//!
//! The agent owns its command list (built-ins, user/project custom commands, skills, MCP
//! prompts) and may resend it at any time, so oxi keeps no catalog of its own: every update
//! replaces the list for that agent + workspace, and the composer's `/` menu reads it from here.
//! Running a command needs nothing special either — ACP agents recognize a prompt that starts
//! with `/name`.
//!
//! The last list seen per launch command is also kept on disk, so the menu is populated right
//! after a restart, before the agent subprocess has been started again.
//!
//! ```text
//! <data_dir>/oxi/acp/commands.json   { "<command line>": [ { name, description, hint } ] }
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One command the agent accepts as `/name [input]`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AcpSlashCommand {
    /// Command name without the leading `/`.
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Placeholder for the command's free-form input; `None` when it takes no input.
    #[serde(default)]
    pub hint: Option<String>,
}

type List = Arc<Vec<AcpSlashCommand>>;

#[derive(Default)]
struct Registry {
    /// Live lists keyed by (launch command, workspace): project commands differ per workspace.
    live: HashMap<(String, PathBuf), List>,
    /// Last list per launch command, loaded from / saved to disk. `None` until first read.
    cached: Option<HashMap<String, List>>,
}

static REGISTRY: Mutex<Option<Registry>> = Mutex::new(None);

fn store_path() -> PathBuf {
    crate::app_dirs::data_dir()
        .join("acp")
        .join("commands.json")
}

fn read_cache(path: &Path) -> HashMap<String, List> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<HashMap<String, Vec<AcpSlashCommand>>>(&s).ok())
        .map(|m| m.into_iter().map(|(k, v)| (k, Arc::new(v))).collect())
        .unwrap_or_default()
}

fn write_cache(path: &Path, cache: &HashMap<String, List>) {
    let plain: HashMap<&String, &Vec<AcpSlashCommand>> =
        cache.iter().map(|(k, v)| (k, v.as_ref())).collect();
    let Ok(json) = serde_json::to_string_pretty(&plain) else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = crate::fsutil::write_atomic(path, json.as_bytes());
}

/// Parse the `availableCommands` array of an `available_commands_update`.
pub(super) fn parse_update(update: &Value) -> Option<Vec<AcpSlashCommand>> {
    let list = update.get("availableCommands")?.as_array()?;
    let mut commands: Vec<AcpSlashCommand> = list
        .iter()
        .filter_map(|c| {
            let name = c.get("name")?.as_str()?.trim().trim_start_matches('/');
            if name.is_empty() || name.contains(char::is_whitespace) {
                return None;
            }
            let description = c
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            let hint = c.get("input").filter(|i| !i.is_null()).map(|i| {
                i.get("hint")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string()
            });
            Some(AcpSlashCommand {
                name: name.to_string(),
                description,
                hint,
            })
        })
        .collect();
    commands.sort_by(|a, b| a.name.cmp(&b.name));
    commands.dedup_by(|a, b| a.name == b.name);
    Some(commands)
}

/// Record the list an agent just advertised.
pub(super) fn store(command_line: &str, cwd: &Path, commands: Vec<AcpSlashCommand>) {
    let list = Arc::new(commands);
    let command_line = command_line.trim().to_string();
    let mut guard = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    let reg = guard.get_or_insert_with(Registry::default);
    reg.live
        .insert((command_line.clone(), cwd.to_path_buf()), list.clone());
    let path = store_path();
    let cache = reg.cached.get_or_insert_with(|| read_cache(&path));
    if cache.get(&command_line) != Some(&list) {
        cache.insert(command_line, list);
        write_cache(&path, cache);
    }
}

/// Commands for the agent launched by `command_line` in `cwd`: the live list when that agent is
/// running there, else the last list any of its sessions advertised. Cheap enough per frame.
pub fn available(command_line: &str, cwd: &Path) -> List {
    let command_line = command_line.trim();
    let mut guard = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    let reg = guard.get_or_insert_with(Registry::default);
    if let Some(list) = reg.live.get(&(command_line.to_string(), cwd.to_path_buf())) {
        return list.clone();
    }
    if let Some(list) = reg
        .live
        .iter()
        .find(|((cmd, _), _)| cmd == command_line)
        .map(|(_, list)| list.clone())
    {
        return list;
    }
    reg.cached
        .get_or_insert_with(|| read_cache(&store_path()))
        .get(command_line)
        .cloned()
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_names_descriptions_and_hints() {
        let update = json!({
            "sessionUpdate": "available_commands_update",
            "availableCommands": [
                { "name": "review", "description": " Review a PR ", "input": { "hint": "PR number" } },
                { "name": "/init", "description": "Create CLAUDE.md", "input": null },
                { "name": "bad name", "description": "" },
                { "name": "" },
                { "name": "init", "description": "duplicate" }
            ]
        });
        let commands = parse_update(&update).unwrap();
        assert_eq!(
            commands,
            vec![
                AcpSlashCommand {
                    name: "init".into(),
                    description: "Create CLAUDE.md".into(),
                    hint: None,
                },
                AcpSlashCommand {
                    name: "review".into(),
                    description: "Review a PR".into(),
                    hint: Some("PR number".into()),
                },
            ]
        );
    }

    #[test]
    fn missing_list_is_not_an_update() {
        assert!(parse_update(&json!({ "sessionUpdate": "available_commands_update" })).is_none());
    }

    #[test]
    fn cache_roundtrip() {
        let dir = std::env::temp_dir().join(format!("oxi-acp-commands-{}", std::process::id()));
        let path = dir.join("commands.json");
        let mut cache = HashMap::new();
        cache.insert(
            "codex-acp".to_string(),
            Arc::new(vec![AcpSlashCommand {
                name: "status".into(),
                description: "Show status".into(),
                hint: None,
            }]),
        );
        write_cache(&path, &cache);
        assert_eq!(read_cache(&path), cache);
        let _ = std::fs::remove_dir_all(dir);
    }
}
