use super::*;

// ─── tool_read ──────────────────────────────────────────────────────

#[test]
fn tool_read_basic() {
    let cwd = temp_workspace("read-basic");
    fs::write(cwd.join("test.txt"), "line1\nline2\nline3").unwrap();
    let res = run_tool(&cwd, "read", &json!({"path": "test.txt"}), &all_enabled());
    assert!(!res.is_error);
    assert!(res.output.contains("Lines 1-3"));
    assert!(res.output.contains("     1\tline1"));
    assert!(res.output.contains("     2\tline2"));
    assert!(res.output.contains("     3\tline3"));
}

#[test]
fn tool_read_with_offset_and_limit() {
    let cwd = temp_workspace("read-offset");
    let content: String = (1..=20)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(cwd.join("data.txt"), &content).unwrap();
    let res = run_tool(
        &cwd,
        "read",
        &json!({"path": "data.txt", "offset": 5, "limit": 3}),
        &all_enabled(),
    );
    assert!(!res.is_error);
    assert!(res.output.contains("Lines 5-7"));
    assert!(res.output.contains("     5\tline5"));
    assert!(res.output.contains("     7\tline7"));
    assert!(!res.output.contains("     8\tline8"));
}

#[test]
fn tool_read_missing_file() {
    let cwd = temp_workspace("read-missing");
    let res = run_tool(&cwd, "read", &json!({"path": "nope.txt"}), &all_enabled());
    assert!(res.is_error);
}

#[test]
fn tool_read_missing_path_arg() {
    let cwd = temp_workspace("read-no-arg");
    let res = run_tool(&cwd, "read", &json!({}), &all_enabled());
    assert!(res.is_error);
    assert!(res.output.contains("missing path"));
}

#[test]
fn tool_read_rejects_path_escape() {
    let cwd = temp_workspace("read-escape");
    let res = run_tool(
        &cwd,
        "read",
        &json!({"path": "/etc/passwd"}),
        &all_enabled(),
    );
    assert!(res.is_error);
}

// ─── tool_write ─────────────────────────────────────────────────────

#[test]
fn tool_write_creates_file() {
    let cwd = temp_workspace("write-create");
    let res = run_tool(
        &cwd,
        "write",
        &json!({"path": "new.txt", "content": "hello world"}),
        &all_enabled(),
    );
    assert!(!res.is_error);
    assert!(res.output.contains("Wrote"));
    assert_eq!(
        fs::read_to_string(cwd.join("new.txt")).unwrap(),
        "hello world"
    );
}

#[test]
fn tool_write_creates_parent_dirs() {
    let cwd = temp_workspace("write-dirs");
    let res = run_tool(
        &cwd,
        "write",
        &json!({"path": "a/b/c.txt", "content": "deep"}),
        &all_enabled(),
    );
    assert!(!res.is_error);
    assert_eq!(fs::read_to_string(cwd.join("a/b/c.txt")).unwrap(), "deep");
}

#[test]
fn tool_write_produces_diff() {
    let cwd = temp_workspace("write-diff");
    fs::write(cwd.join("existing.txt"), "old content").unwrap();
    let res = run_tool(
        &cwd,
        "write",
        &json!({"path": "existing.txt", "content": "new content"}),
        &all_enabled(),
    );
    assert!(!res.is_error);
    assert!(res.diff.is_some());
    let diff = res.diff.unwrap();
    assert!(diff.contains("-old content"));
    assert!(diff.contains("+new content"));
}

#[test]
fn tool_write_missing_content() {
    let cwd = temp_workspace("write-no-content");
    let res = run_tool(&cwd, "write", &json!({"path": "x.txt"}), &all_enabled());
    assert!(res.is_error);
    assert!(res.output.contains("missing content"));
}

// ─── tool_edit ──────────────────────────────────────────────────────

#[test]
fn tool_edit_multiple_edits_are_sequential_and_transactional() {
    let cwd = temp_workspace("edit-sequential");
    fs::write(cwd.join("sequence.txt"), "alpha beta").unwrap();
    let result = run_tool(
        &cwd,
        "edit",
        &serde_json::json!({
            "path": "sequence.txt",
            "edits": [
                {"oldText": "alpha", "newText": "gamma"},
                {"oldText": "gamma beta", "newText": "done"}
            ]
        }),
        &all_enabled(),
    );
    assert!(!result.is_error, "{}", result.output);
    assert_eq!(
        fs::read_to_string(cwd.join("sequence.txt")).unwrap(),
        "done"
    );

    let failed = run_tool(
        &cwd,
        "edit",
        &serde_json::json!({
            "path": "sequence.txt",
            "edits": [
                {"oldText": "done", "newText": "partially changed"},
                {"oldText": "missing", "newText": "never"}
            ]
        }),
        &all_enabled(),
    );
    assert!(failed.is_error);
    assert_eq!(
        fs::read_to_string(cwd.join("sequence.txt")).unwrap(),
        "done"
    );
}

#[test]
fn tool_bash_drains_output_larger_than_os_pipe() {
    let cwd = temp_workspace("bash-large-output");
    let result = run_tool(
        &cwd,
        "bash",
        &serde_json::json!({
            "command": "i=0; while [ $i -lt 20000 ]; do echo 'large output line'; i=$((i+1)); done",
            "timeout": 5
        }),
        &all_enabled(),
    );
    assert!(!result.is_error, "{}", result.output);
    assert!(result.output.contains("exit code: 0"), "{}", result.output);
    assert!(result.output.contains("output truncated"));
}

#[test]
fn tool_edit_single_replacement() {
    let cwd = temp_workspace("edit-single");
    fs::write(cwd.join("code.rs"), "fn main() {}\n").unwrap();
    let res = run_tool(
        &cwd,
        "edit",
        &json!({
            "path": "code.rs",
            "edits": [{"oldText": "fn main() {}", "newText": "fn main() {\n    println!(\"hi\");\n}"}]
        }),
        &all_enabled(),
    );
    assert!(!res.is_error);
    assert!(res.diff.is_some());
    let content = fs::read_to_string(cwd.join("code.rs")).unwrap();
    assert!(content.contains("println"));
}

#[test]
fn tool_edit_rejects_ambiguous_match() {
    let cwd = temp_workspace("edit-ambiguous");
    fs::write(cwd.join("dup.txt"), "hello hello").unwrap();
    let res = run_tool(
        &cwd,
        "edit",
        &json!({
            "path": "dup.txt",
            "edits": [{"oldText": "hello", "newText": "world"}]
        }),
        &all_enabled(),
    );
    assert!(res.is_error);
    assert!(res.output.contains("2 occurrences"));
}

#[test]
fn tool_edit_rejects_no_match() {
    let cwd = temp_workspace("edit-no-match");
    fs::write(cwd.join("file.txt"), "content").unwrap();
    let res = run_tool(
        &cwd,
        "edit",
        &json!({
            "path": "file.txt",
            "edits": [{"oldText": "nonexistent", "newText": "new"}]
        }),
        &all_enabled(),
    );
    assert!(res.is_error);
    assert!(res.output.contains("0 occurrences"));
}

#[test]
fn tool_edit_no_edits_array() {
    let cwd = temp_workspace("edit-no-edits");
    fs::write(cwd.join("file.txt"), "content").unwrap();
    let res = run_tool(&cwd, "edit", &json!({"path": "file.txt"}), &all_enabled());
    assert!(res.is_error);
    assert!(res.output.contains("no edits"));
}

#[test]
fn tool_edit_replace_all_replaces_every_occurrence() {
    let cwd = temp_workspace("edit-replace-all");
    fs::write(cwd.join("dup.txt"), "foo foo foo").unwrap();
    let res = run_tool(
        &cwd,
        "edit",
        &json!({
            "path": "dup.txt",
            "edits": [{"oldText": "foo", "newText": "bar", "replaceAll": true}]
        }),
        &all_enabled(),
    );
    assert!(!res.is_error);
    assert!(res.output.contains("3 replacements"));
    assert!(res.diff.is_some());
    let content = fs::read_to_string(cwd.join("dup.txt")).unwrap();
    assert_eq!(content, "bar bar bar");
}

#[test]
fn tool_edit_replace_all_not_found_errors() {
    let cwd = temp_workspace("edit-replace-all-nf");
    fs::write(cwd.join("file.txt"), "content").unwrap();
    let res = run_tool(
        &cwd,
        "edit",
        &json!({
            "path": "file.txt",
            "edits": [{"oldText": "missing", "newText": "x", "replaceAll": true}]
        }),
        &all_enabled(),
    );
    assert!(res.is_error);
    assert!(res.output.contains("not found"));
}

#[test]
fn tool_edit_empty_old_text_errors() {
    let cwd = temp_workspace("edit-empty-old");
    fs::write(cwd.join("file.txt"), "content").unwrap();
    let res = run_tool(
        &cwd,
        "edit",
        &json!({
            "path": "file.txt",
            "edits": [{"oldText": "", "newText": "x"}]
        }),
        &all_enabled(),
    );
    assert!(res.is_error);
    assert!(res.output.contains("must not be empty"));
}

#[test]
fn tool_edit_matches_lf_old_text_against_crlf_and_preserves_crlf() {
    let cwd = temp_workspace("edit-crlf");
    fs::write(
        cwd.join("windows.txt"),
        "before\r\nold one\r\nold two\r\nafter\r\n",
    )
    .unwrap();
    let res = run_tool(
        &cwd,
        "edit",
        &json!({
            "path": "windows.txt",
            "edits": [{"oldText": "old one\nold two", "newText": "new one\nnew two"}]
        }),
        &all_enabled(),
    );
    assert!(!res.is_error, "{}", res.output);
    assert_eq!(
        fs::read_to_string(cwd.join("windows.txt")).unwrap(),
        "before\r\nnew one\r\nnew two\r\nafter\r\n"
    );
}

#[test]
fn tool_edit_matches_crlf_old_text_against_lf_and_preserves_lf() {
    let cwd = temp_workspace("edit-lf-with-crlf-needle");
    fs::write(cwd.join("unix.txt"), "before\nold one\nold two\nafter\n").unwrap();
    let res = run_tool(
        &cwd,
        "edit",
        &json!({
            "path": "unix.txt",
            "edits": [{"oldText": "old one\r\nold two", "newText": "new one\r\nnew two"}]
        }),
        &all_enabled(),
    );
    assert!(!res.is_error, "{}", res.output);
    assert_eq!(
        fs::read_to_string(cwd.join("unix.txt")).unwrap(),
        "before\nnew one\nnew two\nafter\n"
    );
}

#[test]
fn tool_edit_replace_all_multiline_crlf() {
    let cwd = temp_workspace("edit-all-crlf");
    fs::write(cwd.join("all.txt"), "a\r\nb\r\n--\r\na\r\nb\r\n").unwrap();
    let res = run_tool(
        &cwd,
        "edit",
        &json!({
            "path": "all.txt",
            "edits": [{"oldText": "a\nb", "newText": "x\ny", "replaceAll": true}]
        }),
        &all_enabled(),
    );
    assert!(!res.is_error, "{}", res.output);
    assert!(res.output.contains("2 replacements"));
    assert_eq!(
        fs::read_to_string(cwd.join("all.txt")).unwrap(),
        "x\r\ny\r\n--\r\nx\r\ny\r\n"
    );
}

#[test]
fn tool_edit_preserves_utf8_bom_at_file_start() {
    let cwd = temp_workspace("edit-bom");
    fs::write(cwd.join("bom.txt"), "\u{feff}first\r\nsecond\r\n").unwrap();
    let res = run_tool(
        &cwd,
        "edit",
        &json!({
            "path": "bom.txt",
            "edits": [{"oldText": "first\nsecond", "newText": "changed\ndone"}]
        }),
        &all_enabled(),
    );
    assert!(!res.is_error, "{}", res.output);
    assert_eq!(
        fs::read_to_string(cwd.join("bom.txt")).unwrap(),
        "\u{feff}changed\r\ndone\r\n"
    );
}

#[test]
fn tool_edit_eol_equivalence_still_rejects_ambiguous_matches() {
    let cwd = temp_workspace("edit-eol-ambiguous");
    fs::write(cwd.join("mixed.txt"), "a\r\nb\r\n--\na\nb\n").unwrap();
    let res = run_tool(
        &cwd,
        "edit",
        &json!({
            "path": "mixed.txt",
            "edits": [{"oldText": "a\nb", "newText": "x"}]
        }),
        &all_enabled(),
    );
    assert!(res.is_error);
    assert!(res.output.contains("2 occurrences"));
}

#[test]
fn tool_edit_ambiguous_error_mentions_replace_all() {
    let cwd = temp_workspace("edit-ambiguous-hint");
    fs::write(cwd.join("dup.txt"), "hello hello").unwrap();
    let res = run_tool(
        &cwd,
        "edit",
        &json!({
            "path": "dup.txt",
            "edits": [{"oldText": "hello", "newText": "world"}]
        }),
        &all_enabled(),
    );
    assert!(res.is_error);
    assert!(res.output.contains("replaceAll"));
}
