//! Markdown → egui. Inline text uses [`egui::text::LayoutJob`] with an explicit wrap
//! width so long paths do not layout as one character per row (horizontal_wrapped bug).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use base64::Engine as _;
use eframe::egui::text::{LayoutJob, TextFormat};
use eframe::egui::{
    vec2, Align, Color32, FontFamily, FontId, Frame, Hyperlink, Id, Image, Layout, Margin,
    RichText, Rounding, Stroke, Ui,
};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use crate::theme::content_wrap_width;
use crate::ui::preview_expand::{
    clickable_expand_overlay, expand_persist_id, is_expanded, truncate_lines_preview,
};

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

const BODY: Color32 = Color32::from_rgb(0xd4, 0xd4, 0xd8);
/// Cursor-style link blue (matches oxi `C_ACCENT`).
const LINK: Color32 = Color32::from_rgb(0x5c, 0xb3, 0xff);
const MUTED: Color32 = Color32::from_rgb(0x9d, 0x9d, 0xa4);
const CODE_BG: Color32 = Color32::from_rgb(0x2e, 0x30, 0x36);
const CODE_BLOCK_BG: Color32 = Color32::from_rgb(0x25, 0x25, 0x28);
const SZ_BODY: f32 = 13.5;
/// Slightly smaller than proportional body so `code` does not read as oversized next to prose.
const SZ_CODE_INLINE: f32 = 12.0;
const SZ_CODE: f32 = 12.5;

/// Lists are inset from body text so bullets do not sit flush on the column edge (Cursor-like).
const LIST_BLOCK_MARGIN: f32 = 12.0;
/// Additional left inset for each nested `<ul>` / `<ol>` level (must match CommonMark-style nesting).
const LIST_NEST_STEP: f32 = 28.0;
/// Extra indent when a nested unordered list sits inside an ordered list item (often under-indented in CM).
const NEST_UL_UNDER_OL_EXTRA: f32 = 20.0;
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

#[derive(Clone, Copy)]
enum InlineEnd {
    Paragraph,
    Item,
}

/// Tighter line metrics for list rows vs normal paragraphs.
#[derive(Clone, Copy)]
enum InlineDensity {
    Normal,
    ListItem,
}

impl InlineDensity {
    fn line_height(self, size: f32) -> f32 {
        match self {
            InlineDensity::Normal => (size * 1.35).max(16.0),
            InlineDensity::ListItem => (size * 1.10).max(13.5),
        }
    }
}

fn inline_text_format(size: f32, strong: u32, density: InlineDensity) -> TextFormat {
    let color = if strong > 0 {
        Color32::from_rgb(0xee, 0xee, 0xf4)
    } else {
        BODY
    };
    let lh = density.line_height(size);
    let mut f = TextFormat::simple(FontId::proportional(size), color);
    f.line_height = Some(lh);
    f.valign = Align::Center;
    f
}

fn inline_code_format(density: InlineDensity) -> TextFormat {
    let lh = density.line_height(SZ_BODY);
    let mut f = TextFormat::simple(
        FontId::proportional(SZ_BODY - 0.25),
        Color32::from_rgb(0xe1, 0xe4, 0xea),
    );
    f.background = CODE_BG;
    f.line_height = Some(lh);
    f.valign = Align::Center;
    f
}

const SZ_TINY: f32 = 12.0;

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

fn render_markdown_inline_image(
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
                .color(MUTED),
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

fn consume_until_end(it: &mut ParserPeek<'_>, end: TagEnd) {
    while let Some(ev) = it.next() {
        match ev {
            Event::End(e) if e == end => break,
            Event::Start(tag) => consume_until_end(it, tag.to_end()),
            _ => {}
        }
    }
}

/// Raw HTML / unknown blocks: show monospace so nothing is silently dropped.
fn render_raw_block(ui: &mut Ui, wrap_w: f32, label: &str, body: &str) {
    allocate_full_width_block(ui, wrap_w, |ui| {
        Frame::none()
            .fill(CODE_BLOCK_BG)
            .stroke(Stroke::new(1.0, Color32::from_rgb(0x48, 0x48, 0x4e)))
            .rounding(Rounding::same(8.0))
            .inner_margin(Margin::symmetric(10.0, 8.0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let inner = ui.available_width().max(40.0);
                ui.label(
                    RichText::new(label)
                        .size(SZ_TINY)
                        .color(MUTED)
                        .family(FontFamily::Proportional),
                );
                ui.add_space(4.0);
                let job = LayoutJob::simple(
                    body.to_string(),
                    FontId::monospace(SZ_CODE),
                    Color32::from_rgb(0xcc, 0xcc, 0xd0),
                    inner,
                );
                selectable_job(ui, job);
            });
    });
    ui.add_space(6.0);
}

fn render_html_block(ui: &mut Ui, wrap_w: f32, it: &mut ParserPeek<'_>) {
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

fn append_inline_fallback(job: &mut LayoutJob, wrap_w: f32, s: &str, line_height: f32) {
    let mut f = TextFormat::simple(
        FontId::monospace(SZ_CODE_INLINE),
        Color32::from_rgb(0xc8, 0xc8, 0xd0),
    );
    f.line_height = Some(line_height);
    f.valign = Align::Center;
    job.append(s, 0.0, f);
    set_job_wrap(job, wrap_w);
}

pub fn render_markdown(ui: &mut Ui, src: &str) {
    let wrap_w = content_wrap_width(ui);
    ui.set_max_width(wrap_w);

    let parser = Parser::new_ext(src, MD_OPTIONS);
    let mut it = parser.peekable();
    let mut fence_idx = 0u32;
    while let Some(ev) = it.next() {
        match ev {
            Event::Start(Tag::Paragraph) => render_paragraph(ui, wrap_w, &mut it),
            Event::Start(Tag::Heading { level, .. }) => {
                render_heading(ui, wrap_w, level, &mut it);
            }
            Event::Start(Tag::List(kind)) => render_list(ui, wrap_w, kind, 0, None, &mut it),
            Event::Start(Tag::CodeBlock(kind)) => {
                let base = ui.id().with("md_fence").with(fence_idx);
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

struct TableCellData {
    text: String,
    is_header: bool,
}

fn collect_table_data(it: &mut ParserPeek<'_>) -> Vec<Vec<TableCellData>> {
    let mut rows: Vec<Vec<TableCellData>> = Vec::new();
    loop {
        match it.peek() {
            Some(Event::End(TagEnd::Table)) => {
                it.next();
                break;
            }
            Some(Event::Start(Tag::TableHead)) => {
                it.next();
                loop {
                    match it.peek() {
                        Some(Event::End(TagEnd::TableHead)) => {
                            it.next();
                            break;
                        }
                        Some(Event::Start(Tag::TableRow)) => {
                            it.next();
                            rows.push(collect_row_cells(it, true));
                        }
                        Some(_) => {
                            it.next();
                        }
                        None => break,
                    }
                }
            }
            Some(Event::Start(Tag::TableRow)) => {
                it.next();
                rows.push(collect_row_cells(it, false));
            }
            Some(_) => {
                it.next();
            }
            None => break,
        }
    }
    rows
}

fn collect_row_cells(it: &mut ParserPeek<'_>, is_header: bool) -> Vec<TableCellData> {
    let mut cells = Vec::new();
    loop {
        match it.peek() {
            Some(Event::End(TagEnd::TableRow)) => {
                it.next();
                break;
            }
            Some(Event::Start(Tag::TableCell)) => {
                it.next();
                cells.push(TableCellData {
                    text: collect_cell_text(it),
                    is_header,
                });
            }
            Some(_) => {
                it.next();
            }
            None => break,
        }
    }
    cells
}

fn collect_cell_text(it: &mut ParserPeek<'_>) -> String {
    let mut text = String::new();
    loop {
        let ev = it.next();
        match ev {
            Some(Event::End(TagEnd::TableCell)) | None => break,
            Some(Event::Text(t)) => text.push_str(t.as_ref()),
            Some(Event::Code(c)) => text.push_str(c.as_ref()),
            Some(Event::SoftBreak) => text.push(' '),
            Some(Event::HardBreak) => text.push('\n'),
            Some(Event::InlineHtml(t)) | Some(Event::Html(t)) => text.push_str(t.as_ref()),
            Some(Event::Start(Tag::Link { dest_url, .. })) => {
                let mut label = String::new();
                loop {
                    match it.next() {
                        Some(Event::End(TagEnd::Link)) | None => break,
                        Some(Event::Text(t)) => label.push_str(t.as_ref()),
                        Some(Event::Code(c)) => label.push_str(c.as_ref()),
                        Some(Event::SoftBreak) => label.push(' '),
                        _ => {}
                    }
                }
                text.push_str(if label.is_empty() {
                    dest_url.as_ref()
                } else {
                    &label
                });
            }
            Some(Event::Start(Tag::Image { dest_url, .. })) => {
                let mut alt = String::new();
                loop {
                    match it.next() {
                        Some(Event::End(TagEnd::Image)) | None => break,
                        Some(Event::Text(t)) => alt.push_str(t.as_ref()),
                        _ => {}
                    }
                }
                text.push_str(if alt.is_empty() {
                    dest_url.as_ref()
                } else {
                    &alt
                });
            }
            Some(Event::Start(_)) | Some(Event::End(_)) => {}
            _ => {}
        }
    }
    text
}

fn render_table(ui: &mut Ui, wrap_w: f32, column_count: usize, it: &mut ParserPeek<'_>) {
    let rows = collect_table_data(it);
    if rows.is_empty() {
        return;
    }
    let cols = column_count.max(1);
    let grid = Color32::from_rgb(0x40, 0x40, 0x48);
    let outer = Color32::from_rgb(0x48, 0x48, 0x4e);
    let header_bg = Color32::from_rgb(0x2b, 0x2b, 0x30);
    let body_bg = Color32::from_rgb(0x25, 0x25, 0x28);
    const CELL_PAD_X: f32 = 10.0;
    const CELL_PAD_Y: f32 = 8.0;

    allocate_full_width_block(ui, wrap_w, |ui| {
        let table_w = ui.available_width().max(48.0);
        let cell_w = table_w / cols as f32;

        Frame::none()
            .fill(CODE_BLOCK_BG)
            .stroke(Stroke::new(1.0, outer))
            .rounding(Rounding::same(8.0))
            .inner_margin(Margin::same(0.0))
            .show(ui, |ui| {
                ui.set_width(table_w);
                ui.spacing_mut().item_spacing = vec2(0.0, 0.0);

                for row in rows.iter() {
                    let cell_jobs: Vec<(bool, LayoutJob)> = (0..cols)
                        .map(|col_idx| {
                            let cell = row.get(col_idx);
                            let is_header = cell.map(|c| c.is_header).unwrap_or(false);
                            let text = cell.map(|c| c.text.as_str()).unwrap_or("");
                            let mut job = LayoutJob::default();
                            set_job_wrap(&mut job, (cell_w - CELL_PAD_X * 2.0).max(24.0));
                            let fmt = if is_header {
                                inline_text_format(SZ_BODY, 1, InlineDensity::Normal)
                            } else {
                                inline_text_format(SZ_BODY, 0, InlineDensity::Normal)
                            };
                            job.append(text, 0.0, fmt);
                            (is_header, job)
                        })
                        .collect();

                    let row_h = cell_jobs
                        .iter()
                        .map(|(_, job)| ui.fonts(|fonts| fonts.layout_job(job.clone())).size().y)
                        .fold(0.0_f32, f32::max)
                        .max(22.0)
                        + CELL_PAD_Y * 2.0;

                    ui.horizontal(|ui| {
                        ui.set_width(table_w);
                        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);

                        for (is_header, job) in cell_jobs {
                            let (rect, _) =
                                ui.allocate_exact_size(vec2(cell_w, row_h), egui::Sense::hover());
                            let fill = if is_header { header_bg } else { body_bg };

                            ui.painter().rect_filled(rect, 0.0, fill);
                            ui.painter().rect_stroke(rect, 0.0, Stroke::new(1.0, grid));

                            let inner_rect = rect.shrink2(vec2(CELL_PAD_X, CELL_PAD_Y));
                            let mut child = ui.new_child(
                                egui::UiBuilder::new()
                                    .max_rect(inner_rect)
                                    .layout(Layout::top_down(Align::Min)),
                            );
                            selectable_job(&mut child, job);
                        }
                    });
                }
            });
    });
    ui.add_space(6.0);
}

/// `list_row_w`: width for this list’s rows — full column at depth 0, parent item **content** width
/// when nested (see recursive call with `text_w`).
fn render_list(
    ui: &mut Ui,
    list_row_w: f32,
    list_kind: Option<u64>,
    depth: u32,
    parent_list_ordered: Option<bool>,
    it: &mut ParserPeek<'_>,
) {
    // Caller already consumed `Event::Start(Tag::List(..))`; next is `Item`.
    let mut num = list_kind.unwrap_or(1);
    let ordered = list_kind.is_some();
    let column_w = list_row_w.min(ui.max_rect().width()).max(48.0);
    let nested_ul_under_ol = depth > 0 && parent_list_ordered == Some(true) && !ordered;
    let block_left = LIST_BLOCK_MARGIN
        + depth as f32 * LIST_NEST_STEP
        + if nested_ul_under_ol {
            NEST_UL_UNDER_OL_EXTRA
        } else {
            0.0
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
                                    .color(MUTED)
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
                                    render_inline_until(
                                        ui,
                                        text_w,
                                        it,
                                        InlineEnd::Paragraph,
                                        InlineDensity::ListItem,
                                    );
                                }
                                Some(Event::Start(Tag::List(nested))) => {
                                    let k = *nested;
                                    it.next();
                                    // Nested lists should inherit the full content width of the parent
                                    // item; `render_list` applies the visual indent for the deeper depth.
                                    render_list(ui, column_w, k, depth + 1, Some(ordered), it);
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
                                    render_inline_until(
                                        ui,
                                        text_w,
                                        it,
                                        InlineEnd::Item,
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

fn fmt_body(size: f32, strong: u32) -> TextFormat {
    let color = if strong > 0 {
        Color32::from_rgb(0xee, 0xee, 0xf4)
    } else {
        BODY
    };
    let lh = (size * 1.35).max(16.0);
    let mut f = TextFormat::simple(FontId::proportional(size), color);
    f.line_height = Some(lh);
    f.valign = Align::Center;
    f
}

fn fmt_code(line_height: f32) -> TextFormat {
    let mut f = TextFormat::simple(
        FontId::proportional(SZ_BODY - 0.5),
        Color32::from_rgb(0xe8, 0xe8, 0xee),
    );
    f.background = CODE_BG;
    f.line_height = Some(line_height);
    f.valign = Align::Center;
    f
}

/// Inline until paragraph or list item end, using a single [`LayoutJob`] with wrap width.
fn render_inline_until(
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
                    tf.strikethrough = Stroke::new(1.0, BODY);
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
                    RichText::new(label).color(LINK).size(SZ_BODY),
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
                append_inline_fallback(&mut job, wrap_w, t.as_ref(), density.line_height(SZ_BODY));
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

fn selectable_job(ui: &mut Ui, job: LayoutJob) {
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

fn render_heading(ui: &mut Ui, wrap_w: f32, level: HeadingLevel, it: &mut ParserPeek<'_>) {
    let size = match level {
        HeadingLevel::H1 => 18.5,
        HeadingLevel::H2 => 16.75,
        HeadingLevel::H3 => 15.0,
        HeadingLevel::H4 => 14.0,
        HeadingLevel::H5 | HeadingLevel::H6 => 13.25,
    };
    let mut job = LayoutJob::default();
    set_job_wrap(&mut job, wrap_w);

    let mut bold = 0u32;
    while let Some(ev) = it.next() {
        match ev {
            Event::End(TagEnd::Heading(_)) => break,
            Event::Start(Tag::Strong) | Event::Start(Tag::Emphasis) => bold += 1,
            Event::End(TagEnd::Strong) | Event::End(TagEnd::Emphasis) => {
                bold = bold.saturating_sub(1);
            }
            Event::Text(t) => {
                let strong = if bold > 0 { bold } else { 1 };
                job.append(t.as_ref(), 0.0, fmt_body(size, strong));
            }
            Event::Code(c) => {
                let lh = line_height_for_body_size(size);
                let code_px = (size - 2.25).max(SZ_CODE_INLINE);
                let mut f = fmt_code(lh);
                f.font_id = FontId::monospace(code_px);
                job.append(c.as_ref(), 0.0, f);
            }
            Event::SoftBreak => {
                let strong = if bold > 0 { bold } else { 1 };
                job.append(" ", 0.0, fmt_body(size, strong));
            }
            Event::HardBreak => {
                let strong = if bold > 0 { bold } else { 1 };
                job.append("\n", 0.0, fmt_body(size, strong));
            }
            Event::Html(t) | Event::InlineHtml(t) => {
                let strong = if bold > 0 { bold } else { 1 };
                job.append(t.as_ref(), 0.0, fmt_body(size, strong));
            }
            Event::Start(tag) => {
                consume_until_end(it, tag.to_end());
            }
            _ => {}
        }
    }
    selectable_job(ui, job);
    ui.add_space(6.0);
}

fn render_paragraph(ui: &mut Ui, wrap_w: f32, it: &mut ParserPeek<'_>) {
    render_inline_until(ui, wrap_w, it, InlineEnd::Paragraph, InlineDensity::Normal);
    ui.add_space(4.0);
}

fn render_blockquote(ui: &mut Ui, wrap_w: f32, it: &mut ParserPeek<'_>) {
    allocate_full_width_block(ui, wrap_w, |ui| {
        Frame::none()
            .fill(Color32::from_rgb(0x1a, 0x1a, 0x1e))
            .rounding(Rounding::same(8.0))
            .stroke(Stroke::new(1.0, Color32::from_rgb(0x3a, 0x3a, 0x42)))
            .inner_margin(Margin::symmetric(10.0, 8.0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let inner_w = ui.available_width().max(32.0);
                while let Some(ev) = it.next() {
                    match ev {
                        Event::End(TagEnd::BlockQuote(_)) => break,
                        Event::Start(Tag::Paragraph) => render_paragraph(ui, inner_w, it),
                        Event::Start(Tag::List(kind)) => {
                            render_list(ui, inner_w, kind, 0, None, it)
                        }
                        Event::Start(Tag::Heading { level, .. }) => {
                            render_heading(ui, inner_w, level, it);
                        }
                        _ => {}
                    }
                }
            });
    });
    ui.add_space(6.0);
}

fn render_fenced_block(
    ui: &mut Ui,
    wrap_w: f32,
    _kind: CodeBlockKind<'_>,
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
    const PREVIEW_LINES: usize = 10;
    let overflows = buf.lines().count() > PREVIEW_LINES || buf.len() > 2000;
    let persist_id = expand_persist_id(block_base_id);
    allocate_full_width_block(ui, wrap_w, |ui| {
        let frame = Frame::none()
            .fill(CODE_BLOCK_BG)
            .stroke(Stroke::new(1.0, Color32::from_rgb(0x48, 0x48, 0x4e)))
            .rounding(Rounding::same(8.0))
            .inner_margin(Margin::symmetric(10.0, 8.0))
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
                    Color32::from_rgb(0xcc, 0xcc, 0xd0),
                    inner,
                );
                selectable_job(ui, job);
            });
        if overflows {
            clickable_expand_overlay(ui, frame.response.rect, persist_id);
        }
    });
    ui.add_space(6.0);
}
