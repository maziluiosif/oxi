//! Session config options an ACP agent advertises (`configOptions`): permission mode, model,
//! effort, fast mode, agent persona… The agent owns the list and may change it at any time
//! (a model switch changes the effort levels, fast mode only exists on some models), so the
//! composer renders whatever is published here instead of a fixed set of pickers.
//!
//! Lists are keyed by oxi session and launch command, so a chat that moved to another agent
//! never shows the previous agent's options.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::Value;

/// One option from `configOptions`.
#[derive(Clone, Debug, PartialEq)]
pub struct AcpConfigOption {
    pub id: String,
    pub name: String,
    pub description: String,
    /// `mode`, `model`, `thought_level`, `model_config`, … (empty when not given).
    pub category: String,
    pub kind: AcpConfigKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AcpConfigKind {
    Select {
        current: String,
        values: Vec<AcpConfigValue>,
    },
    Boolean(bool),
}

#[derive(Clone, Debug, PartialEq)]
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

/// Publish the latest `configOptions` of a session's agent.
pub(crate) fn publish(session_key: &str, command_line: &str, options: &Value) {
    let parsed = Arc::new(parse(options));
    let mut registry = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    registry
        .get_or_insert_with(HashMap::new)
        .insert((session_key.to_owned(), command_line.to_owned()), parsed);
}

pub(super) fn forget(session_key: &str) {
    let mut registry = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(map) = registry.as_mut() {
        map.retain(|(key, _), _| key != session_key);
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

/// Show a value the user just picked right away, before the agent confirms it.
pub fn set_local(session_key: &str, command_line: &str, id: &str, value: &Value) {
    let mut registry = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    let Some(list) = registry
        .as_mut()
        .and_then(|m| m.get_mut(&(session_key.to_owned(), command_line.to_owned())))
    else {
        return;
    };
    let list = Arc::make_mut(list);
    if let Some(option) = list.iter_mut().find(|o| o.id == id) {
        match (&mut option.kind, value) {
            (AcpConfigKind::Select { current, .. }, Value::String(v)) => *current = v.clone(),
            (AcpConfigKind::Boolean(on), Value::Bool(v)) => *on = *v,
            _ => {}
        }
    }
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
