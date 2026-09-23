//! Text formatting for tool calls: short argument previews, one-line summaries, diff/
//! output layout jobs, and the Nerd Font icon per tool name.

use eframe::egui::FontId;
use eframe::egui::text::{LayoutJob, TextFormat, TextWrapping};

use crate::theme::*;

pub(super) fn diff_counts(diff: &str) -> (usize, usize) {
    let mut added = 0;
    let mut removed = 0;
    for line in diff.lines() {
        if line.starts_with("+++") || line.starts_with("---") {
            continue;
        }
        if line.starts_with('+') {
            added += 1;
        } else if line.starts_with('-') {
            removed += 1;
        }
    }
    (added, removed)
}

fn short_path(path: &str, max_segments: usize) -> String {
    let segs: Vec<&str> = path
        .trim_start_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    if segs.len() > max_segments && max_segments > 0 {
        let start = segs.len() - max_segments;
        format!("…/{}", segs[start..].join("/"))
    } else {
        path.to_string()
    }
}

/// "https://www.example.com/a/b?x=1" → "example.com/a/b?x=1…" (scheme + www stripped, truncated).
fn short_url(url: &str, max_chars: usize) -> String {
    let s = url
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let s = s.strip_prefix("www.").unwrap_or(s);
    let s = s.trim_end_matches('/');
    let mut out: String = s.chars().take(max_chars).collect();
    if s.chars().count() > max_chars {
        out.push('…');
    }
    out
}

fn command_preview(command: &str, max_chars: usize) -> String {
    let first_line = command.lines().next().unwrap_or(command).trim();
    let mut out: String = first_line.chars().take(max_chars).collect();
    if first_line.chars().count() > max_chars {
        out.push('…');
    }
    out
}

fn count_output_lines(output: &str) -> usize {
    if output.trim().is_empty() {
        0
    } else {
        output.lines().count().max(1)
    }
}

/// Past-tense verb for a finished tool call. Unknown tools (MCP, ACP-provided) fall back to their
/// humanized name, so the pill always says which tool ran.
fn tool_action_label(name: &str) -> String {
    match name {
        "read" => "Read",
        "write" => "Wrote",
        "edit" => "Edited",
        "delete" => "Deleted",
        "move" => "Moved",
        "mkdir" => "Created",
        "bash" => "Ran",
        "grep" => "Searched",
        "codebase_search" => "Searched code",
        "find" => "Found files",
        "ls" => "Listed",
        "git_status" => "Git status",
        "git_diff" => "Git diff",
        "web_search" => "Searched the web",
        "web_fetch" => "Fetched",
        _ => return other_tool_label(name),
    }
    .to_string()
}

/// Label for tools oxi doesn't know by name: MCP tools (`mcp_<server>_<tool>`) and whatever an
/// ACP agent reports.
fn other_tool_label(name: &str) -> String {
    match name.strip_prefix("mcp_") {
        Some(rest) => format!("MCP {}", rest.replace('_', " ")),
        None => tool_status_label(name),
    }
}

/// Present-progressive verb shown while a tool call is still in flight.
fn tool_running_label(name: &str) -> String {
    match name {
        "read" => "Reading",
        "write" => "Writing",
        "edit" => "Editing",
        "delete" => "Deleting",
        "move" => "Moving",
        "mkdir" => "Creating",
        "bash" => "Running",
        "grep" => "Searching",
        "codebase_search" => "Searching code",
        "find" => "Finding files",
        "ls" => "Listing",
        "git_status" => "Git status",
        "git_diff" => "Git diff",
        "web_search" => "Searching the web",
        "web_fetch" => "Fetching",
        _ => return other_tool_label(name),
    }
    .to_string()
}

/// One-line description of a tool call, split so the verb can be set in the UI font and the
/// target (path, command, query) in monospace.
pub(super) struct ToolSummary {
    /// "Ran", "Edited", "Reading"… (or "Failed" when the call errored).
    pub action: String,
    /// The call's target plus a short result meta, e.g. `stats.py · 25 lines`. May be empty.
    pub detail: String,
}

/// Summarize a tool call for its pill/header. Diff counts are not part of the text: callers show
/// them as colored `+N -M` badges.
pub(super) fn tool_summary(
    name: &str,
    args_summary: Option<&String>,
    output: &str,
    diff: Option<&String>,
    is_error: Option<bool>,
    running: bool,
) -> ToolSummary {
    if running {
        return ToolSummary {
            action: tool_running_label(name),
            detail: tool_target(name, args_summary).unwrap_or_default(),
        };
    }

    let action = if is_error == Some(true) {
        format!("{} failed", other_tool_label(name))
    } else {
        tool_action_label(name)
    };
    let mut parts: Vec<String> = tool_target(name, args_summary).into_iter().collect();
    if name == "read" {
        // The output starts with a header naming the lines actually read; counting output
        // lines would include that header and misreport the range.
        if let Some(range) = read_line_range(output) {
            parts.push(range);
        }
    } else if diff.is_none_or(|d| d.trim().is_empty()) {
        let lines = count_output_lines(output);
        if lines > 0 && !matches!(name, "write" | "edit" | "delete" | "move" | "mkdir") {
            parts.push(format!("{lines} line{}", if lines == 1 { "" } else { "s" }));
        }
    }
    ToolSummary {
        action,
        detail: parts.join(" · "),
    }
}

/// `"lines 12–40"` from the `read` tool's `Lines 12-40` header line.
fn read_line_range(output: &str) -> Option<String> {
    let header = output
        .lines()
        .take(3)
        .find_map(|l| l.strip_prefix("Lines "))?;
    let (start, end) = header.trim().split_once('-')?;
    let (start, end) = (start.parse::<usize>().ok()?, end.parse::<usize>().ok()?);
    Some(if start == end {
        format!("line {start}")
    } else {
        format!("lines {start}–{end}")
    })
}

/// The call's main argument, shortened for a one-line pill: path, command, pattern, query, URL.
fn tool_target(name: &str, args_summary: Option<&String>) -> Option<String> {
    let raw = args_summary?;
    let v = serde_json::from_str::<serde_json::Value>(raw).ok()?;
    let str_arg = |key: &str| {
        v.get(key)
            .and_then(|x| x.as_str())
            .filter(|s| !s.is_empty())
    };
    let target = match name {
        "read" => short_path(str_arg("path").or_else(|| str_arg("filePath"))?, 2),
        "bash" => command_preview(str_arg("command")?, 80),
        "grep" | "codebase_search" => {
            let pattern = str_arg("pattern").or_else(|| str_arg("query"))?;
            match str_arg("path") {
                Some(path) if path != "." => {
                    format!(
                        "{}  in {}",
                        command_preview(pattern, 40),
                        short_path(path, 2)
                    )
                }
                _ => command_preview(pattern, 48),
            }
        }
        "move" => {
            let from = short_path(str_arg("from")?, 2);
            match str_arg("to") {
                Some(to) => format!("{from} → {}", short_path(to, 2)),
                None => from,
            }
        }
        "web_search" => command_preview(str_arg("query")?, 60),
        "web_fetch" => short_url(str_arg("url")?, 56),
        _ => return tool_short_arg(name, args_summary),
    };
    Some(target)
}

/// Tool icons — Nerd Font PUA codepoints rendered with the dedicated `icons` font family.
pub(super) fn tool_icon(name: &str) -> &'static str {
    match name {
        "read" => "\u{f09ee}",  // nf-md-file_document_outline
        "write" => "\u{f0dc9}", // nf-md-file_document_edit_outline
        "edit" => "\u{f03eb}",  // nf-md-pencil
        "bash" => "\u{f018d}",  // nf-md-console
        "grep" => "\u{f021e}",  // nf-md-file_find
        "find" => "\u{f0349}",  // nf-md-magnify
        "ls" => "\u{f0645}",    // nf-md-file_tree
        "web_search" => crate::theme::ICON_WEB_SEARCH,
        "web_fetch" => crate::theme::ICON_GLOBE,
        _ => "\u{f0214}", // nf-md-file
    }
}

/// Short, relevant argument from the `args_summary` JSON: path > command > first value.
fn tool_short_arg(name: &str, args_summary: Option<&String>) -> Option<String> {
    let raw = args_summary?;
    let v = serde_json::from_str::<serde_json::Value>(raw).ok()?;
    // path / filePath for read/write/edit/find
    if let Some(p) = v
        .get("path")
        .or_else(|| v.get("filePath"))
        .and_then(|x| x.as_str())
    {
        // Show only the last 2 path segments.
        let segs: Vec<&str> = p.trim_start_matches('/').split('/').collect();
        let short = if segs.len() > 2 {
            format!("…/{}/{}", segs[segs.len() - 2], segs[segs.len() - 1])
        } else {
            p.to_string()
        };
        // Append the line range when present.
        let offset = v.get("offset").and_then(|x| x.as_u64());
        let limit = v.get("limit").and_then(|x| x.as_u64());
        return Some(match (offset, limit) {
            (Some(o), Some(l)) => format!("{short}  L{o}–{}", o + l - 1),
            (Some(o), None) => format!("{short}  L{o}+"),
            _ => short,
        });
    }
    // command for bash
    if let Some(cmd) = v.get("command").and_then(|x| x.as_str()) {
        let tok: String = cmd.split_whitespace().take(6).collect::<Vec<_>>().join(" ");
        let mut s: String = tok.chars().take(60).collect();
        if tok.chars().count() > 60 {
            s.push('…');
        }
        return Some(s);
    }
    // pattern + path for grep
    if name == "grep" {
        let pat = v.get("pattern").and_then(|x| x.as_str()).unwrap_or("");
        let dir = v.get("path").and_then(|x| x.as_str()).unwrap_or("");
        if !pat.is_empty() {
            return Some(if dir.is_empty() {
                format!("`{pat}`")
            } else {
                format!("`{pat}`  in {dir}")
            });
        }
    }
    // Shortened URL for web_fetch.
    if name == "web_fetch"
        && let Some(u) = v.get("url").and_then(|x| x.as_str())
    {
        return Some(short_url(u, 44));
    }
    // Fallback: the first string value in the object.
    if let serde_json::Value::Object(map) = &v
        && let Some(s) = map.values().find_map(|x| x.as_str())
    {
        let mut t: String = s.chars().take(48).collect();
        if s.chars().count() > 48 {
            t.push('…');
        }
        return Some(t);
    }
    None
}

/// Plain monospace layout job for raw tool output shown under an expanded tool pill.
pub(super) fn mono_output_job(text: &str, wrap_width: f32) -> LayoutJob {
    let mut job = LayoutJob {
        wrap: TextWrapping {
            max_width: wrap_width,
            ..Default::default()
        },
        break_on_newline: true,
        ..Default::default()
    };
    job.append(
        text,
        0.0,
        TextFormat::simple(FontId::monospace(FS_TINY), c_text_muted()),
    );
    job
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(name: &str, args: &str, output: &str) -> String {
        let args = args.to_string();
        let s = tool_summary(name, Some(&args), output, None, Some(false), false);
        format!("{} | {}", s.action, s.detail)
    }

    #[test]
    fn summaries_name_the_action_once() {
        assert_eq!(
            summary(
                "read",
                r#"{"path":"src/stats.py"}"#,
                "File: /x/src/stats.py\nLines 1-22\n---\n1\tfoo"
            ),
            "Read | src/stats.py · lines 1–22"
        );
        assert_eq!(
            summary("bash", r#"{"command":"cargo test"}"#, "ok\nok"),
            "Ran | cargo test · 2 lines"
        );
        assert_eq!(
            summary("mcp_github_search", r#"{"q":"oxi"}"#, ""),
            "MCP github search | oxi"
        );
        let running = tool_summary("edit", None, "", None, None, true);
        assert_eq!(running.action, "Editing");
    }
}
