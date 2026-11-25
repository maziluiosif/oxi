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
use std::path::Path;

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
/// Compact monospace size for inline `code` pills so they balance visually inside body prose.
const SZ_CODE_INLINE: f32 = 12.5;
const SZ_CODE: f32 = FS_CODE;
const SZ_TINY: f32 = FS_TINY;

/// Clean Antigravity-style list inset and tight hanging bullet column.
const LIST_BLOCK_MARGIN: f32 = 6.0;
/// Left inset for a nested list, relative to the parent item text column.
const LIST_NESTED_MARGIN: f32 = 12.0;
const LIST_BULLET_GAP: f32 = 4.0;
const LIST_BULLET_COL_UNORD: f32 = 14.0;
/// Wide enough for two-digit ordered markers (`10.`).
const LIST_BULLET_COL_ORD: f32 = 22.0;
/// Gap after each list row. Also the target exterior gap *before* the first item when we swap margins.
const LIST_GAP_AFTER_ITEM: f32 = 6.0;
/// Must match `render_inline_until` Normal tail (`2.0`) + `render_paragraph` spacing (`8.0`): gap the preceding paragraph leaves before the next block.
const PARA_GAP_BEFORE_NEXT_BLOCK: f32 = 2.0 + 8.0;

type ParserPeek<'a> = std::iter::Peekable<MarkdownParser<'a>>;

struct MarkdownParser<'a> {
    parser: Parser<'a>,
    base_dir: Option<&'a Path>,
}

impl<'a> Iterator for MarkdownParser<'a> {
    type Item = Event<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        self.parser.next().map(|event| match event {
            Event::Start(Tag::Image {
                link_type,
                dest_url,
                title,
                id,
            }) => {
                let dest_url = self
                    .base_dir
                    .and_then(|base| resolve_image_uri(base, &dest_url))
                    .map_or(dest_url, Into::into);
                Event::Start(Tag::Image {
                    link_type,
                    dest_url,
                    title,
                    id,
                })
            }
            event => event,
        })
    }
}

fn markdown_parser<'a>(src: &'a str, base_dir: Option<&'a Path>) -> ParserPeek<'a> {
    MarkdownParser {
        parser: Parser::new_ext(src, MD_OPTIONS),
        base_dir,
    }
    .peekable()
}

/// Resolve Markdown URL paths without altering remote or embedded image sources.
fn resolve_image_uri(base_dir: &Path, destination: &str) -> Option<String> {
    if url::Url::parse(destination).is_ok() {
        return None;
    }
    let base = url::Url::from_directory_path(base_dir).ok()?;
    let resolved = base.join(destination).ok()?;
    // egui's file loader expects a filesystem path after `file://`; it does not
    // percent-decode URL paths. Decode spaces/Unicode before passing it to the loader.
    let path = resolved.to_file_path().ok()?;
    if cfg!(windows) {
        let path = path.to_str()?.replace('\\', "/");
        // The loader treats a URI host as a UNC server. Local drive paths need
        // an empty host (`file:///C:/...`), while UNC paths retain their host.
        Some(if path.starts_with("//") {
            format!("file:{path}")
        } else {
            format!("file:///{path}")
        })
    } else {
        Some(format!("file://{}", path.display()))
    }
}

/// Parser flags for assistant markdown: GFM-ish features without enabling math (avoids
/// `$...$` being parsed as formulas and dropped by our inline renderer).
const MD_OPTIONS: Options = Options::ENABLE_STRIKETHROUGH
    .union(Options::ENABLE_TABLES)
    .union(Options::ENABLE_TASKLISTS)
    .union(Options::ENABLE_SMART_PUNCTUATION)
    .union(Options::ENABLE_GFM);

/// Markdown source of the `idx`-th top-level blockquote with its `>` markers removed, so a
/// quoted reply pastes as the markdown it was written in. Parsed on demand (copy clicks only).
fn blockquote_source(src: &str, idx: usize) -> String {
    let mut depth = 0usize;
    let mut seen = 0usize;
    for (ev, range) in Parser::new_ext(src, MD_OPTIONS).into_offset_iter() {
        match ev {
            Event::Start(Tag::BlockQuote(_)) if depth == 0 => {
                if seen == idx {
                    return strip_quote_markers(&src[range]);
                }
                seen += 1;
                depth += 1;
            }
            Event::Start(_) => depth += 1,
            Event::End(_) => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    String::new()
}

/// Drop one level of `>` from each line; lazy continuation lines are kept as written.
fn strip_quote_markers(quote: &str) -> String {
    let lines: Vec<&str> = quote
        .lines()
        .map(|line| {
            let trimmed = line.trim_start();
            match trimmed.strip_prefix('>') {
                Some(rest) if line.len() - trimmed.len() <= 3 => {
                    rest.strip_prefix([' ', '\t']).unwrap_or(rest)
                }
                _ => line,
            }
        })
        .collect();
    lines.join("\n").trim().to_string()
}

#[inline]
fn line_height_for_body_size(size: f32) -> f32 {
    (size * 1.55).max(18.0)
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
    render_markdown_impl(ui, src, None, content_wrap_width(ui));
}

pub(crate) fn render_markdown_for_file(ui: &mut Ui, src: &str, path: &Path) {
    render_markdown_impl(ui, src, path.parent(), ui.available_width().max(1.0));
}

fn render_markdown_impl(ui: &mut Ui, src: &str, base_dir: Option<&Path>, wrap_w: f32) {
    ui.set_max_width(wrap_w);

    let mut it = markdown_parser(src, base_dir);
    let mut fence_idx = 0u32;
    let mut quote_idx = 0usize;
    while let Some(ev) = it.next() {
        match ev {
            Event::Start(Tag::Paragraph) => blocks::render_paragraph(ui, wrap_w, &mut it),
            Event::Start(Tag::Heading { level, .. }) => {
                render_heading(ui, wrap_w, level, &mut it);
            }
            Event::Start(Tag::List(kind)) => {
                render_list(ui, wrap_w, kind, 0, &mut it, &mut fence_idx)
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                let lang = blocks::code_block_language(&kind);
                let base = ui.id().with("md_fence").with(fence_idx).with(&lang);
                fence_idx += 1;
                render_fenced_block(ui, wrap_w, kind, &mut it, base);
            }
            Event::Start(Tag::BlockQuote(_)) => {
                render_blockquote(ui, wrap_w, &mut it, Some((src, quote_idx)), &mut fence_idx);
                quote_idx += 1;
            }
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
                render_table(ui, wrap_w, &alignments, &mut it);
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

/// Block-level events that terminate an inline run inside a tight list item and must be
/// handled by [`render_list`]'s item loop rather than swallowed by [`inline::render_inline_until`].
pub(super) fn is_list_item_block_start(ev: &Event<'_>) -> bool {
    matches!(
        ev,
        Event::Start(
            Tag::Paragraph
                | Tag::List(_)
                | Tag::Heading { .. }
                | Tag::CodeBlock(_)
                | Tag::BlockQuote(_)
                | Tag::Table(_)
                | Tag::HtmlBlock
                | Tag::MetadataBlock(_)
                | Tag::FootnoteDefinition(_)
                | Tag::DefinitionList
                | Tag::DefinitionListTitle
                | Tag::DefinitionListDefinition
        ) | Event::Rule
    )
}

/// `list_row_w`: width for this list’s rows — full column at depth 0, parent item **content** width
/// when nested (see recursive call with `text_w`).
fn render_list(
    ui: &mut Ui,
    list_row_w: f32,
    list_kind: Option<u64>,
    depth: u32,
    it: &mut ParserPeek<'_>,
    fence_idx: &mut u32,
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

    let mut rendered_items = 0usize;
    while let Some(ev) = it.peek() {
        match ev {
            Event::Start(Tag::Item) => {
                it.next();
                if rendered_items > 0 {
                    ui.add_space(LIST_GAP_AFTER_ITEM);
                }
                rendered_items += 1;

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
                        vec2(bullet_col, line_height_for_body_size(SZ_BODY)),
                        Layout::left_to_right(Align::Center),
                        |ui| {
                            ui.set_min_width(bullet_col);
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
                        let mut has_content = false;
                        loop {
                            match it.peek() {
                                Some(Event::End(TagEnd::Item)) => {
                                    it.next();
                                    break;
                                }
                                Some(Event::End(TagEnd::List(_))) | None => break,
                                Some(ev) if is_list_item_block_start(ev) => {
                                    if has_content {
                                        ui.add_space(4.0);
                                    }
                                    has_content = true;
                                    match it.next() {
                                        Some(Event::Start(Tag::Paragraph)) => {
                                            inline::render_inline_until(
                                                ui,
                                                text_w,
                                                it,
                                                inline::InlineEnd::Paragraph,
                                                InlineDensity::ListItem,
                                            );
                                        }
                                        Some(Event::Start(Tag::List(nested))) => {
                                            // Nested lists are already inside the parent item's
                                            // text column. Keep their width/indent relative to
                                            // that column so list-in-list spacing is stable
                                            // instead of accumulating the outer list's marker
                                            // offset again.
                                            render_list(
                                                ui,
                                                text_w,
                                                nested,
                                                depth + 1,
                                                it,
                                                fence_idx,
                                            );
                                        }
                                        Some(Event::Start(Tag::Heading { level, .. })) => {
                                            render_heading(ui, text_w, level, it);
                                        }
                                        Some(Event::Start(Tag::CodeBlock(kind))) => {
                                            let lang = blocks::code_block_language(&kind);
                                            let base = ui
                                                .id()
                                                .with("md_fence")
                                                .with(*fence_idx)
                                                .with(&lang);
                                            *fence_idx += 1;
                                            render_fenced_block(ui, text_w, kind, it, base);
                                        }
                                        Some(Event::Start(Tag::BlockQuote(_))) => {
                                            render_blockquote(ui, text_w, it, None, fence_idx);
                                        }
                                        Some(Event::Start(Tag::Table(alignments))) => {
                                            render_table(ui, text_w, &alignments, it);
                                        }
                                        Some(Event::Start(Tag::HtmlBlock)) => {
                                            render_html_block(ui, text_w, it);
                                        }
                                        Some(Event::Rule) => {
                                            allocate_full_width_block(ui, text_w, |ui| {
                                                ui.add_space(4.0);
                                                ui.separator();
                                                ui.add_space(6.0);
                                            });
                                        }
                                        Some(Event::Start(tag)) => {
                                            consume_until_end(it, tag.to_end());
                                        }
                                        _ => {}
                                    }
                                }
                                Some(_) => {
                                    if has_content {
                                        ui.add_space(4.0);
                                    }
                                    has_content = true;
                                    inline::render_inline_until(
                                        ui,
                                        text_w,
                                        it,
                                        inline::InlineEnd::Item,
                                        InlineDensity::ListItem,
                                    );
                                }
                            }
                        }
                    });
                });
            }
            Event::End(TagEnd::List(_)) => {
                it.next();
                break;
            }
            _ => {
                it.next();
            }
        }
    }

    if depth == 0 {
        if rendered_items > 0 {
            ui.add_space(LIST_GAP_AFTER_ITEM);
        }
        let swap = PARA_GAP_BEFORE_NEXT_BLOCK - LIST_GAP_AFTER_ITEM;
        ui.add_space(swap);
    }
}

#[cfg(test)]
mod image_uri_tests {
    use super::*;

    #[test]
    fn relative_image_uri_decodes_spaces_unicode_and_parent_segments() {
        let base = if cfg!(windows) {
            Path::new(r"C:\oxi docs\nested")
        } else {
            Path::new("/oxi docs/nested")
        };
        let expected = if cfg!(windows) {
            "file:///C:/oxi docs/imagine ș.png"
        } else {
            "file:///oxi docs/imagine ș.png"
        };
        assert_eq!(
            resolve_image_uri(base, "../imagine%20%C8%99.png").as_deref(),
            Some(expected)
        );
        for destination in [
            "https://example.com/image.png",
            "data:image/png;base64,AAAA",
        ] {
            assert_eq!(resolve_image_uri(base, destination), None);
        }
    }

    #[cfg(windows)]
    #[test]
    fn relative_image_uri_preserves_unc_host() {
        assert_eq!(
            resolve_image_uri(
                Path::new(r"\\server\share\oxi docs"),
                "imagine%20%C8%99.png"
            )
            .as_deref(),
            Some("file://server/share/oxi docs/imagine ș.png")
        );
    }
}

#[cfg(test)]
mod blockquote_source_tests {
    use super::*;

    #[test]
    fn copies_the_nth_top_level_quote_as_markdown() {
        let src = "Intro\n\n> First `code`\n>\n> second para\n\n- item\n  > in a list\n\n> Second\nlazy line\n";
        assert_eq!(blockquote_source(src, 0), "First `code`\n\nsecond para");
        // The quote nested in the list is not rendered as a top-level quote, so it is skipped.
        assert_eq!(blockquote_source(src, 1), "Second\nlazy line");
        assert_eq!(blockquote_source(src, 2), "");
    }

    #[test]
    fn strips_only_one_marker_level() {
        assert_eq!(strip_quote_markers("> a\n> > b\n>c"), "a\n> b\nc");
    }
}

#[cfg(test)]
mod nested_list_tests {
    use eframe::egui::{self, Pos2, RawInput, Rect};

    #[derive(Debug)]
    struct RenderedText {
        text: String,
        x: f32,
        y: f32,
    }

    fn render_texts(markdown: &str) -> Vec<RenderedText> {
        let ctx = egui::Context::default();
        crate::theme::setup_style(&ctx);
        let output = ctx.run_ui(
            RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(720.0, 2000.0))),
                ..Default::default()
            },
            |ui| crate::markdown::render_markdown(ui, markdown),
        );
        let mut out = Vec::new();
        fn collect(shapes: &[egui::Shape], out: &mut Vec<RenderedText>) {
            for shape in shapes {
                match shape {
                    egui::Shape::Text(t) => out.push(RenderedText {
                        text: t.galley.text().to_owned(),
                        x: t.pos.x,
                        y: t.pos.y,
                    }),
                    egui::Shape::Vec(inner) => collect(inner, out),
                    _ => {}
                }
            }
        }
        for clipped in &output.shapes {
            match &clipped.shape {
                egui::Shape::Text(t) => out.push(RenderedText {
                    text: t.galley.text().to_owned(),
                    x: t.pos.x,
                    y: t.pos.y,
                }),
                egui::Shape::Vec(inner) => collect(inner, &mut out),
                _ => {}
            }
        }
        out
    }

    fn find_exact<'a>(items: &'a [RenderedText], needle: &str) -> &'a RenderedText {
        items
            .iter()
            .find(|item| item.text == needle)
            .unwrap_or_else(|| panic!("expected rendered text {needle:?}, got: {items:#?}"))
    }

    fn count_exact(items: &[RenderedText], needle: &str) -> usize {
        items.iter().filter(|item| item.text == needle).count()
    }

    #[test]
    fn nested_lists_preserve_all_items_markers_and_separation() {
        let md = "\
### Bug-uri reale / edge cases de reparat
1. **Ștergerea unui chat Lead** — [sessions.rs](file:///Users/manu/Projects/oxi/src/app/sessions.rs#L470-L476)
   - `delete_session` construiește cheia ACP manual.
   - Dacă ștergi un chat Lead care are workeri activi, rămân orfane.
2. **Quota nu se mai reîmprospătează** — [chat_status.rs](file:///Users/manu/Projects/oxi/src/app/chat_status.rs#L486-L524)
   - `subscription_status` declanșează refresh doar pentru Lead.
3. **Antigravity pool** — [quota.rs](file:///Users/manu/Projects/oxi/src/router/quota.rs#L904-L924)
   - Fereastra Gemini are scope None.
4. **Lead la 0% quota** — [routing.rs](file:///Users/manu/Projects/oxi/src/agent/runner/routing.rs#L379-L420)
   - Încearcă mai întâi subprocesul epuizat.

### Lipsuri UX
5. **Înapoi la Lead** — [delegation.rs](file:///Users/manu/Projects/oxi/src/app/delegation.rs#L1387-L1555)
   - Din Lead ai buton `Open`.
6. **Indicator reroute** — [delegation.rs](file:///Users/manu/Projects/oxi/src/app/delegation.rs#L1398-L1436)
   - Nu apare în UI.
";

        let texts = render_texts(md);

        // Every outer ordered marker renders exactly once (no missing items, no misnumbered nested bullets).
        for marker in ["1.", "2.", "3.", "4.", "5.", "6."] {
            assert_eq!(
                count_exact(&texts, marker),
                1,
                "marker {marker:?} should appear once, got: {texts:#?}"
            );
        }
        // All 7 nested unordered bullets render with `•`.
        assert_eq!(
            count_exact(&texts, "•"),
            7,
            "expected 7 nested unordered bullets, got: {texts:#?}"
        );

        // Parent items are rendered intact and NOT glued to nested bullet text.
        let item1 = find_exact(&texts, "Ștergerea unui chat Lead — sessions.rs");
        let sub1a = find_exact(&texts, "delete_session construiește cheia ACP manual.");
        let sub1b = find_exact(
            &texts,
            "Dacă ștergi un chat Lead care are workeri activi, rămân orfane.",
        );
        let item2 = find_exact(&texts, "Quota nu se mai reîmprospătează — chat_status.rs");
        let sub2a = find_exact(
            &texts,
            "subscription_status declanșează refresh doar pentru Lead.",
        );
        let item3 = find_exact(&texts, "Antigravity pool — quota.rs");
        let sub3a = find_exact(&texts, "Fereastra Gemini are scope None.");
        let item4 = find_exact(&texts, "Lead la 0% quota — routing.rs");
        let sub4a = find_exact(&texts, "Încearcă mai întâi subprocesul epuizat.");
        let item5 = find_exact(&texts, "Înapoi la Lead — delegation.rs");
        let sub5a = find_exact(&texts, "Din Lead ai buton Open.");
        let item6 = find_exact(&texts, "Indicator reroute — delegation.rs");
        let sub6a = find_exact(&texts, "Nu apare în UI.");

        assert!(
            texts
                .iter()
                .all(|t| !t.text.contains("sessions.rsdelete_session")
                    && !t.text.contains("delegation.rsDin Lead")),
            "nested bullet text must never glue onto parent link text"
        );

        // Top-to-bottom vertical ordering across all items and sub-items.
        let ordered_y = [
            item1.y, sub1a.y, sub1b.y, item2.y, sub2a.y, item3.y, sub3a.y, item4.y, sub4a.y,
            item5.y, sub5a.y, item6.y, sub6a.y,
        ];
        for pair in ordered_y.windows(2) {
            assert!(
                pair[0] < pair[1],
                "items must advance vertically: {ordered_y:?}"
            );
        }

        // Nested bullets are indented strictly to the right of outer markers and text.
        let m1 = find_exact(&texts, "1.");
        let m2 = find_exact(&texts, "2.");
        assert!(
            (m2.y - item2.y).abs() < 6.0,
            "marker `2.` (y={}) must sit on item 2 (y={}), not nested bullet 1b (y={})",
            m2.y,
            item2.y,
            sub1b.y
        );
        let bullets: Vec<&RenderedText> = texts.iter().filter(|t| t.text == "•").collect();
        for b in &bullets {
            assert!(
                b.x > m1.x,
                "nested bullet x={} must be indented right of outer marker x={}",
                b.x,
                m1.x
            );
        }
        assert!(
            sub1a.x > item1.x && sub1b.x > item1.x && sub5a.x > item5.x,
            "nested item text must be indented right of parent item text"
        );
    }

    #[test]
    fn deeply_nested_mixed_ordered_and_unordered_lists() {
        let md = "\
1. Level one first
   - Level two alpha
     1. Level three one
     2. Level three two
   - Level two beta
2. Level one second
   - Level two gamma
     - Level three bullet
       1. Level four ordered
";
        let texts = render_texts(md);

        let l1_a = find_exact(&texts, "Level one first");
        let l2_a = find_exact(&texts, "Level two alpha");
        let l3_1 = find_exact(&texts, "Level three one");
        let l3_2 = find_exact(&texts, "Level three two");
        let l2_b = find_exact(&texts, "Level two beta");
        let l1_b = find_exact(&texts, "Level one second");
        let l2_c = find_exact(&texts, "Level two gamma");
        let l3_b = find_exact(&texts, "Level three bullet");
        let l4_1 = find_exact(&texts, "Level four ordered");

        // Monotonic indentation by nesting depth.
        assert!(l1_a.x < l2_a.x && l2_a.x < l3_1.x);
        assert!((l3_1.x - l3_2.x).abs() < 1.0);
        assert!((l2_a.x - l2_b.x).abs() < 1.0);
        assert!((l1_a.x - l1_b.x).abs() < 1.0);
        assert!(l1_b.x < l2_c.x && l2_c.x < l3_b.x && l3_b.x < l4_1.x);

        // Correct marker counts: three `1.` (outer, level-3, level-4), two `2.` (outer, level-3), four `•`.
        assert_eq!(count_exact(&texts, "1."), 3);
        assert_eq!(count_exact(&texts, "2."), 2);
        assert_eq!(count_exact(&texts, "•"), 4);

        // Strict top-to-bottom vertical order.
        let ys = [
            l1_a.y, l2_a.y, l3_1.y, l3_2.y, l2_b.y, l1_b.y, l2_c.y, l3_b.y, l4_1.y,
        ];
        for pair in ys.windows(2) {
            assert!(pair[0] < pair[1], "vertical order violated: {ys:?}");
        }
    }

    #[test]
    fn loose_nested_lists_and_multi_paragraph_items() {
        let md = "\
1. First paragraph

   Second paragraph

   - Loose child A

   - Loose child B

2. Outer sibling
";
        let texts = render_texts(md);
        let p1 = find_exact(&texts, "First paragraph");
        let p2 = find_exact(&texts, "Second paragraph");
        let ca = find_exact(&texts, "Loose child A");
        let cb = find_exact(&texts, "Loose child B");
        let s2 = find_exact(&texts, "Outer sibling");

        assert_eq!(count_exact(&texts, "1."), 1);
        assert_eq!(count_exact(&texts, "2."), 1);
        assert_eq!(count_exact(&texts, "•"), 2);
        assert!(p1.y < p2.y && p2.y < ca.y && ca.y < cb.y && cb.y < s2.y);
        assert!(ca.x > p1.x && cb.x > p1.x);
    }

    #[test]
    fn tight_list_items_with_code_block_blockquote_tasks_and_empty_parent() {
        let md = "\
1. Parent with code
   ```rust
   let x = 42;
   ```
2. Parent with quote
   > Quoted line
3. Sibling after quote

- [x] Done parent
  - [ ] Pending child
- [ ] Sibling task

-
  - Only nested child
- After empty parent
";
        let texts = render_texts(md);
        let p_code = find_exact(&texts, "Parent with code");
        let code = find_exact(&texts, "let x = 42;");
        let p_quote = find_exact(&texts, "Parent with quote");
        let quote = find_exact(&texts, "Quoted line");
        let sibling = find_exact(&texts, "Sibling after quote");
        let done_parent = find_exact(&texts, "☑ Done parent");
        let pending_child = find_exact(&texts, "☐ Pending child");
        let sibling_task = find_exact(&texts, "☐ Sibling task");
        let only_nested = find_exact(&texts, "Only nested child");
        let after_empty = find_exact(&texts, "After empty parent");

        assert_eq!(count_exact(&texts, "1."), 1);
        assert_eq!(count_exact(&texts, "2."), 1);
        assert_eq!(count_exact(&texts, "3."), 1);
        assert!(
            p_code.y < code.y && code.y < p_quote.y && p_quote.y < quote.y && quote.y < sibling.y
        );
        assert!(done_parent.y < pending_child.y && pending_child.y < sibling_task.y);
        assert!(pending_child.x > done_parent.x);
        assert!(only_nested.y < after_empty.y && only_nested.x > after_empty.x);
    }
}
