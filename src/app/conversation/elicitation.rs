//! The card for a question an agent asks mid-turn (`elicitation/create`): a form built from its
//! schema, or a page to open for an MCP login.

use eframe::egui::{self, CornerRadius, Frame, Label, Margin, RichText, Stroke, Ui};

use crate::agent::elicitation::{self, Choice, Elicitation, ElicitationKind, Field, FieldInput};
use crate::app::{OxiApp, PendingApproval};
use crate::theme::*;

/// One answer of a choice field: the whole row is the target, with a radio dot or a check box
/// drawn by hand. egui's own radio and checkbox nearly vanish on the card's tinted background.
fn choice_row(ui: &mut Ui, choice: &Choice, picked: bool, multi: bool) -> egui::Response {
    const PAD_X: f32 = 10.0;
    const PAD_Y: f32 = 7.0;
    const MARK: f32 = crate::ui::chrome::CHOICE_MARK;
    const GAP: f32 = 9.0;
    let width = ui.available_width();
    let text_w = (width - PAD_X * 2.0 - MARK - GAP).max(40.0);
    let label = ui.painter().layout(
        choice.label.clone(),
        egui::FontId::proportional(FS_SMALL),
        c_text_strong(),
        text_w,
    );
    let description = (!choice.description.is_empty()).then(|| {
        ui.painter().layout(
            choice.description.clone(),
            egui::FontId::proportional(FS_TINY),
            c_text_muted(),
            text_w,
        )
    });
    let label_row_h = label.size().y / label.rows.len().max(1) as f32;
    let text_h = label.size().y + description.as_ref().map_or(0.0, |d| 2.0 + d.size().y);
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(width, text_h.max(MARK) + PAD_Y * 2.0),
        egui::Sense::click(),
    );
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            if multi {
                egui::WidgetType::Checkbox
            } else {
                egui::WidgetType::RadioButton
            },
            ui.is_enabled(),
            picked,
            &choice.label,
        )
    });
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let hovered = response.hovered() || response.has_focus();
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let painter = ui.painter();
    painter.rect(
        rect,
        CornerRadius::same(RADIUS_CHIP),
        if picked {
            c_pill_selected_bg()
        } else if hovered {
            c_row_hover()
        } else {
            c_bg_input()
        },
        Stroke::new(
            1.0,
            if picked {
                c_accent()
            } else if hovered {
                c_border()
            } else {
                c_border_subtle()
            },
        ),
        egui::StrokeKind::Inside,
    );
    crate::ui::chrome::keyboard_focus_ring(ui, &response, CornerRadius::same(RADIUS_CHIP));

    let mark = egui::Rect::from_center_size(
        egui::pos2(
            rect.left() + PAD_X + MARK * 0.5,
            rect.top() + PAD_Y + label_row_h * 0.5,
        ),
        egui::vec2(MARK, MARK),
    );
    crate::ui::chrome::paint_choice_mark(painter, mark, multi, picked, hovered);

    let text_pos = egui::pos2(rect.left() + PAD_X + MARK + GAP, rect.top() + PAD_Y);
    let label_h = label.size().y;
    painter.galley(text_pos, label, c_text_strong());
    if let Some(description) = description {
        painter.galley(
            text_pos + egui::vec2(0.0, label_h + 2.0),
            description,
            c_text_muted(),
        );
    }
    response
}

/// The preview an agent attached to the picked answer (a snippet, a mock-up).
fn choice_preview(ui: &mut Ui, choice: &Choice) {
    Frame::new()
        .fill(c_bg_input())
        .stroke(Stroke::new(1.0, c_border_subtle()))
        .corner_radius(CornerRadius::same(RADIUS_CHIP))
        .inner_margin(Margin::symmetric(8, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(
                Label::new(
                    RichText::new(&choice.preview)
                        .size(FS_SMALL)
                        .color(c_text())
                        .monospace(),
                )
                .wrap()
                .selectable(true),
            );
        });
}

fn field_ui(ui: &mut Ui, field: &mut Field) {
    ui.spacing_mut().item_spacing.y = 5.0;
    if !field.title.is_empty() {
        let title = if field.required {
            format!("{} *", field.title)
        } else {
            field.title.clone()
        };
        ui.label(
            RichText::new(title)
                .size(FS_SMALL)
                .color(c_text_strong())
                .strong(),
        );
    }
    if !field.description.is_empty() {
        ui.add(
            Label::new(
                RichText::new(&field.description)
                    .size(FS_SMALL)
                    .color(c_text_muted()),
            )
            .wrap(),
        );
    }
    match &mut field.input {
        FieldInput::Text { value, .. } | FieldInput::Number { value, .. } => {
            crate::ui::chrome::settings_text_field(ui, value, "");
        }
        FieldInput::Boolean(on) => {
            crate::ui::chrome::toggle(ui, on, RichText::new("Yes").size(FS_SMALL));
        }
        FieldInput::Choice { options, selected } => {
            for (i, choice) in options.iter().enumerate() {
                let picked = *selected == Some(i);
                if choice_row(ui, choice, picked, false).clicked() {
                    *selected = (!picked).then_some(i);
                }
                if picked && !choice.preview.is_empty() {
                    choice_preview(ui, choice);
                }
            }
        }
        FieldInput::Multi {
            options, selected, ..
        } => {
            for (choice, on) in options.iter().zip(selected.iter_mut()) {
                if choice_row(ui, choice, *on, true).clicked() {
                    *on = !*on;
                }
                if *on && !choice.preview.is_empty() {
                    choice_preview(ui, choice);
                }
            }
        }
    }
}

impl OxiApp {
    pub(super) fn render_elicitation_card(&mut self, ui: &mut Ui, pa: PendingApproval) {
        let Some(mut form) = pa.elicitation else {
            return;
        };
        let before = form.clone();
        Frame::new()
            .fill(c_info_bg())
            .stroke(Stroke::new(1.0, c_accent()))
            .corner_radius(CornerRadius::same(RADIUS_CARD))
            .inner_margin(Margin::symmetric(14, 12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.add(
                    Label::new(
                        RichText::new(&form.message)
                            .size(FS_BODY)
                            .color(c_text_strong())
                            .strong(),
                    )
                    .wrap()
                    .selectable(true),
                );
                ui.add_space(6.0);
                match &mut form.kind {
                    ElicitationKind::Url(url) => {
                        let url = url.clone();
                        self.url_body(ui, &url);
                    }
                    ElicitationKind::Form(fields) => {
                        for field in fields.iter_mut() {
                            ui.push_id(field.key.clone(), |ui| field_ui(ui, field));
                            ui.add_space(8.0);
                        }
                        self.form_buttons(ui, &before);
                    }
                }
            });
        ui.add_space(4.0);
        if form != before {
            let key = self.active_session_key();
            if let Some(pending) = self.run_state_mut(key).pending_approval.as_mut() {
                pending.elicitation = Some(form);
            }
        }
    }

    fn form_buttons(&mut self, ui: &mut Ui, form: &Elicitation) {
        let content = form.content();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let submit = ui.add_enabled_ui(content.is_ok(), |ui| {
                crate::ui::chrome::primary_button(ui, "Submit")
            });
            if submit.inner.clicked()
                && let Ok(content) = &content
            {
                self.respond_to_approval(elicitation::accepted(content.clone()));
            }
            if crate::ui::chrome::ghost_button(ui, "Skip", false)
                .on_hover_text("Answer nothing; the agent is told you skipped this")
                .clicked()
            {
                self.respond_to_approval(elicitation::declined());
            }
            if let Err(problem) = &content {
                ui.label(RichText::new(problem).size(FS_SMALL).color(c_text_muted()));
            }
        });
    }

    fn url_body(&mut self, ui: &mut Ui, url: &str) {
        let openable = elicitation::is_openable(url);
        // Shown in full: the address is the only thing to judge the request by.
        ui.add(
            Label::new(
                RichText::new(url)
                    .size(FS_SMALL)
                    .color(c_text())
                    .monospace(),
            )
            .wrap()
            .selectable(true),
        );
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let open = ui.add_enabled_ui(openable, |ui| {
                crate::ui::chrome::primary_button(ui, "Open in browser")
            });
            if open.inner.clicked() {
                ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                self.respond_to_approval(elicitation::accepted(serde_json::json!({})));
            }
            if crate::ui::chrome::ghost_button(ui, "Decline", false).clicked() {
                self.respond_to_approval(elicitation::declined());
            }
        });
    }
}
