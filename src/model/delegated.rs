//! Provider-owned conversations. They share the parent's connection and approval policy;
//! opening a transcript never launches another agent or gives it an independent prompt loop.
use super::{ChatMessage, MsgRole};
use crate::agent::{AgentEvent, acp::UpdateState};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DelegatedChats {
    pub chats: Vec<DelegatedChat>,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DelegatedChat {
    pub root: String,
    pub id: String,
    pub parent: String,
    pub title: String,
    pub description: String,
    pub state: String,
    pub outcome: String,
    pub model: String,
    pub capabilities: Value,
    pub task: bool,
    pub progress: Value,
    pub tool_call_id: Option<String>,
    #[serde(with = "transcript")]
    pub messages: Vec<ChatMessage>,
    pub message_ids: HashSet<String>,
    pub used: u64,
    pub window: u64,
    #[serde(skip)]
    decoder: UpdateState,
    #[serde(skip)]
    decoder_restored: bool,
    /// Replayed/persisted running states do not prove a process is still alive.
    #[serde(skip)]
    pub live: bool,
}

impl DelegatedChat {
    pub fn running(&self) -> bool {
        matches!(
            self.state.as_str(),
            "running" | "requires_action" | "paused"
        )
    }
    pub fn label(&self) -> &str {
        if self.title.is_empty() {
            &self.id
        } else {
            &self.title
        }
    }
    pub fn status(&self) -> &str {
        if !self.outcome.is_empty() {
            &self.outcome
        } else if self.state.is_empty() {
            "unknown"
        } else {
            &self.state
        }
    }
    fn reply(&mut self) -> &mut ChatMessage {
        if self
            .messages
            .last()
            .is_none_or(|m| m.role != MsgRole::Assistant || !m.streaming)
        {
            self.messages.push(ChatMessage::streaming_assistant(None));
        }
        self.messages.last_mut().unwrap()
    }
    pub fn finish(&mut self) {
        if let Some(m) = self
            .messages
            .last_mut()
            .filter(|m| m.role == MsgRole::Assistant)
        {
            m.finish_streaming();
        }
    }
    fn update(&mut self, update: &Value) {
        if !self.decoder_restored {
            self.decoder.restore_tools(&self.messages);
            self.decoder_restored = true;
        }
        if update["sessionUpdate"] == "workflow_transcript_reset" {
            self.messages.clear();
            self.message_ids.clear();
            self.decoder = Default::default();
            return;
        }
        if update["sessionUpdate"] == "workflow_transcript_settled" {
            if !self.running() {
                self.finish();
            }
            return;
        }
        if update["sessionUpdate"] == "workflow_entry" {
            self.workflow_entry(&update["entry"]);
            return;
        }
        let kind = update["sessionUpdate"].as_str().unwrap_or("");
        if kind == "session_message" || kind == "user_message_chunk" {
            // The parent's outgoing copy of a delegation is not another prompt to the child.
            if kind == "session_message" && update["recipientSessionId"].as_str() != Some(&self.id)
            {
                return;
            }
            if kind == "session_message"
                && let Some(id) = update["messageId"].as_str()
                && !self.message_ids.insert(id.to_string())
            {
                return;
            }
            let text = content_text(&update["content"]);
            if !text.is_empty() {
                self.finish();
                self.messages.push(ChatMessage::user(&text, &[]));
            }
            return;
        }
        if kind == "config_option_update" {
            if let Some(opts) = update["configOptions"].as_array() {
                for opt in opts {
                    if (opt["category"] == "model" || opt["id"] == "model")
                        && let Some(model) = opt["currentValue"].as_str()
                    {
                        self.model = model.into();
                    }
                }
            }
            return;
        }
        if kind == "state_update" {
            self.apply_state(update);
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        self.decoder.emit_update(update, &tx);
        for event in rx.try_iter() {
            match event {
                AgentEvent::TextDelta(s) => self.reply().append_answer(&s),
                AgentEvent::ThinkingDelta(s) => self.reply().append_thinking(&s),
                AgentEvent::ToolUpdate(t) => {
                    if let Some(m) = self.messages.iter_mut().rev().find(|m| m.blocks.iter().any(|b| matches!(b,
                        super::AssistantBlock::Tool {tool_call_id,..} if tool_call_id == &t.tool_call_id))) {
                        super::apply_tool_update(&mut m.blocks,*t);
                    } else { super::apply_tool_update(&mut self.reply().blocks,*t); }
                },
                AgentEvent::ContextUsage { used, size } => {
                    self.used = used;
                    self.window = size;
                }
                _ => {}
            }
        }
    }
    fn workflow_entry(&mut self, entry: &Value) {
        if entry["isMeta"] == true || !matches!(entry["type"].as_str(), Some("user" | "assistant"))
        {
            return;
        }
        if let Some(id) = entry["uuid"].as_str()
            && !self.message_ids.insert(id.into())
        {
            return;
        }
        let m = &entry["message"];
        if let Some(model) = m["model"].as_str() {
            self.model = model.into();
        }
        if let Some(used) = m["usage"]["input_tokens"].as_u64() {
            self.used = used;
        }
        let user = entry["type"] == "user";
        if let Some(text) = m["content"].as_str() {
            if user {
                self.finish();
                self.messages.push(ChatMessage::user(text, &[]));
            } else {
                self.reply().append_answer(text);
            }
        }
        for block in m["content"].as_array().into_iter().flatten() {
            match block["type"].as_str() {
                Some("text") => {
                    let text = block["text"].as_str().unwrap_or("");
                    if user { self.finish(); self.messages.push(ChatMessage::user(text,&[])); }
                    else { self.reply().append_answer(text); }
                }
                Some("thinking") => self.reply().append_thinking(block["thinking"].as_str().unwrap_or("")),
                Some("tool_use") => self.update(&serde_json::json!({
                    "sessionUpdate":"tool_call", "toolCallId":block["id"], "name":block["name"],
                    "title":block["name"], "kind":"other", "status":"in_progress", "rawInput":block["input"]
                })),
                Some("tool_result") => self.update(&serde_json::json!({
                    "sessionUpdate":"tool_call_update", "toolCallId":block["tool_use_id"],
                    "status":if block["is_error"]==true {"failed"} else {"completed"},
                    "rawOutput":block["content"]
                })),
                _ => {}
            }
        }
    }
    fn apply_state(&mut self, value: &Value) {
        let state = if value["state"].is_object() {
            &value["state"]
        } else {
            value
        };
        if let Some(s) = state["state"].as_str() {
            self.state = s.into();
            if self.running() {
                self.outcome.clear();
            }
        }
        if let Some(s) = state["outcome"].as_str() {
            self.outcome = s.into();
        }
        if state["error"].is_string() {
            self.outcome = "failed".into();
            self.description = state["error"].as_str().unwrap().into();
        }
        if !self.running() {
            self.finish();
        }
    }
}

impl DelegatedChats {
    /// Apply only announcements or notifications belonging to an already announced child.
    /// Returns false for ordinary root-session updates.
    pub fn apply(&mut self, root: &str, params: &Value) -> bool {
        let u = &params["update"];
        if u["sessionUpdate"] == "delegation_disconnected" {
            self.disconnected(root);
            return true;
        }
        if u["sessionUpdate"] == "delegation_replay" {
            let replayed = u["sessions"].as_array();
            self.chats.retain(|c| {
                c.root != root
                    || !replayed.is_some_and(|ids| ids.iter().any(|id| id.as_str() == Some(&c.id)))
            });
            return true;
        }
        let kind = u["sessionUpdate"]
            .as_str()
            .unwrap_or("")
            .trim_start_matches('_');
        let parent = params["sessionId"].as_str().unwrap_or(root);
        let announcement = matches!(
            kind,
            "subagent_update" | "subagent_spawned" | "subagent_state_update"
        );
        let task = kind.starts_with("async_task_");
        let id = if task {
            u["asyncTaskId"].as_str()
        } else if announcement {
            u["sessionId"]
                .as_str()
                .or_else(|| u["subagentSessionId"].as_str())
        } else {
            Some(parent)
        };
        let Some(id) = id.filter(|id| !id.is_empty() && *id != root) else {
            return false;
        };
        let index = self.chats.iter().position(|c| c.root == root && c.id == id);
        let index = match index {
            Some(i) => i,
            None if announcement || task => {
                self.chats.push(DelegatedChat {
                    root: root.into(),
                    id: id.into(),
                    parent: parent.into(),
                    task,
                    ..Default::default()
                });
                self.chats.len() - 1
            }
            _ => return false,
        };
        let c = &mut self.chats[index];
        c.live = true;
        if announcement || task {
            if kind == "subagent_spawned" && c.state.is_empty() {
                c.state = "running".into();
            }
            for (key, target) in [("title", &mut c.title), ("description", &mut c.description)] {
                if let Some(s) = u[key].as_str() {
                    *target = s.into();
                }
            }
            if let Some(s) = u["name"].as_str() {
                c.title = s.into();
            }
            if let Some(s) = u["task"].as_str() {
                c.description = s.into();
            }
            if let Some(s) = u["model"].as_str() {
                c.model = s.into();
            }
            if u.get("usage").is_some() {
                c.progress = merge(&c.progress, u);
            }
            if let Some(s) = u["toolCallId"].as_str() {
                c.tool_call_id = Some(s.into());
            }
            if let Some(caps) = u["capabilities"].as_object() {
                if !c.capabilities.is_object() {
                    c.capabilities = serde_json::json!({});
                }
                for (k, v) in caps {
                    c.capabilities[k] = v.clone();
                }
            }
            if task {
                c.progress = merge(&c.progress, u);
                if let Some(can_stop) = u["canStop"].as_bool() {
                    if !c.capabilities.is_object() {
                        c.capabilities = serde_json::json!({});
                    }
                    c.capabilities["cancel"] = if can_stop {
                        serde_json::json!({})
                    } else {
                        Value::Null
                    };
                }
                if kind == "async_task_spawned" && c.state.is_empty() {
                    c.state = "running".into();
                }
            }
            if u["state"].is_string() {
                c.state = u["state"].as_str().unwrap().into();
                if !c.running() {
                    c.finish();
                }
            } else if u["state"].is_object() {
                c.apply_state(u);
            }
            if let Some(prompt) = u["prompt"].as_str().filter(|s| !s.is_empty())
                && c.messages.is_empty()
            {
                c.messages.push(ChatMessage::user(prompt, &[]));
            }
        } else {
            c.update(u);
        }
        true
    }
    fn disconnected(&mut self, root: &str) {
        for c in self.chats.iter_mut().filter(|c| c.root == root) {
            c.live = false;
            c.finish();
        }
    }
}
fn merge(a: &Value, b: &Value) -> Value {
    let mut out = a.as_object().cloned().unwrap_or_default();
    if let Some(b) = b.as_object() {
        out.extend(b.clone());
    }
    Value::Object(out)
}
pub fn content_text(v: &Value) -> String {
    if let Some(a) = v.as_array() {
        return a
            .iter()
            .map(content_text)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
    }
    v["text"].as_str().unwrap_or("").to_string()
}
mod transcript {
    use super::*;
    pub fn serialize<S: serde::Serializer>(
        messages: &[ChatMessage],
        s: S,
    ) -> Result<S::Ok, S::Error> {
        crate::remote::protocol::messages_to_json(messages).serialize(s)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        d: D,
    ) -> Result<Vec<ChatMessage>, D::Error> {
        Ok(crate::remote::protocol::messages_from_json(
            &Value::deserialize(d)?,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn apply(chats: &mut DelegatedChats, session: &str, update: Value) {
        assert!(chats.apply("root", &json!({"sessionId":session,"update":update})));
    }
    #[test]
    fn delegated_transcripts_isolate_tools_and_keep_nested_messages() {
        let mut chats = DelegatedChats::default();
        for (parent, id) in [("root", "reader"), ("reader", "reviewer")] {
            apply(
                &mut chats,
                parent,
                json!({"sessionUpdate":"subagent_update","sessionId":id,
                "title":id,"capabilities":{"cancel":{}},"state":{"state":"running"}}),
            );
            apply(
                &mut chats,
                id,
                json!({"sessionUpdate":"session_message","messageId":"prompt",
                "recipientSessionId":id,"content":[{"type":"text","text":"Find the bug"}]}),
            );
            apply(
                &mut chats,
                id,
                json!({"sessionUpdate":"tool_call","toolCallId":"same-id","kind":"read",
                "title":"Read","rawInput":{"path":id},"status":"in_progress"}),
            );
        }
        apply(
            &mut chats,
            "reader",
            json!({"sessionUpdate":"tool_call_update","toolCallId":"same-id","status":"completed","rawOutput":"reader-output"}),
        );
        apply(
            &mut chats,
            "reviewer",
            json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Found a bug."}}),
        );
        apply(
            &mut chats,
            "reviewer",
            json!({"sessionUpdate":"usage_update","used":420,"size":1000}),
        );
        apply(
            &mut chats,
            "reader",
            json!({"sessionUpdate":"subagent_update","sessionId":"reviewer","state":{"state":"idle","outcome":"completed"}}),
        );
        assert_eq!(chats.chats[1].parent, "reader");
        assert_eq!(chats.chats[1].messages[0].text, "Find the bug");
        assert!(!chats.chats[1].messages.last().unwrap().streaming);
        let serialized = serde_json::to_string(&chats).unwrap();
        let mut restored: DelegatedChats = serde_json::from_str(&serialized).unwrap();
        apply(
            &mut restored,
            "reader",
            json!({"sessionUpdate":"tool_call_update","toolCallId":"same-id","status":"completed"}),
        );
        assert!(
            crate::remote::protocol::messages_to_json(&restored.chats[0].messages)
                .to_string()
                .contains("reader-output")
        );
        assert!(!restored.chats[1].live);
        assert_eq!(restored.chats[1].used, 420);
        assert!(
            serde_json::to_string(&restored)
                .unwrap()
                .contains("reader-output")
        );
        assert!(
            !crate::remote::protocol::messages_to_json(&restored.chats[1].messages)
                .to_string()
                .contains("reader-output")
        );
        apply(
            &mut chats,
            "reader",
            json!({"sessionUpdate":"subagent_update","sessionId":"reviewer","capabilities":{"cancel":null},"state":{"state":"running"}}),
        );
        assert!(!chats.chats[1].capabilities["cancel"].is_object());
        assert_eq!(chats.chats[1].status(), "running");
    }
    #[test]
    fn air_tasks_and_workflow_entries_survive_roundtrip() {
        let mut chats = DelegatedChats::default();
        apply(
            &mut chats,
            "root",
            json!({"sessionUpdate":"async_task_spawned","asyncTaskId":"wf","name":"Review","canStop":true,"taskType":"workflow"}),
        );
        apply(
            &mut chats,
            "wf",
            json!({"sessionUpdate":"subagent_spawned","subagentSessionId":"member","name":"Auditor","prompt":"Review this","capabilities":{}}),
        );
        for entry in [
            json!({"type":"assistant","uuid":"a","message":{"role":"assistant","content":[{"type":"tool_use","id":"read1","name":"Read","input":{"path":"src/lib.rs"}}]}}),
            json!({"type":"user","uuid":"b","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"read1","content":"file content"}]}}),
            json!({"type":"assistant","uuid":"c","message":{"role":"assistant","model":"opus","content":[{"type":"text","text":"No issues."}]}}),
        ] {
            apply(
                &mut chats,
                "member",
                json!({"sessionUpdate":"workflow_entry","entry":entry}),
            );
        }
        apply(
            &mut chats,
            "wf",
            json!({"sessionUpdate":"subagent_state_update","subagentSessionId":"member","state":"completed"}),
        );
        assert_eq!(chats.chats[1].model, "opus");
        assert_eq!(chats.chats[1].messages.len(), 2);
        assert!(
            crate::remote::protocol::messages_to_json(&chats.chats[1].messages)
                .to_string()
                .contains("file content")
        );
        let mut restored: DelegatedChats =
            serde_json::from_value(serde_json::to_value(&chats).unwrap()).unwrap();
        assert!(!restored.chats[0].live);
        apply(
            &mut restored,
            "member",
            json!({"sessionUpdate":"workflow_transcript_reset"}),
        );
        assert!(restored.chats[1].messages.is_empty());
    }
    #[test]
    fn session_messages_are_deduplicated_and_outgoing_copies_ignored() {
        let mut chats = DelegatedChats::default();
        apply(
            &mut chats,
            "root",
            json!({"sessionUpdate":"subagent_update","sessionId":"c"}),
        );
        let prompt = json!({"sessionUpdate":"session_message","messageId":"p","recipientSessionId":"c","senderSessionId":"root","content":[{"type":"text","text":"Task"}]});
        apply(&mut chats, "c", prompt.clone());
        apply(&mut chats, "c", prompt);
        assert_eq!(chats.chats[0].messages.len(), 1);
        assert!(!chats.apply("root",&json!({"sessionId":"foreign","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Ignore"}}})));
        apply(
            &mut chats,
            "c",
            json!({"sessionUpdate":"session_message","recipientSessionId":"root","messageId":"report","content":[{"type":"text","text":"Done"}]}),
        );
        assert_eq!(chats.chats[0].messages.len(), 1);
    }
    #[test]
    fn delegated_session_file_preserves_children_without_changing_parent_history() {
        let dir =
            crate::fsutil::unique_temp_path(&std::env::temp_dir(), "oxi-delegated-store", "d");
        std::fs::create_dir_all(&dir).unwrap();
        let mut session = crate::app::OxiApp::blank_session("Parent");
        session.messages.push(ChatMessage::user("Parent task", &[]));
        apply(
            &mut session.delegated,
            "root",
            json!({"sessionUpdate":"subagent_update","sessionId":"child","title":"Child"}),
        );
        apply(
            &mut session.delegated,
            "child",
            json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Child result"}}),
        );
        crate::session_store::save_session_messages(dir.to_str().unwrap(), &mut session).unwrap();
        let restored =
            crate::session_store::load_session(session.session_file.as_deref().unwrap()).unwrap();
        assert_eq!(restored.messages.len(), 1);
        assert_eq!(restored.delegated.chats.len(), 1);
        assert!(
            crate::remote::protocol::messages_to_json(&restored.delegated.chats[0].messages)
                .to_string()
                .contains("Child result")
        );
        assert!(!restored.delegated.chats[0].live);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
