//! The card for a question an agent asks mid-turn (`elicitation/create`): a form built from its
//! schema, or a page to open for an MCP login.

use eframe::egui::{self, CornerRadius, Frame, Label, Margin, RichText, Stroke, TextEdit, Ui};

use crate::agent::elicitation::{self, Choice, Elicitation, ElicitationKind, Field, FieldInput};
use crate::app::{OxiApp, PendingApproval};
use crate::theme::*;

fn option_label(ui: &mut Ui, choice: &Choice, picked: bool, input: impl FnOnce(&mut Ui, &str)) {
    input(ui, &choice.label);
    if !choice.description.is_empty() {
        ui.indent("description", |ui| {
            ui.add(
                Label::new(
                    RichText::new(&choice.description)
                        .size(FS_SMALL)
                        .color(c_text_muted()),
                )
                .wrap(),
            );
        });
    }
    if picked && !choice.preview.is_empty() {
        ui.indent("preview", |ui| {
            Frame::new()
                .fill(c_bg_input())
                .stroke(Stroke::new(1.0, c_border_subtle()))
                .corner_radius(CornerRadius::same(RADIUS_CHIP))
                .inner_margin(Margin::symmetric(8, 6))
                .show(ui, |ui| {
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
        });
    }
}

fn field_ui(ui: &mut Ui, field: &mut Field) {
    ui.visuals_mut().indent_has_left_vline = false;
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
            ui.add(
                TextEdit::singleline(value)
                    .font(egui::FontId::proportional(FS_SMALL))
                    .desired_width(f32::INFINITY),
            );
        }
        FieldInput::Boolean(on) => {
            ui.checkbox(on, RichText::new("Yes").size(FS_SMALL));
        }
        FieldInput::Choice { options, selected } => {
            for (i, choice) in options.iter().enumerate() {
                ui.push_id(i, |ui| {
                    let picked = *selected == Some(i);
                    option_label(ui, choice, picked, |ui, label| {
                        if ui
                            .radio(picked, RichText::new(label).size(FS_SMALL))
                            .clicked()
                        {
                            *selected = (!picked).then_some(i);
                        }
                    });
                });
            }
        }
        FieldInput::Multi {
            options, selected, ..
        } => {
            for (i, (choice, on)) in options.iter().zip(selected.iter_mut()).enumerate() {
                ui.push_id(i, |ui| {
                    let picked = *on;
                    option_label(ui, choice, picked, |ui, label| {
                        ui.checkbox(on, RichText::new(label).size(FS_SMALL));
                    });
                });
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
            .corner_radius(CornerRadius::same(RADIUS_BUTTON))
            .inner_margin(Margin::symmetric(10, 8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.add(
                    Label::new(
                        RichText::new(&form.message)
                            .size(FS_SMALL)
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
                            ui.add_space(6.0);
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
