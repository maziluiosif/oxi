//! A live tool-heavy turn must only grow at its tail: blocks already on screen keep their
//! position while thinking, tool calls and bash output stream in. With the transcript stuck to
//! the bottom, any reflow above the tail moves the whole visible history (seen as flicker).

use super::*;

type Step = Box<dyn Fn(&mut Vec<AssistantBlock>)>;

fn tool(id: &str, name: &str, args: &str) -> AssistantBlock {
    AssistantBlock::Tool {
        tool_call_id: id.into(),
        name: name.into(),
        args_summary: Some(args.into()),
        output: String::new(),
        diff: None,
        is_error: None,
        full_output_path: None,
        output_truncated: false,
        metadata: None,
    }
}

/// Render the run for a couple of frames; return the y of every painted text containing `needle`.
fn text_ys(ctx: &egui::Context, frame: &mut u32, msg: &ChatMessage, needle: &str) -> Vec<f32> {
    let mut ys = Vec::new();
    for _ in 0..2 {
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(900.0, 5000.0),
            )),
            time: Some(f64::from(*frame) / 60.0),
            ..Default::default()
        };
        *frame += 1;
        let out = ctx.run_ui(raw, |ui| {
            render_assistant_message_run(ui, 0, std::slice::from_ref(msg), None);
        });
        ys = out
            .shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::epaint::Shape::Text(text) if text.galley.text().contains(needle) => {
                    Some(text.pos.y)
                }
                _ => None,
            })
            .collect();
    }
    ys
}

fn check_stream(thinking: &str) {
    let ctx = egui::Context::default();
    crate::theme::apply_theme(&ctx, "dark");
    let mut frame = 0;
    let mut msg = ChatMessage {
        role: MsgRole::Assistant,
        text: String::new(),
        is_summary: false,
        attachments: vec![],
        blocks: vec![AssistantBlock::Thinking(thinking.into())],
        streaming: true,
        started_at: Some(std::time::Instant::now()),
        worked_duration: None,
        route: None,
        changes: None,
        evidence: None,
    };
    // While live, the thinking caption stays anchored at the top even as long reasoning streams.
    let live_caption_y = text_ys(&ctx, &mut frame, &msg, "Thinking")[0];

    let mut folded_thinking_y = None;
    let mut first_pill_y = None;
    for (index, name) in ["read", "grep", "read", "bash", "read", "edit"]
        .into_iter()
        .enumerate()
    {
        let args = match name {
            "bash" => r#"{"command":"cargo test retry"}"#.to_owned(),
            _ => format!(r#"{{"path":"src/file{index}.rs"}}"#),
        };
        msg.blocks.push(tool(&index.to_string(), name, &args));
        let mut steps: Vec<Step> = Vec::new();
        if name == "bash" {
            for lines in 1..6 {
                steps.push(Box::new(move |blocks| {
                    if let Some(AssistantBlock::Tool { output, .. }) = blocks.last_mut() {
                        *output = (0..lines).map(|i| format!("line {i}\n")).collect();
                    }
                }));
            }
        }
        steps.push(Box::new(|blocks| {
            if let Some(AssistantBlock::Tool {
                output, is_error, ..
            }) = blocks.last_mut()
            {
                output.push_str("ok");
                *is_error = Some(false);
            }
        }));
        steps.push(Box::new(|blocks| {
            blocks.push(AssistantBlock::Thinking("Now the next file.".into()));
        }));
        for (step, apply) in steps.iter().enumerate() {
            let before = text_ys(&ctx, &mut frame, &msg, "Thought")[0];
            apply(&mut msg.blocks);
            let label = format!("{name}#{index} step {step}");
            let after = text_ys(&ctx, &mut frame, &msg, "Thought")[0];
            assert_eq!(before, after, "first thinking row moved at {label}");
            assert_eq!(
                *folded_thinking_y.get_or_insert(after),
                after,
                "first thinking row drifted at {label}"
            );
            assert!(
                (after - live_caption_y).abs() <= 2.0,
                "thinking caption jumped across live->done: live={live_caption_y}, done={after}"
            );
            let pill_y = text_ys(&ctx, &mut frame, &msg, "src/file0.rs")[0];
            assert_eq!(
                *first_pill_y.get_or_insert(pill_y),
                pill_y,
                "first tool pill moved at {label}"
            );
        }
    }
}

#[test]
fn tool_heavy_stream_keeps_earlier_blocks_in_place() {
    check_stream("I should look at how the config is loaded and where retries are handled.");
}

#[test]
fn long_thinking_stays_bounded_while_live_and_folds_cleanly_when_tools_start() {
    check_stream(
        &"I should look at how the config is loaded and where retries are handled.\n".repeat(25),
    );
}

#[test]
fn streaming_commentary_keeps_activity_visible_below_the_answer_until_finished() {
    for activity in [
        vec![],
        vec![AssistantBlock::Thinking("Checking the code.".into())],
        {
            let mut completed = tool("read", "read", r#"{"path":"src/main.rs"}"#);
            if let AssistantBlock::Tool { is_error, .. } = &mut completed {
                *is_error = Some(false);
            }
            vec![completed]
        },
    ] {
        let ctx = egui::Context::default();
        crate::theme::apply_theme(&ctx, "dark");
        let mut frame = 0;
        let mut msg = ChatMessage::streaming_assistant(None);
        msg.blocks = activity;
        msg.blocks
            .push(AssistantBlock::Answer("I will check the next file.".into()));

        let answer_y = text_ys(&ctx, &mut frame, &msg, "I will check the next file.")[0];
        for _ in 0..3 {
            let status_ys = text_ys(&ctx, &mut frame, &msg, "Working");
            assert!(
                status_ys.iter().any(|&y| y > answer_y),
                "a running turn must keep an activity indicator at its visible tail"
            );
            assert_eq!(
                text_ys(&ctx, &mut frame, &msg, "I will check the next file.")[0],
                answer_y,
                "the idle activity indicator must not move the answer"
            );
        }

        msg.finish_streaming();
        assert!(text_ys(&ctx, &mut frame, &msg, "Working").is_empty());
        assert!(!text_ys(&ctx, &mut frame, &msg, "I will check the next file.").is_empty());
    }
}

#[test]
fn done_thinking_defaults_collapsed_and_expanded_stays_height_bounded() {
    let ctx = egui::Context::default();
    crate::theme::apply_theme(&ctx, "dark");
    let long_thought = (0..80)
        .map(|i| format!("Reasoning line {i}: inspecting retry backoff and timeout behavior."))
        .collect::<Vec<_>>()
        .join("\n");

    let run = |ctx: &egui::Context, events: Vec<egui::Event>| -> (egui::Rect, egui::FullOutput) {
        let mut rect = egui::Rect::NOTHING;
        let out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(720.0, 2000.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                rect = ui
                    .vertical(|ui| {
                        thinking::render_thinking_group_block(
                            ui,
                            0,
                            0,
                            long_thought.clone(),
                            false,
                        );
                    })
                    .response
                    .rect;
            },
        );
        (rect, out)
    };

    let (collapsed_rect, _) = run(&ctx, vec![]);
    assert!(
        collapsed_rect.height() <= 36.0,
        "completed thinking block should default to a compact 1-line row, got {}pt",
        collapsed_rect.height()
    );

    // Click the header row to expand.
    let click_pos = collapsed_rect.center();
    run(
        &ctx,
        vec![
            egui::Event::PointerMoved(click_pos),
            egui::Event::PointerButton {
                pos: click_pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::PointerButton {
                pos: click_pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ],
    );
    let (expanded_rect, _) = run(&ctx, vec![]);
    assert!(
        expanded_rect.height() > collapsed_rect.height() + 100.0,
        "clicking header should expand thinking body (got {}pt)",
        expanded_rect.height()
    );
    assert!(
        expanded_rect.height() <= 290.0,
        "expanded thinking body must stay height-bounded with scroll (got {}pt)",
        expanded_rect.height()
    );
}

#[test]
fn expanded_tool_output_and_edit_diff_stay_height_bounded_in_one_click() {
    let ctx = egui::Context::default();
    crate::theme::apply_theme(&ctx, "dark");

    let mut bash = tool("bash_long", "bash", r#"{"command":"cargo test --all"}"#);
    if let AssistantBlock::Tool {
        output, is_error, ..
    } = &mut bash
    {
        *output = (0..180)
            .map(|i| format!("test module::case_{i} ... ok"))
            .collect::<Vec<_>>()
            .join("\n");
        *is_error = Some(false);
    }

    let old_src = (0..120)
        .map(|i| format!("let old_val_{i} = {i};"))
        .collect::<Vec<_>>()
        .join("\n");
    let new_src = (0..120)
        .map(|i| format!("let new_val_{i} = {};", i + 1))
        .collect::<Vec<_>>()
        .join("\n");
    let mut edit = tool("edit_long", "edit", r#"{"path":"src/big.rs"}"#);
    if let AssistantBlock::Tool { diff, is_error, .. } = &mut edit {
        *diff = Some(crate::agent::tools::make_unified_diff(
            "src/big.rs",
            &old_src,
            &new_src,
        ));
        *is_error = Some(false);
    }

    let render_block = |ctx: &egui::Context, block: &AssistantBlock, events: Vec<egui::Event>| {
        let mut rect = egui::Rect::NOTHING;
        let _ = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(720.0, 2000.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                rect = ui
                    .vertical(|ui| {
                        tool_pill::render_single_tool_block(ui, 0, 0, block, false, false, false);
                    })
                    .response
                    .rect;
            },
        );
        rect
    };

    // Bash pill starts folded (~30pt).
    let folded_bash = render_block(&ctx, &bash, vec![]);
    assert!(
        folded_bash.height() <= 40.0,
        "folded tool pill should be compact, got {}pt",
        folded_bash.height()
    );

    // One click opens the full output inside a height-bounded scrollable drawer.
    let click_pos = folded_bash.center();
    render_block(
        &ctx,
        &bash,
        vec![
            egui::Event::PointerMoved(click_pos),
            egui::Event::PointerButton {
                pos: click_pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::PointerButton {
                pos: click_pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ],
    );
    let open_bash = render_block(&ctx, &bash, vec![]);
    assert!(
        open_bash.height() > 180.0 && open_bash.height() <= 320.0,
        "open tool drawer must be height-bounded with scroll, got {}pt",
        open_bash.height()
    );

    // Completed edit card defaults open in Balanced mode, height-bounded even for a 120-line diff.
    let open_edit = render_block(&ctx, &edit, vec![]);
    assert!(
        open_edit.height() > 200.0 && open_edit.height() <= 360.0,
        "open edit diff card must stay height-bounded with scroll, got {}pt",
        open_edit.height()
    );
}
