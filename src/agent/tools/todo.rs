//! `todo_write`: the agent's running checklist for multi-step work. The tool only validates and
//! echoes the list; the UI reads the latest call's arguments to show progress above the composer.

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoItem {
    pub content: String,
    pub status: TodoStatus,
}

/// Parse the `todos` array of a `todo_write` call. `None` when the arguments are malformed.
pub fn parse_todos(args: &Value) -> Option<Vec<TodoItem>> {
    args.get("todos")?
        .as_array()?
        .iter()
        .map(|t| {
            let content = t.get("content")?.as_str()?.trim();
            if content.is_empty() {
                return None;
            }
            let status = match t
                .get("status")
                .and_then(|s| s.as_str())
                .unwrap_or("pending")
            {
                "pending" => TodoStatus::Pending,
                "in_progress" => TodoStatus::InProgress,
                "completed" => TodoStatus::Completed,
                _ => return None,
            };
            Some(TodoItem {
                content: content.to_string(),
                status,
            })
        })
        .collect()
}

/// The `todo_write` function definition (name, description, JSON schema) shared by oxi's own
/// agent and the MCP server ACP agents get.
pub(crate) fn todo_write_definition() -> Value {
    serde_json::json!({
        "name": "todo_write",
        "description": "Create or update your checklist for the current task; the user sees it live. Send the complete list every time (it replaces the previous one). Use it for work with 3+ steps: write all steps up front, keep exactly one item in_progress, and mark items completed as soon as they are done. Skip it for trivial one-step requests.",
        "parameters": {
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "content": { "type": "string", "description": "Short imperative step, e.g. \"Fix the parser bug\"" },
                            "status": { "type": "string", "enum": ["pending", "in_progress", "completed"] }
                        },
                        "required": ["content", "status"]
                    }
                }
            },
            "required": ["todos"]
        }
    })
}

pub(crate) fn tool_todo_write(args: &Value) -> Result<String, String> {
    let todos = parse_todos(args).ok_or_else(|| {
        "`todos` must be an array of { content: non-empty string, status: pending | in_progress | completed }"
            .to_string()
    })?;
    if todos.is_empty() {
        return Ok("Todo list cleared.".into());
    }
    let done = todos
        .iter()
        .filter(|t| t.status == TodoStatus::Completed)
        .count();
    let mut out = format!("Todo list updated ({done}/{} done):", todos.len());
    for t in &todos {
        let mark = match t.status {
            TodoStatus::Pending => "[ ]",
            TodoStatus::InProgress => "[~]",
            TodoStatus::Completed => "[x]",
        };
        out.push_str(&format!("\n{mark} {}", t.content));
    }
    let in_progress = todos
        .iter()
        .filter(|t| t.status == TodoStatus::InProgress)
        .count();
    if in_progress > 1 {
        out.push_str("\nNote: keep exactly one item in_progress at a time.");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn formats_checklist_with_progress() {
        let out = tool_todo_write(&json!({"todos": [
            {"content": "Read the parser", "status": "completed"},
            {"content": "Fix the bug", "status": "in_progress"},
            {"content": "Run tests", "status": "pending"}
        ]}))
        .unwrap();
        assert!(out.starts_with("Todo list updated (1/3 done):"));
        assert!(out.contains("[x] Read the parser"));
        assert!(out.contains("[~] Fix the bug"));
        assert!(out.contains("[ ] Run tests"));
    }

    #[test]
    fn rejects_bad_status_and_empty_content() {
        assert!(tool_todo_write(&json!({"todos": [{"content": "x", "status": "done"}]})).is_err());
        assert!(tool_todo_write(&json!({"todos": [{"content": "  "}]})).is_err());
        assert!(tool_todo_write(&json!({})).is_err());
    }
}
