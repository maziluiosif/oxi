//! Session modes: putting the agent into its own plan mode for oxi's plan-mode turns.
//!
//! Agents expose plan mode differently: Claude Code as a `plan` value of its `mode` config
//! option, Codex as a `plan` value of its `collaboration_mode` option, older adapters through
//! the legacy `modes` / `session/set_mode` pair. Whichever is advertised is switched; when none
//! is, the caller falls back to asking for a plan in the prompt.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::process::ChildStdin;
use tokio::sync::Mutex as AsyncMutex;

use super::{Pending, request};

const PLAN: &str = "plan";

/// Mode state as the agent last reported it. Shared with the reader task, which applies
/// `config_option_update` / `current_mode_update` notifications.
#[derive(Default)]
pub(super) struct ModeState {
    /// Latest `configOptions` array.
    config_options: Value,
    /// Legacy `modes` object (`currentModeId`, `availableModes`).
    modes: Value,
    /// Value oxi replaced when it switched the agent into plan mode, restored on the next
    /// regular turn.
    restore: Option<String>,
    /// The user put the agent into plan mode themselves (an agent slash command), so regular
    /// turns leave it there.
    user_plan: bool,
}

pub(super) type SharedModes = Arc<Mutex<ModeState>>;

/// Where an agent's plan mode lives.
#[derive(Debug, PartialEq)]
enum PlanSwitch {
    Config {
        id: String,
        current: String,
        values: Vec<String>,
    },
    Legacy {
        current: String,
        ids: Vec<String>,
    },
}

impl PlanSwitch {
    fn current(&self) -> &str {
        match self {
            Self::Config { current, .. } | Self::Legacy { current, .. } => current,
        }
    }

    /// Where a regular turn goes back to when nothing was recorded: `default` when offered,
    /// else the first non-plan value.
    fn fallback(&self) -> Option<String> {
        let values = match self {
            Self::Config { values, .. } => values,
            Self::Legacy { ids, .. } => ids,
        };
        values
            .iter()
            .find(|v| *v == "default")
            .or_else(|| values.iter().find(|v| *v != PLAN))
            .cloned()
    }
}

impl ModeState {
    pub(super) fn new(config_options: Value, modes: Value) -> Self {
        Self {
            config_options,
            modes,
            ..Self::default()
        }
    }

    pub(super) fn apply_notification(&mut self, update: &Value) {
        match update["sessionUpdate"].as_str() {
            Some("config_option_update") => {
                if let Some(options) = update.get("configOptions").filter(|v| v.is_array()) {
                    self.config_options = options.clone();
                }
            }
            Some("current_mode_update") => {
                if let (Some(id), Some(modes)) =
                    (update["currentModeId"].as_str(), self.modes.as_object_mut())
                {
                    modes.insert("currentModeId".into(), json!(id));
                }
            }
            _ => return,
        }
        if self.plan_switch().is_none_or(|s| s.current() != PLAN) {
            self.restore = None;
            self.user_plan = false;
        }
    }

    /// After a turn that ran an agent slash command: a plan mode it turned on is the user's.
    pub(super) fn note_user_command(&mut self) {
        if self.plan_switch().is_some_and(|s| s.current() == PLAN) && self.restore.is_none() {
            self.user_plan = true;
        }
    }

    fn plan_switch(&self) -> Option<PlanSwitch> {
        let option = self.config_options.as_array().and_then(|options| {
            options.iter().find(|o| {
                let category = o["category"].as_str().or(o["id"].as_str());
                matches!(category, Some("mode" | "collaboration_mode"))
                    && option_values(o).iter().any(|v| v == PLAN)
            })
        });
        if let Some(option) = option {
            return Some(PlanSwitch::Config {
                id: option["id"].as_str()?.to_owned(),
                current: option["currentValue"].as_str().unwrap_or("").to_owned(),
                values: option_values(option),
            });
        }
        let ids: Vec<String> = self.modes["availableModes"]
            .as_array()?
            .iter()
            .filter_map(|m| m["id"].as_str().map(str::to_owned))
            .collect();
        ids.iter().any(|id| id == PLAN).then(|| PlanSwitch::Legacy {
            current: self.modes["currentModeId"]
                .as_str()
                .unwrap_or("")
                .to_owned(),
            ids,
        })
    }

    /// The value to switch to for a turn, if any, and what to restore afterwards.
    fn plan_target(&self, plan_mode: bool) -> Option<(PlanSwitch, String, Option<String>)> {
        let switch = self.plan_switch()?;
        let current = switch.current().to_owned();
        if plan_mode {
            if current == PLAN {
                return None;
            }
            let restore = Some(current)
                .filter(|c| !c.is_empty())
                .or_else(|| switch.fallback());
            return Some((switch, PLAN.to_owned(), restore));
        }
        if current != PLAN || self.user_plan {
            return None;
        }
        let target = self.restore.clone().or_else(|| switch.fallback())?;
        Some((switch, target, None))
    }
}

fn option_values(option: &Value) -> Vec<String> {
    option["options"]
        .as_array()
        .map(|values| {
            values
                .iter()
                .filter_map(|v| v["value"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Switch the agent into its plan mode for a plan-mode turn, or back out of the one oxi put it
/// in for a regular turn. Returns whether the agent is in its own plan mode for this turn; when
/// it is not, the caller asks for a plan in the prompt instead.
pub(super) async fn sync_plan_mode(
    stdin: &Arc<AsyncMutex<ChildStdin>>,
    next_id: &Arc<std::sync::atomic::AtomicI64>,
    pending: &Pending,
    session_id: &str,
    modes: &SharedModes,
    plan_mode: bool,
) -> bool {
    let lock = || modes.lock().unwrap_or_else(|e| e.into_inner());
    let target = lock().plan_target(plan_mode);
    let Some((switch, value, restore)) = target else {
        return plan_mode && lock().plan_switch().is_some();
    };
    let result = match &switch {
        PlanSwitch::Config { id, .. } => request(
            stdin,
            next_id,
            pending,
            "session/set_config_option",
            json!({ "sessionId": session_id, "configId": id, "value": value }),
        )
        .await
        .map(|res| {
            let mut state = lock();
            match res.get("configOptions").filter(|v| v.is_array()) {
                Some(options) => state.config_options = options.clone(),
                None => set_config_value(&mut state.config_options, id, &value),
            }
        }),
        PlanSwitch::Legacy { .. } => request(
            stdin,
            next_id,
            pending,
            "session/set_mode",
            json!({ "sessionId": session_id, "modeId": value }),
        )
        .await
        .map(|_| {
            if let Some(modes) = lock().modes.as_object_mut() {
                modes.insert("currentModeId".into(), json!(value));
            }
        }),
    };
    match result {
        Ok(()) => {
            let mut state = lock();
            state.restore = restore;
            state.user_plan = false;
            plan_mode
        }
        Err(e) => {
            eprintln!("[acp] could not switch the agent to mode {value}: {e}");
            false
        }
    }
}

fn set_config_value(options: &mut Value, id: &str, value: &str) {
    if let Some(option) = options
        .as_array_mut()
        .and_then(|opts| opts.iter_mut().find(|o| o["id"].as_str() == Some(id)))
    {
        option["currentValue"] = json!(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claude_options(current: &str) -> Value {
        json!([
            {"id": "model", "category": "model", "currentValue": "opus",
             "options": [{"value": "opus"}, {"value": "sonnet"}]},
            {"id": "mode", "category": "mode", "currentValue": current,
             "options": [{"value": "default"}, {"value": "acceptEdits"}, {"value": "plan"}]}
        ])
    }

    fn codex_options(collab: &str) -> Value {
        json!([
            {"id": "mode", "category": "mode", "currentValue": "agent",
             "options": [{"value": "read-only"}, {"value": "agent"}]},
            {"id": "collaboration_mode", "category": "collaboration_mode", "currentValue": collab,
             "options": [{"value": "default"}, {"value": "plan"}]}
        ])
    }

    #[test]
    fn finds_claude_and_codex_plan_options() {
        let claude = ModeState::new(claude_options("acceptEdits"), Value::Null);
        let (switch, value, restore) = claude.plan_target(true).unwrap();
        assert!(matches!(switch, PlanSwitch::Config { ref id, .. } if id == "mode"));
        assert_eq!(value, "plan");
        assert_eq!(restore.as_deref(), Some("acceptEdits"));

        let codex = ModeState::new(codex_options("default"), Value::Null);
        let (switch, _, restore) = codex.plan_target(true).unwrap();
        assert!(matches!(switch, PlanSwitch::Config { ref id, .. } if id == "collaboration_mode"));
        assert_eq!(restore.as_deref(), Some("default"));
    }

    #[test]
    fn legacy_modes_and_agents_without_plan_mode() {
        let legacy = ModeState::new(
            Value::Null,
            json!({"currentModeId": "ask", "availableModes": [{"id": "ask"}, {"id": "plan"}]}),
        );
        let (switch, value, restore) = legacy.plan_target(true).unwrap();
        assert!(matches!(switch, PlanSwitch::Legacy { .. }));
        assert_eq!((value.as_str(), restore.as_deref()), ("plan", Some("ask")));

        let none = ModeState::new(
            json!([{"id": "mode", "category": "mode", "currentValue": "agent",
                    "options": [{"value": "agent"}]}]),
            Value::Null,
        );
        assert!(none.plan_switch().is_none());
        assert!(none.plan_target(true).is_none());
    }

    #[test]
    fn regular_turn_restores_only_what_oxi_switched() {
        let mut state = ModeState::new(claude_options("plan"), Value::Null);
        state.restore = Some("acceptEdits".into());
        let (_, value, _) = state.plan_target(false).unwrap();
        assert_eq!(value, "acceptEdits");

        // Already planning and plan mode wanted: nothing to do.
        assert!(state.plan_target(true).is_none());

        // The user turned plan mode on through a slash command: regular turns keep it.
        let mut user = ModeState::new(claude_options("plan"), Value::Null);
        user.note_user_command();
        assert!(user.plan_target(false).is_none());

        // A resumed session that came back in plan mode without a record goes to `default`.
        let resumed = ModeState::new(claude_options("plan"), Value::Null);
        assert_eq!(resumed.plan_target(false).unwrap().1, "default");
    }

    #[test]
    fn notifications_track_the_current_mode() {
        let mut state = ModeState::new(
            Value::Null,
            json!({"currentModeId": "default", "availableModes": [{"id": "default"}, {"id": "plan"}]}),
        );
        state.apply_notification(
            &json!({"sessionUpdate": "current_mode_update", "currentModeId": "plan"}),
        );
        assert_eq!(state.plan_switch().unwrap().current(), "plan");

        state.restore = Some("default".into());
        state.apply_notification(&json!({
            "sessionUpdate": "config_option_update",
            "configOptions": claude_options("acceptEdits"),
        }));
        // Leaving plan mode (here: the agent approved its own plan) forgets the restore value.
        assert_eq!(state.plan_switch().unwrap().current(), "acceptEdits");
        assert!(state.restore.is_none());
    }
}
