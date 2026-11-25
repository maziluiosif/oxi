//! Short clip for the README: Claude Code fixes a bug, then the same chat moves to Codex, which
//! picks up where Claude left off. Rendered by `scripts/render-demo.sh switch`.
//!
//! Like the tour, the turns are scripted and their tools run for real. The handoff itself (oxi
//! hands the new agent the transcript so far) is what a real switch does.

use std::path::PathBuf;

use eframe::egui;
use serde_json::{Value, json};

use super::{Recorder, fix_turn_finish, fix_turn_tools, setup};
use crate::settings::LlmProviderKind;

#[test]
#[ignore = "renders the README agent-switch clip; run through scripts/render-demo.sh switch"]
fn record_switch() {
    let video = PathBuf::from(std::env::var("OXI_DEMO_VIDEO").expect("OXI_DEMO_VIDEO"));
    let stills = std::env::var_os("OXI_DEMO_STILLS").map(PathBuf::from);
    let mut rec = setup(stills, Some(&video), vec!["Claude Code", "Codex"]);
    switch_clip(&mut rec);
    if let Some(stage) = rec.stage.take() {
        stage.finish();
    }
}

/// What each agent advertises, as their adapters report it (trimmed), so the composer shows
/// their real model and effort pickers without launching them.
fn agent_options(kind: LlmProviderKind) -> Value {
    let select = |id: &str, name: &str, category: &str, current: &str, values: &[(&str, &str)]| {
        json!({
            "id": id, "name": name, "category": category, "type": "select",
            "currentValue": current,
            "options": values.iter().map(|(v, n)| json!({"value": v, "name": n})).collect::<Vec<_>>(),
        })
    };
    match kind {
        LlmProviderKind::ClaudeCodeAcp => json!([
            select(
                "mode",
                "Mode",
                "mode",
                "acceptEdits",
                &[
                    ("default", "Manual"),
                    ("acceptEdits", "Accept edits"),
                    ("plan", "Plan")
                ]
            ),
            select(
                "model",
                "Model",
                "model",
                "opus",
                &[
                    ("opus", "Opus 5.5"),
                    ("sonnet", "Sonnet 5.5"),
                    ("haiku", "Haiku 5.5")
                ]
            ),
            select(
                "effort",
                "Effort",
                "thought_level",
                "high",
                &[("low", "Low"), ("medium", "Medium"), ("high", "High")]
            ),
        ]),
        _ => json!([
            select(
                "mode",
                "Mode",
                "mode",
                "workspace-write",
                &[
                    ("read-only", "Read-only"),
                    ("workspace-write", "Workspace access")
                ]
            ),
            select(
                "model",
                "Model",
                "model",
                "gpt-6.1-sol",
                &[("gpt-6.1-sol", "6.1 Sol"), ("gpt-6-astra", "6 Astra")]
            ),
            select(
                "reasoning_effort",
                "Reasoning effort",
                "thought_level",
                "high",
                &[("medium", "Medium"), ("high", "High")]
            ),
        ]),
    }
}

/// Make `kind` the chat's agent, with its pickers filled in.
fn use_agent(rec: &mut Recorder<'_>, kind: LlmProviderKind) {
    let app = rec.app();
    app.set_active_session_provider(kind);
    let key = app.active_session_key();
    let session_key = app.acp_session_key(key);
    let command = app.conv.settings.active_config().effective_acp_command();
    crate::agent::acp::config_options::publish(&session_key, &command, &agent_options(kind));
}

fn switch_clip(rec: &mut Recorder<'_>) {
    use_agent(rec, LlmProviderKind::ClaudeCodeAcp);
    rec.app().conv.composer.focus_next_frame = true;
    rec.harness.run_steps(6);

    // 1. Claude Code fixes the failing test.
    rec.chapter(
        0,
        "Start with Claude Code",
        "Your Claude Code login, directly. No API key.",
    );
    rec.hold(1.0);
    rec.type_text("Run the tests and fix the failing one", 1);
    rec.hold(0.3);
    rec.begin_turn("Run the tests and fix the failing one");
    // Saving the chat (on send, and again when the turn ends) moves its agent to a new key, so
    // the pickers are republished each time.
    rec.harness.run_steps(1);
    use_agent(rec, LlmProviderKind::ClaudeCodeAcp);
    rec.hold(0.4);
    fix_turn_tools(rec);
    fix_turn_finish(rec);
    rec.harness.run_steps(1);
    use_agent(rec, LlmProviderKind::ClaudeCodeAcp);
    rec.hold(1.2);

    // 2. Hand the same chat to Codex from the composer.
    rec.chapter(
        1,
        "Hand the same chat to Codex",
        "Switch agents mid-conversation. The context comes along.",
    );
    rec.hold(0.6);
    if let Some(picker) = rec.label_center("Claude") {
        rec.move_pointer(picker, 0.8);
        rec.click();
    }
    rec.hold(0.4);
    rec.type_text("codex", 1);
    rec.hold(0.3);
    if let Some(item) = rec.label_center("Codex") {
        rec.move_pointer(item, 0.6);
        rec.hold(0.2);
        // Only the press is drawn: picking through the menu would also launch the real agent
        // to fetch its models, so the pick happens below.
        if let Some(stage) = &mut rec.stage {
            stage.pointer_down = true;
        }
        rec.hold(0.12);
        if let Some(stage) = &mut rec.stage {
            stage.pointer_down = false;
        }
    }
    egui::Popup::close_all(&rec.harness.ctx);
    use_agent(rec, LlmProviderKind::CodexAcp);
    rec.app().conv.composer.focus_next_frame = true;
    rec.hold(0.6);
    rec.pointer(None);
    rec.hold(0.4);

    let prompt = "Double-check that fix with an odd-length list";
    rec.type_text(prompt, 1);
    rec.hold(0.3);
    rec.begin_turn(prompt);
    rec.hold(0.4);
    rec.stream_thinking(
        "The previous turn changed median() in stats.py to average the two middle values \
         when the length is even. I'll check both paths.",
    );
    rec.tool(
        "codex_1",
        "bash",
        json!({ "command": "python3 -c 'from stats import median; print(median([3, 1, 2]), median([4, 1, 3, 2]))'" }),
        0.9,
    );
    rec.stream_text(
        "Claude's fix holds: `median([3, 1, 2])` is `2` and `median([4, 1, 3, 2])` is \
         `2.5`. Odd lengths still take the middle element, so nothing else changed.",
    );
    rec.finish_turn();
    rec.hold(1.0);
    rec.still("switch");
    rec.framed_still("switch");
    rec.hold(2.4);
}
