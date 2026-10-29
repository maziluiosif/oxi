//! Claude Workflow agents are not ACP child sessions: the CLI writes them to its workflow
//! journal/transcripts. Read only the current native session's directory, on the machine
//! running the provider. Never execute a workflow script or follow paths from its output.
use super::ConnHandles;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const MAX_READ: u64 = 16 * 1024 * 1024;
#[derive(Default)]
struct Cursor {
    offset: u64,
}
#[derive(Default)]
struct Reader {
    summaries: HashMap<PathBuf, (SystemTime, u64)>,
    cursors: HashMap<PathBuf, Cursor>,
    agents: HashMap<String, Value>,
}
fn safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 160
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
}
fn contained(root: &Path, file: &Path) -> Option<PathBuf> {
    let file = file.canonicalize().ok()?;
    file.starts_with(root).then_some(file)
}
impl Reader {
    fn poll(&mut self, dir: &Path, root_id: &str) -> Vec<Value> {
        let mut out = Vec::new();
        let Some(base) = dir.canonicalize().ok() else {
            return out;
        };
        let summaries = base.join("workflows");
        let Ok(entries) = std::fs::read_dir(&summaries) else {
            return out;
        };
        for entry in entries.filter_map(Result::ok).take(256) {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let Some(path) = contained(&base, &path) else {
                continue;
            };
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            if meta.len() > MAX_READ {
                continue;
            }
            let stamp = (
                meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                meta.len(),
            );
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
                continue;
            };
            let Some(run) = value["runId"].as_str().filter(|s| safe_id(s)) else {
                continue;
            };
            let id = value["taskId"]
                .as_str()
                .filter(|s| safe_id(s))
                .unwrap_or(run);
            if self.summaries.get(&path) != Some(&stamp) {
                out.push(json!({"sessionId":root_id,"update": {
                    "sessionUpdate":"async_task_progress", "asyncTaskId":id, "taskType":"workflow",
                    "name":value["workflowName"], "description":value["summary"],
                    "state":value["status"], "phases":value["phases"], "workflowProgress":value["workflowProgress"]
                }}));
                self.summaries.insert(path, stamp);
            }
            for agent in value["workflowProgress"].as_array().into_iter().flatten() {
                let Some(agent_id) = agent["agentId"].as_str().filter(|s| safe_id(s)) else {
                    continue;
                };
                let child = format!("workflow/{run}/{agent_id}");
                // Metadata may be updated before the transcript, and vice versa.
                let announcement = json!({"sessionId":id,"update": {"sessionUpdate":"subagent_update",
                    "sessionId":child, "title":agent["label"], "description":agent["phaseTitle"],
                    "state": {"state":match agent["state"].as_str() {
                        Some("running" | "queued") => "running", _ => "idle"
                    }, "outcome":match agent["state"].as_str() {
                        Some("error" | "failed") => "failed", Some("completed" | "done") => "completed",
                        Some("cancelled" | "stopped") => "cancelled", _ => ""
                    }}, "model":agent["model"],"usage": {"tokens":agent["tokens"],"toolCalls":agent["toolCalls"]},
                    "lastToolName":agent["lastToolName"],"summary":agent["error"]
                }});
                if self.agents.get(&child) != Some(&announcement) {
                    self.agents.insert(child.clone(), announcement.clone());
                    out.push(announcement);
                }
                let file = base
                    .join("subagents")
                    .join("workflows")
                    .join(run)
                    .join(format!("agent-{agent_id}.jsonl"));
                let Some(file) = contained(&base, &file) else {
                    continue;
                };
                let cursor = self.cursors.entry(file.clone()).or_default();
                let Ok(mut input) = std::fs::File::open(&file) else {
                    continue;
                };
                if input.metadata().is_ok_and(|m| m.len() < cursor.offset) {
                    cursor.offset = 0;
                }
                if input.seek(SeekFrom::Start(cursor.offset)).is_err() {
                    continue;
                }
                let mut bytes = Vec::new();
                if input.take(MAX_READ).read_to_end(&mut bytes).is_err() {
                    continue;
                }
                let Some(end) = bytes.iter().rposition(|b| *b == b'\n') else {
                    continue;
                };
                // Re-read from the start (a new reader, or a rewritten file): replace the saved
                // transcript. Only once a complete line is there, so a member whose first
                // record is still being written does not reset its chat on every poll.
                if cursor.offset == 0 {
                    out.push(json!({"sessionId":child,"update":{"sessionUpdate":"workflow_transcript_reset"}}));
                }
                for line in bytes[..=end].split(|b| *b == b'\n') {
                    if let Ok(entry) = serde_json::from_slice::<Value>(line) {
                        out.push(json!({"sessionId":child,"update":{"sessionUpdate":"workflow_entry","entry":entry}}));
                    }
                }
                cursor.offset += (end + 1) as u64;
                if std::fs::metadata(&file).is_ok_and(|m| cursor.offset >= m.len()) {
                    out.push(json!({"sessionId":child,"update":{"sessionUpdate":"workflow_transcript_settled"}}));
                }
            }
        }
        out
    }
}

pub(super) fn watch(handles: &ConnHandles, cwd: &Path, command: &str) {
    if !command.to_ascii_lowercase().contains("claude") || !safe_id(&handles.session_id) {
        return;
    }
    let Some(home) = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|p| p.join(".claude")))
    else {
        return;
    };
    let slug: String = cwd
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let dir = home.join("projects").join(slug).join(&handles.session_id);
    let handles = handles.clone();
    tokio::spawn(async move {
        let mut reader = Reader::default();
        while handles.alive.load(std::sync::atomic::Ordering::SeqCst) {
            // File reads and JSON parsing stay off the RPC reader and UI threads.
            let root = handles.session_id.clone();
            let dir = dir.clone();
            let read = tokio::task::spawn_blocking(move || {
                let updates = reader.poll(&dir, &root);
                (reader, updates)
            })
            .await;
            let Ok((next, updates)) = read else {
                return;
            };
            reader = next;
            for params in updates {
                handles.held.note_child(&params);
                let mut ctx = handles.prompt_ctx.lock().await;
                if let Some(ctx) = ctx.as_mut() {
                    ctx.emit_notification(&params);
                } else {
                    handles.held.hold(&params);
                }
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });
}

#[cfg(test)]
pub(crate) fn saved_workflow_snapshot(
    dir: &Path,
    root: &str,
) -> crate::model::delegated::DelegatedChats {
    let mut reader = Reader::default();
    let mut chats = crate::model::delegated::DelegatedChats::default();
    for _ in 0..16 {
        let updates = reader.poll(dir, root);
        if updates.is_empty() {
            break;
        }
        for params in updates {
            chats.apply(root, &params);
        }
    }
    // Reading an archive is not evidence of current live processes.
    serde_json::from_value(serde_json::to_value(chats).unwrap()).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn workflow_reader_follows_complete_lines_and_preserves_member_failures() {
        let dir =
            crate::fsutil::unique_temp_path(&std::env::temp_dir(), "oxi-workflow-reader", "d");
        std::fs::create_dir_all(dir.join("workflows")).unwrap();
        let members = dir.join("subagents/workflows/wf-test");
        std::fs::create_dir_all(&members).unwrap();
        std::fs::write(dir.join("workflows/wf-test.json"),json!({"runId":"wf-test","taskId":"task1","status":"completed","workflowName":"Review","phases":[{"title":"Inspect"}],
            "workflowProgress":[{"agentId":"a1","label":"Audit","model":"opus","state":"error","error":"quota"},{"agentId":"a2","label":"Docs","state":"running"}]}).to_string()).unwrap();
        let file = members.join("agent-a1.jsonl");
        let first = json!({"type":"user","uuid":"u","message":{"role":"user","content":"Task"}})
            .to_string();
        std::fs::write(&file, format!("{first}\n{{\"type\":")).unwrap();
        let mut reader = Reader::default();
        let pending = members.join("agent-a2.jsonl");
        std::fs::write(&pending, "{\"type\":").unwrap();
        let first = reader.poll(&dir, "root");
        assert_eq!(
            first
                .iter()
                .filter(|v| v["update"]["sessionUpdate"] == "workflow_entry")
                .count(),
            1
        );
        let mut chats = crate::model::delegated::DelegatedChats::default();
        for value in &first {
            chats.apply("root", value);
        }
        assert_eq!(chats.chats[1].status(), "failed");
        assert_eq!(chats.chats[1].messages[0].text, "Task");
        // Nothing new, and a member file without one complete record yet: no updates at all.
        assert!(reader.poll(&dir, "root").is_empty());
        assert!(
            !first
                .iter()
                .any(|v| v["sessionId"] == "workflow/wf-test/a2")
        );
        use std::io::Write;
        writeln!(std::fs::OpenOptions::new().append(true).open(&file).unwrap(),"\"assistant\",\"uuid\":\"a\",\"message\":{{\"role\":\"assistant\",\"content\":\"Result\"}}}}").unwrap();
        let next = reader.poll(&dir, "root");
        assert_eq!(
            next.iter()
                .filter(|v| v["update"]["sessionUpdate"] == "workflow_entry")
                .count(),
            1
        );
        for value in &next {
            chats.apply("root", value);
        }
        assert!(
            crate::remote::protocol::messages_to_json(&chats.chats[1].messages)
                .to_string()
                .contains("Result")
        );
        assert!(!chats.chats[1].messages.last().unwrap().streaming);
        assert!(!safe_id("../outside"));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(std::env::temp_dir(), dir.join("escape")).unwrap();
            assert!(contained(&dir.canonicalize().unwrap(), &dir.join("escape")).is_none());
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
