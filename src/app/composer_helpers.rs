//! Pure formatting, painting, and context-estimation helpers for the composer.

use eframe::egui::{self, Color32, Pos2, Stroke, Ui};

use crate::model::{AssistantBlock, ChatMessage, MsgRole};
use crate::theme::{c_accent, c_danger, c_warning_fg};

/// Truncate by characters, not bytes: provider/model identifiers may contain non-ASCII text.
pub(super) fn truncate_label(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let prefix: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}

/// Compact display name for a model id in the composer combo.
///
/// Strips org/router prefixes (`openai/…`, `anthropic/…`), path segments, `.gguf`
/// extensions, and trailing dated pins (`-20250929`, `-2024-07-18`) so the selected
/// text stays a stable length across providers.
pub(super) fn short_model_label(model_id: &str, max_chars: usize) -> String {
    let trimmed = model_id.trim();
    if trimmed.is_empty() {
        return "(custom)".to_string();
    }
    let base = trimmed.rsplit(['/', '\\']).next().unwrap_or(trimmed);
    let base = base
        .strip_suffix(".gguf")
        .or_else(|| base.strip_suffix(".GGUF"))
        .unwrap_or(base);
    let base = strip_dated_model_suffix(base);
    truncate_label(base, max_chars)
}

fn strip_dated_model_suffix(s: &str) -> &str {
    // `…-20250929` (8-digit date pin)
    if s.len() > 9 {
        let tail = &s[s.len() - 9..];
        if tail.as_bytes()[0] == b'-' && tail.as_bytes()[1..].iter().all(u8::is_ascii_digit) {
            return &s[..s.len() - 9];
        }
    }
    // `…-2024-07-18`
    if s.len() > 11 {
        let tail = &s[s.len() - 11..];
        let b = tail.as_bytes();
        if b[0] == b'-'
            && b[5] == b'-'
            && b[8] == b'-'
            && b[1..5].iter().all(u8::is_ascii_digit)
            && b[6..8].iter().all(u8::is_ascii_digit)
            && b[9..11].iter().all(u8::is_ascii_digit)
        {
            return &s[..s.len() - 11];
        }
    }
    s
}

pub(super) fn paint_arc(
    ui: &Ui,
    center: Pos2,
    radius: f32,
    start: f32,
    sweep: f32,
    stroke: Stroke,
) {
    if sweep <= 0.0 {
        return;
    }
    let segments = ((sweep.abs() / std::f32::consts::TAU) * 48.0)
        .ceil()
        .max(6.0) as usize;
    let mut points = Vec::with_capacity(segments + 1);
    for i in 0..=segments {
        let t = start + sweep * (i as f32 / segments as f32);
        points.push(Pos2::new(
            center.x + radius * t.cos(),
            center.y + radius * t.sin(),
        ));
    }
    ui.painter().add(egui::Shape::line(points, stroke));
}

pub(super) fn context_indicator_color(pct: f32) -> Color32 {
    if pct >= 0.9 {
        c_danger()
    } else if pct >= 0.75 {
        c_warning_fg()
    } else {
        c_accent()
    }
}

pub(super) fn format_context_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}m", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

pub(super) fn estimate_message_chars(m: &ChatMessage) -> usize {
    match m.role {
        MsgRole::User => {
            m.text.len()
                + m.attachments
                    .iter()
                    .map(|a| match a {
                        crate::model::UserAttachment::Image { data, .. } => data.len() * 4 / 3,
                    })
                    .sum::<usize>()
        }
        MsgRole::Assistant => m
            .blocks
            .iter()
            .map(|b| match b {
                AssistantBlock::Thinking(t) | AssistantBlock::Answer(t) => t.len(),
                AssistantBlock::Tool {
                    name,
                    output,
                    args_summary,
                    ..
                } => {
                    name.len()
                        + output.len().min(8_000)
                        + args_summary.as_deref().unwrap_or("").len()
                }
            })
            .sum(),
    }
}

#[cfg(test)]
mod tests {
    use super::{short_model_label, truncate_label};

    #[test]
    fn truncate_label_respects_unicode_boundaries() {
        assert_eq!(truncate_label("mødel-α", 5), "mødel…");
        assert_eq!(truncate_label("short", 8), "short");
    }

    #[test]
    fn short_model_label_strips_prefixes_and_dates() {
        assert_eq!(short_model_label("openai/gpt-4o-mini", 20), "gpt-4o-mini");
        assert_eq!(
            short_model_label("anthropic/claude-sonnet-4-5-20250929", 24),
            "claude-sonnet-4-5"
        );
        assert_eq!(
            short_model_label("gpt-4o-mini-2024-07-18", 20),
            "gpt-4o-mini"
        );
        assert_eq!(
            short_model_label("org/repo/qwen2.5-coder-7b.gguf", 24),
            "qwen2.5-coder-7b"
        );
        assert_eq!(short_model_label("qwen2.5-coder:7b", 20), "qwen2.5-coder:7b");
        assert_eq!(short_model_label("", 10), "(custom)");
        assert_eq!(short_model_label("claude-sonnet-4-5", 10), "claude-son…");
    }
}
