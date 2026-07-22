use super::*;

// ─── tool_bash ──────────────────────────────────────────────────────

#[test]
fn tool_bash_echo() {
    let cwd = temp_workspace("bash-echo");
    let res = run_tool(
        &cwd,
        "bash",
        &json!({"command": "echo hello"}),
        &all_enabled(),
    );
    assert!(!res.is_error);
    assert!(res.output.contains("hello"));
    assert!(res.output.contains("exit code: 0"));
}

#[test]
fn tool_bash_cwd_respected() {
    let cwd = temp_workspace("bash-cwd");
    let res = run_tool(&cwd, "bash", &json!({"command": "pwd"}), &all_enabled());
    assert!(!res.is_error);
    let canonical = cwd.canonicalize().unwrap();
    assert!(res.output.contains(canonical.to_str().unwrap()));
}

#[test]
fn tool_bash_denied_sudo() {
    let cwd = temp_workspace("bash-sudo");
    let res = run_tool(
        &cwd,
        "bash",
        &json!({"command": "sudo rm -rf /"}),
        &all_enabled(),
    );
    assert!(res.is_error);
    assert!(res.output.contains("Refusing"));
}

#[test]
fn tool_bash_denied_fork_bomb() {
    let cwd = temp_workspace("bash-fork");
    let res = run_tool(
        &cwd,
        "bash",
        &json!({"command": ":(){ :|:& };:"}),
        &all_enabled(),
    );
    assert!(res.is_error);
    assert!(res.output.contains("Refusing"));
}

#[test]
fn tool_bash_timeout() {
    let cwd = temp_workspace("bash-timeout");
    let res = run_tool(
        &cwd,
        "bash",
        &json!({"command": "sleep 60", "timeout": 0.3}),
        &all_enabled(),
    );
    assert!(!res.is_error);
    assert!(res.output.contains("timeout"));
}

#[test]
fn tool_bash_timeout_cap_clamps_requested_timeout() {
    let cwd = temp_workspace("bash-cap-clamp");
    let mut env = all_enabled();
    env.bash_timeout_cap_secs = 1;
    let start = std::time::Instant::now();
    // Requests 30s but the cap is 1s, so it must be killed at ~1s.
    let res = run_tool(
        &cwd,
        "bash",
        &json!({"command": "sleep 30", "timeout": 30}),
        &env,
    );
    assert!(!res.is_error);
    assert!(res.output.contains("timeout"));
    assert!(start.elapsed() < std::time::Duration::from_secs(3));
}

#[test]
fn tool_bash_default_timeout_respects_low_cap() {
    let cwd = temp_workspace("bash-cap-default");
    let mut env = all_enabled();
    env.bash_timeout_cap_secs = 1;
    let start = std::time::Instant::now();
    // No explicit timeout: default 15s would exceed the 1s cap, so it clamps to 1s.
    let res = run_tool(&cwd, "bash", &json!({"command": "sleep 30"}), &env);
    assert!(!res.is_error);
    assert!(res.output.contains("timeout"));
    assert!(start.elapsed() < std::time::Duration::from_secs(3));
}

#[test]
fn tool_bash_missing_command() {
    let cwd = temp_workspace("bash-no-cmd");
    let res = run_tool(&cwd, "bash", &json!({}), &all_enabled());
    assert!(res.is_error);
    assert!(res.output.contains("missing command"));
}

#[test]
fn tool_bash_nonzero_exit() {
    let cwd = temp_workspace("bash-exit");
    let res = run_tool(&cwd, "bash", &json!({"command": "exit 42"}), &all_enabled());
    assert!(!res.is_error);
    assert!(res.output.contains("exit code: 42"));
}

// ─── tool_grep ──────────────────────────────────────────────────────

#[test]
fn tool_grep_finds_match() {
    let cwd = temp_workspace("grep-match");
    fs::write(cwd.join("a.txt"), "apple\nbanana\ncherry").unwrap();
    let res = run_tool(&cwd, "grep", &json!({"pattern": "banana"}), &all_enabled());
    assert!(!res.is_error);
    assert!(res.output.contains("banana"));
    assert!(res.output.contains("a.txt:2"));
}

#[test]
fn tool_grep_no_match() {
    let cwd = temp_workspace("grep-no-match");
    fs::write(cwd.join("a.txt"), "hello\n").unwrap();
    let res = run_tool(&cwd, "grep", &json!({"pattern": "zzz"}), &all_enabled());
    assert!(!res.is_error);
    assert!(res.output.contains("No matches"));
}

#[test]
fn tool_grep_specific_file() {
    let cwd = temp_workspace("grep-file");
    fs::write(cwd.join("a.txt"), "alpha\n").unwrap();
    fs::write(cwd.join("b.txt"), "beta\n").unwrap();
    let res = run_tool(
        &cwd,
        "grep",
        &json!({"pattern": "alpha", "path": "a.txt"}),
        &all_enabled(),
    );
    assert!(!res.is_error);
    assert!(res.output.contains("alpha"));
}

#[test]
fn tool_grep_with_limit() {
    let cwd = temp_workspace("grep-limit");
    let content: String = (1..=50)
        .map(|i| format!("match {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(cwd.join("big.txt"), &content).unwrap();
    let res = run_tool(
        &cwd,
        "grep",
        &json!({"pattern": "match", "limit": 5}),
        &all_enabled(),
    );
    assert!(!res.is_error);
    assert!(res.output.contains("match limit 5 reached"));
}

#[test]
fn tool_grep_missing_pattern() {
    let cwd = temp_workspace("grep-no-pattern");
    let res = run_tool(&cwd, "grep", &json!({}), &all_enabled());
    assert!(res.is_error);
}

// ─── tool_find ──────────────────────────────────────────────────────

#[test]
fn tool_find_glob() {
    let cwd = temp_workspace("find-glob");
    fs::write(cwd.join("a.rs"), "").unwrap();
    fs::write(cwd.join("b.txt"), "").unwrap();
    let res = run_tool(&cwd, "find", &json!({"pattern": "*.rs"}), &all_enabled());
    assert!(!res.is_error);
    assert!(res.output.contains("a.rs"));
    assert!(!res.output.contains("b.txt"));
}

#[test]
fn tool_find_missing_pattern() {
    let cwd = temp_workspace("find-no-pattern");
    let res = run_tool(&cwd, "find", &json!({}), &all_enabled());
    assert!(res.is_error);
}

// ─── tool_ls ────────────────────────────────────────────────────────

#[test]
fn tool_ls_basic() {
    let cwd = temp_workspace("ls-basic");
    fs::write(cwd.join("file1.txt"), "").unwrap();
    fs::write(cwd.join("file2.txt"), "").unwrap();
    fs::create_dir(cwd.join("subdir")).unwrap();
    let res = run_tool(&cwd, "ls", &json!({}), &all_enabled());
    assert!(!res.is_error);
    assert!(res.output.contains("file1.txt"));
    assert!(res.output.contains("file2.txt"));
    assert!(res.output.contains("subdir"));
}

#[test]
fn tool_ls_empty_dir() {
    let cwd = temp_workspace("ls-empty");
    let sub = cwd.join("empty");
    fs::create_dir(&sub).unwrap();
    let res = run_tool(&cwd, "ls", &json!({"path": "empty"}), &all_enabled());
    assert!(!res.is_error);
    assert!(res.output.contains("[empty directory]"));
}

#[test]
fn tool_ls_with_limit() {
    let cwd = temp_workspace("ls-limit");
    for i in 0..10 {
        fs::write(cwd.join(format!("file{i:02}.txt")), "").unwrap();
    }
    let res = run_tool(&cwd, "ls", &json!({"limit": 3}), &all_enabled());
    assert!(!res.is_error);
    assert!(res.output.contains("limit 3 reached"));
}
