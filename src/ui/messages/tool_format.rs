//! Text formatting for tool calls: short argument previews, one-line summaries, diff/
//! output layout jobs, and the Nerd Font icon per tool name.

use eframe::egui::text::{LayoutJob, LayoutSection, TextFormat, TextWrapping};
use eframe::egui::{Color32, FontId};

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

pub(super) fn diff_wrapped_job(diff: &str, wrap_width: f32) -> LayoutJob {
    let lines: Vec<&str> = diff.lines().collect();
    let mut job = LayoutJob {
        wrap: TextWrapping {
            max_width: wrap_width,
            ..Default::default()
        },
        break_on_newline: true,
        ..Default::default()
    };

    let context_text = c_text_muted();

    for (i, line) in lines.iter().enumerate() {
        let start = job.text.len();
        job.text.push_str(line);
        if i + 1 < lines.len() {
            job.text.push('\n');
        }
        let end = job.text.len();
        let (color, background) = if line.starts_with('+') {
            (c_diff_add_fg(), c_diff_add_bg())
        } else if line.starts_with('-') {
            (c_diff_del_fg(), c_diff_del_bg())
        } else {
            (context_text, Color32::TRANSPARENT)
        };
        job.sections.push(LayoutSection {
            leading_space: 0.0,
            byte_range: start..end,
            format: TextFormat {
                font_id: FontId::monospace(FS_TINY),
                color,
                background,
                ..Default::default()
            },
        });
    }

    job
}

fn tool_path_from_args(args_summary: Option<&String>) -> Option<String> {
    let raw = args_summary?;
    let value = serde_json::from_str::<serde_json::Value>(raw).ok()?;
    value
        .get("path")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .or_else(|| {
            value
                .get("filePath")
                .and_then(|v| v.as_str())
                .map(str::to_owned)
        })
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

fn tool_action_label(name: &str) -> &'static str {
    match name {
        "read" => "Read",
        "write" => "Wrote",
        "edit" => "Edited",
        "bash" => "Ran",
        "grep" => "Searched",
        "find" => "Found files",
        "ls" => "Listed",
        "web_search" => "Searched",
        "web_fetch" => "Fetched",
        _ => "Used",
    }
}

pub(super) fn tool_summary_text(
    name: &str,
    args_summary: Option<&String>,
    output: &str,
    diff: Option<&String>,
    is_error: Option<bool>,
    running: bool,
) -> String {
    if running {
        let target = tool_short_arg(name, args_summary)
            .map(|s| format!(" · {s}"))
            .unwrap_or_default();
        return format!("{}{}", tool_status_label(name), target);
    }

    let has_error = is_error == Some(true);
    let action = if has_error {
        "Failed"
    } else {
        tool_action_label(name)
    };
    let mut parts = vec![action.to_string()];

    match name {
        "bash" => {
            if let Some(raw) = args_summary {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) {
                    if let Some(cmd) = v.get("command").and_then(|x| x.as_str()) {
                        let p = command_preview(cmd, 42);
                        if !p.is_empty() {
                            parts.push(format!("`{p}`"));
                        }
                    }
                }
            }
        }
        "grep" => {
            if let Some(raw) = args_summary {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) {
                    if let Some(pattern) = v.get("pattern").and_then(|x| x.as_str()) {
                        parts.push(format!("`{}`", command_preview(pattern, 32)));
                    }
                    if let Some(path) = v.get("path").and_then(|x| x.as_str()) {
                        if !path.is_empty() {
                            parts.push(format!("in {}", short_path(path, 2)));
                        }
                    }
                }
            }
        }
        "read" | "write" | "edit" | "find" | "ls" => {
            if let Some(path) = tool_path_from_args(args_summary) {
                parts.push(short_path(&path, 2));
            }
        }
        "web_search" => {
            if let Some(raw) = args_summary {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) {
                    if let Some(q) = v.get("query").and_then(|x| x.as_str()) {
                        let p = command_preview(q, 40);
                        if !p.is_empty() {
                            parts.push(p);
                        }
                    }
                }
            }
        }
        "web_fetch" => {
            if let Some(raw) = args_summary {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) {
                    if let Some(u) = v.get("url").and_then(|x| x.as_str()) {
                        parts.push(short_url(u, 44));
                    }
                }
            }
        }
        _ => {
            if let Some(arg) = tool_short_arg(name, args_summary) {
                parts.push(arg);
            }
        }
    }

    if let Some(diff_text) = diff.filter(|d| !d.trim().is_empty()) {
        let (added, removed) = diff_counts(diff_text);
        parts.push(format!("+{added} -{removed}"));
    } else {
        let lines = count_output_lines(output);
        if lines > 0 {
            parts.push(format!("{lines} line{}", if lines == 1 { "" } else { "s" }));
        }
    }

    parts.join(" · ")
}

/// Tool icons — Nerd Font PUA codepoints rendered with the dedicated `icons` font family.
pub(super) fn tool_icon(name: &str) -> &'static str {
    match name {
        "read" => "\u{f021b}",  // nf-md-file_document
        "write" => "\u{f0193}", // nf-md-file_edit
        "edit" => "\u{f03eb}",  // nf-md-pencil
        "bash" => "\u{f018d}",  // nf-md-console
        "grep" => "\u{f021e}",  // nf-md-file_find
        "find" => "\u{f0349}",  // nf-md-magnify
        "ls" => "\u{f0645}",    // nf-md-folder_open
        "web_search" => crate::theme::ICON_WEB_SEARCH,
        "web_fetch" => crate::theme::ICON_GLOBE,
        _ => "\u{f0214}", // nf-md-file
    }
}

/// Argument scurt, relevant, din `args_summary` JSON: path > command > prima valoare.
fn tool_short_arg(name: &str, args_summary: Option<&String>) -> Option<String> {
    let raw = args_summary?;
    let v = serde_json::from_str::<serde_json::Value>(raw).ok()?;
    // path / filePath pentru read/write/edit/find
    if let Some(p) = v
        .get("path")
        .or_else(|| v.get("filePath"))
        .and_then(|x| x.as_str())
    {
        // afișăm doar ultimele 2 segmente
        let segs: Vec<&str> = p.trim_start_matches('/').split('/').collect();
        let short = if segs.len() > 2 {
            format!("…/{}/{}", segs[segs.len() - 2], segs[segs.len() - 1])
        } else {
            p.to_string()
        };
        // adaugă range de linii dacă există
        let offset = v.get("offset").and_then(|x| x.as_u64());
        let limit = v.get("limit").and_then(|x| x.as_u64());
        return Some(match (offset, limit) {
            (Some(o), Some(l)) => format!("{short}  L{o}–{}", o + l - 1),
            (Some(o), None) => format!("{short}  L{o}+"),
            _ => short,
        });
    }
    // command pentru bash
    if let Some(cmd) = v.get("command").and_then(|x| x.as_str()) {
        let tok: String = cmd.split_whitespace().take(6).collect::<Vec<_>>().join(" ");
        let mut s: String = tok.chars().take(60).collect();
        if tok.chars().count() > 60 {
            s.push('…');
        }
        return Some(s);
    }
    // pattern + path pentru grep
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
    // URL scurtat pentru web_fetch
    if name == "web_fetch" {
        if let Some(u) = v.get("url").and_then(|x| x.as_str()) {
            return Some(short_url(u, 44));
        }
    }
    // fallback: prima string din obiect
    if let serde_json::Value::Object(map) = &v {
        if let Some(s) = map.values().find_map(|x| x.as_str()) {
            let mut t: String = s.chars().take(48).collect();
            if s.chars().count() > 48 {
                t.push('…');
            }
            return Some(t);
        }
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
