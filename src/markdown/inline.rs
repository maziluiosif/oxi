//! Inline text formatting: the run of text/code/link/image events between block
//! boundaries, laid out into a single [`LayoutJob`] per run.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use base64::Engine as _;
use eframe::egui::load::BytesPoll;
use eframe::egui::text::{LayoutJob, TextFormat};
use eframe::egui::{Align, CornerRadius, CursorIcon, FontId, Image, RichText, Sense, Stroke, Ui};
use pulldown_cmark::{Event, Tag, TagEnd};

use crate::theme::*;

use super::{ParserPeek, SZ_BODY, SZ_CODE_INLINE, consume_until_end, set_job_wrap};

#[derive(Clone, Copy)]
pub(super) enum InlineEnd {
    Paragraph,
    Item,
}

/// Tighter line metrics for list rows vs normal paragraphs.
#[derive(Clone, Copy)]
pub(super) enum InlineDensity {
    Normal,
    ListItem,
}

impl InlineDensity {
    pub(super) fn line_height(self, size: f32) -> f32 {
        // One airy line-height for all flowing prose (paragraphs and list items) so wrapped lines
        // breathe comfortably without crowding inline code pills.
        match self {
            InlineDensity::Normal | InlineDensity::ListItem => (size * 1.55).max(18.0),
        }
    }
}

pub(super) fn inline_text_format(
    size: f32,
    strong: u32,
    italic: bool,
    density: InlineDensity,
) -> TextFormat {
    let color = if strong > 0 {
        c_text_strong()
    } else {
        c_text()
    };
    let lh = density.line_height(size);
    let mut f = TextFormat::simple(prose_font(size, strong), color);
    f.italics = italic;
    f.line_height = Some(lh);
    f.valign = Align::Center;
    f
}

pub(super) fn inline_code_format(density: InlineDensity) -> TextFormat {
    // Monospace at a slightly tighter optical size inside a rounded pill (all bundled code
    // faces are optically centered so the glyphs sit cleanly on the prose line).
    let lh = density.line_height(SZ_BODY);
    let mut f = TextFormat::simple(FontId::monospace(SZ_CODE_INLINE), c_md_code_fg());
    f.background = c_md_code_bg();
    f.line_height = Some(lh);
    f.valign = Align::Center;
    f
}

#[inline]
fn inline_image_uri_id(uri: &str) -> u64 {
    let mut h = DefaultHasher::new();
    uri.hash(&mut h);
    h.finish()
}

/// `data:image/...;base64,...` only (common for embedded thumbnails).
fn try_decode_data_url_image(uri: &str) -> Option<Vec<u8>> {
    let rest = uri.strip_prefix("data:")?;
    let comma = rest.find(',')?;
    let header = &rest[..comma];
    let data = rest[comma + 1..].trim();
    if !header.contains("base64") {
        return None;
    }
    base64::engine::general_purpose::STANDARD
        .decode(data.as_bytes())
        .ok()
}

pub(super) fn render_markdown_inline_image(
    ui: &mut Ui,
    wrap_w: f32,
    dest_url: &str,
    alt: &str,
    compact: bool,
) {
    let max_w = if compact {
        (wrap_w * 0.98).clamp(24.0, 220.0)
    } else {
        wrap_w.clamp(32.0, 560.0)
    };
    let img: Image<'_> = if let Some(bytes) = try_decode_data_url_image(dest_url) {
        let id = format!("bytes://md-inline-{}", inline_image_uri_id(dest_url));
        Image::from_bytes(id, bytes)
    } else if dest_url.starts_with("https://")
        || dest_url.starts_with("http://")
        || dest_url.starts_with("file://")
    {
        Image::from_uri(dest_url.to_owned())
    } else {
        ui.label(
            RichText::new(format!("![{alt}]({dest_url})"))
                .monospace()
                .size(SZ_CODE_INLINE)
                .color(c_text_muted()),
        );
        ui.add_space(4.0);
        return;
    };
    // Native size capped to the column: the default `Fraction` fit scales to the remaining
    // space of the wrapped row, which is only a line tall, so images rendered as tiny thumbs.
    let max_h = if compact { 120.0 } else { 480.0 };
    let img = img
        .fit_to_original_size(1.0)
        .max_size(eframe::egui::vec2(max_w, max_h))
        .corner_radius(CornerRadius::same(crate::theme::RADIUS_BUTTON))
        .show_loading_spinner(true)
        .sense(Sense::click());
    let uri = img.source(ui.ctx()).uri().map(str::to_owned);
    let resp = ui.add(img).on_hover_cursor(CursorIcon::ZoomIn);
    if resp.clicked()
        && let Some(uri) = uri
        && let Ok(BytesPoll::Ready { bytes, .. }) = ui.ctx().try_load_bytes(&uri)
    {
        crate::ui::image_viewer::open(ui.ctx(), &bytes);
    }
    if !alt.is_empty() {
        if dest_url.starts_with("http://") || dest_url.starts_with("https://") {
            resp.on_hover_text(format!("{alt}\n\n{dest_url}"));
        } else {
            resp.on_hover_text(alt);
        }
    } else if dest_url.starts_with("http://") || dest_url.starts_with("https://") {
        resp.on_hover_text(dest_url);
    }
    ui.add_space(if compact { 2.0 } else { 4.0 });
}

pub(super) fn append_inline_fallback(job: &mut LayoutJob, wrap_w: f32, s: &str, line_height: f32) {
    let mut f = TextFormat::simple(FontId::monospace(SZ_CODE_INLINE), c_md_code_fg());
    f.line_height = Some(line_height);
    f.valign = Align::Center;
    job.append(s, 0.0, f);
    set_job_wrap(job, wrap_w);
}

/// Prose font: [`strong_font`] (a semibold face when the UI font has one) for bold runs.
fn prose_font(size: f32, strong: u32) -> FontId {
    if strong > 0 {
        FontId::new(size, strong_font())
    } else {
        FontId::proportional(size)
    }
}

pub(super) fn fmt_body(size: f32, strong: u32) -> TextFormat {
    let color = if strong > 0 {
        c_text_strong()
    } else {
        c_text()
    };
    let lh = (size * 1.55).max(18.0);
    let mut f = TextFormat::simple(prose_font(size, strong), color);
    f.line_height = Some(lh);
    f.valign = Align::Center;
    f
}

pub(super) fn fmt_code(line_height: f32) -> TextFormat {
    let mut f = TextFormat::simple(FontId::monospace(SZ_CODE_INLINE), c_md_code_fg());
    f.background = c_md_code_bg();
    f.line_height = Some(line_height);
    f.valign = Align::Center;
    f
}

/// Inline until paragraph or list item end, using a single [`LayoutJob`] with wrap width.
pub(super) fn render_inline_until(
    ui: &mut Ui,
    wrap_w: f32,
    it: &mut ParserPeek<'_>,
    end: InlineEnd,
    density: InlineDensity,
) {
    // Inline images participate in the same wrapping row as the surrounding text. Links
    // stay inside the text galley so selection and copying remain continuous. Use zero
    // initial height instead of `horizontal_wrapped`'s `interact_size.y` (24 pt) so single-
    // and multi-line list items start at the exact same y as their bullet.
    ui.allocate_ui_with_layout(
        eframe::egui::vec2(ui.available_size_before_wrap().x, 0.0),
        eframe::egui::Layout::left_to_right(Align::Center).with_main_wrap(true),
        |ui| {
            ui.spacing_mut().item_spacing.x = 0.0;

            let mut job = LayoutJob::default();
            set_job_wrap(&mut job, wrap_w);

            let mut links = Vec::new();
            let mut active_link: Option<(usize, usize, String)> = None;
            let mut bold = 0u32;
            let mut italic = 0u32;
            let mut strike = 0u32;
            while let Some(ev) = it.next() {
                match (&ev, end) {
                    (Event::End(TagEnd::Paragraph), InlineEnd::Paragraph) => break,
                    (Event::End(TagEnd::Item), InlineEnd::Item) => break,
                    _ => {}
                }
                match ev {
                    Event::Start(Tag::Strong) => bold += 1,
                    Event::End(TagEnd::Strong) => bold = bold.saturating_sub(1),
                    Event::Start(Tag::Emphasis) => italic += 1,
                    Event::End(TagEnd::Emphasis) => italic = italic.saturating_sub(1),
                    Event::Start(Tag::Strikethrough) => strike += 1,
                    Event::End(TagEnd::Strikethrough) => strike = strike.saturating_sub(1),
                    Event::Text(t) => {
                        let mut tf = inline_text_format(SZ_BODY, bold, italic > 0, density);
                        if strike > 0 {
                            tf.strikethrough = Stroke::new(1.0, c_text());
                        }
                        job.append(t.as_ref(), 0.0, tf);
                    }
                    Event::Code(c) => {
                        job.append(c.as_ref(), 0.0, inline_code_format(density));
                    }
                    Event::SoftBreak => {
                        job.append(
                            " ",
                            0.0,
                            inline_text_format(SZ_BODY, bold, italic > 0, density),
                        );
                    }
                    Event::HardBreak => {
                        job.append(
                            "\n",
                            0.0,
                            inline_text_format(SZ_BODY, bold, italic > 0, density),
                        );
                    }
                    Event::Start(Tag::Link { dest_url, .. }) => {
                        active_link = Some((
                            job.text.chars().count(),
                            job.text.len(),
                            dest_url.into_string(),
                        ));
                    }
                    Event::End(TagEnd::Link) => {
                        finish_link(&mut job, &mut links, active_link.take());
                    }
                    Event::Start(Tag::Image { dest_url, .. }) => {
                        if !job.text.is_empty() {
                            if let Some((start, section, url)) = active_link.take() {
                                finish_link(
                                    &mut job,
                                    &mut links,
                                    Some((start, section, url.clone())),
                                );
                                active_link = Some((0, 0, url));
                            }
                            selectable_text_job_with_links(ui, std::mem::take(&mut job), &links);
                            links.clear();
                            set_job_wrap(&mut job, wrap_w);
                        }
                        let mut alt = String::new();
                        while let Some(inner) = it.next() {
                            match inner {
                                Event::End(TagEnd::Image) => break,
                                Event::Text(t) => alt.push_str(t.as_ref()),
                                Event::Code(c) => alt.push_str(c.as_ref()),
                                Event::SoftBreak => alt.push(' '),
                                Event::HardBreak => alt.push('\n'),
                                Event::Start(nested) => consume_until_end(it, nested.to_end()),
                                _ => {}
                            }
                        }
                        render_markdown_inline_image(
                            ui,
                            wrap_w,
                            dest_url.as_ref(),
                            alt.trim(),
                            false,
                        );
                    }
                    Event::Html(t) | Event::InlineHtml(t) => {
                        append_inline_fallback(
                            &mut job,
                            wrap_w,
                            t.as_ref(),
                            density.line_height(SZ_BODY),
                        );
                    }
                    Event::FootnoteReference(t) => {
                        let s = format!("[^{}]", t);
                        job.append(
                            &s,
                            0.0,
                            inline_text_format(SZ_BODY, bold, italic > 0, density),
                        );
                    }
                    Event::TaskListMarker(done) => {
                        let mark = if done { "☑ " } else { "☐ " };
                        job.append(
                            mark,
                            0.0,
                            inline_text_format(SZ_BODY, bold, italic > 0, density),
                        );
                    }
                    Event::InlineMath(t) | Event::DisplayMath(t) => {
                        job.append(t.as_ref(), 0.0, inline_code_format(density));
                    }
                    _ => {}
                }
            }
            finish_link(&mut job, &mut links, active_link.take());
            selectable_text_job_with_links(ui, job, &links);
        },
    );
    let tail = match (end, density) {
        (InlineEnd::Paragraph | InlineEnd::Item, InlineDensity::Normal) => 2.0,
        (InlineEnd::Paragraph | InlineEnd::Item, InlineDensity::ListItem) => 0.0,
    };
    ui.add_space(tail);
}

pub(super) fn selectable_job(ui: &mut Ui, job: LayoutJob) {
    crate::theme::selectable_text_job(ui, job);
}

/// Record the link inside the same galley, preserving emphasis and inline-code formatting.
/// `link` holds the start as (char offset, byte offset, url). `LayoutJob::append` merges runs
/// with equal formats, so the section holding the link start may also hold preceding text and
/// must be split before recoloring.
fn finish_link(
    job: &mut LayoutJob,
    links: &mut Vec<TextLink>,
    link: Option<(usize, usize, String)>,
) {
    let Some((start, start_byte, url)) = link else {
        return;
    };
    let end = job.text.chars().count();
    if start >= end {
        return;
    }
    if let Some(i) = job
        .sections
        .iter()
        .position(|s| s.byte_range.start.0 < start_byte && start_byte < s.byte_range.end.0)
    {
        let mut tail = job.sections[i].clone();
        job.sections[i].byte_range.end.0 = start_byte;
        tail.byte_range.start.0 = start_byte;
        tail.leading_space = 0.0;
        job.sections.insert(i + 1, tail);
    }
    for section in &mut job.sections {
        if section.byte_range.start.0 >= start_byte {
            section.format.color = c_accent();
            section.format.underline = Stroke::new(1.0, c_accent());
        }
    }
    links.push(TextLink {
        range: start..end,
        url,
    });
}

#[cfg(test)]
mod link_selection_tests {
    use eframe::egui::{self, Event, Modifiers, PointerButton, Pos2, RawInput, Rect};

    const MARKDOWN: &str = "Și înainte [**open source cu diacritice șț 🔗 1️⃣ ❤️‍🔥 și mai multe cuvinte**](https://example.com/font) după.";
    const TEXT: &str =
        "Și înainte open source cu diacritice șț 🔗 1️⃣ ❤️‍🔥 și mai multe cuvinte după.";

    fn frame(ctx: &egui::Context, events: Vec<Event>) -> egui::FullOutput {
        ctx.run_ui(
            RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(270.0, 300.0))),
                events,
                ..Default::default()
            },
            |ui| crate::markdown::render_markdown(ui, MARKDOWN),
        )
    }

    fn pointer(pos: Pos2, pressed: bool) -> Event {
        Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        }
    }

    fn text_shape(output: &egui::FullOutput) -> &egui::epaint::TextShape {
        output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.text() == TEXT => Some(text),
                _ => None,
            })
            .expect("the paragraph and its link must be one selectable text galley")
    }

    fn opens_url(output: &egui::FullOutput) -> bool {
        output.platform_output.commands.iter().any(|command| {
            matches!(command,
            egui::OutputCommand::OpenUrl(url) if url.url == "https://example.com/font")
        })
    }

    /// A plain link after plain text shares its format, so `append` merges the two runs.
    #[test]
    fn plain_link_after_plain_text_gets_link_style() {
        let ctx = egui::Context::default();
        crate::theme::setup_style(&ctx);
        let output = ctx.run_ui(RawInput::default(), |ui| {
            crate::markdown::render_markdown(ui, "Este [open source](https://rsms.me/inter/).")
        });
        let galley = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.text() == "Este open source." => {
                    Some(text.galley.clone())
                }
                _ => None,
            })
            .unwrap();
        let styled = |range: std::ops::Range<usize>| {
            galley.job.sections.iter().any(|s| {
                s.byte_range.start.0 <= range.start
                    && range.end <= s.byte_range.end.0
                    && s.format.color == crate::theme::c_accent()
                    && s.format.underline.width > 0.0
            })
        };
        assert!(styled(5..16), "link text must be accent + underlined");
        assert!(
            !styled(0..4) && !styled(16..17),
            "surrounding text keeps body style"
        );
    }

    #[test]
    fn wrapped_link_is_selected_and_copied_in_both_directions() {
        for backwards in [false, true] {
            let ctx = egui::Context::default();
            crate::theme::setup_style(&ctx);
            let output = frame(&ctx, vec![]);
            let text = text_shape(&output);
            assert!(text.galley.rows.len() > 1);
            let first = text.galley.rows.first().unwrap();
            let last = text.galley.rows.last().unwrap();
            let a = text.pos + first.pos.to_vec2() + egui::vec2(0.0, first.row.height() / 2.0);
            let b = text.pos
                + last.pos.to_vec2()
                + egui::vec2(last.row.size.x, last.row.height() / 2.0);
            let (start, end) = if backwards { (b, a) } else { (a, b) };
            frame(&ctx, vec![Event::PointerMoved(start), pointer(start, true)]);
            let selected = frame(&ctx, vec![Event::PointerMoved(end)]);
            let selected_text = text_shape(&selected);
            assert!(selected_text.galley.rows.iter().zip(&text.galley.rows).all(
                |(selected, original)| selected.row.visuals.mesh.vertices.len()
                    == original.row.visuals.mesh.vertices.len() + 4
            ));
            assert!(!opens_url(&selected));
            let released = frame(&ctx, vec![pointer(end, false)]);
            assert!(!opens_url(&released));
            let copied = frame(&ctx, vec![Event::Copy]);
            assert!(
                copied
                    .platform_output
                    .commands
                    .iter()
                    .any(|command| matches!(command,
                egui::OutputCommand::CopyText(text) if text == TEXT))
            );
        }
    }

    #[test]
    fn link_click_opens_but_drag_from_link_selects() {
        let ctx = egui::Context::default();
        crate::theme::setup_style(&ctx);
        let output = frame(&ctx, vec![]);
        let text = text_shape(&output);
        let range = egui::text::CCursorRange::two(
            egui::text::CCursor::new("Și înainte ".chars().count()),
            egui::text::CCursor::new(TEXT.chars().count() - " după.".chars().count()),
        );
        let rects =
            crate::ui::text_selection::galley_selection_rects(&text.galley, text.pos, range);
        assert!(rects.len() > 1, "exercise a link spanning wrapped rows");
        let point = rects.last().unwrap().center();
        frame(&ctx, vec![Event::PointerMoved(point), pointer(point, true)]);
        assert!(opens_url(&frame(&ctx, vec![pointer(point, false)])));

        frame(&ctx, vec![Event::PointerMoved(point), pointer(point, true)]);
        let end = text.pos + egui::vec2(0.0, text.galley.rows[0].row.height() / 2.0);
        frame(&ctx, vec![Event::PointerMoved(end)]);
        assert!(!opens_url(&frame(&ctx, vec![pointer(end, false)])));
        let copied = frame(&ctx, vec![Event::Copy]);
        assert!(
            copied
                .platform_output
                .commands
                .iter()
                .any(|command| matches!(command,
            egui::OutputCommand::CopyText(text) if text.contains("Și înainte open source")))
        );
    }

    #[test]
    fn inline_code_aligns_vertically_with_prose_and_pill_across_bundled_fonts() {
        let pairs = [
            ("default", "default"),
            ("bundled:noto_sans", "bundled:lilex"),
            ("bundled:ibm_plex_sans", "bundled:lilex"),
            ("bundled:noto_sans", "bundled:ubuntu_mono"),
        ];
        for (ui_font, mono_font) in pairs {
            let ctx = egui::Context::default();
            crate::theme::setup_style(&ctx);
            ctx.set_fonts(crate::theme::font_definitions(
                &crate::theme::FontSelection {
                    ui: ui_font.into(),
                    mono: mono_font.into(),
                },
            ));
            let render = || {
                ctx.run_ui(
                    RawInput {
                        screen_rect: Some(Rect::from_min_size(
                            Pos2::ZERO,
                            egui::vec2(600.0, 200.0),
                        )),
                        ..Default::default()
                    },
                    |ui| {
                        crate::markdown::render_markdown(ui, "1. Hx `Hx` Hx\n\n- xoxo `xoxo` xoxo");
                    },
                )
            };
            // Frame 1 applies `set_fonts`; frames 2 and 3 verify cached galleys do not double-shift.
            let _ = render();
            let frame2 = render();
            let frame3 = render();

            let glyph_center_y = |output: &egui::FullOutput,
                                  needle: &str,
                                  range: std::ops::Range<usize>| {
                let text = output
                    .shapes
                    .iter()
                    .find_map(|s| match &s.shape {
                        egui::Shape::Text(t) if t.galley.text() == needle => Some(t),
                        _ => None,
                    })
                    .unwrap_or_else(|| panic!("missing text {needle:?} for {ui_font}/{mono_font}"));
                let row = &text.galley.rows[0];
                let end = row.row.visuals.glyph_vertex_range.end;
                let mut min_y = f32::INFINITY;
                let mut max_y = f32::NEG_INFINITY;
                for idx in range {
                    let v_start = row.row.glyphs[idx].first_vertex as usize;
                    let v_end = row
                        .row
                        .glyphs
                        .get(idx + 1)
                        .map_or(end, |g| g.first_vertex as usize);
                    for v in &row.row.visuals.mesh.vertices[v_start..v_end] {
                        let y = text.pos.y + row.pos.y + v.pos.y;
                        min_y = min_y.min(y);
                        max_y = max_y.max(y);
                    }
                }
                (min_y + max_y) * 0.5
            };

            let pill_center_y = |output: &egui::FullOutput, target_y: f32| {
                output
                    .shapes
                    .iter()
                    .flat_map(|s| match &s.shape {
                        egui::Shape::Vec(shapes) => shapes.as_slice(),
                        _ => &[],
                    })
                    .filter_map(|inner| match inner {
                        egui::Shape::Rect(r) if r.fill == crate::theme::c_md_code_bg() => {
                            Some(r.rect.center().y)
                        }
                        _ => None,
                    })
                    .min_by(|a, b| (a - target_y).abs().total_cmp(&(b - target_y).abs()))
                    .expect("inline code pill rect")
            };

            let prose_y2 = glyph_center_y(&frame2, "xoxo xoxo xoxo", 0..4);
            let code_y2 = glyph_center_y(&frame2, "xoxo xoxo xoxo", 5..9);
            let pill_y2 = pill_center_y(&frame2, code_y2);
            let ord_marker_y2 = glyph_center_y(&frame2, "1.", 0..1);
            let ord_cap_y2 = glyph_center_y(&frame2, "Hx Hx Hx", 0..1);

            let prose_y3 = glyph_center_y(&frame3, "xoxo xoxo xoxo", 0..4);
            let code_y3 = glyph_center_y(&frame3, "xoxo xoxo xoxo", 5..9);

            assert!(
                (prose_y2 - prose_y3).abs() < 0.01 && (code_y2 - code_y3).abs() < 0.01,
                "{ui_font}/{mono_font}: cached galley shifted twice across frames: ({prose_y2}, {code_y2}) vs ({prose_y3}, {code_y3})"
            );
            assert!(
                (prose_y2 - code_y2).abs() <= 1.0,
                "{ui_font}/{mono_font}: prose center {prose_y2:.2} vs inline code center {code_y2:.2}"
            );
            assert!(
                (code_y2 - pill_y2).abs() <= 1.5,
                "{ui_font}/{mono_font}: inline code center {code_y2:.2} vs pill center {pill_y2:.2}"
            );
            assert!(
                (ord_marker_y2 - ord_cap_y2).abs() <= 1.0,
                "{ui_font}/{mono_font}: list marker center {ord_marker_y2:.2} vs item center {ord_cap_y2:.2}"
            );
        }
    }
}
