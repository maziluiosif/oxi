//! Stopping agent subprocesses nobody is using. Each ACP chat keeps its own agent (a Node
//! adapter plus the agent binary, a few hundred MB together) alive after its turn so the next
//! prompt answers right away. Left alone, every chat touched during the day holds one. An agent
//! idle for longer than the configured limit is stopped, as is the least recently used one
//! beyond the configured count; the next prompt relaunches it and resumes the agent's own
//! session, so the conversation carries over.
//!
//! An agent is never stopped mid-turn, while it holds a turn of its own for the UI, or while a
//! request to it is outstanding. One that started background work (a background command, a
//! scheduled wake-up, a monitor) gets a longer grace, since stopping it would drop that work.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

/// Idle time after which an agent is stopped; `0` never stops one for being idle.
static IDLE_SECS: AtomicU64 = AtomicU64::new(15 * 60);
/// Most agents kept running at once; `0` is no limit.
static MAX_LIVE: AtomicUsize = AtomicUsize::new(4);

/// How long an agent that started background work is kept after it last said anything. Claude
/// Code's scheduled wake-ups are at most an hour apart.
const BACKGROUND_GRACE: Duration = Duration::from_secs(2 * 60 * 60);
/// An agent used more recently than this is never stopped to make room for another.
const MIN_IDLE_FOR_LIMIT: Duration = Duration::from_secs(60);
/// How often idle agents are looked for.
pub(super) const SWEEP_EVERY: Duration = Duration::from_secs(30);

/// Tools whose work outlives the turn that started them.
const BACKGROUND_TOOLS: &[&str] = &["ScheduleWakeup", "Monitor", "CronCreate"];

/// Set the stopping policy from the settings.
pub fn set_policy(idle_minutes: u32, max_live: u32) {
    IDLE_SECS.store(u64::from(idle_minutes) * 60, Ordering::Relaxed);
    MAX_LIVE.store(max_live as usize, Ordering::Relaxed);
}

fn policy() -> (Option<Duration>, Option<usize>) {
    let idle = IDLE_SECS.load(Ordering::Relaxed);
    let max = MAX_LIVE.load(Ordering::Relaxed);
    (
        (idle > 0).then(|| Duration::from_secs(idle)),
        (max > 0).then_some(max),
    )
}

/// When an agent was last used, and whether it started work that outlives its turns.
pub(super) struct Activity {
    last: Mutex<Instant>,
    background: AtomicBool,
}

impl Activity {
    pub(super) fn new() -> Self {
        Self {
            last: Mutex::new(Instant::now()),
            background: AtomicBool::new(false),
        }
    }

    pub(super) fn touch(&self) {
        *self.last.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
    }

    fn idle(&self, now: Instant) -> Duration {
        now.saturating_duration_since(*self.last.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// Whether the agent started work that keeps running after its turns.
    pub(super) fn background(&self) -> bool {
        self.background.load(Ordering::Relaxed)
    }

    /// Record a message from the agent: it is in use, and it may have started background work.
    pub(super) fn note(&self, msg: &Value) {
        self.touch();
        if msg["method"].as_str() == Some("session/update")
            && starts_background(&msg["params"]["update"])
        {
            self.background.store(true, Ordering::Relaxed);
        }
    }
}

/// Whether a tool call starts work that keeps running after the turn.
fn starts_background(update: &Value) -> bool {
    matches!(
        update["sessionUpdate"].as_str(),
        Some("tool_call" | "tool_call_update")
    ) && (update["rawInput"]["run_in_background"].as_bool() == Some(true)
        || update["_meta"]["claudeCode"]["toolName"]
            .as_str()
            .is_some_and(|name| BACKGROUND_TOOLS.contains(&name)))
}

/// One running agent as the sweep sees it.
pub(super) struct Candidate {
    pub(super) key: String,
    pub(super) idle: Duration,
    /// In a turn, holding a turn of its own, or answering a request.
    pub(super) busy: bool,
    pub(super) background: bool,
}

impl Candidate {
    pub(super) fn of(key: &str, activity: &Activity, busy: bool, now: Instant) -> Self {
        Self {
            key: key.to_owned(),
            idle: activity.idle(now),
            busy,
            background: activity.background(),
        }
    }
}

/// The agents to stop now: idle past the limit, then the least recently used beyond the count.
pub(super) fn victims(mut agents: Vec<Candidate>) -> Vec<String> {
    let (idle_limit, max_live) = policy();
    victims_with(&mut agents, idle_limit, max_live)
}

fn victims_with(
    agents: &mut [Candidate],
    idle_limit: Option<Duration>,
    max_live: Option<usize>,
) -> Vec<String> {
    let stoppable = |a: &Candidate, after: Duration| {
        let after = if a.background {
            after.max(BACKGROUND_GRACE)
        } else {
            after
        };
        !a.busy && a.idle >= after
    };
    let mut out: Vec<String> = match idle_limit {
        Some(limit) => agents
            .iter()
            .filter(|a| stoppable(a, limit))
            .map(|a| a.key.clone())
            .collect(),
        None => Vec::new(),
    };
    if let Some(max) = max_live {
        let mut left = agents.len() - out.len();
        // Longest idle first.
        agents.sort_by_key(|a| std::cmp::Reverse(a.idle));
        for a in agents.iter() {
            if left <= max {
                break;
            }
            if !out.contains(&a.key) && stoppable(a, MIN_IDLE_FOR_LIMIT) {
                out.push(a.key.clone());
                left -= 1;
            }
        }
    }
    out
}

/// Agents currently running, by oxi session key, with their liveness flag.
static LIVE: Mutex<Option<HashMap<String, Arc<AtomicBool>>>> = Mutex::new(None);

pub(super) fn mark_live(key: &str, alive: Arc<AtomicBool>) {
    LIVE.lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashMap::new)
        .insert(key.to_owned(), alive);
}

pub(super) fn is_current(key: &str, alive: &Arc<AtomicBool>) -> bool {
    LIVE.lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|m| m.get(key))
        .is_none_or(|current| Arc::ptr_eq(current, alive))
}

pub(super) fn mark_stopped(key: &str) {
    if let Some(map) = LIVE.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        map.remove(key);
    }
}

/// Whether the chat's agent is running, so its next prompt answers without a launch.
pub fn is_live(session_key: &str) -> bool {
    LIVE.lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|map| map.get(session_key))
        .is_some_and(|alive| alive.load(Ordering::SeqCst))
}

/// How many agents are running.
pub fn live_count() -> usize {
    LIVE.lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map_or(0, |map| {
            map.values()
                .filter(|alive| alive.load(Ordering::SeqCst))
                .count()
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const MIN: Duration = Duration::from_secs(60);

    fn agent(key: &str, idle_min: u64) -> Candidate {
        Candidate {
            key: key.into(),
            idle: MIN * idle_min as u32,
            busy: false,
            background: false,
        }
    }

    #[test]
    fn stops_agents_idle_past_the_limit_but_never_busy_ones() {
        let mut agents = vec![agent("a", 20), agent("b", 5), agent("c", 30)];
        agents[2].busy = true;
        assert_eq!(victims_with(&mut agents, Some(MIN * 15), None), ["a"]);
        assert!(victims_with(&mut agents, None, None).is_empty());
    }

    #[test]
    fn background_work_gets_a_longer_grace() {
        let mut agents = vec![agent("a", 30), agent("b", 200)];
        agents[0].background = true;
        agents[1].background = true;
        assert_eq!(victims_with(&mut agents, Some(MIN * 15), None), ["b"]);
    }

    #[test]
    fn over_the_count_stops_the_least_recently_used() {
        let mut agents = vec![agent("a", 3), agent("b", 9), agent("c", 0), agent("d", 5)];
        // `c` was just used, so only two can go to get down to two.
        let mut out = victims_with(&mut agents, None, Some(2));
        out.sort();
        assert_eq!(out, ["b", "d"]);
        // Nothing is stopped twice, and the idle ones count towards the limit.
        let mut agents = vec![agent("a", 20), agent("b", 9), agent("c", 3)];
        let mut out = victims_with(&mut agents, Some(MIN * 15), Some(1));
        out.sort();
        assert_eq!(out, ["a", "b"]);
    }

    #[test]
    fn notices_tool_calls_that_outlive_the_turn() {
        let update = |u: Value| json!({"method": "session/update", "params": {"update": u}});
        let activity = Activity::new();
        activity.note(&update(
            json!({"sessionUpdate": "tool_call", "rawInput": {"command": "ls"}}),
        ));
        assert!(!activity.background.load(Ordering::Relaxed));
        activity.note(&update(json!({"sessionUpdate": "tool_call",
            "rawInput": {"command": "cargo build", "run_in_background": true}})));
        assert!(activity.background.load(Ordering::Relaxed));

        let activity = Activity::new();
        activity.note(&update(json!({"sessionUpdate": "tool_call_update",
            "_meta": {"claudeCode": {"toolName": "ScheduleWakeup"}}})));
        assert!(activity.background.load(Ordering::Relaxed));
    }
}
