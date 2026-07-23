use super::*;

// ─── run_tool routing ───────────────────────────────────────────────

#[test]
fn run_tool_unknown_tool() {
    let cwd = temp_workspace("unknown");
    let res = run_tool(&cwd, "destroy", &json!({}), &all_enabled());
    assert!(res.is_error);
    assert!(res.output.contains("Unknown tool"));
}

#[test]
fn run_tool_disabled_tool() {
    let cwd = temp_workspace("disabled");
    let mut enabled = all_enabled();
    enabled.enabled[0] = false; // disable "read"
    let res = run_tool(&cwd, "read", &json!({"path": "x"}), &enabled);
    assert!(res.is_error);
    assert!(res.output.contains("disabled"));
}

// ─── per-turn undo journal ─────────────────────────────────────────

#[test]
fn undo_journal_restores_modified_and_created_files() {
    use std::sync::{Arc, Mutex};

    let cwd = temp_workspace("undo-restore");
    fs::write(cwd.join("existing.txt"), "before").unwrap();
    let journal = Arc::new(Mutex::new(crate::agent::tools::TurnUndoJournal::default()));
    let mut env = all_enabled();
    env.undo_journal = Some(journal.clone());

    assert!(
        !run_tool(
            &cwd,
            "write",
            &json!({"path": "existing.txt", "content": "after"}),
            &env,
        )
        .is_error
    );
    assert!(
        !run_tool(
            &cwd,
            "write",
            &json!({"path": "new/created.txt", "content": "created"}),
            &env,
        )
        .is_error
    );

    journal
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .restore(&cwd)
        .unwrap();
    assert_eq!(
        fs::read_to_string(cwd.join("existing.txt")).unwrap(),
        "before"
    );
    assert!(!cwd.join("new/created.txt").exists());
}

#[test]
fn undo_journal_refuses_to_overwrite_later_user_edit() {
    use std::sync::{Arc, Mutex};

    let cwd = temp_workspace("undo-conflict");
    fs::write(cwd.join("file.txt"), "before").unwrap();
    let journal = Arc::new(Mutex::new(crate::agent::tools::TurnUndoJournal::default()));
    let mut env = all_enabled();
    env.undo_journal = Some(journal.clone());
    assert!(
        !run_tool(
            &cwd,
            "write",
            &json!({"path": "file.txt", "content": "agent"}),
            &env,
        )
        .is_error
    );
    fs::write(cwd.join("file.txt"), "user").unwrap();

    assert!(
        journal
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .restore(&cwd)
            .is_err()
    );
    assert_eq!(fs::read_to_string(cwd.join("file.txt")).unwrap(), "user");
}

#[test]
fn readonly_bash_keeps_turn_undo_available() {
    use std::sync::{Arc, Mutex};

    let cwd = temp_workspace("undo-bash");
    let journal = Arc::new(Mutex::new(crate::agent::tools::TurnUndoJournal::default()));
    let mut env = all_enabled();
    env.undo_journal = Some(journal.clone());
    assert!(!run_tool(&cwd, "bash", &json!({"command": "pwd"}), &env).is_error);
    assert!(
        journal
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .restore(&cwd)
            .is_ok()
    );
}

#[test]
fn reversible_delete_move_and_mkdir_restore_cleanly() {
    use std::sync::{Arc, Mutex};

    let cwd = temp_workspace("undo-path-ops");
    fs::write(cwd.join("delete-me.txt"), "original").unwrap();
    fs::write(cwd.join("move-me.txt"), "moved content").unwrap();
    let journal = Arc::new(Mutex::new(crate::agent::tools::TurnUndoJournal::default()));
    let mut env = all_enabled();
    env.undo_journal = Some(journal.clone());

    assert!(!run_tool(&cwd, "delete", &json!({"path": "delete-me.txt"}), &env,).is_error);
    assert!(
        !run_tool(
            &cwd,
            "move",
            &json!({"from": "move-me.txt", "to": "moved.txt"}),
            &env,
        )
        .is_error
    );
    assert!(!run_tool(&cwd, "mkdir", &json!({"path": "created-dir"}), &env,).is_error);

    assert!(!cwd.join("delete-me.txt").exists());
    assert!(!cwd.join("move-me.txt").exists());
    assert!(cwd.join("moved.txt").exists());
    assert!(cwd.join("created-dir").is_dir());

    journal
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .restore(&cwd)
        .unwrap();
    assert_eq!(
        fs::read_to_string(cwd.join("delete-me.txt")).unwrap(),
        "original"
    );
    assert_eq!(
        fs::read_to_string(cwd.join("move-me.txt")).unwrap(),
        "moved content"
    );
    assert!(!cwd.join("moved.txt").exists());
    assert!(!cwd.join("created-dir").exists());
}

#[test]
fn undo_refuses_untracked_file_inside_created_directory() {
    use std::sync::{Arc, Mutex};

    let cwd = temp_workspace("undo-created-dir-conflict");
    let journal = Arc::new(Mutex::new(crate::agent::tools::TurnUndoJournal::default()));
    let mut env = all_enabled();
    env.undo_journal = Some(journal.clone());
    assert!(!run_tool(&cwd, "mkdir", &json!({"path": "created-dir"}), &env,).is_error);
    fs::write(cwd.join("created-dir/user.txt"), "user").unwrap();

    assert!(
        journal
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .restore(&cwd)
            .is_err()
    );
    assert!(cwd.join("created-dir/user.txt").exists());
}

#[test]
fn delete_refuses_non_empty_directory() {
    let cwd = temp_workspace("delete-non-empty");
    fs::create_dir(cwd.join("dir")).unwrap();
    fs::write(cwd.join("dir/file.txt"), "keep").unwrap();
    let result = run_tool(&cwd, "delete", &json!({"path": "dir"}), &all_enabled());
    assert!(result.is_error);
    assert!(cwd.join("dir/file.txt").exists());
}
