//! Small formatting/animation helpers used throughout the transcript and sidebar UI.

use std::time::Duration;

use eframe::egui::text::{LayoutJob, TextFormat};
use eframe::egui::{Color32, FontId, Label, Painter, Pos2, Sense, Stroke, Ui};

use super::palette::{active_palette, c_accent, c_bg_main, c_text_muted};

/// Human-readable byte size (B / MB / GB).
pub fn fmt_bytes(n: u64) -> String {
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    if n as f64 >= GB {
        format!("{:.2} GB", n as f64 / GB)
    } else if n as f64 >= MB {
        format!("{:.1} MB", n as f64 / MB)
    } else {
        format!("{n} B")
    }
}

pub fn blend_color(from: Color32, to: Color32, t: f32) -> Color32 {
    let mix = t.clamp(0.0, 1.0);
    let lerp = |a: u8, b: u8| -> u8 {
        let af = a as f32;
        let bf = b as f32;
        (af + (bf - af) * mix).round().clamp(0.0, 255.0) as u8
    };
    Color32::from_rgba_unmultiplied(
        lerp(from.r(), to.r()),
        lerp(from.g(), to.g()),
        lerp(from.b(), to.b()),
        lerp(from.a(), to.a()),
    )
}

/// The workspace editor's selection wash: `selection_bg` blended over the main background.
/// Shared by the editor and every selectable text run so selections look identical app-wide.
pub fn editor_selection_fill() -> Color32 {
    blend_color(c_bg_main(), active_palette().selection_bg, 0.80)
}

/// Lay out `job` and run egui's label text selection styled like the workspace editor.
/// egui recolors selected glyphs to `selection.stroke.color`; feeding it the job's dominant
/// section color keeps the text visually unchanged, so only the wash marks the selection.
pub fn selectable_text_job(ui: &mut Ui, job: LayoutJob) {
    if job.text.is_empty() {
        return;
    }
    let dominant = job
        .sections
        .iter()
        .max_by_key(|section| {
            section
                .byte_range
                .end
                .0
                .saturating_sub(section.byte_range.start.0)
        })
        .map(|section| section.format.color)
        .unwrap_or_else(|| ui.style().visuals.text_color());
    let fallback = ui.style().visuals.text_color();
    let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
    let (rect, response) = ui.allocate_exact_size(galley.size(), Sense::click_and_drag());
    let saved = ui.visuals().selection;
    ui.visuals_mut().selection.bg_fill = editor_selection_fill();
    ui.visuals_mut().selection.stroke.color = dominant;
    eframe::egui::text_selection::LabelSelectionState::label_text_selection(
        ui,
        &response,
        rect.left_top(),
        galley,
        fallback,
        Stroke::NONE,
    );
    ui.visuals_mut().selection = saved;
}

pub fn animated_status_job(label: &str, size: f32, time: f64) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = f32::INFINITY;
    let chars: Vec<char> = label.chars().collect();
    let len = chars.len().max(1) as f64;
    let highlight = (time * 7.0) % (len + 3.0);
    for (idx, ch) in chars.iter().enumerate() {
        let dist = (idx as f64 - highlight).abs();
        let mix = if dist < 0.6 {
            1.0
        } else if dist < 1.4 {
            0.55
        } else if dist < 2.2 {
            0.22
        } else {
            0.0
        };
        let color = blend_color(c_text_muted(), c_accent(), mix as f32);
        job.append(
            &ch.to_string(),
            0.0,
            TextFormat::simple(FontId::proportional(size), color),
        );
    }
    job
}

pub fn animated_status_label(ui: &mut Ui, label: &str, size: f32) {
    let time = ui.input(|i| i.time);
    ui.add(Label::new(animated_status_job(label, size, time)).selectable(false));
}

/// Three dots that pulse in and out in sequence, left to right — a compact "still working"
/// indicator for spots too small for [`animated_status_label`] (e.g. inside a round icon
/// button). Caller is responsible for requesting repaints while this is visible; the dots
/// only animate as often as the surrounding UI redraws.
pub fn paint_three_dots(
    painter: &Painter,
    center: Pos2,
    time: f64,
    color: Color32,
    dot_radius: f32,
) {
    let spacing = dot_radius * 3.0;
    for i in 0..3 {
        let phase = time * 3.2 - i as f64 * 0.5;
        let alpha = (0.25 + 0.75 * (0.5 + 0.5 * phase.sin())) as f32;
        let pos = center + eframe::egui::vec2((i as f32 - 1.0) * spacing, 0.0);
        painter.circle_filled(pos, dot_radius, color.gamma_multiply(alpha.clamp(0.0, 1.0)));
    }
}

pub fn format_stream_elapsed(d: Duration) -> String {
    let total_ms = d.as_millis() as u64;
    if total_ms < 1000 {
        return format!("{total_ms}ms");
    }
    let s = total_ms / 1000;
    if s < 60 {
        return format!("{s}s");
    }
    let m = s / 60;
    let rs = s % 60;
    format!("{m}m{rs:02}")
}

/// Coarse "time ago" label for sidebar rows: "now", "5m", "6h", "18h", "3d".
pub fn format_relative_time(t: std::time::SystemTime) -> String {
    let elapsed = std::time::SystemTime::now()
        .duration_since(t)
        .unwrap_or_default();
    let s = elapsed.as_secs();
    if s < 60 {
        return "now".to_string();
    }
    let m = s / 60;
    if m < 60 {
        return format!("{m}m");
    }
    let h = m / 60;
    if h < 24 {
        return format!("{h}h");
    }
    format!("{}d", h / 24)
}

/// Short label for a workspace root path (last two path segments, e.g. `owner/repo`).
pub fn workspace_sidebar_label(root_path: &str) -> String {
    let path = std::path::Path::new(root_path);
    let parts: Vec<&str> = path
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect();
    match parts.len() {
        0 => root_path.to_string(),
        1 => parts[0].to_string(),
        _ => format!("{}/{}", parts[parts.len() - 2], parts[parts.len() - 1]),
    }
}

/// Full title for sidebar rows; empty/whitespace shows as "New chat". Ellipsis is handled by
/// [`egui::Label::truncate`] with the row’s title width.
pub fn sidebar_session_title_display(title: &str) -> String {
    let t = title.trim();
    if t.is_empty() {
        "New chat".to_string()
    } else {
        t.to_string()
    }
}

pub fn tool_status_label(name: &str) -> String {
    let trimmed = name.trim().replace('_', " ");
    if trimmed.is_empty() {
        "Running".to_string()
    } else {
        let mut chars = trimmed.chars();
        let first = chars
            .next()
            .map(|ch| ch.to_uppercase().collect::<String>())
            .unwrap_or_default();
        format!("{}{rest}", first, rest = chars.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_relative_time_coarse_units() {
        use std::time::SystemTime;
        let now = SystemTime::now();
        assert_eq!(format_relative_time(now), "now");
        assert_eq!(format_relative_time(now - Duration::from_secs(59)), "now");
        assert_eq!(
            format_relative_time(now - Duration::from_secs(5 * 60)),
            "5m"
        );
        assert_eq!(
            format_relative_time(now - Duration::from_secs(6 * 3600)),
            "6h"
        );
        assert_eq!(
            format_relative_time(now - Duration::from_secs(18 * 3600)),
            "18h"
        );
        assert_eq!(
            format_relative_time(now - Duration::from_secs(3 * 86_400)),
            "3d"
        );
        // Future timestamps (clock skew) clamp to "now" rather than underflowing.
        assert_eq!(format_relative_time(now + Duration::from_secs(3600)), "now");
    }

    #[test]
    fn tool_status_label_humanizes_names() {
        assert_eq!(tool_status_label("web_search"), "Web search");
        assert_eq!(tool_status_label("bash"), "Bash");
        assert_eq!(tool_status_label("  "), "Running");
    }
}
