//! Session config options an ACP agent advertises (`configOptions`): permission mode, model,
//! effort, fast mode, agent persona… The agent owns the list and may change it at any time
//! (a model switch changes the effort levels, fast mode only exists on some models), so the
//! composer renders whatever is published here instead of a fixed set of pickers.
//!
//! Lists are keyed by oxi session and launch command, so a chat that moved to another agent
//! never shows the previous agent's options.
//!
//! Launching an agent takes seconds, so the options each launch command advertised by default
//! are also kept on disk; until a chat's agent is up, the composer shows those instead of
//! nothing. Values picked in that window wait in [`set_local`] and are applied at launch.
//!
//! ```text
//! <data_dir>/oxi/acp/config-options.json   { "<launch command>": <configOptions> }
//! ```

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One option from `configOptions`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AcpConfigOption {
    pub id: String,
    pub name: String,
    pub description: String,
    /// `mode`, `model`, `thought_level`, `model_config`, … (empty when not given).
    pub category: String,
    pub kind: AcpConfigKind,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum AcpConfigKind {
    Select {
        current: String,
        values: Vec<AcpConfigValue>,
    },
    Boolean(bool),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AcpConfigValue {
    pub value: String,
    pub name: String,
    pub description: String,
    /// Name of the group the agent listed this value under, if any.
    pub group: Option<String>,
}

impl AcpConfigOption {
    pub fn is_model(&self) -> bool {
        self.category == "model" || self.id == "model"
    }

    pub fn is_thought_level(&self) -> bool {
        self.category == "thought_level"
            || matches!(
                self.id.as_str(),
                "thought_level" | "reasoning_effort" | "effort"
            )
    }
}

/// Parse a `configOptions` array, skipping entries of unknown shape.
pub fn parse(options: &Value) -> Vec<AcpConfigOption> {
    let Some(options) = options.as_array() else {
        return Vec::new();
    };
    options.iter().filter_map(parse_option).collect()
}

fn parse_option(o: &Value) -> Option<AcpConfigOption> {
    let id = o["id"].as_str()?.to_owned();
    let text = |key: &str| o[key].as_str().unwrap_or_default().to_owned();
    let kind = match (o["type"].as_str(), &o["currentValue"]) {
        (Some("boolean"), current) | (None, current @ Value::Bool(_)) => {
            AcpConfigKind::Boolean(current.as_bool().unwrap_or(false))
        }
        (Some("select") | None, current) => {
            let mut values = Vec::new();
            for entry in o["options"].as_array()? {
                // Grouped options: `{ group, name, options: [...] }`.
                if let Some(grouped) = entry["options"].as_array() {
                    let group = entry["name"]
                        .as_str()
                        .or(entry["group"].as_str())
                        .map(str::to_owned);
                    values.extend(grouped.iter().filter_map(|v| parse_value(v, group.clone())));
                } else if let Some(v) = parse_value(entry, None) {
                    values.push(v);
                }
            }
            AcpConfigKind::Select {
                current: current.as_str().unwrap_or_default().to_owned(),
                values,
            }
        }
        _ => return None,
    };
    Some(AcpConfigOption {
        name: o["name"].as_str().unwrap_or(&id).to_owned(),
        id,
        description: text("description"),
        category: text("category"),
        kind,
    })
}

fn parse_value(v: &Value, group: Option<String>) -> Option<AcpConfigValue> {
    let value = v["value"].as_str()?.to_owned();
    Some(AcpConfigValue {
        name: v["name"].as_str().unwrap_or(&value).to_owned(),
        description: v["description"].as_str().unwrap_or_default().to_owned(),
        value,
        group,
    })
}

type Key = (String, String);

static REGISTRY: Mutex<Option<HashMap<Key, Arc<Vec<AcpConfigOption>>>>> = Mutex::new(None);

type Listener = Box<dyn Fn(&str, &[AcpConfigOption]) + Send>;

/// Told of every list an agent publishes: the server passes them on to its clients.
static LISTENER: Mutex<Option<Listener>> = Mutex::new(None);

pub fn set_listener(listener: impl Fn(&str, &[AcpConfigOption]) + Send + 'static) {
    *LISTENER.lock().unwrap_or_else(|e| e.into_inner()) = Some(Box::new(listener));
}

/// Publish the latest `configOptions` of a session's agent.
pub(crate) fn publish(session_key: &str, command_line: &str, options: &Value) {
    let parsed = parse(options);
    if let Some(listener) = LISTENER.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        listener(session_key, &parsed);
    }
    show(session_key, command_line, parsed);
}

/// Show `options` as what the session's agent offers now. Used directly for an agent running on
/// a server.
pub(crate) fn show(session_key: &str, command_line: &str, options: Vec<AcpConfigOption>) {
    let mut registry = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    registry.get_or_insert_with(HashMap::new).insert(
        (session_key.to_owned(), command_line.to_owned()),
        Arc::new(options),
    );
    drop(registry);
    // The agent's own list replaces whatever was picked before it ran.
    let mut picks = PICKS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(map) = picks.as_mut() {
        map.remove(&(session_key.to_owned(), command_line.to_owned()));
    }
}

pub(super) fn forget(session_key: &str) {
    let mut registry = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(map) = registry.as_mut() {
        map.retain(|(key, _), _| key != session_key);
    }
    drop(registry);
    let mut picks = PICKS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(map) = picks.as_mut() {
        map.retain(|(key, _), _| key != session_key);
    }
}

/// `(option id, value)` pairs in pick order.
type Picks = Vec<(String, Value)>;

/// Values picked for a chat whose agent was not running yet.
static PICKS: Mutex<Option<HashMap<Key, Picks>>> = Mutex::new(None);

/// The options each launch command advertised on its last `session/new`, before oxi changed
/// anything; loaded from disk on first use.
static DEFAULTS: Mutex<Option<HashMap<String, Arc<Vec<AcpConfigOption>>>>> = Mutex::new(None);

fn defaults_path() -> std::path::PathBuf {
    crate::app_dirs::data_dir()
        .join("acp")
        .join("config-options.json")
}

fn read_defaults() -> HashMap<String, Value> {
    std::fs::read_to_string(defaults_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn with_defaults<R>(f: impl FnOnce(&mut HashMap<String, Arc<Vec<AcpConfigOption>>>) -> R) -> R {
    let mut defaults = DEFAULTS.lock().unwrap_or_else(|e| e.into_inner());
    let map = defaults.get_or_insert_with(|| {
        read_defaults()
            .into_iter()
            .map(|(command_line, options)| (command_line, Arc::new(parse(&options))))
            .collect()
    });
    f(map)
}

/// Keep the options an agent advertised for a new session as what the composer shows the next
/// time this command is launched.
pub(super) fn remember_defaults(command_line: &str, options: &Value) {
    let parsed = parse(options);
    if parsed.is_empty() {
        return;
    }
    let changed = with_defaults(|map| {
        if map.get(command_line).is_some_and(|known| **known == parsed) {
            return false;
        }
        map.insert(command_line.to_owned(), Arc::new(parsed));
        true
    });
    if !changed {
        return;
    }
    let mut stored = read_defaults();
    stored.insert(command_line.to_owned(), options.clone());
    let written = serde_json::to_vec_pretty(&stored)
        .map_err(|e| e.to_string())
        .and_then(|json| crate::fsutil::write_atomic(&defaults_path(), &json));
    if let Err(e) = written {
        log::warn!("could not save ACP config options: {e}");
    }
}

/// What to show for a chat whose agent is not running: the command's remembered options with
/// the values picked since applied. `None` when this command never ran.
pub fn before_launch(session_key: &str, command_line: &str) -> Option<Arc<Vec<AcpConfigOption>>> {
    let mut options = with_defaults(|map| map.get(command_line).cloned())?;
    let picks = PICKS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(picks) = picks
        .as_ref()
        .and_then(|m| m.get(&(session_key.to_owned(), command_line.to_owned())))
    {
        let list = Arc::make_mut(&mut options);
        for (id, value) in picks {
            set_value(list, id, value);
        }
    }
    Some(options)
}

/// The options picked before the agent launched, as they should end up on it.
pub(crate) fn picked_before_launch(session_key: &str, command_line: &str) -> Vec<AcpConfigOption> {
    let ids: Vec<String> = {
        let picks = PICKS.lock().unwrap_or_else(|e| e.into_inner());
        let Some(picks) = picks
            .as_ref()
            .and_then(|m| m.get(&(session_key.to_owned(), command_line.to_owned())))
        else {
            return Vec::new();
        };
        picks.iter().map(|(id, _)| id.clone()).collect()
    };
    before_launch(session_key, command_line)
        .map(|options| {
            options
                .iter()
                .filter(|o| ids.contains(&o.id))
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

fn set_value(list: &mut [AcpConfigOption], id: &str, value: &Value) {
    if let Some(option) = list.iter_mut().find(|o| o.id == id) {
        match (&mut option.kind, value) {
            (AcpConfigKind::Select { current, .. }, Value::String(v)) => *current = v.clone(),
            (AcpConfigKind::Boolean(on), Value::Bool(v)) => *on = *v,
            _ => {}
        }
    }
}

/// The options the session's agent last advertised; `None` before it has been started.
pub fn options(session_key: &str, command_line: &str) -> Option<Arc<Vec<AcpConfigOption>>> {
    let registry = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    registry
        .as_ref()?
        .get(&(session_key.to_owned(), command_line.to_owned()))
        .cloned()
}

/// Show a value the user just picked right away, before the agent confirms it. Without a
/// running agent the pick is kept for its launch.
pub fn set_local(session_key: &str, command_line: &str, id: &str, value: &Value) {
    let key = (session_key.to_owned(), command_line.to_owned());
    let mut registry = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(list) = registry.as_mut().and_then(|m| m.get_mut(&key)) {
        set_value(Arc::<Vec<_>>::make_mut(list), id, value);
        return;
    }
    drop(registry);
    let mut picks = PICKS.lock().unwrap_or_else(|e| e.into_inner());
    let list = picks
        .get_or_insert_with(HashMap::new)
        .entry(key)
        .or_default();
    list.retain(|(picked, _)| picked != id);
    list.push((id.to_owned(), value.clone()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_selects_groups_and_booleans() {
        let options = parse(&json!([
            {"id": "mode", "name": "Mode", "category": "mode", "type": "select",
             "currentValue": "auto",
             "options": [{"value": "default", "name": "Manual"}, {"value": "auto", "name": "Auto"}]},
            {"id": "model", "name": "Model", "category": "model", "type": "select",
             "currentValue": "opus",
             "options": [{"group": "anthropic", "name": "Anthropic",
                          "options": [{"value": "opus", "name": "Opus 5.5", "description": "Most capable"}]}]},
            {"id": "fast", "name": "Fast mode", "category": "model_config", "type": "boolean",
             "currentValue": true},
            {"id": "weird", "type": "slider"}
        ]));
        assert_eq!(options.len(), 3);
        assert!(
            matches!(&options[0].kind, AcpConfigKind::Select { current, .. } if current == "auto")
        );
        assert!(options[1].is_model());
        let AcpConfigKind::Select { values, .. } = &options[1].kind else {
            panic!("model is a select");
        };
        assert_eq!(values[0].group.as_deref(), Some("Anthropic"));
        assert_eq!(values[0].description, "Most capable");
        assert_eq!(options[2].kind, AcpConfigKind::Boolean(true));
    }

    #[test]
    fn remembered_options_show_before_launch_with_picks_kept_for_it() {
        let command = "agent --before-launch";
        assert!(before_launch("chat", command).is_none());
        remember_defaults(
            command,
            &json!([
                {"id": "mode", "type": "select", "currentValue": "default",
                 "options": [{"value": "default"}, {"value": "acceptEdits"}]},
                {"id": "fast", "type": "boolean", "currentValue": false}
            ]),
        );
        set_local("chat", command, "fast", &json!(true));
        let shown = before_launch("chat", command).unwrap();
        assert_eq!(shown[1].kind, AcpConfigKind::Boolean(true));
        // Another chat on the same command still sees the agent's defaults.
        assert_eq!(
            before_launch("other", command).unwrap()[1].kind,
            AcpConfigKind::Boolean(false)
        );
        let picked = picked_before_launch("chat", command);
        assert_eq!(picked.len(), 1);
        assert_eq!(picked[0].id, "fast");
        // Once the agent publishes its own list, the picks are done with.
        publish("chat", command, &json!([]));
        assert!(picked_before_launch("chat", command).is_empty());
        forget("chat");
    }

    #[test]
    fn local_updates_apply_to_the_published_list() {
        publish(
            "s",
            "agent",
            &json!([{"id": "effort", "category": "thought_level", "type": "select",
                     "currentValue": "low", "options": [{"value": "low"}, {"value": "high"}]}]),
        );
        set_local("s", "agent", "effort", &json!("high"));
        let options = options("s", "agent").unwrap();
        assert!(options[0].is_thought_level());
        assert!(
            matches!(&options[0].kind, AcpConfigKind::Select { current, .. } if current == "high")
        );
        assert!(super::options("s", "other").is_none());
        forget("s");
        assert!(super::options("s", "agent").is_none());
    }
}
