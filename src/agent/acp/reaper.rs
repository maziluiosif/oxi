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

/// A non-daemon command/tool that was still running when its ACP turn ended cleanly (e.g. an
/// Antigravity `run_command` that crossed `WaitMsBeforeAsync` and moved to a background task).
#[derive(Clone, Debug)]
pub struct BackgroundTool {
    pub tool_call_id: String,
    pub label: String,
    pub started_at: Instant,
}

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
const BACKGROUND_TOOLS: &[&str] = &[
    "ScheduleWakeup",
    "Monitor",
    "CronCreate",
    "schedule",
    "run_workflow",
    "invoke_subagent",
    "swarm",
];

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
    /// Non-daemon commands that were still running when a turn ended cleanly.
    background_tools: Mutex<Vec<BackgroundTool>>,
}

impl Activity {
    pub(super) fn new() -> Self {
        Self {
            last: Mutex::new(Instant::now()),
            background: AtomicBool::new(false),
            background_tools: Mutex::new(Vec::new()),
        }
    }

    pub(super) fn touch(&self) {
        *self.last.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
    }

    /// How long ago the agent last sent or was sent anything.
    pub(super) fn quiet_for(&self) -> Duration {
        self.idle(Instant::now())
    }

    /// A tracked background command ended. Returns whether it was tracked.
    pub(super) fn finish_background_tool(&self, tool_call_id: &str) -> bool {
        let mut tools = self
            .background_tools
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let before = tools.len();
        tools.retain(|t| t.tool_call_id != tool_call_id);
        tools.len() != before
    }

    fn idle(&self, now: Instant) -> Duration {
        now.saturating_duration_since(*self.last.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// Whether the agent started work that keeps running after its turns.
    pub(super) fn background(&self) -> bool {
        self.background.load(Ordering::Relaxed) || self.has_background_tools()
    }

    /// Whether any finite background commands from an earlier turn are still running.
    pub(super) fn has_background_tools(&self) -> bool {
        let mut tools = self
            .background_tools
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        tools.retain(|t| t.started_at.elapsed() < BACKGROUND_GRACE);
        !tools.is_empty()
    }

    /// Snapshot active background commands, oldest first.
    pub(super) fn snapshot_background_tools(&self) -> Vec<BackgroundTool> {
        let mut tools = self
            .background_tools
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        tools.retain(|t| t.started_at.elapsed() < BACKGROUND_GRACE);
        tools.clone()
    }

    /// Record finite commands that were still running when a turn ended cleanly.
    pub(super) fn add_background_tools(&self, incoming: Vec<BackgroundTool>) {
        if incoming.is_empty() {
            return;
        }
        self.background.store(true, Ordering::Relaxed);
        let mut tools = self
            .background_tools
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for tool in incoming {
            if let Some(existing) = tools
                .iter_mut()
                .find(|t| t.tool_call_id == tool.tool_call_id)
            {
                if existing.label.is_empty() && !tool.label.is_empty() {
                    existing.label = tool.label;
                }
            } else {
                tools.push(tool);
            }
        }
    }

    /// Record a message from the agent: it is in use, and it may have started or finished
    /// background work. Returns `true` when a tracked background command finished.
    pub(super) fn note(&self, msg: &Value) -> bool {
        self.touch();
        if msg["method"].as_str() != Some("session/update") {
            return false;
        }
        let update = &msg["params"]["update"];
        if starts_background(update) {
            self.background.store(true, Ordering::Relaxed);
        }
        if matches!(
            update["sessionUpdate"].as_str(),
            Some("tool_call" | "tool_call_update")
        ) && let Some(id) = update["toolCallId"].as_str().filter(|id| !id.is_empty())
        {
            let mut tools = self
                .background_tools
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(pos) = tools.iter().position(|t| t.tool_call_id == id) {
                if matches!(
                    update["status"].as_str(),
                    Some("completed" | "failed" | "cancelled")
                ) {
                    tools.remove(pos);
                    return true;
                }
                if tools[pos].label.is_empty()
                    && let Some(title) = update["title"].as_str().filter(|s| !s.trim().is_empty())
                {
                    tools[pos].label = title.trim().to_owned();
                }
            }
        }
        false
    }
}

/// Whether `name` is a standing/delegated background tool (timers, monitors, subagents, workflows).
pub(super) fn is_standing_background_tool(name: &str) -> bool {
    BACKGROUND_TOOLS.contains(&name)
}

/// Whether a tool call is a finite command that can transition into a background task mid-turn
/// (e.g. Antigravity `run_command` with `WaitMsBeforeAsync`), as opposed to a daemon or standing
/// timer.
pub(super) fn is_async_backgroundable(update: &Value) -> bool {
    if !matches!(
        update["sessionUpdate"].as_str(),
        Some("tool_call" | "tool_call_update")
    ) {
        return false;
    }
    let input = match &update["rawInput"] {
        Value::Null => &update["args"],
        input => input,
    };
    if input["IsDaemon"].as_bool() == Some(true) {
        return false;
    }
    let tool_name = update["_meta"]["claudeCode"]["toolName"]
        .as_str()
        .or_else(|| update["name"].as_str());
    if tool_name.is_some_and(is_standing_background_tool) {
        return false;
    }
    input.get("WaitMsBeforeAsync").is_some_and(|v| !v.is_null())
        || input
            .get("NotificationTimeoutSeconds")
            .is_some_and(|v| !v.is_null())
        || input["run_in_background"].as_bool() == Some(true)
}

/// Whether a tool call starts work that keeps running after the turn.
pub(super) fn starts_background(update: &Value) -> bool {
    // Antigravity sends a tool's arguments as `args` instead of `rawInput`.
    let input = match &update["rawInput"] {
        Value::Null => &update["args"],
        input => input,
    };
    matches!(
        update["sessionUpdate"].as_str(),
        Some("tool_call" | "tool_call_update")
    ) && (input["run_in_background"].as_bool() == Some(true)
        || input["IsDaemon"].as_bool() == Some(true)
        || update["_meta"]["claudeCode"]["toolName"]
            .as_str()
            .or_else(|| update["name"].as_str())
            .is_some_and(is_standing_background_tool))
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

type LiveEntry = (Arc<AtomicBool>, Arc<Activity>);

/// Agents currently running, by oxi session key, with their liveness flag and activity tracker.
static LIVE: Mutex<Option<HashMap<String, LiveEntry>>> = Mutex::new(None);

pub(super) fn mark_live(key: &str, alive: Arc<AtomicBool>, activity: Arc<Activity>) {
    LIVE.lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashMap::new)
        .insert(key.to_owned(), (alive, activity));
}

pub(super) fn is_current(key: &str, alive: &Arc<AtomicBool>) -> bool {
    LIVE.lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|m| m.get(key))
        .is_none_or(|current| Arc::ptr_eq(&current.0, alive))
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
        .is_some_and(|(alive, _)| alive.load(Ordering::SeqCst))
}

/// Finite background commands still running in `session_key`'s live ACP subprocess.
pub fn background_tools(session_key: &str) -> Vec<BackgroundTool> {
    let activity = LIVE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|map| map.get(session_key))
        .filter(|(alive, _)| alive.load(Ordering::SeqCst))
        .map(|(_, activity)| activity.clone());
    activity
        .map(|a| a.snapshot_background_tools())
        .unwrap_or_default()
}

/// Whether `session_key`'s live ACP subprocess is waiting on a finite background command.
pub fn has_background_tools(session_key: &str) -> bool {
    let activity = LIVE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|map| map.get(session_key))
        .filter(|(alive, _)| alive.load(Ordering::SeqCst))
        .map(|(_, activity)| activity.clone());
    activity.is_some_and(|a| a.has_background_tools())
}

/// Whether any live ACP subprocess is waiting on a finite background command.
pub fn any_background_tools() -> bool {
    let activities: Vec<Arc<Activity>> = LIVE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|map| {
            map.values()
                .filter(|(alive, _)| alive.load(Ordering::SeqCst))
                .map(|(_, activity)| activity.clone())
                .collect()
        })
        .unwrap_or_default();
    activities.into_iter().any(|a| a.has_background_tools())
}

/// How many agents are running.
pub fn live_count() -> usize {
    LIVE.lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map_or(0, |map| {
            map.values()
                .filter(|(alive, _)| alive.load(Ordering::SeqCst))
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

        let activity = Activity::new();
        activity.note(&update(
            json!({"sessionUpdate": "tool_call", "name": "run_command",
            "rawInput": {"CommandLine": "npm run dev", "IsDaemon": true}}),
        ));
        assert!(activity.background.load(Ordering::Relaxed));

        let activity = Activity::new();
        activity.note(&update(
            json!({"sessionUpdate": "tool_call", "name": "schedule"}),
        ));
        assert!(activity.background.load(Ordering::Relaxed));

        let activity = Activity::new();
        activity.note(&update(
            json!({"sessionUpdate": "tool_call", "name": "run_command",
            "args": {"CommandLine": "npm run dev", "IsDaemon": true}}),
        ));
        assert!(activity.background.load(Ordering::Relaxed));
    }

    #[test]
    fn tracks_and_clears_finite_background_commands() {
        let update = |u: Value| json!({"method": "session/update", "params": {"update": u}});
        let activity = Arc::new(Activity::new());
        let alive = Arc::new(AtomicBool::new(true));
        let key = format!("bg-tool-test-{}", rand::random::<u64>());
        mark_live(&key, alive.clone(), activity.clone());

        assert!(!has_background_tools(&key));
        assert!(!activity.background());

        activity.add_background_tools(vec![BackgroundTool {
            tool_call_id: "call_1".into(),
            label: "Watch CI checks".into(),
            started_at: Instant::now(),
        }]);
        assert!(has_background_tools(&key));
        assert!(activity.background());
        assert_eq!(background_tools(&key).len(), 1);
        assert_eq!(background_tools(&key)[0].label, "Watch CI checks");

        // Intermediate progress update keeps the command active.
        assert!(!activity.note(&update(json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call_1",
            "status": "in_progress"
        }))));
        assert!(has_background_tools(&key));

        // Terminal update clears it and signals wakeup.
        assert!(activity.note(&update(json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call_1",
            "status": "completed"
        }))));
        assert!(!has_background_tools(&key));

        // Disconnecting hides any leftover background tools immediately.
        activity.add_background_tools(vec![BackgroundTool {
            tool_call_id: "call_2".into(),
            label: "cargo test".into(),
            started_at: Instant::now(),
        }]);
        assert!(has_background_tools(&key));
        alive.store(false, Ordering::SeqCst);
        assert!(!has_background_tools(&key));
        mark_stopped(&key);
    }

    #[test]
    fn distinguishes_async_commands_from_daemons_and_foreground_tools() {
        assert!(is_async_backgroundable(&json!({
            "sessionUpdate": "tool_call",
            "name": "run_command",
            "args": {"CommandLine": "gh pr checks --watch", "WaitMsBeforeAsync": 5000}
        })));
        assert!(!is_async_backgroundable(&json!({
            "sessionUpdate": "tool_call",
            "name": "run_command",
            "args": {"CommandLine": "npm run dev", "IsDaemon": true, "WaitMsBeforeAsync": 5000}
        })));
        assert!(!is_async_backgroundable(&json!({
            "sessionUpdate": "tool_call",
            "name": "read_file",
            "args": {"Path": "/repo/src/lib.rs"}
        })));
    }
}
