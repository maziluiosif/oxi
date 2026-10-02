use super::*;
use crate::model::{AssistantBlock, ChatMessage};

fn answer(size: usize) -> ChatMessage {
    ChatMessage {
        role: MsgRole::Assistant,
        text: String::new(),
        is_summary: false,
        attachments: Vec::new(),
        blocks: vec![AssistantBlock::Answer("x".repeat(size))],
        streaming: false,
        started_at: None,
        worked_duration: None,
        route: None,
    }
}

fn user(text: &str) -> ChatMessage {
    ChatMessage {
        role: MsgRole::User,
        text: text.to_owned(),
        is_summary: false,
        attachments: Vec::new(),
        blocks: Vec::new(),
        streaming: false,
        started_at: None,
        worked_duration: None,
        route: None,
    }
}

#[test]
fn units_group_contiguous_assistant_messages() {
    let messages = vec![user("a"), answer(10), answer(10), user("b"), answer(10)];
    assert_eq!(
        transcript_units(&messages),
        vec![(0, 1), (1, 3), (3, 4), (4, 5)]
    );
}

#[test]
fn fingerprint_changes_when_content_grows() {
    let before = vec![answer(100)];
    let after = vec![answer(101)];
    assert_ne!(
        transcript_unit_fingerprint(&before),
        transcript_unit_fingerprint(&after)
    );
}

#[test]
fn fingerprint_changes_when_streaming_ends() {
    let mut streaming = answer(100);
    streaming.streaming = true;
    let done = answer(100);
    assert_ne!(
        transcript_unit_fingerprint(std::slice::from_ref(&streaming)),
        transcript_unit_fingerprint(std::slice::from_ref(&done))
    );
}

#[test]
fn selection_scroll_survives_nested_scroll_areas_and_stops_on_release() {
    use eframe::egui::text::{LayoutJob, TextFormat};
    use eframe::egui::{
        self, Color32, Event, FontId, Modifiers, PointerButton, Pos2, RawInput, Rect,
    };
    let ctx = egui::Context::default();
    let mut offset = 0.0;
    let mut frame = |events| {
        let _ = ctx.run_ui(
            RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(500.0, 350.0))),
                events,
                ..Default::default()
            },
            |ui| {
                let output = egui::ScrollArea::vertical()
                    .id_salt("outer")
                    .max_height(200.0)
                    .show(ui, |ui| {
                        let mut job = LayoutJob::default();
                        job.append(
                            &"select this row\n".repeat(60),
                            0.0,
                            TextFormat::simple(FontId::monospace(16.0), Color32::WHITE),
                        );
                        crate::theme::selectable_text_job(ui, job);
                        egui::ScrollArea::horizontal()
                            .id_salt("code-block")
                            .show(ui, |ui| {
                                ui.label("nested code block");
                            });
                        let (delta, _) = super::conversation_selection_scroll_delta(ui);
                        ui.scroll_with_delta_animation(delta, egui::style::ScrollAnimation::none());
                    });
                offset = output.state.offset.y;
            },
        );
        offset
    };
    frame(vec![]);
    frame(vec![
        Event::PointerMoved(egui::pos2(12.0, 12.0)),
        Event::PointerButton {
            pos: egui::pos2(12.0, 12.0),
            button: PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        },
    ]);
    let mut down = 0.0;
    for _ in 0..8 {
        down = frame(vec![Event::PointerMoved(egui::pos2(50.0, 230.0))]);
    }
    assert!(down > 30.0, "drag must reveal text below: {down}");
    let mut up = down;
    for _ in 0..4 {
        up = frame(vec![Event::PointerMoved(egui::pos2(50.0, 0.0))]);
    }
    assert!(up < down, "drag must reveal text above");
    let released = frame(vec![Event::PointerButton {
        pos: egui::pos2(50.0, 0.0),
        button: PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    }]);
    let stopped = frame(vec![]);
    assert_eq!(released, stopped);
}
