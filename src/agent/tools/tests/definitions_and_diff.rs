use super::*;

// ─── tool_definitions_json ──────────────────────────────────────────

#[test]
fn tool_definitions_all_enabled() {
    let defs = tool_definitions_json(&all_enabled().enabled, 300);
    assert_eq!(defs.len(), ALL_TOOL_NAMES.len());
    let names: Vec<&str> = defs
        .iter()
        .filter_map(|d| d.get("function")?.get("name")?.as_str())
        .collect();
    assert!(names.contains(&"web_search"));
    assert!(names.contains(&"web_fetch"));
    assert!(names.contains(&"read"));
    assert!(names.contains(&"bash"));
    assert!(names.contains(&"ls"));
}

#[test]
fn tool_definitions_some_disabled() {
    let mut enabled = vec![false; ALL_TOOL_NAMES.len()];
    enabled[0] = true; // only "read"
    let defs = tool_definitions_json(&enabled, 300);
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

#[test]
fn floor_char_boundary_moves_back_from_multibyte_boundary() {
    let s = format!("{}😀", "x".repeat(MAX_TOOL_OUTPUT_CHARS - 1));
    assert_eq!(
        floor_char_boundary(&s, MAX_TOOL_OUTPUT_CHARS),
        MAX_TOOL_OUTPUT_CHARS - 1
    );
}

#[test]
fn truncate_out_multibyte_does_not_panic() {
    let s = format!(
        "{}😀{}",
        "x".repeat(MAX_TOOL_OUTPUT_CHARS - 1),
        "y".repeat(100)
    );
    let truncated = truncate_out(s);
    assert!(truncated.contains("[output truncated"));
    assert!(truncated.starts_with(&"x".repeat(MAX_TOOL_OUTPUT_CHARS - 1)));
}
