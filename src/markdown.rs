//! Markdown → egui. Inline text uses [`egui::text::LayoutJob`] with an explicit wrap
//! width so long paths do not layout as one character per row (horizontal_wrapped bug).
//!
//! Split by rendering responsibility: [`inline`] (text runs, links, inline images),
//! [`blocks`] (headings, paragraphs, quotes, code fences, raw HTML), and [`table`] (GFM
//! tables). This file keeps the shared layout helpers/constants every submodule depends
//! on, plus [`render_markdown`] (the only symbol used outside this module) and
//! [`render_list`], which recurses through both `inline` and `blocks`.

mod blocks;
mod inline;
mod table;

use eframe::egui::text::LayoutJob;
use eframe::egui::{Align, FontFamily, Layout, RichText, Ui, vec2};
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

use crate::theme::*;

use blocks::{render_blockquote, render_fenced_block, render_heading, render_html_block};
use inline::{InlineDensity, append_inline_fallback, fmt_body, fmt_code, selectable_job};
use table::render_table;

/// Prose wrap at word boundaries (do not set [`TextWrapping::break_anywhere`] — it splits mid-word).
#[inline]
fn set_job_wrap(job: &mut LayoutJob, wrap_w: f32) {
    job.wrap.max_width = wrap_w.max(32.0);
}

/// Reserve the full markdown column width so bordered frames (code, tables, quotes) match prose
/// width instead of shrinking to the longest line of content.
fn allocate_full_width_block(ui: &mut Ui, column_w: f32, add_contents: impl FnOnce(&mut Ui)) {
    let w = column_w.max(48.0);
    ui.allocate_ui_with_layout(vec2(w, 0.0), Layout::top_down(Align::Min), add_contents);
}

// Markdown colors live in the active theme palette (see `theme::Palette`); accessed
// through the `c_*` / `c_md_*` functions so they track the selected theme.
// All markdown sizes route through the shared type scale in `theme` (FS_*) so prose, code,
// and headings stay uniform with the rest of the app.
const SZ_BODY: f32 = FS_BODY;
/// Floor for raw-HTML / math fallbacks shown in monospace; real inline `code` matches prose.
const SZ_CODE_INLINE: f32 = FS_CODE;
const SZ_CODE: f32 = FS_CODE;
const SZ_TINY: f32 = FS_TINY;

/// Lists are inset from body text so bullets do not sit flush on the column edge (Cursor-like).
const LIST_BLOCK_MARGIN: f32 = 12.0;
/// Left inset for a nested list, relative to the parent item text column.
///
/// Nested lists are rendered inside the parent item's content column, so applying the full
/// root-list margin plus a depth multiplier here makes deeper items drift too far right and makes
/// spacing look inconsistent between ordered/unordered combinations.
const LIST_NESTED_MARGIN: f32 = 10.0;
const LIST_BULLET_GAP: f32 = 8.0;
const LIST_BULLET_COL_UNORD: f32 = 22.0;
/// Wide enough for two-digit ordered markers (`10.`).
const LIST_BULLET_COL_ORD: f32 = 34.0;
/// Gap after each list row. Also the target exterior gap *before* the first item when we swap margins.
const LIST_GAP_AFTER_ITEM: f32 = 2.0;
/// Must match `render_inline_until` Normal tail (`1.5`) + `render_paragraph` spacing (`4.0`): gap the preceding paragraph leaves before the next block.
const PARA_GAP_BEFORE_NEXT_BLOCK: f32 = 1.5 + 4.0;

type ParserPeek<'a> = std::iter::Peekable<Parser<'a>>;

/// Parser flags for assistant markdown: GFM-ish features without enabling math (avoids
/// `$...$` being parsed as formulas and dropped by our inline renderer).
const MD_OPTIONS: Options = Options::ENABLE_STRIKETHROUGH
    .union(Options::ENABLE_TABLES)
    .union(Options::ENABLE_TASKLISTS)
    .union(Options::ENABLE_SMART_PUNCTUATION)
    .union(Options::ENABLE_GFM);

#[inline]
fn line_height_for_body_size(size: f32) -> f32 {
    (size * 1.35).max(16.0)
}

fn consume_until_end(it: &mut ParserPeek<'_>, end: TagEnd) {
    while let Some(ev) = it.next() {
        match ev {
            Event::End(e) if e == end => break,
            Event::Start(tag) => consume_until_end(it, tag.to_end()),
            _ => {}
        }
    }
}

pub fn render_markdown(ui: &mut Ui, src: &str) {
    let wrap_w = content_wrap_width(ui);
    ui.set_max_width(wrap_w);

    let parser = Parser::new_ext(src, MD_OPTIONS);
    let mut it = parser.peekable();
    let mut fence_idx = 0u32;
    while let Some(ev) = it.next() {
        match ev {
            Event::Start(Tag::Paragraph) => blocks::render_paragraph(ui, wrap_w, &mut it),
            Event::Start(Tag::Heading { level, .. }) => {
                render_heading(ui, wrap_w, level, &mut it);
            }
            Event::Start(Tag::List(kind)) => render_list(ui, wrap_w, kind, 0, &mut it),
            Event::Start(Tag::CodeBlock(kind)) => {
                let lang = blocks::code_block_language(&kind);
                let base = ui.id().with("md_fence").with(fence_idx).with(&lang);
                fence_idx += 1;
                render_fenced_block(ui, wrap_w, kind, &mut it, base);
            }
            Event::Start(Tag::BlockQuote(_)) => render_blockquote(ui, wrap_w, &mut it),
            Event::Start(Tag::HtmlBlock) => render_html_block(ui, wrap_w, &mut it),
            Event::Start(Tag::MetadataBlock(k)) => {
                consume_until_end(&mut it, TagEnd::MetadataBlock(k));
            }
            Event::Start(Tag::FootnoteDefinition(_)) => {
                consume_until_end(&mut it, TagEnd::FootnoteDefinition);
            }
            Event::Start(Tag::DefinitionList) => {
                consume_until_end(&mut it, TagEnd::DefinitionList);
            }
            Event::Start(Tag::DefinitionListTitle) => {
                consume_until_end(&mut it, TagEnd::DefinitionListTitle);
            }
            Event::Start(Tag::DefinitionListDefinition) => {
                consume_until_end(&mut it, TagEnd::DefinitionListDefinition);
            }
            Event::Rule => {
                allocate_full_width_block(ui, wrap_w, |ui| {
                    ui.add_space(4.0);
                    ui.separator();
                    ui.add_space(6.0);
                });
            }
            Event::Start(Tag::Table(alignments)) => {
                let cols = alignments.len().max(1);
                render_table(ui, wrap_w, cols, &mut it);
            }
            Event::Text(t) => {
                let mut job = LayoutJob::default();
                set_job_wrap(&mut job, wrap_w);
                job.append(t.as_ref(), 0.0, fmt_body(SZ_BODY, 0));
                selectable_job(ui, job);
                ui.add_space(4.0);
            }
            Event::Html(t) | Event::InlineHtml(t) => {
                let mut job = LayoutJob::default();
                set_job_wrap(&mut job, wrap_w);
                append_inline_fallback(
                    &mut job,
                    wrap_w,
                    t.as_ref(),
                    InlineDensity::Normal.line_height(SZ_BODY),
                );
                selectable_job(ui, job);
                ui.add_space(4.0);
            }
            Event::InlineMath(t) | Event::DisplayMath(t) => {
                let mut job = LayoutJob::default();
                set_job_wrap(&mut job, wrap_w);
                job.append(
                    t.as_ref(),
                    0.0,
                    fmt_code(line_height_for_body_size(SZ_BODY)),
                );
                selectable_job(ui, job);
                ui.add_space(4.0);
            }
            Event::Start(tag) => consume_until_end(&mut it, tag.to_end()),
            Event::End(_) => {}
            _ => {}
        }
    }
}

/// `list_row_w`: width for this list’s rows — full column at depth 0, parent item **content** width
/// when nested (see recursive call with `text_w`).
fn render_list(
    ui: &mut Ui,
    list_row_w: f32,
    list_kind: Option<u64>,
    depth: u32,
    it: &mut ParserPeek<'_>,
) {
    // Caller already consumed `Event::Start(Tag::List(..))`; next is `Item`.
    let mut num = list_kind.unwrap_or(1);
    let ordered = list_kind.is_some();
    let column_w = list_row_w.min(ui.max_rect().width()).max(48.0);
    let block_left = if depth == 0 {
        LIST_BLOCK_MARGIN
    } else {
        LIST_NESTED_MARGIN
    };
    let bullet_col = if ordered {
        LIST_BULLET_COL_ORD
    } else {
        LIST_BULLET_COL_UNORD
    };
    let text_w = (column_w - block_left - bullet_col - LIST_BULLET_GAP).max(24.0);

    // Swap exterior rhythm: paragraph leaves `PARA_GAP_BEFORE_NEXT_BLOCK` before the list; each item
    // ends with `LIST_GAP_AFTER_ITEM`. Pull the first row up and add the difference after the list
    // so the gap before the first bullet matches the old between-items gap, and the gap after the
    // last item matches the old paragraph-to-list gap.
    if depth == 0 {
        let swap = PARA_GAP_BEFORE_NEXT_BLOCK - LIST_GAP_AFTER_ITEM;
        ui.add_space(-swap);
    }

    while let Some(ev) = it.peek() {
        match ev {
            Event::Start(Tag::Item) => {
                it.next();
                let bullet = if ordered {
                    let s = format!("{num}.");
                    num += 1;
                    s
                } else {
                    "•".to_string()
                };

                // `Align::TOP`: default `horizontal` vertically centers the bullet with the full
                // multi-line item, which misaligns list markers vs first-line text.
                ui.with_layout(Layout::left_to_right(Align::TOP), |ui| {
                    ui.add_space(block_left);
                    ui.allocate_ui_with_layout(
                        vec2(bullet_col, 0.0),
                        Layout::left_to_right(Align::TOP),
                        |ui| {
                            ui.label(
                                RichText::new(bullet)
                                    .color(c_text_muted())
                                    .size(SZ_BODY)
                                    .family(FontFamily::Proportional),
                            );
                        },
                    );
                    ui.add_space(LIST_BULLET_GAP);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 1.0;
                        ui.set_width(text_w);
                        loop {
                            match it.peek() {
                                Some(Event::End(TagEnd::Item)) => {
                                    it.next();
                                    break;
                                }
                                Some(Event::Start(Tag::Paragraph)) => {
                                    it.next();
                                    inline::render_inline_until(
                                        ui,
                                        text_w,
                                        it,
                                        inline::InlineEnd::Paragraph,
                                        InlineDensity::ListItem,
                                    );
                                }
                                Some(Event::Start(Tag::List(nested))) => {
                                    let k = *nested;
                                    it.next();
                                    // Nested lists are already inside the parent item's text column. Keep
                                    // their width/indent relative to that column so list-in-list spacing is
                                    // stable instead of accumulating the outer list's marker offset again.
                                    render_list(ui, text_w, k, depth + 1, it);
                                }
                                Some(Event::Start(Tag::Heading { .. })) => {
                                    if let Some(Event::Start(Tag::Heading { level, .. })) =
                                        it.next()
                                    {
                                        render_heading(ui, text_w, level, it);
                                    }
                                }
                                None => break,
                                _ => {
                                    inline::render_inline_until(
                                        ui,
                                        text_w,
                                        it,
                                        inline::InlineEnd::Item,
                                        InlineDensity::ListItem,
                                    );
                                    break;
                                }
                            }
                        }
                    });
                });
                ui.add_space(LIST_GAP_AFTER_ITEM);
            }
            Event::End(TagEnd::List(_)) => {
                it.next();
                if depth == 0 {
                    let swap = PARA_GAP_BEFORE_NEXT_BLOCK - LIST_GAP_AFTER_ITEM;
                    ui.add_space(swap);
                }
                break;
            }
            _ => {
                it.next();
            }
        }
    }
}
