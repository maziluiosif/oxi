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
    // egui 0.35 exposes label selection through its painted galley. Each selected row has
    // exactly four appended background vertices. Keep its cross-label drag/copy handling,
    // replace only those quads with our editor contour, and preserve original glyph colors.
    let mut rects = Vec::new();
    let mut backgrounds = Vec::new();
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
                            // Paint section backgrounds (including inline code) below the
                            // continuous contour, while keeping glyphs above it.
                            let background_end = row.visuals.glyph_index_start;
                            if background_end > 0 {
                                let mut mesh = row.visuals.mesh.clone();
                                mesh.indices = mesh.indices[..background_end].to_vec();
                                mesh.translate(placed.pos.to_vec2() + text.pos.to_vec2());
                                backgrounds.push(eframe::egui::Shape::mesh(mesh));
                                row.visuals.mesh.indices.drain(..background_end);
                                row.visuals.glyph_index_start = 0;
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
        let background_index = selected.shapes.iter().position(|s| matches!(&s.shape, egui::Shape::Vec(v) if v.iter().any(|s| matches!(s, egui::Shape::Mesh(_))))).unwrap();
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
