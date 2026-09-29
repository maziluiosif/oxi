use super::*;

// ─── todo_write / task / diagnostics ────────────────────────────────

#[test]
fn todo_write_routes_through_run_tool() {
    let cwd = temp_workspace("todo-route");
    let res = run_tool(
        &cwd,
        "todo_write",
        &json!({"todos": [{"content": "Ship it", "status": "in_progress"}]}),
        &all_enabled(),
    );
    assert!(!res.is_error, "{}", res.output);
    assert!(res.output.contains("[~] Ship it"));
}

#[test]
fn task_without_a_runner_reports_unavailable() {
    let cwd = temp_workspace("task-no-runner");
    let res = run_tool(
        &cwd,
        "task",
        &json!({"description": "look", "prompt": "find x"}),
        &all_enabled(),
    );
    assert!(res.is_error);
    assert!(res.output.contains("not available"));
}

#[test]
fn diagnostics_without_a_project_explains_itself() {
    let cwd = temp_workspace("diag-empty");
    let res = run_tool(&cwd, "diagnostics", &json!({}), &all_enabled());
    assert!(!res.is_error, "{}", res.output);
    assert!(res.output.contains("No supported project"));
}

#[test]
fn diagnostics_rejects_paths_outside_the_workspace() {
    let cwd = temp_workspace("diag-escape");
    let res = run_tool(
        &cwd,
        "diagnostics",
        &json!({"path": "../.."}),
        &all_enabled(),
    );
    assert!(res.is_error);
}

/// Runs a real `cargo check` on a two-line crate with a type error.
#[test]
fn diagnostics_reports_cargo_errors_with_locations() {
    let cwd = temp_workspace("diag-cargo");
    fs::write(
        cwd.join("Cargo.toml"),
        "[package]\nname = \"diag_probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    )
    .unwrap();
    fs::create_dir_all(cwd.join("src")).unwrap();
    fs::write(
        cwd.join("src/main.rs"),
        "fn main() {\n    let x: u32 = \"text\";\n}\n",
    )
    .unwrap();
    #[cfg(windows)]
    {
        // Cargo can expand the test process's PATH beyond cmd's 8191-character limit.
        // Give this fixture a short path to the same toolchain while running the real checker.
        let cargo = std::path::Path::new(env!("CARGO"));
        fs::write(
            cwd.join("cargo.cmd"),
            format!(
                "@echo off\r\nset \"PATH={};%SystemRoot%\\System32\"\r\n\"{}\" %*\r\n",
                cargo.parent().unwrap().display(),
                cargo.display()
            ),
        )
        .unwrap();
    }
    let res = run_tool(&cwd, "diagnostics", &json!({}), &all_enabled());
    assert!(!res.is_error, "{}", res.output);
    assert!(
        res.output.contains("src/main.rs:2:18: error[E0308]"),
        "{}",
        res.output
    );
}
