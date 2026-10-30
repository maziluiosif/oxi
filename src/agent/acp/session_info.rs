//! What an agent reports about its session outside of any turn: the title it generated
//! (`session_info_update`, sent once the turn is over) and what the session cost so far (the
//! `cost` of `usage_update`). Kept per oxi session key for the UI to pick up.

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::Value;

#[derive(Default)]
struct Info {
    /// A title the UI has not applied yet.
    title: Option<String>,
    /// Cost per agent session, in the currency the agent reported.
    costs: HashMap<String, Cost>,
}

#[derive(Default)]
struct Cost {
    currency: String,
    /// Spent by earlier runs of the agent: a relaunched agent counts from zero again.
    earlier: f64,
    last: f64,
}

static INFO: Mutex<Option<HashMap<String, Info>>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut HashMap<String, Info>) -> R) -> R {
    let mut info = INFO.lock().unwrap_or_else(|e| e.into_inner());
    f(info.get_or_insert_with(HashMap::new))
}

/// Record what `update` (a `session/update` of agent session `session_id`) says about the
/// session. Returns whether it was a title, which the UI is woken for.
pub(super) fn note(session_key: &str, session_id: &str, update: &Value) -> bool {
    match update["sessionUpdate"].as_str() {
        Some("session_info_update") => {
            let Some(title) = update["title"]
                .as_str()
                .map(str::trim)
                .filter(|t| !t.is_empty())
            else {
                return false;
            };
            with(|info| {
                info.entry(session_key.to_owned()).or_default().title = Some(title.to_owned())
            });
            true
        }
        Some("usage_update") => {
            let cost = &update["cost"];
            if let Some(amount) = cost["amount"]
                .as_f64()
                .filter(|a| a.is_finite() && *a >= 0.0)
            {
                let currency = cost["currency"].as_str().unwrap_or("USD").to_owned();
                with(|info| {
                    let costs = &mut info.entry(session_key.to_owned()).or_default().costs;
                    let entry = costs.entry(session_id.to_owned()).or_default();
                    if amount < entry.last || entry.currency != currency {
                        entry.earlier += entry.last;
                    }
                    entry.currency = currency;
                    entry.last = amount;
                });
            }
            false
        }
        _ => false,
    }
}

/// Titles agents generated since the last call, by oxi session key.
pub fn take_titles() -> Vec<(String, String)> {
    with(|info| {
        info.iter_mut()
            .filter_map(|(key, i)| Some((key.clone(), i.title.take()?)))
            .collect()
    })
}

/// Whether the agent sent `session_key` a title the UI has not applied yet.
pub fn has_title(session_key: &str) -> bool {
    with(|info| info.get(session_key).is_some_and(|i| i.title.is_some()))
}

/// Put back a title the UI could not apply yet (the chat is answering).
pub fn defer_title(session_key: &str, title: String) {
    with(|info| {
        let slot = &mut info.entry(session_key.to_owned()).or_default().title;
        if slot.is_none() {
            *slot = Some(title);
        }
    });
}

/// What the chat's agents reported spending since oxi started, when they report it in one
/// currency.
pub fn reported_cost(session_key: &str) -> Option<(f64, String)> {
    with(|info| {
        let costs = &info.get(session_key)?.costs;
        let currency = costs.values().next()?.currency.clone();
        costs
            .values()
            .all(|c| c.currency == currency)
            .then(|| (costs.values().map(|c| c.earlier + c.last).sum(), currency))
    })
}

/// The chat was deleted or rewritten.
pub(super) fn forget(session_key: &str) {
    with(|info| info.remove(session_key));
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn usage(amount: f64) -> Value {
        json!({ "sessionUpdate": "usage_update", "used": 1, "size": 2,
                "cost": { "amount": amount, "currency": "USD" } })
    }

    #[test]
    fn cost_adds_up_across_agents_and_relaunches() {
        let key = "session-info-cost";
        note(key, "a", &usage(0.5));
        note(key, "a", &usage(1.0));
        note(key, "b", &usage(0.25));
        // Agent `a` relaunched: its cost counts from zero again.
        note(key, "a", &usage(0.1));
        let (total, currency) = reported_cost(key).unwrap();
        assert!((total - 1.35).abs() < 1e-9, "{total}");
        assert_eq!(currency, "USD");
        forget(key);
        assert!(reported_cost(key).is_none());
    }

    #[test]
    fn a_title_is_taken_once_and_can_be_deferred() {
        let key = "session-info-title";
        assert!(note(
            key,
            "a",
            &json!({ "sessionUpdate": "session_info_update", "title": " Fix login " })
        ));
        assert!(!note(
            key,
            "a",
            &json!({ "sessionUpdate": "session_info_update" })
        ));
        let taken: Vec<_> = take_titles()
            .into_iter()
            .filter(|(k, _)| k == key)
            .collect();
        assert_eq!(taken, vec![(key.to_owned(), "Fix login".to_owned())]);
        assert!(take_titles().iter().all(|(k, _)| k != key));
        assert!(!has_title(key));
        defer_title(key, "Fix login".into());
        assert!(has_title(key));
        assert!(take_titles().iter().any(|(k, _)| k == key));
        forget(key);
    }
}
