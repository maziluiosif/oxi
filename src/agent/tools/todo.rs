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

pub(super) fn tool_todo_write(args: &Value) -> Result<String, String> {
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
