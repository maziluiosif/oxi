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

                    let row_h = cell_jobs
                        .iter()
                        .map(|(_, job)| {
                            ui.fonts_mut(|fonts| fonts.layout_job(job.clone())).size().y
                        })
                        .fold(0.0_f32, f32::max)
                        .max(22.0)
                        + CELL_PAD_Y * 2.0;

                    ui.horizontal(|ui| {
                        ui.set_width(table_w);
                        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);

                        for (col_idx, (is_header, job)) in cell_jobs.into_iter().enumerate() {
                            let (rect, _) = ui.allocate_exact_size(
                                vec2(cell_w, row_h),
                                eframe::egui::Sense::hover(),
                            );
                            let fill = if is_header { header_bg } else { body_bg };

                            ui.painter().rect_filled(rect, 0.0, fill);
                            ui.painter().rect_stroke(
                                rect,
                                0.0,
                                Stroke::new(1.0, grid),
                                egui::StrokeKind::Middle,
                            );

                            let halign = match alignments.get(col_idx) {
                                Some(Alignment::Center) => Align::Center,
                                Some(Alignment::Right) => Align::Max,
                                _ => Align::Min,
                            };
                            let inner_rect = rect.shrink2(vec2(CELL_PAD_X, CELL_PAD_Y));
                            let mut child = ui.new_child(
                                eframe::egui::UiBuilder::new()
                                    .max_rect(inner_rect)
                                    .layout(Layout::top_down(halign)),
                            );
                            selectable_job(&mut child, job);
                        }
                    });
                }
            });
    });
    ui.add_space(6.0);
}
