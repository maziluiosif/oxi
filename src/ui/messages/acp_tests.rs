use super::*;
use crate::model::{ToolMetadata, ToolStatus, ToolUpdate, apply_tool_update};
use serde_json::json;

fn fixture_blocks() -> Vec<AssistantBlock> {
    let mut blocks = Vec::new();
    for (id, name, title, args, output, diff, status) in [
        (
            "read",
            "read",
            "Read project configuration",
            json!({"path":"src/config.rs"}),
            "Lines 1-12\nProject settings loaded".to_owned(),
            None,
            ToolStatus::Completed,
        ),
        (
            "shell",
            "bash",
            "Run project checks",
            json!({"command":"cargo check --locked"}),
            "Checking stats-kit...\nFinished dev profile".to_owned(),
            None,
            ToolStatus::InProgress,
        ),
        (
            "edit",
            "edit",
            "Update configuration and documentation",
            json!({}),
            "Updated two files.".to_owned(),
            Some(format!(
                "{}{}",
                crate::agent::tools::make_unified_diff(
                    "src/config.rs",
                    "const RETRIES: u32 = 1;\n",
                    "const RETRIES: u32 = 3;\n"
                ),
                crate::agent::tools::make_unified_diff(
                    "README.md",
                    "# Stats kit\nRetry once.\n",
                    "# Stats kit\nRetry three times.\n"
                )
            )),
            ToolStatus::InProgress,
        ),
        (
            "failed",
            "edit",
            "Update protected configuration",
            json!({"path":"config/protected.toml"}),
            "Permission denied: file is read-only.".to_owned(),
            None,
            ToolStatus::Failed,
        ),
    ] {
        apply_tool_update(
            &mut blocks,
            ToolUpdate {
                tool_call_id: id.into(),
                name: name.into(),
                args: Some(args),
                output,
                diff,
                output_truncated: false,
                metadata: ToolMetadata {
                    title: title.into(),
                    kind: name.into(),
                    status,
                    ..Default::default()
                },
            },
        );
    }
    blocks
}

#[test]
fn acp_summaries_keep_titles_failure_and_locations() {
    let mut blocks = fixture_blocks();
    let summary = tool_format::tool_display_summary(&blocks[3], false);
    assert!(summary.action.contains("Update protected configuration"));
    assert!(summary.action.contains("Failed"));
    if let AssistantBlock::Tool {
        metadata: Some(meta),
        ..
    } = &mut blocks[2]
    {
        meta.locations =
            serde_json::from_value(json!([{"path":"src/config.rs","line":7}])).unwrap();
    }
    assert_eq!(
        tool_format::tool_display_summary(&blocks[2], true).detail,
        "src/config.rs:7"
    );
    assert!(is_edit_like_tool(&blocks[2]));
}

#[test]
#[ignore = "renders ACP tool review screenshots; set OXI_ACP_GALLERY to an output directory"]
fn render_acp_tool_gallery() {
    let output =
        std::path::PathBuf::from(std::env::var("OXI_ACP_GALLERY").expect("OXI_ACP_GALLERY"));
    std::fs::create_dir_all(&output).unwrap();
    let mut setup = false;
    let mut harness = egui_kittest::Harness::builder()
        .with_size(egui::vec2(1000.0, 950.0))
        .wgpu()
        .build_ui_state(
            |ui, state: &mut (Vec<AssistantBlock>, bool)| {
                if !setup {
                    crate::theme::apply_theme(ui.ctx(), "dark");
                    setup = true;
                    return;
                }
                ui.heading(if state.1 {
                    "ACP · Live tool updates"
                } else {
                    "ACP · Restored conversation"
                });
                ui.add_space(12.0);
                for (index, block) in state.0.iter().enumerate() {
                    tool_pill::render_single_tool_block(ui, 0, index, block, state.1, true, true);
                }
            },
            (fixture_blocks(), true),
        );
    harness.run_steps(4);
    harness
        .render()
        .unwrap()
        .save(output.join("acp-live.png"))
        .unwrap();
    // Save and reload the exact displayed tool blocks through the real persistence format.
    for block in &mut harness.state_mut().0 {
        if let AssistantBlock::Tool {
            metadata: Some(meta),
            is_error,
            ..
        } = block
            && meta.status == ToolStatus::InProgress
        {
            meta.status = ToolStatus::Completed;
            *is_error = Some(false);
        }
    }
    let message = ChatMessage {
        role: MsgRole::Assistant,
        text: String::new(),
        is_summary: false,
        attachments: vec![],
        blocks: harness.state().0.clone(),
        streaming: false,
        started_at: None,
        worked_duration: None,
        route: None,
        changes: None,
    };
    let entries = crate::session_store::chat_message_to_json_entries(&message);
    let restored = crate::hydrate::messages_from_get_messages(&json!({"messages":entries}));
    harness.state_mut().0 = restored[0].blocks.clone();
    harness.state_mut().1 = false;
    harness.run_steps(4);
    harness
        .render()
        .unwrap()
        .save(output.join("acp-restored.png"))
        .unwrap();
    harness.set_size(egui::vec2(360.0, 700.0));
    for block in &mut harness.state_mut().0 {
        if let AssistantBlock::Tool {
            metadata: Some(meta),
            ..
        } = block
        {
            meta.title = format!(
                "{} — {}",
                meta.title,
                "long command or file description ".repeat(12)
            );
        }
    }
    harness.run_steps(4);
    harness
        .render()
        .unwrap()
        .save(output.join("acp-narrow.png"))
        .unwrap();
}

#[test]
fn tool_cards_stay_inside_narrow_chat_with_long_acp_and_native_content() {
    for width in [220.0, 360.0, 640.0] {
        for acp in [true, false] {
            let mut blocks = fixture_blocks();
            for block in &mut blocks {
                if let AssistantBlock::Tool {
                    name,
                    metadata,
                    args_summary,
                    output,
                    diff,
                    ..
                } = block
                {
                    if acp {
                        metadata.as_mut().unwrap().title =
                            format!("Run {}\nwith more details", "very_long_command_".repeat(30));
                    } else {
                        *metadata = None;
                        if name == "read" {
                            *name = format!("mcp_{}", "long_tool_name_".repeat(30));
                        }
                    }
                    *args_summary = Some(
                        json!({"path":"long_directory/".repeat(80),"command":"echo ".repeat(200)})
                            .to_string(),
                    );
                    *output = "long_output_".repeat(300);
                    if diff.is_some() {
                        *diff = Some(crate::agent::tools::make_unified_diff(
                            "long.rs",
                            &"a".repeat(2000),
                            &"b".repeat(2000),
                        ));
                    }
                }
            }
            for streaming in [true, false] {
                let mut setup = false;
                let mut harness = egui_kittest::Harness::builder().with_size(egui::vec2(width, 900.0))
                    .build_ui(|ui| {
                        if !setup { crate::theme::apply_theme(ui.ctx(), "dark"); setup=true; return; }
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            for (index, block) in blocks.iter().enumerate() {
                                let available = ui.available_width();
                                let response = ui.scope(|ui| tool_pill::render_single_tool_block(ui, 0, index, block, streaming, true, true));
                                assert!(response.response.rect.width() <= available + 0.5,
                                    "acp={acp}, streaming={streaming}, width={width}, block={index}: {} > {available}", response.response.rect.width());
                            }
                        });
                    });
                harness.run_steps(4);
            }
        }
    }
}
