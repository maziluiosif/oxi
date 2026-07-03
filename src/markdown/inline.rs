//! Inline text formatting: the run of text/code/link/image events between block
//! boundaries, laid out into a single [`LayoutJob`] per run.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use base64::Engine as _;
use eframe::egui::text::{LayoutJob, TextFormat};
use eframe::egui::{Align, FontId, Hyperlink, Image, RichText, Rounding, Stroke, Ui};
use pulldown_cmark::{Event, Tag, TagEnd};

use crate::theme::*;

use super::{consume_until_end, set_job_wrap, ParserPeek, SZ_BODY, SZ_CODE_INLINE};

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
        // One line-height for all flowing prose (paragraphs and list items) so a wrapped list
        // line matches a wrapped paragraph line. List compactness comes from the inter-item gap
        // (`LIST_GAP_AFTER_ITEM`) and the zero tail below, not from a tighter line-height.
        match self {
            InlineDensity::Normal | InlineDensity::ListItem => (size * 1.35).max(16.0),
        }
    }
}

pub(super) fn inline_text_format(size: f32, strong: u32, density: InlineDensity) -> TextFormat {
    let color = if strong > 0 {
        c_text_strong()
    } else {
        c_text()
    };
    let lh = density.line_height(size);
    let mut f = TextFormat::simple(FontId::proportional(size), color);
    f.line_height = Some(lh);
    f.valign = Align::Center;
    f
}

pub(super) fn inline_code_format(density: InlineDensity) -> TextFormat {
    // Use the same proportional font and size as the surrounding prose so inline code shares
    // its exact baseline — the monospace font's much shorter ascent (Ubuntu Mono ~0.83 vs
    // Noto Sans ~1.07) made code glyphs ride above the text line. The code styling now comes
    // from the background tint and color, not a different typeface.
    let lh = density.line_height(SZ_BODY);
    let mut f = TextFormat::simple(FontId::proportional(SZ_BODY), c_md_code_fg());
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
    let mut img = img
        .max_width(max_w)
        .rounding(Rounding::same(6.0))
        .show_loading_spinner(true);
    if compact {
        img = img.max_height(120.0);
    }
    let resp = ui.add(img);
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

pub(super) fn fmt_body(size: f32, strong: u32) -> TextFormat {
    let color = if strong > 0 {
        c_text_strong()
    } else {
        c_text()
    };
    let lh = (size * 1.35).max(16.0);
    let mut f = TextFormat::simple(FontId::proportional(size), color);
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
    let mut job = LayoutJob::default();
    set_job_wrap(&mut job, wrap_w);

    let mut bold = 0u32;
    let mut strike = 0u32;
    while let Some(ev) = it.next() {
        match (&ev, end) {
            (Event::End(TagEnd::Paragraph), InlineEnd::Paragraph) => break,
            (Event::End(TagEnd::Item), InlineEnd::Item) => break,
            _ => {}
        }
        match ev {
            Event::Start(Tag::Strong) | Event::Start(Tag::Emphasis) => bold += 1,
            Event::End(TagEnd::Strong) | Event::End(TagEnd::Emphasis) => {
                bold = bold.saturating_sub(1);
            }
            Event::Start(Tag::Strikethrough) => strike += 1,
            Event::End(TagEnd::Strikethrough) => strike = strike.saturating_sub(1),
            Event::Text(t) => {
                let mut tf = inline_text_format(SZ_BODY, bold, density);
                if strike > 0 {
                    tf.strikethrough = Stroke::new(1.0, c_text());
                }
                job.append(t.as_ref(), 0.0, tf);
            }
            Event::Code(c) => {
                job.append(c.as_ref(), 0.0, inline_code_format(density));
            }
            Event::SoftBreak => {
                job.append(" ", 0.0, inline_text_format(SZ_BODY, bold, density));
            }
            Event::HardBreak => {
                job.append("\n", 0.0, inline_text_format(SZ_BODY, bold, density));
            }
            Event::Start(Tag::Link {
                link_type: _,
                dest_url,
                title: _,
                id: _,
            }) => {
                if !job.text.is_empty() {
                    selectable_job(ui, std::mem::take(&mut job));
                    set_job_wrap(&mut job, wrap_w);
                }
                let dest = dest_url.to_string();
                let mut label = String::new();
                while let Some(inner) = it.next() {
                    match inner {
                        Event::End(TagEnd::Link) => break,
                        Event::Text(t) => label.push_str(t.as_ref()),
                        Event::Code(c) => label.push_str(c.as_ref()),
                        Event::SoftBreak => label.push(' '),
                        Event::HardBreak => label.push('\n'),
                        Event::Html(t) | Event::InlineHtml(t) => label.push_str(t.as_ref()),
                        Event::FootnoteReference(t) => {
                            label.push_str(&format!("[^{}]", t));
                        }
                        Event::TaskListMarker(done) => {
                            label.push_str(if done { "[x] " } else { "[ ] " });
                        }
                        Event::Start(nested) => consume_until_end(it, nested.to_end()),
                        _ => {}
                    }
                }
                ui.add(Hyperlink::from_label_and_url(
                    RichText::new(label).color(c_accent()).size(SZ_BODY),
                    dest,
                ));
                ui.add_space(2.0);
            }
            Event::Start(Tag::Image {
                dest_url, title: _, ..
            }) => {
                if !job.text.is_empty() {
                    selectable_job(ui, std::mem::take(&mut job));
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
                render_markdown_inline_image(ui, wrap_w, dest_url.as_ref(), alt.trim(), false);
            }
            Event::Html(t) | Event::InlineHtml(t) => {
                append_inline_fallback(
                    &mut job,
                    wrap_w,
                    t.as_ref(),
                    density.line_height(SZ_CODE_INLINE),
                );
            }
            Event::FootnoteReference(t) => {
                let s = format!("[^{}]", t);
                job.append(&s, 0.0, inline_text_format(SZ_BODY, bold, density));
            }
            Event::TaskListMarker(done) => {
                let mark = if done { "☑ " } else { "☐ " };
                job.append(mark, 0.0, inline_text_format(SZ_BODY, bold, density));
            }
            Event::InlineMath(t) | Event::DisplayMath(t) => {
                job.append(t.as_ref(), 0.0, inline_code_format(density));
            }
            _ => {}
        }
    }
    selectable_job(ui, job);
    let tail = match (end, density) {
        (InlineEnd::Paragraph | InlineEnd::Item, InlineDensity::Normal) => 1.5,
        (InlineEnd::Paragraph | InlineEnd::Item, InlineDensity::ListItem) => 0.0,
    };
    ui.add_space(tail);
}

pub(super) fn selectable_job(ui: &mut Ui, job: LayoutJob) {
    if job.text.is_empty() {
        return;
    }

    let galley = ui.fonts(|fonts| fonts.layout_job(job));
    let (rect, response) =
        ui.allocate_exact_size(galley.size(), eframe::egui::Sense::click_and_drag());
    let galley_pos = rect.left_top();
    eframe::egui::text_selection::LabelSelectionState::label_text_selection(
        ui,
        &response,
        galley_pos,
        galley,
        ui.style().visuals.text_color(),
        Stroke::NONE,
    );
}
