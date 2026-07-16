//! Block-level renderers: headings, paragraphs, blockquotes, fenced/indented code
//! blocks, and the raw-HTML fallback.

use eframe::egui::text::LayoutJob;
use eframe::egui::{
    self, Align, CornerRadius, FontFamily, FontId, Frame, Id, Layout, Margin, RichText, Stroke, Ui,
    vec2,
};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Tag, TagEnd};

use crate::theme::*;
use crate::ui::preview_expand::{
    clickable_expand_overlay, expand_persist_id, is_expanded, truncate_lines_preview,
};

use super::inline::{InlineDensity, InlineEnd, fmt_body, fmt_code, selectable_job};
use super::{
    ParserPeek, SZ_CODE, SZ_TINY, allocate_full_width_block, consume_until_end,
    line_height_for_body_size, render_list, set_job_wrap,
};

/// Raw HTML / unknown blocks: show monospace so nothing is silently dropped.
fn render_raw_block(ui: &mut Ui, wrap_w: f32, label: &str, body: &str) {
    allocate_full_width_block(ui, wrap_w, |ui| {
        Frame::new()
            .fill(c_md_code_block_bg())
            .stroke(Stroke::new(1.0, c_border()))
            .corner_radius(CornerRadius::same(crate::theme::RADIUS_CHIP))
            .inner_margin(Margin::symmetric(10, 8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let inner = ui.available_width().max(40.0);
                ui.label(
                    RichText::new(label)
                        .size(SZ_TINY)
                        .color(c_text_muted())
                        .family(FontFamily::Proportional),
                );
                ui.add_space(4.0);
                let job = LayoutJob::simple(
                    body.to_string(),
                    FontId::monospace(SZ_CODE),
                    c_text_muted(),
                    inner,
                );
                selectable_job(ui, job);
            });
    });
    ui.add_space(6.0);
}

pub(super) fn render_html_block(ui: &mut Ui, wrap_w: f32, it: &mut ParserPeek<'_>) {
    let mut buf = String::new();
    loop {
        match it.next() {
            Some(Event::End(TagEnd::HtmlBlock)) => break,
            Some(Event::Text(t)) => buf.push_str(t.as_ref()),
            Some(Event::Html(t)) => buf.push_str(t.as_ref()),
            Some(Event::Start(t)) => consume_until_end(it, t.to_end()),
            Some(_) => {}
            None => break,
        }
    }
    render_raw_block(ui, wrap_w, "HTML", &buf);
}

pub(super) fn render_heading(
    ui: &mut Ui,
    wrap_w: f32,
    level: HeadingLevel,
    it: &mut ParserPeek<'_>,
) {
    match level {
        HeadingLevel::H1 => ui.add_space(8.0),
        HeadingLevel::H2 => ui.add_space(6.0),
        HeadingLevel::H3 => ui.add_space(4.0),
        _ => ui.add_space(2.0),
    }

    let size = match level {
        HeadingLevel::H1 => FS_H1,
        HeadingLevel::H2 => FS_H2,
        HeadingLevel::H3 => FS_H3,
        HeadingLevel::H4 => FS_BODY,
        HeadingLevel::H5 | HeadingLevel::H6 => FS_CODE,
    };
    let mut job = LayoutJob::default();
    set_job_wrap(&mut job, wrap_w);

    let mut bold = 0u32;
    let mut italic = 0u32;
    // Headings are already rendered strong, so bold only nests on top of that;
    // emphasis inside a heading still slants the glyphs.
    let heading_fmt = |bold: u32, italic: u32| {
        let mut f = fmt_body(size, if bold > 0 { bold } else { 1 });
        f.italics = italic > 0;
        f
    };
    while let Some(ev) = it.next() {
        match ev {
            Event::End(TagEnd::Heading(_)) => break,
            Event::Start(Tag::Strong) => bold += 1,
            Event::End(TagEnd::Strong) => bold = bold.saturating_sub(1),
            Event::Start(Tag::Emphasis) => italic += 1,
            Event::End(TagEnd::Emphasis) => italic = italic.saturating_sub(1),
            Event::Text(t) => {
                job.append(t.as_ref(), 0.0, heading_fmt(bold, italic));
            }
            Event::Code(c) => {
                // Match the heading's proportional font and size so inline code shares its
                // baseline; the monospace font's shorter ascent otherwise makes code ride high.
                let mut f = fmt_code(line_height_for_body_size(size));
                f.font_id = FontId::proportional(size);
                job.append(c.as_ref(), 0.0, f);
            }
            Event::SoftBreak => {
                job.append(" ", 0.0, heading_fmt(bold, italic));
            }
            Event::HardBreak => {
                job.append("\n", 0.0, heading_fmt(bold, italic));
            }
            Event::Html(t) | Event::InlineHtml(t) => {
                job.append(t.as_ref(), 0.0, heading_fmt(bold, italic));
            }
            Event::Start(tag) => {
                consume_until_end(it, tag.to_end());
            }
            _ => {}
        }
    }
    selectable_job(ui, job);
    if matches!(level, HeadingLevel::H1 | HeadingLevel::H2) {
        allocate_full_width_block(ui, wrap_w, |ui| {
            ui.add_space(2.0);
            let (rect, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), 1.0), egui::Sense::hover());
            ui.painter()
                .rect_filled(rect, 0.0, c_md_code_block_border());
        });
        ui.add_space(7.0);
    } else {
        ui.add_space(5.0);
    }
}

pub(super) fn render_paragraph(ui: &mut Ui, wrap_w: f32, it: &mut ParserPeek<'_>) {
    super::inline::render_inline_until(ui, wrap_w, it, InlineEnd::Paragraph, InlineDensity::Normal);
    ui.add_space(4.0);
}

pub(super) fn render_blockquote(ui: &mut Ui, wrap_w: f32, it: &mut ParserPeek<'_>) {
    allocate_full_width_block(ui, wrap_w, |ui| {
        Frame::new()
            .fill(c_md_code_block_bg())
            .corner_radius(CornerRadius::same(crate::theme::RADIUS_CHIP))
            .stroke(Stroke::new(1.0, c_md_code_block_border()))
            .inner_margin(Margin::same(0))
            .show(ui, |ui| {
                let full_w = ui.available_width().max(48.0);
                ui.set_width(full_w);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    let (bar_rect, _) =
                        ui.allocate_exact_size(vec2(3.0, 1.0), egui::Sense::hover());
                    ui.painter()
                        .rect_filled(bar_rect, CornerRadius::same(2), c_md_quote_accent());
                    ui.add_space(10.0);
                    ui.vertical(|ui| {
                        ui.add_space(8.0);
                        let inner_w = (full_w - 24.0).max(32.0);
                        ui.set_width(inner_w);
                        while let Some(ev) = it.next() {
                            match ev {
                                Event::End(TagEnd::BlockQuote(_)) => break,
                                Event::Start(Tag::Paragraph) => render_paragraph(ui, inner_w, it),
                                Event::Start(Tag::List(kind)) => {
                                    render_list(ui, inner_w, kind, 0, it)
                                }
                                Event::Start(Tag::Heading { level, .. }) => {
                                    render_heading(ui, inner_w, level, it);
                                }
                                Event::Start(Tag::CodeBlock(kind)) => {
                                    render_fenced_block(
                                        ui,
                                        inner_w,
                                        kind,
                                        it,
                                        ui.id().with("quote_fence"),
                                    );
                                }
                                _ => {}
                            }
                        }
                        ui.add_space(4.0);
                    });
                });
            });
    });
    ui.add_space(7.0);
}

pub(super) fn code_block_language(kind: &CodeBlockKind<'_>) -> String {
    match kind {
        CodeBlockKind::Fenced(info) => info
            .split_whitespace()
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or("code")
            .to_string(),
        CodeBlockKind::Indented => "code".to_string(),
    }
}

pub(super) fn render_fenced_block(
    ui: &mut Ui,
    wrap_w: f32,
    kind: CodeBlockKind<'_>,
    it: &mut ParserPeek<'_>,
    block_base_id: Id,
) {
    let mut buf = String::new();
    for ev in it.by_ref() {
        match ev {
            Event::Text(t) => buf.push_str(t.as_ref()),
            Event::End(TagEnd::CodeBlock) => break,
            _ => {}
        }
    }
    while buf.ends_with('\n') {
        buf.pop();
    }
    const PREVIEW_LINES: usize = 14;
    let overflows = buf.lines().count() > PREVIEW_LINES || buf.len() > 2600;
    let persist_id = expand_persist_id(block_base_id);
    let lang = code_block_language(&kind);
    allocate_full_width_block(ui, wrap_w, |ui| {
        let frame = Frame::new()
            .fill(c_md_code_block_bg())
            .stroke(Stroke::new(1.0, c_md_code_block_border()))
            .corner_radius(CornerRadius::same(crate::theme::RADIUS_CHIP))
            .inner_margin(Margin::same(0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());

                Frame::new()
                    .fill(c_md_code_block_header_bg())
                    .corner_radius(egui::CornerRadius {
                        nw: crate::theme::RADIUS_CHIP,
                        ne: crate::theme::RADIUS_CHIP,
                        sw: 0,
                        se: 0,
                    })
                    .inner_margin(Margin::symmetric(10, 5))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            ui.label(RichText::new("●").size(7.0).color(c_text_muted()));
                            ui.label(
                                RichText::new(lang.as_str())
                                    .size(SZ_TINY)
                                    .color(c_text_muted())
                                    .family(FontFamily::Monospace),
                            );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                crate::ui::chrome::copy_chip(ui, persist_id.with("copy"), &buf);
                                if overflows && !is_expanded(ui, persist_id) {
                                    ui.label(
                                        RichText::new("click to expand")
                                            .size(SZ_TINY)
                                            .color(c_text_faint()),
                                    );
                                }
                            });
                        });
                    });

                Frame::new()
                    .fill(c_md_code_block_bg())
                    .corner_radius(egui::CornerRadius {
                        nw: 0,
                        ne: 0,
                        sw: crate::theme::RADIUS_CHIP,
                        se: crate::theme::RADIUS_CHIP,
                    })
                    .inner_margin(Margin::symmetric(11, 9))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        let inner = ui.available_width().max(40.0);
                        let text = if overflows && !is_expanded(ui, persist_id) {
                            truncate_lines_preview(&buf, PREVIEW_LINES)
                        } else {
                            buf.clone()
                        };
                        let job = LayoutJob::simple(
                            text,
                            FontId::monospace(SZ_CODE),
                            c_md_code_fg(),
                            inner,
                        );
                        selectable_job(ui, job);
                    })
                    .response
                    .rect
            });
        if overflows {
            // Only the body toggles expansion — the header hosts the copy button, and a
            // raw-pointer overlay over it would also fire on that click.
            clickable_expand_overlay(ui, frame.inner, persist_id);
        }
    });
    ui.add_space(8.0);
}
