//! GFM table rendering: collect a table's cell text from the pulldown-cmark event
//! stream, then paint a bordered grid sized to the column count.

use eframe::egui::text::LayoutJob;
use eframe::egui::{Align, Layout, Stroke, vec2};
use pulldown_cmark::{Alignment, Event, Tag, TagEnd};

use crate::theme::*;

use super::inline::{InlineDensity, inline_text_format, selectable_job};
use super::{ParserPeek, SZ_BODY, allocate_full_width_block, set_job_wrap};

struct TableCellData {
    text: String,
    images: Vec<(String, String)>,
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
                // pulldown-cmark puts the header cells directly inside `TableHead`, with no
                // `TableRow` around them; accept both shapes.
                let mut header = Vec::new();
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
                        Some(Event::Start(Tag::TableCell)) => {
                            it.next();
                            header.push(collect_cell(it, true));
                        }
                        Some(_) => {
                            it.next();
                        }
                        None => break,
                    }
                }
                if !header.is_empty() {
                    rows.insert(0, header);
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
                cells.push(collect_cell(it, is_header));
            }
            Some(_) => {
                it.next();
            }
            None => break,
        }
    }
    cells
}

fn collect_cell(it: &mut ParserPeek<'_>, is_header: bool) -> TableCellData {
    let mut cell = TableCellData {
        text: String::new(),
        images: Vec::new(),
        is_header,
    };
    let mut link = None;
    let mut link_start = 0;
    let mut image_start = 0;
    loop {
        match it.next() {
            Some(Event::End(TagEnd::TableCell)) | None => break,
            Some(Event::Text(t) | Event::Code(t) | Event::InlineHtml(t) | Event::Html(t)) => {
                cell.text.push_str(&t);
            }
            Some(Event::SoftBreak) => cell.text.push(' '),
            Some(Event::HardBreak) => cell.text.push('\n'),
            Some(Event::Start(Tag::Link { dest_url, .. })) => {
                link = Some(dest_url);
                link_start = cell.text.len();
                image_start = cell.images.len();
            }
            Some(Event::End(TagEnd::Link)) => {
                if cell.text.len() == link_start
                    && cell.images.len() == image_start
                    && let Some(destination) = link.take()
                {
                    cell.text.push_str(&destination);
                }
                link = None;
            }
            Some(Event::Start(Tag::Image { dest_url, .. })) => {
                let mut alt = String::new();
                for inner in it.by_ref() {
                    match inner {
                        Event::End(TagEnd::Image) => break,
                        Event::Text(text) | Event::Code(text) => alt.push_str(&text),
                        _ => {}
                    }
                }
                cell.images.push((dest_url.to_string(), alt));
            }
            _ => {}
        }
    }
    cell
}

pub(super) fn render_table(
    ui: &mut eframe::egui::Ui,
    wrap_w: f32,
    alignments: &[Alignment],
    it: &mut ParserPeek<'_>,
) {
    let rows = collect_table_data(it);
    if rows.is_empty() {
        return;
    }
    let cols = alignments.len().max(1);
    let grid = c_md_code_block_border();
    let outer = c_border();
    let header_bg = c_md_code_block_header_bg();
    let body_bg = c_md_code_block_bg();
    const CELL_PAD_X: f32 = 10.0;
    const CELL_PAD_Y: f32 = 8.0;

    allocate_full_width_block(ui, wrap_w, |ui| {
        let table_w = ui.available_width().max(48.0);
        let cell_w = table_w / cols as f32;

        eframe::egui::Frame::new()
            .fill(c_md_code_block_bg())
            .stroke(Stroke::new(1.0, outer))
            .corner_radius(eframe::egui::CornerRadius::same(crate::theme::RADIUS_CHIP))
            .inner_margin(eframe::egui::Margin::same(0))
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
                                inline_text_format(SZ_BODY, 1, false, InlineDensity::Normal)
                            } else {
                                inline_text_format(SZ_BODY, 0, false, InlineDensity::Normal)
                            };
                            job.append(text, 0.0, fmt);
                            (is_header, job)
                        })
                        .collect();

                    let mut backgrounds = Vec::new();
                    let row = ui.horizontal_top(|ui| {
                        ui.set_width(table_w);
                        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                        for (col_idx, (is_header, job)) in cell_jobs.into_iter().enumerate() {
                            // Reserve the background before rendering so it stays behind images/text.
                            let background = ui.painter().add(egui::Shape::Noop);
                            let halign = match alignments.get(col_idx) {
                                Some(Alignment::Center) => Align::Center,
                                Some(Alignment::Right) => Align::Max,
                                _ => Align::Min,
                            };
                            let response = ui.allocate_ui_with_layout(
                                vec2(cell_w, 0.0),
                                Layout::top_down(halign),
                                |ui| {
                                    eframe::egui::Frame::new()
                                        .inner_margin(eframe::egui::Margin::symmetric(
                                            CELL_PAD_X as i8,
                                            CELL_PAD_Y as i8,
                                        ))
                                        .show(ui, |ui| {
                                            let inner_w = (cell_w - CELL_PAD_X * 2.0).max(24.0);
                                            ui.set_width(inner_w);
                                            if !job.text.is_empty() {
                                                selectable_job(ui, job);
                                            }
                                            if let Some(cell) = row.get(col_idx) {
                                                for (uri, alt) in &cell.images {
                                                    super::inline::render_markdown_inline_image(
                                                        ui, inner_w, uri, alt, false,
                                                    );
                                                }
                                            }
                                        });
                                },
                            );
                            backgrounds.push((background, response.response.rect, is_header));
                        }
                    });
                    for (background, mut rect, is_header) in backgrounds {
                        rect.max.y = row.response.rect.bottom();
                        ui.painter().set(
                            background,
                            egui::Shape::Vec(vec![
                                egui::Shape::rect_filled(
                                    rect,
                                    0.0,
                                    if is_header { header_bg } else { body_bg },
                                ),
                                egui::Shape::rect_stroke(
                                    rect,
                                    0.0,
                                    Stroke::new(1.0, grid),
                                    egui::StrokeKind::Middle,
                                ),
                            ]),
                        );
                    }
                }
            });
    });
    ui.add_space(6.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table_rows(markdown: &str) -> Vec<Vec<(String, bool)>> {
        let mut it = super::super::markdown_parser(markdown, None);
        while let Some(event) = it.next() {
            if let Event::Start(Tag::Table(_)) = event {
                return collect_table_data(&mut it)
                    .into_iter()
                    .map(|row| row.into_iter().map(|c| (c.text, c.is_header)).collect())
                    .collect();
            }
        }
        Vec::new()
    }

    #[test]
    fn table_keeps_its_header_row() {
        let rows = table_rows("| column | value |\n|---|---|\n| retries | 3 |\n");
        assert_eq!(
            rows,
            vec![
                vec![("column".into(), true), ("value".into(), true)],
                vec![("retries".into(), false), ("3".into(), false)],
            ]
        );
    }
}
