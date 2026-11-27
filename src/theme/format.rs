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
    selectable_text_job_with_links(ui, job, &[]);
}

/// A link's range counts characters, matching egui's selection cursors (not UTF-8 bytes).
pub struct TextLink {
    pub range: std::ops::Range<usize>,
    pub url: String,
}

/// Links share the paragraph's galley and selection, with click targets on their text only.
pub fn selectable_text_job_with_links(ui: &mut Ui, mut job: LayoutJob, links: &[TextLink]) {
    if job.text.is_empty() {
        return;
    }
    // set_fonts takes effect on the next frame; callers may render during setup.
    if ui.fonts(|fonts| {
        fonts
            .families()
            .contains(&eframe::egui::FontFamily::Name("emoji".into()))
    }) {
        use_emoji_font(&mut job);
    }
    let section_backgrounds = take_section_backgrounds(&mut job);
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
    let mut galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
    center_line_height_glyphs(&mut galley);
    let (rect, response) = ui.allocate_exact_size(galley.size(), Sense::click_and_drag());
    let saved = ui.visuals().selection;
    ui.visuals_mut().selection.bg_fill = editor_selection_fill();
    ui.visuals_mut().selection.stroke.color = dominant;
    let original = galley.clone();
    let background_slot = ui.painter().add(eframe::egui::Shape::Noop);
    let selection_slot = ui.painter().add(eframe::egui::Shape::Noop);
    let text_slot = ui
        .ctx()
        .graphics(|g| g.get(ui.layer_id()).unwrap().next_idx());
    eframe::egui::text_selection::LabelSelectionState::label_text_selection(
        ui,
        &response,
        rect.left_top(),
        galley,
        fallback,
        Stroke::NONE,
    );
    ui.visuals_mut().selection = saved;
    let backgrounds = section_background_shapes(&original, rect.left_top(), &section_backgrounds);
    // egui 0.35 exposes label selection through its painted galley. Each selected row has
    // exactly four appended background vertices. Keep its cross-label drag/copy handling,
    // replace only those quads with our editor contour, and preserve original glyph colors.
    let mut rects = Vec::new();
    ui.ctx().graphics_mut(|g| {
        if let Some(list) = g.get_mut(ui.layer_id()) {
            list.mutate_shape(text_slot, |shape| {
                if let eframe::egui::Shape::Text(text) = &mut shape.shape {
                    if !text
                        .galley
                        .rows
                        .iter()
                        .zip(&original.rows)
                        .any(|(selected, original)| {
                            selected.row.visuals.mesh.vertices.len()
                                == original.row.visuals.mesh.vertices.len() + 4
                        })
                    {
                        return;
                    }
                    let selected = std::sync::Arc::make_mut(&mut text.galley);
                    for (placed, original) in selected.rows.iter_mut().zip(&original.rows) {
                        let base = original.row.visuals.mesh.vertices.len();
                        if placed.row.visuals.mesh.vertices.len() == base + 4 {
                            let row = std::sync::Arc::make_mut(&mut placed.row);
                            let mut rect = eframe::egui::Rect::NOTHING;
                            for vertex in &mut row.visuals.mesh.vertices[base..] {
                                rect.extend_with(vertex.pos);
                                vertex.color = Color32::TRANSPARENT;
                            }
                            if rect.width() > 0.0 {
                                rects.push(
                                    rect.translate(placed.pos.to_vec2() + text.pos.to_vec2()),
                                );
                            }
                            for (vertex, source) in row.visuals.mesh.vertices[..base]
                                .iter_mut()
                                .zip(&original.row.visuals.mesh.vertices)
                            {
                                vertex.color = source.color;
                            }
                        }
                    }
                }
            });
        }
    });
    ui.painter()
        .set(background_slot, eframe::egui::Shape::Vec(backgrounds));
    ui.painter().set(
        selection_slot,
        crate::ui::text_selection::selection_shape(&rects, editor_selection_fill()),
    );
    if let Some(pointer) = ui.input(|i| i.pointer.interact_pos()) {
        for link in links {
            let range = eframe::egui::text::CCursorRange::two(
                eframe::egui::text::CCursor::new(link.range.start),
                eframe::egui::text::CCursor::new(link.range.end),
            );
            let rects = crate::ui::text_selection::galley_selection_rects(
                &original,
                rect.left_top(),
                range,
            );
            if response.contains_pointer() && rects.iter().any(|rect| rect.contains(pointer)) {
                ui.set_cursor_icon(eframe::egui::CursorIcon::PointingHand);
                if response.clicked_with_open_in_background() || response.clicked() {
                    ui.open_url(eframe::egui::OpenUrl {
                        url: link.url.clone(),
                        new_tab: response.clicked_with_open_in_background(),
                    });
                }
                if ui.style().url_in_tooltip {
                    response.clone().on_hover_text(&link.url);
                }
                break;
            }
        }
    }
}

/// Keep emoji graphemes in the emoji face. Font fallback otherwise chooses only the
/// base character: hearts and keycap digits resolve to Inter, losing their sequence.
/// Only formats change; byte/character offsets and the copied text remain intact.
fn use_emoji_font(job: &mut LayoutJob) {
    use eframe::egui::epaint::text::ByteIndex;
    use unicode_segmentation::UnicodeSegmentation as _;
    let emoji_ranges: Vec<_> = job
        .text
        .grapheme_indices(true)
        .filter_map(|(start, grapheme)| {
            let first = grapheme.chars().next()?;
            let emoji = matches!(first, '\u{1f000}'..='\u{1faff}')
                || grapheme.contains('\u{fe0f}')
                || grapheme.contains('\u{20e3}')
                || (matches!(first, '\u{2600}'..='\u{27bf}') && grapheme.contains('\u{200d}'));
            emoji.then_some(start..start + grapheme.len())
        })
        .collect();
    if emoji_ranges.is_empty() {
        return;
    }
    let mut sections = Vec::new();
    for section in std::mem::take(&mut job.sections) {
        let start = section.byte_range.start.0;
        let end = section.byte_range.end.0;
        let mut cursor = start;
        for range in emoji_ranges
            .iter()
            .skip(emoji_ranges.partition_point(|range| range.end <= start))
            .take_while(|range| range.start < end)
        {
            let a = range.start.max(start);
            let b = range.end.min(end);
            if a >= b {
                continue;
            }
            if cursor < a {
                let mut before = section.clone();
                before.byte_range = ByteIndex(cursor)..ByteIndex(a);
                if cursor != start {
                    before.leading_space = 0.0;
                }
                sections.push(before);
            }
            let mut emoji = section.clone();
            emoji.byte_range = ByteIndex(a)..ByteIndex(b);
            if a != start {
                emoji.leading_space = 0.0;
            }
            emoji.format.font_id.family = eframe::egui::FontFamily::Name("emoji".into());
            sections.push(emoji);
            cursor = b;
        }
        if cursor < end || start == end {
            let mut after = section;
            after.byte_range = ByteIndex(cursor)..ByteIndex(end);
            if cursor != start {
                after.leading_space = 0.0;
            }
            sections.push(after);
        }
    }
    job.sections = sections;
}

/// Center glyphs vertically inside an explicit `TextFormat::line_height`.
///
/// epaint's `galley_from_rows` applies `valign` to `max_row_height - glyph.line_height` rather
/// than `max_row_height - glyph.font_height`. When prose and inline code both set the same
/// `line_height` (e.g. `1.55x` body size), `max_row_height - glyph.line_height` is zero, so
/// `Align::Center` has no effect and every face sits top-aligned inside the row. Shorter faces
/// (such as smaller inline code monospace fonts) then ride visibly above the prose and above
/// their centered pill background.
fn center_line_height_glyphs(galley: &mut std::sync::Arc<eframe::egui::Galley>) {
    let has_centered_override =
        galley.job.sections.iter().any(|s| {
            s.format.line_height.is_some() && s.format.valign == eframe::egui::Align::Center
        });
    if !has_centered_override {
        return;
    }
    let ppp = if galley.pixels_per_point > 0.0 {
        galley.pixels_per_point
    } else {
        1.0
    };
    let galley_mut = std::sync::Arc::make_mut(galley);
    let job = galley_mut.job.clone();
    let mut chars = job.text.char_indices();
    let mut section_idx = 0usize;
    let mut galley_bounds = eframe::egui::Rect::NOTHING;
    for placed in &mut galley_mut.rows {
        let mut shifts: Vec<(usize, usize, f32)> = Vec::new();
        {
            let glyphs = &placed.row.glyphs;
            let glyph_vertex_end = placed.row.visuals.glyph_vertex_range.end;
            for (i, glyph) in glyphs.iter().enumerate() {
                let byte = chars.next().map_or(job.text.len(), |(byte, _)| byte);
                while section_idx + 1 < job.sections.len()
                    && job.sections[section_idx].byte_range.end.0 <= byte
                {
                    section_idx += 1;
                }
                let Some(section) = job.sections.get(section_idx) else {
                    continue;
                };
                if section.format.line_height.is_none()
                    || section.format.valign != eframe::egui::Align::Center
                {
                    continue;
                }
                let raw_dy = 0.5 * (glyph.line_height - glyph.font_height);
                let dy = (raw_dy * ppp).round() / ppp;
                if dy == 0.0 {
                    continue;
                }
                let v_start = glyph.first_vertex as usize;
                let v_end = glyphs
                    .get(i + 1)
                    .map_or(glyph_vertex_end, |next| next.first_vertex as usize);
                if v_start < v_end {
                    shifts.push((v_start, v_end, dy));
                }
            }
        }
        if placed.ends_with_newline {
            chars.next();
        }
        if !shifts.is_empty() {
            let row = std::sync::Arc::make_mut(&mut placed.row);
            let vertex_len = row.visuals.mesh.vertices.len();
            for (v_start, v_end, dy) in shifts {
                let end = v_end.min(vertex_len);
                if v_start < end {
                    for vertex in &mut row.visuals.mesh.vertices[v_start..end] {
                        vertex.pos.y += dy;
                    }
                }
            }
            row.visuals.mesh_bounds = row.visuals.mesh.calc_bounds();
        }
        galley_bounds |= placed
            .row
            .visuals
            .mesh_bounds
            .translate(placed.pos.to_vec2());
    }
    galley_mut.mesh_bounds = galley_bounds;
}

/// Clears every section background in `job` and returns them by section index, for
/// [`section_background_shapes`]. egui paints section backgrounds as square boxes grown a point
/// past the row on every side, which hug the glyphs and run into the row above and below.
fn take_section_backgrounds(job: &mut LayoutJob) -> Vec<Color32> {
    job.sections
        .iter_mut()
        .map(|section| std::mem::replace(&mut section.format.background, Color32::TRANSPARENT))
        .collect()
}

/// Rounded pills behind each run of glyphs that had a section background (inline code): a little
/// air left and right of the text, and a point clear of the neighboring rows.
fn section_background_shapes(
    galley: &eframe::egui::Galley,
    origin: Pos2,
    backgrounds: &[Color32],
) -> Vec<eframe::egui::Shape> {
    use eframe::egui::{CornerRadius, Rect, Shape};
    const PAD_X: f32 = 2.5;
    const INSET_Y: f32 = 2.5;
    if backgrounds
        .iter()
        .all(|color| *color == Color32::TRANSPARENT)
    {
        return Vec::new();
    }
    // Rows hold one glyph per char (the newline excepted), so walking the job's text alongside
    // them gives each glyph's byte offset, and from it its section.
    let job = &galley.job;
    let mut chars = job.text.char_indices();
    let mut section = 0;
    let mut shapes = Vec::new();
    for placed in &galley.rows {
        let row_rect = placed.rect().translate(origin.to_vec2());
        let mut run: Option<(Color32, f32, f32)> = None;
        let mut flush = |run: Option<(Color32, f32, f32)>| {
            if let Some((color, left, right)) = run {
                let rect = Rect::from_x_y_ranges(
                    left - PAD_X..=right + PAD_X,
                    row_rect.top() + INSET_Y..=row_rect.bottom() - INSET_Y,
                );
                shapes.push(Shape::rect_filled(rect, CornerRadius::same(4), color));
            }
        };
        for glyph in &placed.row.glyphs {
            let byte = chars.next().map_or(job.text.len(), |(byte, _)| byte);
            while section + 1 < job.sections.len() && job.sections[section].byte_range.end.0 <= byte
            {
                section += 1;
            }
            let color = backgrounds
                .get(section)
                .copied()
                .unwrap_or(Color32::TRANSPARENT);
            let left = row_rect.left() + glyph.pos.x;
            let right = row_rect.left() + glyph.max_x();
            match &mut run {
                Some((run_color, _, run_right)) if *run_color == color => *run_right = right,
                _ => {
                    flush(run.take());
                    if color != Color32::TRANSPARENT {
                        run = Some((color, left, right));
                    }
                }
            }
        }
        flush(run);
        if placed.ends_with_newline {
            chars.next();
        }
    }
    shapes
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

/// Whole-second elapsed label for live timers: "0s", "42s", "3m 05s", "1h 02m". Sub-second
/// precision would make the label flicker on every repaint.
pub fn format_stream_elapsed(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        return format!("{s}s");
    }
    let m = s / 60;
    if m < 60 {
        return format!("{m}m {:02}s", s % 60);
    }
    format!("{}h {:02}m", m / 60, m % 60)
}

/// `128000` → `"128,000"`.
pub fn group_thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
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

/// The project's folder name alone (`oxi` instead of `Projects/oxi`), as the breadcrumb and
/// the sidebar show it. Remote projects add their host.
pub fn workspace_short_label(root_path: &str) -> String {
    if crate::app_dirs::is_general_chats_dir(root_path) {
        return "General".to_string();
    }
    if let Some((host, path)) = crate::remote::split(root_path) {
        let trimmed = path.trim_end_matches('/');
        let name = trimmed
            .rsplit('/')
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or(trimmed);
        return format!("{name} · {host}");
    }
    std::path::Path::new(root_path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| workspace_sidebar_label(root_path))
}

/// Short label for a workspace root path (last two path segments, e.g. `owner/repo`).
pub fn workspace_sidebar_label(root_path: &str) -> String {
    if let Some((host, path)) = crate::remote::split(root_path) {
        let path = path.trim_end_matches('/');
        let name = if path.ends_with("/oxi-general") {
            "General"
        } else {
            path.rsplit('/')
                .next()
                .filter(|n| !n.is_empty())
                .unwrap_or(path)
        };
        return format!("{name} · {host}");
    }
    if crate::app_dirs::is_general_chats_dir(root_path) {
        return "General".to_string();
    }
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
    fn group_thousands_inserts_commas() {
        assert_eq!(group_thousands(0), "0");
        assert_eq!(group_thousands(999), "999");
        assert_eq!(group_thousands(128_000), "128,000");
        assert_eq!(group_thousands(1_048_576), "1,048,576");
    }

    #[test]
    fn format_stream_elapsed_whole_seconds() {
        assert_eq!(format_stream_elapsed(Duration::from_millis(237)), "0s");
        assert_eq!(format_stream_elapsed(Duration::from_secs(42)), "42s");
        assert_eq!(format_stream_elapsed(Duration::from_secs(185)), "3m 05s");
        assert_eq!(format_stream_elapsed(Duration::from_secs(3720)), "1h 02m");
    }

    #[test]
    fn tool_status_label_humanizes_names() {
        assert_eq!(tool_status_label("web_search"), "Web search");
        assert_eq!(tool_status_label("bash"), "Bash");
        assert_eq!(tool_status_label("  "), "Running");
    }
}

#[cfg(test)]
mod selection_tests {
    use super::*;
    use eframe::egui::{self, Event, Modifiers, PointerButton, RawInput, Rect};

    fn frame(ctx: &egui::Context, events: Vec<Event>) -> egui::FullOutput {
        ctx.run_ui(
            RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(500.0, 350.0))),
                events,
                ..Default::default()
            },
            |ui| {
                for text in ["first paragraph\nsecond row", "third paragraph"] {
                    let mut job = LayoutJob::default();
                    job.append(
                        text,
                        0.0,
                        egui::text::TextFormat::simple(FontId::monospace(16.0), Color32::WHITE),
                    );
                    job.wrap.max_width = 400.0;
                    selectable_text_job(ui, job);
                }
            },
        )
    }

    #[test]
    fn emoji_sequences_shape_as_one_visible_glyph_and_keep_original_text() {
        let ctx = egui::Context::default();
        crate::theme::setup_style(&ctx);
        let _ = ctx.run_ui(RawInput::default(), |_| {});
        for sample in ["1️⃣", "#️⃣", "❤️‍🔥", "❤️‍🩹", "👩‍💻", "🇷🇴"]
        {
            let mut job = LayoutJob::simple(
                sample.into(),
                FontId::proportional(20.0),
                Color32::WHITE,
                400.0,
            );
            use_emoji_font(&mut job);
            let galley = ctx.fonts_mut(|fonts| fonts.layout_job(job));
            assert_eq!(galley.text(), sample);
            let visible = galley
                .rows
                .iter()
                .flat_map(|r| &r.glyphs)
                .filter(|g| g.advance_width > 0.0)
                .count();
            assert_eq!(visible, 1, "sequence not shaped: {sample}");
        }
    }

    #[test]
    fn custom_chat_selection_preserves_copy_across_blocks() {
        let ctx = egui::Context::default();
        let output = frame(&ctx, vec![]);
        let text: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|s| {
                if let egui::Shape::Text(t) = &s.shape {
                    Some(t)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(text.len(), 2);
        let start = text[0].pos + egui::vec2(0.0, 8.0);
        let end = text[1].pos + egui::vec2(text[1].galley.size().x, 8.0);
        frame(
            &ctx,
            vec![
                Event::PointerMoved(start),
                Event::PointerButton {
                    pos: start,
                    button: PointerButton::Primary,
                    pressed: true,
                    modifiers: Modifiers::NONE,
                },
            ],
        );
        let selected = frame(&ctx, vec![Event::PointerMoved(end)]);
        let contours = selected.shapes.iter().filter(|s| matches!(&s.shape, egui::Shape::Vec(shapes) if shapes.iter().any(|s| matches!(s, egui::Shape::Path(_))))).count();
        assert_eq!(contours, 2, "one outer contour per selected text block");
        frame(
            &ctx,
            vec![Event::PointerButton {
                pos: end,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            }],
        );
        let copied = frame(&ctx, vec![Event::Copy]);
        assert!(copied.platform_output.commands.iter().any(|command| matches!(command, egui::OutputCommand::CopyText(text) if text.contains("first paragraph") && text.contains("second row") && text.contains("third paragraph"))));
    }

    #[test]
    fn inline_code_background_is_below_selection_and_glyphs_keep_colors() {
        let ctx = egui::Context::default();
        let render = |events| {
            ctx.run_ui(
                RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(500.0, 350.0))),
                    events,
                    ..Default::default()
                },
                |ui| {
                    let mut job = LayoutJob::default();
                    job.append(
                        "before ",
                        0.0,
                        TextFormat::simple(FontId::proportional(16.0), Color32::WHITE),
                    );
                    let mut code = TextFormat::simple(FontId::proportional(16.0), Color32::YELLOW);
                    code.background = Color32::DARK_GRAY;
                    job.append("inline code", 0.0, code);
                    job.append(
                        " after",
                        0.0,
                        TextFormat::simple(FontId::proportional(16.0), Color32::WHITE),
                    );
                    selectable_text_job(ui, job);
                },
            )
        };
        let initial = render(vec![]);
        let original = initial
            .shapes
            .iter()
            .find_map(|s| {
                if let egui::Shape::Text(t) = &s.shape {
                    Some(t)
                } else {
                    None
                }
            })
            .unwrap();
        let start = original.pos + egui::vec2(1.0, 8.0);
        let end = original.pos + egui::vec2(original.galley.size().x, 8.0);
        render(vec![
            Event::PointerMoved(start),
            Event::PointerButton {
                pos: start,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ]);
        let selected = render(vec![Event::PointerMoved(end)]);
        let text_index = selected
            .shapes
            .iter()
            .position(|s| matches!(s.shape, egui::Shape::Text(_)))
            .unwrap();
        let contour_index = selected.shapes.iter().position(|s| matches!(&s.shape, egui::Shape::Vec(v) if v.iter().any(|s| matches!(s, egui::Shape::Path(_))))).unwrap();
        let background_index = selected.shapes.iter().position(|s| matches!(&s.shape, egui::Shape::Vec(v) if v.iter().any(|s| matches!(s, egui::Shape::Rect(r) if r.fill == Color32::DARK_GRAY)))).unwrap();
        assert!(background_index < contour_index && contour_index < text_index);
        let egui::Shape::Text(text) = &selected.shapes[text_index].shape else {
            unreachable!()
        };
        assert_eq!(text.galley.rows[0].row.visuals.glyph_index_start, 0);
        for (vertex, original) in text.galley.rows[0]
            .row
            .visuals
            .mesh
            .vertices
            .iter()
            .zip(&original.galley.rows[0].row.visuals.mesh.vertices)
        {
            assert_eq!(vertex.color, original.color);
        }
    }

    #[test]
    #[ignore = "UI review artifact; run with --ignored"]
    fn render_multilingual_font_review() {
        crate::theme::system_cjk_fonts();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(850.0, 600.0))
            .wgpu()
            .build_ui(|ui| {
                crate::theme::setup_style(ui.ctx());
                for sample in [
                    "Română: Știință, învățământ, țară, pâine, înghețată.",
                    "日本語: こんにちは 世界",
                    "中文: 你好，世界",
                    "한국어: 안녕하세요 세계",
                    "العربية: مرحبًا بالعالم",
                    "עברית: שלום עולם",
                    "हिन्दी: नमस्ते दुनिया",
                    "Emoji: ❤️‍🔥 ❤️‍🩹 👩‍💻 👨‍🚀 👨‍👩‍👧‍👦 🏳️‍🌈 🇷🇴 🇯🇵",
                    "Keycaps: 1️⃣ 2️⃣ 3️⃣ #️⃣ *️⃣  plain: 123#*",
                    "Monochrome tones: 👍 👍🏻 👍🏼 👍🏽 👍🏾 👍🏿",
                ] {
                    selectable_text_job(
                        ui,
                        LayoutJob::simple(
                            sample.into(),
                            FontId::proportional(22.0),
                            Color32::WHITE,
                            820.0,
                        ),
                    );
                    ui.add_space(8.0);
                }
                selectable_text_job(
                    ui,
                    LayoutJob::simple(
                        "Code: 日本語 中文 한국어 हिन्दी 👩‍💻 1️⃣ ❤️‍🔥".into(),
                        FontId::monospace(20.0),
                        Color32::WHITE,
                        820.0,
                    ),
                );
            });
        harness.run_steps(3);
        harness
            .render()
            .unwrap()
            .save("/tmp/oxi-multilingual-font-review.png")
            .unwrap();
    }

    #[test]
    #[ignore = "UI review artifact; run with --ignored"]
    fn render_selection_review() {
        let mut harness = egui_kittest::Harness::builder().with_size(egui::vec2(650.0, 420.0)).wgpu().build_ui(|ui| {
            crate::theme::apply_theme(ui.ctx(), "dark");
            ui.label("Chat selection uses the editor contour");
            let mut job = LayoutJob::default();
            job.append("First selected row of the chat.\nA longer middle row without an internal border.\nLast selected row.", 0.0, egui::text::TextFormat::simple(FontId::proportional(16.0), Color32::WHITE));
            let mut code = TextFormat::simple(FontId::proportional(16.0), crate::theme::c_md_code_fg());
            code.background = crate::theme::c_md_code_bg();
            job.append(" Inline code selection", 0.0, code);
            job.wrap.max_width = 550.0;
            selectable_text_job(ui, job);
            ui.separator();
            ui.button("pasted-1.txt · 120 lines").on_hover_text("Click to preview the full text");
            let rects = vec![Rect::from_min_max(egui::pos2(20.0, 210.0), egui::pos2(480.0, 228.0)), Rect::from_min_max(egui::pos2(20.0, 228.0), egui::pos2(600.0, 246.0)), Rect::from_min_max(egui::pos2(20.0, 246.0), egui::pos2(200.0, 264.0))];
            crate::ui::text_selection::paint_selection(ui.painter(), &rects, editor_selection_fill());
            for (row, text) in ["Terminal selected row", "Middle row joins continuously", "Last row"].iter().enumerate() {
                ui.painter().text(egui::pos2(22.0, 210.0 + row as f32 * 18.0), egui::Align2::LEFT_TOP, text, FontId::monospace(14.0), Color32::WHITE);
            }
        });
        harness.run_steps(3);
        harness.event(Event::PointerMoved(egui::pos2(10.0, 40.0)));
        harness.event(Event::PointerButton {
            pos: egui::pos2(10.0, 40.0),
            button: PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        });
        harness.step();
        harness.event(Event::PointerMoved(egui::pos2(280.0, 84.0)));
        harness.step();
        harness
            .render()
            .unwrap()
            .save("/tmp/oxi-selection-review.png")
            .unwrap();
    }
}
