//! Agent access to the global, user-visible scratchpad.

use serde_json::Value;
use std::path::Path;

use super::{ToolEnv, ToolResult, make_unified_diff};

pub(super) fn tool_scratchpad(args: &Value, env: &ToolEnv) -> ToolResult {
    run_at(&crate::scratchpad::path(), args, env)
}

fn run_at(path: &Path, args: &Value, env: &ToolEnv) -> ToolResult {
    let result = (|| {
        let mode = args
            .get("mode")
            .and_then(Value::as_str)
            .ok_or("Missing string `mode`.")?;
        let content = args
            .get("content")
            .and_then(Value::as_str)
            .ok_or("Missing string `content`.")?;
        let (before, after) = crate::scratchpad::update(path, mode, content)?;
        if let Some(journal) = &env.undo_journal {
            journal.lock().unwrap_or_else(|e| e.into_inner()).mark_non_reversible(
                "This response updated the global scratchpad, which is shared across workspaces and is not restored by Regenerate.",
            );
        }
        Ok::<_, String>((mode, before, after))
    })();
    match result {
        Ok((mode, before, after)) => ToolResult {
            output: format!("Scratchpad updated ({mode}, {} bytes).", after.len()),
            is_error: false,
            diff: Some(make_unified_diff("scratchpad.md", &before, &after)),
            full_output_path: None,
        },
        Err(output) => ToolResult {
            output,
            is_error: true,
            diff: None,
            full_output_path: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    #[test]
    fn validates_arguments_and_marks_global_mutation() {
        let path =
            std::env::temp_dir().join(format!("oxi-scratchpad-tool-{}.md", rand::random::<u64>()));
        let journal = Arc::new(Mutex::new(super::super::TurnUndoJournal::default()));
        let env = ToolEnv {
            enabled: vec![true; crate::settings::ALL_TOOL_NAMES.len()],
            web_search_url: String::new(),
            web_search_backend: crate::settings::WebSearchBackend::default(),
            bash_timeout_cap_secs: 300,
            mcp: None,
            subagent: None,
            undo_journal: Some(journal.clone()),
        };
        for args in [
            json!({}),
            json!({"mode": "append"}),
            json!({"mode": "bad", "content": "x"}),
            json!({"mode": "rewrite", "content": 12}),
        ] {
            assert!(run_at(&path, &args, &env).is_error);
        }
        assert!(!path.exists());
        assert!(journal.lock().unwrap().unavailable_reason().is_none());
        let result = run_at(&path, &json!({"mode": "append", "content": "notes"}), &env);
        assert!(!result.is_error);
        assert!(result.diff.unwrap().contains("+notes"));
        assert!(
            journal
                .lock()
                .unwrap()
                .unavailable_reason()
                .unwrap()
                .contains("scratchpad")
        );
        std::fs::remove_file(path).unwrap();
    }
}
