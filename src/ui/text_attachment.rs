//! Editable pasted-text previews; sent attachments become new composer copies.
use crate::model::UserAttachment;
use eframe::egui::{self, Id, ScrollArea};

#[derive(Clone)]
struct Preview {
    name: String,
    original: String,
    text: String,
    draft_scope: Option<Id>,
    notice: Option<String>,
}

fn preview_id() -> Id {
    Id::new("text_attachment_preview")
}

pub fn open(ctx: &egui::Context, name: &str, text: &str) {
    open_preview(ctx, name, text, None);
}

pub fn open_draft(ctx: &egui::Context, name: &str, text: &str, scope: Id) {
    open_preview(ctx, name, text, Some(scope));
}

fn open_preview(ctx: &egui::Context, name: &str, text: &str, draft_scope: Option<Id>) {
    ctx.data_mut(|data| {
        data.insert_temp(
            preview_id(),
            Preview {
                name: name.into(),
                original: text.into(),
                text: text.into(),
                draft_scope,
                notice: None,
            },
        )
    });
}

fn apply(
    preview: &Preview,
    scope: Id,
    pending: &mut Vec<UserAttachment>,
) -> Result<(), &'static str> {
    if let Some(target) = preview.draft_scope {
        if target != scope {
            return Err("Return to the original chat to save this attachment.");
        }
        let attachment = pending.iter_mut().find(|a| matches!(a, UserAttachment::Text { name, text } if name == &preview.name && text == &preview.original))
            .ok_or("This attachment was removed, changed or sent. Close this preview and reopen it.")?;
        if let UserAttachment::Text { text, .. } = attachment {
            *text = preview.text.clone();
        }
    } else {
        let mut index = 1;
        let name = loop {
            let candidate = format!("pasted-{index}.txt");
            if !pending
                .iter()
                .any(|a| matches!(a, UserAttachment::Text { name, .. } if name == &candidate))
            {
                break candidate;
            }
            index += 1;
        };
        pending.push(UserAttachment::Text {
            name,
            text: preview.text.clone(),
        });
    }
    Ok(())
}

pub fn show(ctx: &egui::Context, scope: Id, pending: &mut Vec<UserAttachment>) {
    let id = preview_id();
    let Some(mut preview) = ctx.data(|data| data.get_temp::<Preview>(id)) else {
        return;
    };
    let mut open = true;
    let mut save = false;
    let mut cancel = false;
    egui::Window::new(&preview.name)
        .id(id)
        .open(&mut open)
        .default_width(640.0)
        .default_height(420.0)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                save = ui
                    .button(if preview.draft_scope.is_some() {
                        "Save changes"
                    } else {
                        "Add edited copy"
                    })
                    .clicked();
                cancel = ui.button("Cancel").clicked();
                if ui.button("Copy full text").clicked() {
                    ctx.copy_text(preview.text.clone());
                }
            });
            if let Some(notice) = &preview.notice {
                ui.label(notice);
            }
            ScrollArea::vertical()
                .max_height(ui.available_height().max(120.0))
                .show(ui, |ui| {
                    let slot = ui.painter().add(egui::Shape::Noop);
                    let output = ui.scope(|ui| {
                        ui.visuals_mut().selection.bg_fill = egui::Color32::TRANSPARENT;
                        ui.visuals_mut().selection.stroke.color = crate::theme::c_text();
                        egui::TextEdit::multiline(&mut preview.text)
                            .font(egui::TextStyle::Monospace)
                            .desired_width(f32::INFINITY)
                            .desired_rows(16)
                            .show(ui)
                    });
                    let output = output.inner;
                    if let Some(range) = output.cursor_range
                        && !range.is_empty()
                    {
                        let rects = crate::ui::text_selection::galley_selection_rects(
                            &output.galley,
                            output.galley_pos,
                            range,
                        );
                        ui.painter().set(
                            slot,
                            crate::ui::text_selection::selection_shape(
                                &rects,
                                crate::theme::editor_selection_fill(),
                            ),
                        );
                    }
                });
        });
    if save {
        match apply(&preview, scope, pending) {
            Ok(()) => open = false,
            Err(notice) => preview.notice = Some(notice.into()),
        }
    }
    ctx.data_mut(|data| {
        if open && !cancel {
            data.insert_temp(id, preview);
        } else {
            data.remove::<Preview>(id);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_updates_only_original_draft_and_preserves_full_text() {
        let scope = Id::new("chat-a");
        let mut pending = vec![UserAttachment::Text {
            name: "pasted-1.txt".into(),
            text: "original".into(),
        }];
        let preview = Preview {
            name: "pasted-1.txt".into(),
            original: "original".into(),
            text: "  ă\n".repeat(3000),
            draft_scope: Some(scope),
            notice: None,
        };
        assert!(apply(&preview, Id::new("chat-b"), &mut pending).is_err());
        apply(&preview, scope, &mut pending).unwrap();
        assert!(matches!(&pending[0], UserAttachment::Text { text, .. } if text == &preview.text));
        assert!(apply(&preview, scope, &mut pending).is_err());
        pending.clear();
        assert!(apply(&preview, scope, &mut pending).is_err());
    }

    #[test]
    fn sent_attachment_edits_create_a_separate_draft_copy() {
        let mut pending = vec![UserAttachment::Text {
            name: "pasted-1.txt".into(),
            text: "keep".into(),
        }];
        let preview = Preview {
            name: "pasted-1.txt".into(),
            original: "sent".into(),
            text: "edited".into(),
            draft_scope: None,
            notice: None,
        };
        apply(&preview, Id::new("chat"), &mut pending).unwrap();
        assert!(matches!(&pending[0], UserAttachment::Text { text, .. } if text == "keep"));
        assert!(
            matches!(&pending[1], UserAttachment::Text { name, text } if name == "pasted-2.txt" && text == "edited")
        );
    }
    #[test]
    #[ignore = "UI review artifact; run with --ignored"]
    fn render_editable_attachment_review() {
        let scope = Id::new("review");
        let mut pending = vec![UserAttachment::Text { name: "pasted-1.txt".into(), text: "Editable pasted text\n\nYou can change this content before sending.\nUnicode is preserved: început, selecție.\n".repeat(20) }];
        let mut initialized = false;
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(800.0, 600.0))
            .wgpu()
            .build_ui(move |ui| {
                crate::theme::apply_theme(ui.ctx(), "dark");
                if !initialized {
                    let UserAttachment::Text { name, text } = &pending[0] else {
                        unreachable!()
                    };
                    open_draft(ui.ctx(), name, text, scope);
                    initialized = true;
                }
                show(ui.ctx(), scope, &mut pending);
            });
        harness.run_steps(3);
        harness
            .render()
            .unwrap()
            .save("/tmp/oxi-text-attachment-editor.png")
            .unwrap();
    }
}
