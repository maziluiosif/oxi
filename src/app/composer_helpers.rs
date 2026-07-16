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
    use super::truncate_label;

    #[test]
    fn truncate_label_respects_unicode_boundaries() {
        assert_eq!(truncate_label("mødel-α", 5), "mødel…");
        assert_eq!(truncate_label("short", 8), "short");
    }
}
