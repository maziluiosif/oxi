//! Scripted product demo, rendered offscreen from the real UI.
//!
//! `scripts/render-demo.sh` runs this ignored test and turns the frames into the website video
//! and the README GIF. The agent's turn is scripted (no model is called), but its tools run for
//! real against a generated sample project, so the test output and the diff are genuine.
//!
//! Isolation: the process uses a throwaway `HOME` and an in-memory credential store, so it
//! never touches the user's settings, chats or keychain.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Event};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::{Value, json};

use super::OxiApp;
use crate::agent::tools::{ToolEnv, run_tool};
use crate::agent::{AgentEvent, AgentOutcome, TokenUsage};
use crate::model::{AssistantBlock, ChatMessage, MsgRole};
use crate::settings::LlmProviderKind;

const FPS: u64 = 20;
const FRAME: Duration = Duration::from_millis(1000 / FPS);
const SIZE: egui::Vec2 = egui::vec2(1240.0, 780.0);
const SCALE: f32 = 2.0;
const MODEL: &str = "qwen2.5-coder-7b-instruct-q4_k_m.gguf";

const STATS_PY: &str = r#""""Small statistics helpers used by the reporting pipeline."""


def mean(values):
    if not values:
        raise ValueError("mean() requires at least one value")
    return sum(values) / len(values)


def median(values):
    if not values:
        raise ValueError("median() requires at least one value")
    ordered = sorted(values)
    return ordered[len(ordered) // 2]


def percentile(values, p):
    if not values:
        raise ValueError("percentile() requires at least one value")
    ordered = sorted(values)
    index = round(p / 100 * (len(ordered) - 1))
    return ordered[index]
"#;

const TEST_STATS_PY: &str = r#"import unittest

from stats import mean, median, percentile


class StatsTest(unittest.TestCase):
    def test_mean(self):
        self.assertEqual(mean([1, 2, 3, 4]), 2.5)

    def test_median_odd_length(self):
        self.assertEqual(median([3, 1, 2]), 2)

    def test_median_even_length(self):
        self.assertEqual(median([1, 2, 3, 4]), 2.5)

    def test_percentile(self):
        self.assertEqual(percentile([10, 20, 30, 40, 50], 50), 30)

    def test_empty_input_raises(self):
        with self.assertRaises(ValueError):
            median([])


if __name__ == "__main__":
    unittest.main()
"#;

#[test]
#[ignore = "renders the website demo; run through scripts/render-demo.sh"]
fn record_demo() {
    let out = PathBuf::from(std::env::var("OXI_DEMO_FRAMES").expect("OXI_DEMO_FRAMES"));
    let stills = std::env::var_os("OXI_DEMO_STILLS").map(PathBuf::from);
    run_demo(Some(out), stills);
}

/// Every key screen as a still, without the video frames or real-time pacing. Used to review UI
/// changes: `OXI_GALLERY=/some/dir cargo test --release render_gallery -- --ignored`.
#[test]
#[ignore = "renders UI review stills; set OXI_GALLERY to the output folder"]
fn render_gallery() {
    let stills = PathBuf::from(std::env::var("OXI_GALLERY").expect("OXI_GALLERY"));
    run_demo(None, Some(stills));
}

fn run_demo(out: Option<PathBuf>, stills: Option<PathBuf>) {
    let scratch = std::env::temp_dir().join(format!("oxi-demo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    let home = scratch.join("home");
    let project = scratch.join("Projects").join("stats-kit");
    std::fs::create_dir_all(&home).unwrap();
    if let Some(out) = &out {
        std::fs::create_dir_all(out).unwrap();
    }
    if let Some(stills) = &stills {
        std::fs::create_dir_all(stills).unwrap();
    }

    crate::secrets::use_mock_store();
    // SAFETY: set before any thread that reads the environment is started.
    unsafe { std::env::set_var("HOME", &home) };
    create_project(&project);
    // The app keys chats by the canonical workspace path (/private/var/… on macOS).
    let project = std::fs::canonicalize(&project).unwrap();
    std::env::set_current_dir(&project).unwrap();
    seed_settings();
    seed_chats(&project);

    let harness = Harness::builder()
        .with_size(SIZE)
        .with_pixels_per_point(SCALE)
        .wgpu()
        .build_eframe(|cc| {
            let mut app = OxiApp::new();
            crate::theme::apply_theme(&cc.egui_ctx, &app.conv.settings.theme_id);
            egui_extras::install_image_loaders(&cc.egui_ctx);
            seed_local_model(&mut app);
            app.conv.sidebar.width = 250.0;
            app.new_chat();
            app
        });
    let mut rec = Recorder {
        harness,
        out,
        stills,
        frame: 0,
        project: project.clone(),
        tx: None,
        tool_env: ToolEnv {
            enabled: vec![true; crate::settings::ALL_TOOL_NAMES.len()],
            web_search_url: String::new(),
            web_search_backend: Default::default(),
            bash_timeout_cap_secs: 60,
            mcp: None,
            undo_journal: None,
            subagent: None,
        },
    };

    if std::env::var_os("OXI_WORKTREE_REVIEW").is_some() {
        review_worktree_sidebar(&mut rec);
        return;
    }

    if std::env::var_os("OXI_PICKER_REVIEW").is_some() {
        review_composer_pickers(&mut rec);
        return;
    }

    if std::env::var_os("OXI_COMPOSER_REVIEW").is_some() {
        review_composer_typing(&mut rec);
        return;
    }

    if std::env::var_os("OXI_ACP_REVIEW").is_some() {
        review_acp_ui(&mut rec);
        return;
    }

    let gallery = rec.out.is_none();
    // 1. The model is already downloaded and running: the guided Local HF setup.
    rec.harness.run_steps(4);
    if gallery {
        rec.still("empty-chat");
        rec.app().conv.settings.active_provider = LlmProviderKind::OpenRouter;
        rec.harness.run_steps(3);
        rec.still("empty-chat-setup");
        rec.app().conv.settings.active_provider = LlmProviderKind::LocalHf;
        use super::state::SettingsTab;
        {
            let app = rec.app();
            app.conv.settings.mcp_servers = vec![
                crate::settings::McpServerConfig {
                    name: "github".into(),
                    command: "npx".into(),
                    args: vec!["-y".into(), "@modelcontextprotocol/server-github".into()],
                    ..Default::default()
                },
                crate::settings::McpServerConfig {
                    name: "docs".into(),
                    transport: crate::settings::McpTransport::Http,
                    url: "http://127.0.0.1:9/mcp".into(),
                    ..Default::default()
                },
            ];
            let servers = app.conv.settings.mcp_servers[1..].to_vec();
            app.mcp.sync_servers(&servers);
        }
        rec.app().open_settings_page();
        for (tab, name) in [
            (SettingsTab::Agent, "settings-agent"),
            (SettingsTab::GitHub, "settings-github"),
            (SettingsTab::Prompts, "settings-prompts"),
            (SettingsTab::Voice, "settings-voice"),
            (SettingsTab::Terminal, "settings-terminal"),
            (SettingsTab::Appearance, "settings-appearance"),
            (SettingsTab::About, "settings-about"),
        ] {
            rec.app().conv.settings_page.tab = tab;
            rec.harness.run_steps(3);
            rec.still(name);
            if tab == SettingsTab::Agent {
                rec.scroll_by(-700.0);
                rec.still("settings-agent-mcp");
            }
        }
        rec.app()
            .conv
            .settings
            .provider_mut(LlmProviderKind::LlamaCpp)
            .base_url = "http://localhost:8080".into();
        crate::router::quota::set_snapshot_for_tests(
            LlmProviderKind::ClaudeCodeAcp,
            crate::router::quota::QuotaSnapshot {
                windows: vec![
                    crate::router::quota::UsageWindow {
                        label: "5h".into(),
                        used_pct: 32.0,
                        resets_at: Some(crate::router::quota::now_secs() + 4_200),
                        model_scope: None,
                    },
                    crate::router::quota::UsageWindow {
                        label: "7d".into(),
                        used_pct: 61.0,
                        resets_at: Some(crate::router::quota::now_secs() + 200_000),
                        model_scope: None,
                    },
                ],
                plan: Some("max".into()),
                source: "Claude usage (Claude Code login)".into(),
                updated_at: crate::router::quota::now_secs(),
                ..Default::default()
            },
        );
        for provider in [
            LlmProviderKind::Router,
            LlmProviderKind::LlamaCpp,
            LlmProviderKind::OpenAi,
            LlmProviderKind::ClaudeCodeAcp,
            LlmProviderKind::Ollama,
        ] {
            let app = rec.app();
            app.conv.settings_page.tab = SettingsTab::Providers;
            app.conv.settings_page.provider_tab = provider;
            rec.harness.run_steps(3);
            rec.still(&format!("settings-provider-{provider:?}").to_lowercase());
            if provider == LlmProviderKind::Router {
                rec.scroll_by(-900.0);
                rec.still("settings-provider-router-quota");
            }
        }
        rec.app().conv.settings_page.tab = SettingsTab::Providers;
        rec.app().conv.settings_page.open = false;
        rec.app().conv.settings_page.original = None;
        rec.harness.run_steps(3);
    }
    {
        let app = rec.app();
        app.open_settings_page();
        app.conv.settings_page.provider_tab = LlmProviderKind::LocalHf;
    }
    rec.hold(2.6);
    rec.still("local-models");
    {
        let app = rec.app();
        app.conv.settings_page.open = false;
        app.conv.settings_page.original = None;
        app.conv.composer.focus_next_frame = true;
    }

    // 2. Ask for a fix.
    rec.hold(1.0);
    rec.type_text("Run the tests and fix the failing one", 2);
    rec.hold(0.4);
    rec.begin_turn("Run the tests and fix the failing one");
    if gallery {
        rec.send(AgentEvent::Routed(Box::new(crate::model::RouteNote {
            provider: LlmProviderKind::ClaudeCodeAcp,
            model: "sonnet".into(),
            effort: "medium".into(),
            tier: "standard".into(),
            strategy: "Balanced".into(),
            reason: "Standard task (fix, test). Claude Code (ACP) · sonnet because: right size \
                     for the task; your standard-task model; subscription, 5h 32% used, 7d 61% \
                     used. Next best: OpenRouter · anthropic/claude-sonnet-4.5 (pay per use, ~$0.17)."
                .into(),
            alternatives: vec![
                "OpenRouter · anthropic/claude-sonnet-4.5 — score -8: pay per use, ~$0.17".into(),
            ],
            failover_from: None,
        })));
        rec.harness.run_steps(4);
        rec.still("chat-routed");
    }
    rec.hold(0.5);

    // 3. The agent works through it with real tools.
    rec.stream_thinking(
        "The user wants the failing test fixed. I'll run the suite first to see which test \
         fails and why.",
    );
    rec.tool(
        "call_1",
        "bash",
        json!({ "command": "python3 -m unittest -q" }),
        1.2,
    );
    rec.stream_text(
        "`test_median_even_length` fails: `median([1, 2, 3, 4])` returns `3` instead of \
         `2.5`. With an even number of values the median is the mean of the two middle \
         ones. Let me look at the implementation.\n\n",
    );
    rec.tool("call_2", "read", json!({ "path": "stats.py" }), 0.6);
    rec.stream_thinking(
        "median() always returns the upper middle element. For even lengths it should \
         average ordered[mid - 1] and ordered[mid].",
    );
    rec.tool(
        "call_3",
        "edit",
        json!({
            "path": "stats.py",
            "edits": [{
                "oldText": "    return ordered[len(ordered) // 2]",
                "newText": "    mid = len(ordered) // 2\n    if len(ordered) % 2 == 0:\n        return (ordered[mid - 1] + ordered[mid]) / 2\n    return ordered[mid]",
            }],
        }),
        0.7,
    );
    rec.still("agent-run");
    if gallery {
        for id in ["call_1", "call_2"] {
            let persist =
                crate::ui::preview_expand::expand_persist_id(egui::Id::new(("tool_pill", id)));
            rec.harness
                .ctx
                .data_mut(|d| d.insert_persisted(persist, true));
        }
        rec.harness.run_steps(4);
        rec.still("agent-run-expanded");
        for id in ["call_1", "call_2"] {
            let persist =
                crate::ui::preview_expand::expand_persist_id(egui::Id::new(("tool_pill", id)));
            rec.harness
                .ctx
                .data_mut(|d| d.insert_persisted(persist, false));
        }
    }
    // The same moment in every theme (stills only, not part of the video).
    for theme in ["mariana", "sublime", "midnight", "light", "dark"] {
        crate::theme::apply_theme(&rec.harness.ctx, theme);
        rec.harness.run_steps(3);
        rec.still(&format!("theme-{theme}"));
    }
    crate::theme::apply_theme(&rec.harness.ctx, "mariana");
    rec.harness.run_steps(3);
    rec.tool(
        "call_4",
        "bash",
        json!({ "command": "python3 -m unittest -q" }),
        1.0,
    );
    rec.stream_text(
        "Fixed `median()` in `stats.py`: for an even number of values it now averages the \
         two middle elements.\n\nAll 5 tests pass.",
    );
    rec.finish_turn();
    rec.hold(2.2);
    rec.still("chat");
    if gallery {
        {
            use crate::agent::activity_log::{self as activity, ActivityKind};
            activity::set_enabled(true);
            activity::log_json(
                ActivityKind::Request,
                "POST http://localhost:8080/v1/chat/completions",
                &serde_json::json!({
                    "model": "qwen2.5-coder-7b",
                    "stream": true,
                    "messages": [{ "role": "user", "content": "Run the tests and fix the failing one" }],
                }),
            );
            activity::log(
                ActivityKind::Retry,
                "Attempt 1/5 failed · POST http://localhost:8080/v1/chat/completions",
                "HTTP 503: loading model",
            );
            activity::log(
                ActivityKind::Response,
                "HTTP 200 OK · http://localhost:8080/v1/chat/completions · round 1",
                "data: {\"choices\":[{\"delta\":{\"content\":\"Running\"}}]}\n\ndata: [DONE]\n",
            );
            activity::log(
                ActivityKind::Tool,
                "bash",
                "Arguments:\n{ \"command\": \"python -m pytest -q\" }\n\nResult:\n5 passed",
            );
            let app = rec.app();
            app.conv.settings.activity_log_enabled = true;
            super::activity_window::toggle_activity_window(&rec.harness.ctx);
        }
        rec.harness.run_steps(3);
        rec.still("activity-log");
        super::activity_window::toggle_activity_window(&rec.harness.ctx);
        rec.app().conv.settings.activity_log_enabled = false;
        crate::agent::activity_log::set_enabled(false);
        {
            let app = rec.app();
            let key = app.active_session_key();
            app.run_state_mut(key).stream_error =
                Some("HTTP 401 Unauthorized: invalid API key for this endpoint".into());
        }
        rec.harness.run_steps(3);
        rec.still("chat-error");
        {
            let app = rec.app();
            let key = app.active_session_key();
            let run = app.run_state_mut(key);
            run.stream_error = None;
            run.pending_approval = Some(super::state::PendingApproval {
                name: "bash".into(),
                summary: "rm -rf build/ && python3 -m unittest -q".into(),
                allow_prefix: None,
            });
        }
        rec.harness.run_steps(3);
        rec.still("chat-approval");
        // A small window: every row has to wrap or truncate instead of overflowing.
        rec.harness.set_size(egui::vec2(760.0, 560.0));
        rec.harness.run_steps(4);
        rec.still("narrow-chat");
        rec.app().open_settings_page();
        rec.harness.run_steps(4);
        rec.still("narrow-settings");
        rec.app().conv.settings_page.open = false;
        rec.app().conv.settings_page.original = None;
        rec.harness.set_size(SIZE);
        rec.harness.run_steps(4);
        {
            let app = rec.app();
            let key = app.active_session_key();
            app.run_state_mut(key).pending_approval = None;
        }
        rec.harness.run_steps(2);
    }
    if gallery {
        let selected = rec.drag_select_label("All 5 tests pass", false);
        println!("PROFILE text selection in the short demo chat: {selected}");
        rec.profile("chat idle", false);
        rec.profile("chat hover", true);
        // A long, markdown-heavy conversation, scrolled with the wheel.
        {
            let app = rec.app();
            app.active_session_mut().messages = heavy_transcript(300);
            app.conv.transcript.scroll_to_bottom_once = true;
        }
        rec.harness.run_steps(4);
        rec.still("chat-heavy");
        rec.profile("heavy idle", false);
        rec.profile_scroll("heavy scroll");
        rec.profile("heavy hover", true);
        rec.profile_streaming("heavy stream");
        rec.profile_composer_typing("composer typing");
        println!(
            "PROFILE composer received {} chars",
            rec.app().conv.composer.input.len()
        );
        rec.app().conv.composer.input.clear();
        for streaming in [false, true] {
            let selected = rec.drag_select_label("That is the whole story for turn", streaming);
            println!(
                "PROFILE text selection in a 300-turn chat (streaming={streaming}): {selected}"
            );
        }
        // Scrolled up into the middle of the history, where units on both sides are culled.
        rec.harness
            .event(Event::PointerMoved(egui::pos2(700.0, 400.0)));
        for _ in 0..40 {
            rec.harness.event(Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, 400.0),
                modifiers: egui::Modifiers::NONE,
                phase: egui::TouchPhase::Move,
            });
            rec.harness.step();
        }
        rec.harness.run_steps(20);
        let selected = rec.drag_select_label("That is the whole story for turn", false);
        println!("PROFILE text selection after scrolling up a 300-turn chat: {selected}");
        rec.app().conv.transcript.scroll_to_bottom_once = true;
        rec.harness.run_steps(4);
        // A long chat history in the sidebar.
        {
            let app = rec.app();
            let wi = app.conv.active_workspace;
            for i in 0..400 {
                let mut session =
                    OxiApp::blank_session(format!("Earlier chat number {i} about retries"));
                session.messages_loaded = false;
                app.conv.workspaces[wi].sessions.push(session);
            }
        }
        rec.harness.run_steps(3);
        rec.still("sidebar-400");
        rec.app().conv.sidebar.search = "csv".into();
        rec.harness.run_steps(3);
        rec.still("sidebar-search");
        rec.app().conv.sidebar.search.clear();
        // Folded date group: the header keeps its count, the rows are hidden.
        {
            let app = rec.app();
            let wi = app.conv.active_workspace;
            app.conv.workspaces[wi].folded_groups = vec!["today".into()];
        }
        rec.harness.run_steps(3);
        rec.still("sidebar-folded-group");
        {
            let app = rec.app();
            let wi = app.conv.active_workspace;
            app.conv.workspaces[wi].folded_groups.clear();
        }
        rec.profile("400 chats idle", false);
        rec.profile("400 chats hover", true);
        {
            let app = rec.app();
            let wi = app.conv.active_workspace;
            app.conv.workspaces[wi].sessions.truncate(5);
        }
        {
            let app = rec.app();
            app.active_session_mut().messages.truncate(2);
            app.conv.transcript.scroll_to_bottom_once = true;
        }
        rec.harness.run_steps(4);
    }

    // 4. Review the change in the editor, with the Git panel open.
    {
        let app = rec.app();
        let path = std::fs::canonicalize(project.join("stats.py")).unwrap();
        app.open_editor_file(path.clone());
        app.conv.editor.git_full_highlight_path = Some(path);
        app.conv.git_ui.open = true;
    }
    rec.hold(4.0);
    rec.still("editor-git");
    if gallery {
        use crate::app::git_panel::GitTab;
        // An empty scratchpad must still show its caret at line 1, column 1.
        rec.app().open_scratchpad();
        rec.harness.run_steps(4);
        rec.still("scratchpad-empty");
        rec.app().reveal_chat_view();
        rec.app()
            .open_editor_file(std::fs::canonicalize(project.join("stats.py")).unwrap());
        rec.harness.run_steps(4);
        rec.app().conv.git_ui.tab = GitTab::History;
        rec.harness.run_steps(4);
        rec.still("git-history");
        rec.app().conv.git_ui.tab = GitTab::Branches;
        rec.harness.run_steps(4);
        rec.still("git-branches");
        rec.app().conv.git_ui.tab = GitTab::Changes;
        rec.app().conv.editor.find_open = true;
        rec.app().conv.editor.find_query = "ordered".into();
        rec.harness.run_steps(4);
        rec.still("editor-find");
        rec.app().conv.editor.find_replace_open = true;
        rec.app().conv.editor.find_options.whole_word = true;
        rec.app().conv.editor.replace_query = "sorted".into();
        rec.harness.run_steps(4);
        rec.still("editor-find-replace");
        rec.app().conv.editor.find_query = "no such text".into();
        rec.harness.run_steps(4);
        rec.still("editor-find-none");
        rec.app().conv.editor.find_open = false;
        rec.app().conv.editor.find_replace_open = false;
        rec.app().open_file_picker();
        rec.harness.run_steps(4);
        rec.still("file-picker");
        rec.app().cancel_file_picker();
        rec.harness.run_steps(2);
        // Sublime-style tabs: an unsaved active tab, the pointer over an inactive one.
        {
            let app = rec.app();
            let active = app.conv.editor.active;
            app.open_editor_file_only(
                std::fs::canonicalize(project.join("test_stats.py")).unwrap(),
            );
            app.conv.editor.active = active;
            if let Some(document) = app.conv.editor.active_document_mut() {
                document.content.push('\n');
                document.dirty = true;
                document.content_revision += 1;
            }
        }
        rec.harness.run_steps(4);
        let tab = rec
            .harness
            .query_all_by_label("test_stats.py")
            .map(|node| node.rect())
            .min_by(|a, b| a.top().total_cmp(&b.top()));
        if let Some(tab) = tab {
            // AccessKit rects are in physical pixels.
            let scale = rec.harness.ctx.pixels_per_point();
            rec.harness
                .hover_at((tab.center().to_vec2() / scale).to_pos2());
        }
        rec.harness.run_steps(4);
        rec.still("editor-tabs-hover");
        rec.app().open_file_picker_with("@");
        rec.harness.run_steps(4);
        rec.still("goto-symbol");
        rec.app().cancel_file_picker();
        if let Some(document) = rec.app().conv.editor.active_document_mut() {
            document.content = document.saved_content.clone();
            document.dirty = false;
            document.content_revision += 1;
        }
        rec.harness.run_steps(2);
        rec.check_editor_commands();
        // Diff mode in the editor tab: a working-tree change side by side (the right side is
        // the editable file), inline, then a commit's patch.
        assert!(rec.app().open_diff_editor(
            "stats.py",
            crate::app::file_explorer::DiffSource::WorkTree,
            None
        ));
        rec.wait_for_diff_editor();
        rec.still("diff-split");
        // Hovering a change shows its block actions (Stage / Discard).
        let mid_line = rec.diff_change_point(0);
        rec.harness.hover_at(mid_line);
        rec.harness.run_steps(4);
        rec.still("diff-block-actions");
        // Drag-select from inside that line down two lines: it is the editor's own selection.
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let select_to = mid_line + egui::vec2(110.0, 35.0);
        rec.harness.event(button(mid_line, true));
        rec.harness.step();
        for i in 1..=6 {
            rec.harness.event(egui::Event::PointerMoved(
                mid_line.lerp(select_to, i as f32 / 6.0),
            ));
            rec.harness.step();
        }
        rec.harness.event(button(select_to, false));
        rec.harness.run_steps(2);
        rec.still("diff-selection");
        rec.profile("diff split idle", false);
        rec.app().conv.git_ui.open = false;
        rec.harness.run_steps(4);
        rec.still("diff-split-wide");
        rec.app().conv.git_ui.open = true;
        rec.app().conv.editor.diff_inline = true;
        rec.harness.run_steps(4);
        rec.still("diff-inline");
        rec.app().conv.editor.diff_inline = false;
        let hash = rec.app().conv.git.log.first().map(|c| c.hash.clone());
        if let Some(hash) = hash {
            rec.app()
                .request(crate::git::GitOp::ShowCommit(hash.clone()));
            rec.wait_for_git_diff(&format!("Commit {hash}"));
            rec.still("diff-commit");
            // The commit being shown is highlighted in the History list.
            rec.app().conv.git_ui.tab = crate::app::git_panel::GitTab::History;
            rec.harness.run_steps(4);
            rec.still("git-history-selected");
            rec.app().conv.git_ui.tab = crate::app::git_panel::GitTab::Changes;
        }
        // Branch compare: a base branch one commit back, plus the uncommitted edit.
        if let Ok(repo) = git2::Repository::discover(&project)
            && let Ok(head) = repo.head().and_then(|head| head.peel_to_commit())
        {
            let base = head.parent(0).unwrap_or_else(|_| head.clone());
            let _ = repo.branch("base-demo", &base, true);
            rec.app().conv.git_ui.tab = crate::app::git_panel::GitTab::Compare;
            for _ in 0..200 {
                rec.harness.run_steps(1);
                if rec.app().conv.git_ui.compare.data.is_some() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            rec.harness.run_steps(4);
            rec.still("git-compare");
            let target = rec
                .app()
                .conv
                .git_ui
                .compare
                .data
                .as_ref()
                .and_then(|data| {
                    let file = data.files.iter().find(|f| f.status != 'D')?;
                    Some((data.base.clone(), file.path.clone()))
                });
            if let Some((base, path)) = target {
                assert!(rec.app().open_diff_editor(
                    &path,
                    crate::app::file_explorer::DiffSource::Compare {
                        base: base.clone(),
                        old_path: None,
                    },
                    None,
                ));
                rec.wait_for_diff_editor();
                rec.still("diff-compare");
                rec.app().open_changed_file(&path);
                rec.harness.run_steps(4);
                rec.still("editor-compare-gutter");
                // Clicking a gutter marker opens the diff at that change.
                if let Some(index) = rec.app().conv.editor.active {
                    let source = rec.app().gutter_diff_source();
                    rec.app().open_document_diff(index, source);
                }
                rec.wait_for_diff_editor();
                rec.app().conv.editor.diff_inline = true;
                rec.harness.run_steps(4);
                rec.still("editor-gutter-diff");
                // Discard from the hover buttons, clicked for real: the edit lands mid-render.
                let at = rec.diff_change_point(0);
                rec.harness.hover_at(at);
                rec.harness.run_steps(2);
                let scale = rec.harness.ctx.pixels_per_point();
                let button = rec
                    .harness
                    .query_all_by_label("Discard")
                    .map(|node| node.rect())
                    .next();
                assert!(button.is_some(), "Discard button not found");
                let original = std::fs::read_to_string(project.join(&path)).unwrap();
                if let Some(button) = button {
                    let at = (button.center().to_vec2() / scale).to_pos2();
                    rec.harness.hover_at(at);
                    rec.harness.run_steps(1);
                    rec.harness.drag_at(at);
                    rec.harness.run_steps(1);
                    rec.harness.drop_at(at);
                }
                rec.harness.run_steps(4);
                let app = rec.app();
                if let Some(index) = app.conv.editor.active {
                    let document = &mut app.conv.editor.documents[index];
                    let revision = document.content_revision;
                    let left = document
                        .diff
                        .as_mut()
                        .and_then(|diff| diff.decor_at(revision, &document.content))
                        .map_or(usize::MAX, |decor| decor.changes.len());
                    let reverted = document.content != original;
                    println!("CHECK diff discard: {reverted}, changes left {left}");
                    assert!(reverted, "diff discard failed");
                    // Put the file back for the rest of the demo.
                    std::fs::write(project.join(&path), &original).unwrap();
                    document.content = original.clone();
                    document.saved_content = original;
                    document.dirty = false;
                    document.content_revision += 1;
                    document.diff = None;
                }
                rec.app().conv.editor.diff_inline = false;
            }
            rec.app().conv.git_ui.tab = crate::app::git_panel::GitTab::Changes;
        }
        rec.app().close_editor_git_diff();
        rec.harness.run_steps(2);
        rec.profile("editor idle", false);
        rec.profile("editor hover", true);
        // A typical source file (~2k lines): typing at the caret.
        let medium = project.join("medium.rs");
        let mut source = String::new();
        for i in 0..400 {
            source.push_str(&format!(
                "/// Doubles every value below the limit.\nfn handler_{i}(values: &[u32]) -> Vec<u32> {{\n    values.iter().filter(|v| **v < {i}).map(|v| v * 2).collect()\n}}\n\n"
            ));
        }
        std::fs::write(&medium, source).unwrap();
        rec.app()
            .open_editor_file(std::fs::canonicalize(&medium).unwrap());
        rec.hold(1.0);
        rec.profile_typing("2k file typing");
        // A 20k-line file: idle, wheel scrolling, and typing at the caret.
        let big = project.join("big.py");
        let mut source = String::new();
        for i in 0..20_000 {
            source.push_str(&format!(
                "def handler_{i}(values, limit={i}):\n    return [v * 2 for v in values if v < limit]  # {i}\n"
            ));
        }
        std::fs::write(&big, source).unwrap();
        rec.app()
            .open_editor_file(std::fs::canonicalize(&big).unwrap());
        rec.hold(1.0);
        rec.profile("big file idle", false);
        rec.profile_scroll("big file scroll");
        rec.profile_typing("big file typing");
        // Colors must still match the text after edits (the highlight is now per window).
        rec.harness.run_steps(4);
        rec.still("big-file-after-typing");
    }
    if gallery {
        // A deep explorer tree with long names and a busy Git panel: row selection,
        // indentation, status letters and truncation at a normal sidebar width.
        let nested = project.join("src/reporting/exporters");
        std::fs::create_dir_all(&nested).unwrap();
        for name in [
            "csv.py",
            "a_really_long_exporter_module_name_that_overflows.py",
            "json.py",
        ] {
            std::fs::write(nested.join(name), "x = 1\n").unwrap();
        }
        for i in 0..30 {
            std::fs::write(project.join(format!("module_{i:02}.py")), "x = 1\n").unwrap();
        }
        let deep = std::fs::canonicalize(
            nested.join("a_really_long_exporter_module_name_that_overflows.py"),
        )
        .unwrap();
        {
            let app = rec.app();
            for dir in ["src", "src/reporting", "src/reporting/exporters"] {
                app.conv
                    .explorer
                    .expanded
                    .insert(std::fs::canonicalize(project.join(dir)).unwrap());
            }
            app.conv.explorer.cache.invalidate();
            app.conv.sidebar.open = true;
            app.conv.sidebar.mode = crate::app::state::SidebarMode::Explorer;
            app.open_editor_file(deep);
            app.request(crate::git::GitOp::Refresh);
        }
        rec.hold(1.5);
        rec.still("explorer-tree");
    }
    if gallery {
        rec.app().conv.terminal_panel.open = true;
        rec.hold(1.5);
        rec.still("terminal");
        crate::theme::apply_theme(&rec.harness.ctx, "light");
        rec.harness.run_steps(3);
        rec.still("editor-light");
        {
            let app = rec.app();
            app.conv.terminal_panel.open = false;
            app.conv.git_ui.open = false;
            app.conv.editor.documents.clear();
            app.conv.editor.active = None;
        }
        rec.harness.run_steps(3);
        rec.still("chat-light");

        // Plan mode: a planning turn with a live checklist, then the hand-off bar.
        crate::theme::apply_theme(&rec.harness.ctx, "dark");
        rec.app().new_chat();
        let key = rec.app().active_session_key();
        rec.app().run_state_mut(key).plan_mode = true;
        rec.harness.run_steps(3);
        rec.still("plan-mode-empty");
        rec.app().run_state_mut(key).last_turn_planned = true;
        rec.begin_turn("Add CSV export to the stats report");
        rec.app().active_session_mut().chars_per_token = Some(3.25);
        rec.send(AgentEvent::SubagentUsage(TokenUsage {
            input_tokens: 1200,
            output_tokens: 24,
            cache_read_input_tokens: 300,
            ..Default::default()
        }));
        rec.harness.run_steps(3);
        let run = rec.app().run_state(key).unwrap();
        assert_eq!(run.turn_usage.total_input(), 1500);
        assert_eq!(run.turn_usage.output_tokens, 24);
        assert_eq!(run.session_usage.total_input(), 1500);
        assert_eq!(rec.app().active_session().chars_per_token, Some(3.25));
        rec.tool(
            "todo1",
            "todo_write",
            json!({"todos": [
                {"content": "Map how reports are rendered", "status": "completed"},
                {"content": "Find where output formats are chosen", "status": "in_progress"},
                {"content": "Draft the CSV writer and CLI flag", "status": "pending"},
                {"content": "List tests to add", "status": "pending"}
            ]}),
            0.2,
        );
        rec.harness.run_steps(3);
        rec.still("tasks-panel");
        rec.tool(
            "todo2",
            "todo_write",
            json!({"todos": [
                {"content": "Map how reports are rendered", "status": "completed"},
                {"content": "Find where output formats are chosen", "status": "completed"},
                {"content": "Draft the CSV writer and CLI flag", "status": "completed"},
                {"content": "List tests to add", "status": "completed"}
            ]}),
            0.2,
        );
        rec.stream_text(
            "## Plan\n\n1. Add `write_csv(report, path)` in `stats/report.py` next to \
             `write_markdown`.\n2. Add `--format csv` to the CLI in `stats/cli.py`.\n3. Tests: \
             round-trip a small report through `csv.reader`.\n",
        );
        rec.finish_turn();
        rec.hold(1.0);
        rec.still("plan-ready");
        assert!(rec.harness.query_by_label("Implement plan").is_some());
        let message_count = rec.app().active_session().messages.len();
        rec.app().conv.composer.editing_last_prompt = Some(super::state::PromptEditState {
            previous_input: String::new(),
            previous_images: Vec::new(),
            previous_texts: Vec::new(),
        });
        rec.app().conv.composer.input = "Revise the CSV plan".into();
        rec.harness.run_steps(3);
        assert!(rec.harness.query_by_label("Implement plan").is_none());
        rec.still("plan-editing");
        rec.app().cancel_edit_last_prompt();
        rec.harness.run_steps(3);
        assert!(rec.harness.query_by_label("Implement plan").is_some());
        assert_eq!(rec.app().active_session().messages.len(), message_count);
    }
}

struct Recorder<'a> {
    harness: Harness<'a, OxiApp>,
    /// Video frames go here; `None` renders stills only, as fast as possible.
    out: Option<PathBuf>,
    stills: Option<PathBuf>,
    frame: usize,
    project: PathBuf,
    tx: Option<mpsc::Sender<AgentEvent>>,
    tool_env: ToolEnv,
}

impl Recorder<'_> {
    fn app(&mut self) -> &mut OxiApp {
        self.harness.state_mut()
    }

    /// Advance one frame and save it. Paced to real time so on-screen timers stay honest.
    fn shot(&mut self) {
        let started = Instant::now();
        self.harness.step();
        let Some(out) = self.out.clone() else {
            return;
        };
        let mut image = self.harness.render().expect("render frame");
        paint_traffic_lights(&mut image);
        image
            .save(out.join(format!("frame_{:05}.png", self.frame)))
            .expect("save frame");
        self.frame += 1;
        if let Some(rest) = FRAME.checked_sub(started.elapsed()) {
            std::thread::sleep(rest);
        }
    }

    /// Save the current screen as a named screenshot for the website.
    fn still(&mut self, name: &str) {
        let Some(dir) = self.stills.clone() else {
            return;
        };
        self.harness.step();
        let mut image = self.harness.render().expect("render still");
        paint_traffic_lights(&mut image);
        image
            .save(dir.join(format!("{name}.png")))
            .expect("save still");
    }

    /// Time `frames` UI frames of the current app state and print mean/p50/max.
    ///
    /// Frames run on a separate plain egui context, not the harness: kittest keeps AccessKit
    /// on, and exporting every text row to the accessibility tree dominated the numbers (the real
    /// app only pays that while an assistive client is connected). `events` supplies the input
    /// for frame `i` and may mutate the app first (e.g. to stream text in).
    fn measure(
        &mut self,
        label: &str,
        frames: usize,
        mut events: impl FnMut(usize, &mut OxiApp) -> Vec<Event>,
    ) {
        const WARM_UP: usize = 12;
        let ctx = egui::Context::default();
        let theme = self.app().conv.settings.theme_id.clone();
        crate::theme::apply_theme(&ctx, &theme);
        egui_extras::install_image_loaders(&ctx);
        let mut frame = eframe::Frame::_new_kittest();
        let mut times = Vec::with_capacity(frames);
        let mut repaint_causes = Vec::new();
        for i in 0..WARM_UP + frames {
            let app = self.harness.state_mut();
            let mut raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, SIZE)),
                time: Some(i as f64 / 60.0),
                predicted_dt: 1.0 / 60.0,
                focused: true,
                events: if i < WARM_UP {
                    Vec::new()
                } else {
                    events(i - WARM_UP, app)
                },
                ..Default::default()
            };
            raw.viewports
                .entry(egui::ViewportId::ROOT)
                .or_default()
                .native_pixels_per_point = Some(SCALE);
            let started = Instant::now();
            let output = ctx.run_ui(raw, |ui| {
                eframe::App::logic(app, ui.ctx(), &mut frame);
                eframe::App::ui(app, ui, &mut frame);
            });
            // Tessellation runs on the UI thread every painted frame too.
            let _ = ctx.tessellate(output.shapes, output.pixels_per_point);
            if i >= WARM_UP {
                times.push(started.elapsed());
            }
            if i + 1 == WARM_UP + frames {
                repaint_causes = ctx
                    .repaint_causes()
                    .iter()
                    .map(|c| format!("{}:{} {}", c.file, c.line, c.reason))
                    .collect();
            }
        }
        times.sort();
        let mean = times.iter().sum::<Duration>() / frames as u32;
        println!(
            "PROFILE {label:<16} mean {:>7.3} ms  p50 {:>7.3} ms  max {:>7.3} ms",
            mean.as_secs_f64() * 1e3,
            times[frames / 2].as_secs_f64() * 1e3,
            times[frames - 1].as_secs_f64() * 1e3,
        );
        if label.ends_with("idle") {
            // Anything still asking for frames while idle keeps the CPU awake for nothing.
            repaint_causes.sort();
            repaint_causes.dedup();
            for cause in repaint_causes {
                println!("PROFILE   repaint requested by {cause}");
            }
        }
    }

    /// Press-drag-release across the on-screen label containing `text`, the way a user selects
    /// it, and report whether the transcript then holds a selection. With `streaming` the last
    /// reply keeps growing (and the view stuck to the bottom) while the pointer moves.
    fn drag_select_label(&mut self, text: &str, streaming: bool) -> bool {
        if streaming {
            let app = self.app();
            let mut reply = message(
                MsgRole::Assistant,
                "",
                vec![AssistantBlock::Answer(String::new())],
            );
            reply.streaming = true;
            app.active_session_mut().messages.push(reply);
            app.conv.transcript.scroll_to_bottom_once = true;
        }
        let grow = |rec: &mut Self| {
            if !streaming {
                return;
            }
            if let Some(AssistantBlock::Answer(t)) = rec
                .app()
                .active_session_mut()
                .messages
                .last_mut()
                .and_then(|m| m.blocks.last_mut())
            {
                t.push_str("more streamed words ");
            }
        };
        self.harness.run_steps(6);
        // AccessKit bounds are physical pixels; pointer events are in points. Several turns
        // share the text: take the on-screen match nearest the window's middle.
        let Some(rect) = self
            .harness
            .query_all_by_label_contains(text)
            .map(|node| {
                let r = node.rect();
                egui::Rect::from_min_max(
                    (r.min.to_vec2() / SCALE).to_pos2(),
                    (r.max.to_vec2() / SCALE).to_pos2(),
                )
            })
            .filter(|r| (60.0..SIZE.y - 160.0).contains(&r.center().y))
            .min_by_key(|r| (r.center().y - SIZE.y / 2.0).abs() as i64)
        else {
            println!("PROFILE   label {text:?} not on screen");
            return false;
        };
        let from = egui::pos2(rect.left() + 3.0, rect.center().y);
        let to = egui::pos2(rect.right() - 3.0, rect.center().y);
        let button = |pos, pressed| Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        self.harness.event(Event::PointerMoved(from));
        for _ in 0..6 {
            grow(self);
            self.harness.step();
        }
        self.harness.event(button(from, true));
        self.harness.step();
        for i in 1..=8 {
            grow(self);
            self.harness
                .event(Event::PointerMoved(from.lerp(to, i as f32 / 8.0)));
            self.harness.step();
            if std::env::var_os("OXI_DEBUG_SELECT").is_some() {
                let ctx = &self.harness.ctx;
                println!(
                    "SEL frame {i}: down={} press_origin={:?} dragged={:?} exists={} sel={} over_egui={} time={}",
                    ctx.input(|i| i.pointer.primary_down()),
                    ctx.input(|i| i.pointer.press_origin()),
                    ctx.dragged_id(),
                    ctx.dragged_id()
                        .and_then(|id| ctx.read_response(id))
                        .is_some(),
                    ctx.plugin::<egui::text_selection::LabelSelectionState>()
                        .lock()
                        .has_selection(),
                    ctx.is_pointer_over_egui(),
                    ctx.input(|i| i.time),
                );
            }
        }
        self.harness.event(button(to, false));
        self.harness.step();
        let selected = self
            .harness
            .ctx
            .plugin::<egui::text_selection::LabelSelectionState>()
            .lock()
            .has_selection();
        self.harness.event(button(to, true));
        self.harness.event(button(to, false));
        self.harness.run_steps(2);
        if streaming {
            self.app().active_session_mut().messages.pop();
        }
        selected
    }

    /// Frames with no input, or with the pointer wandering over the window.
    fn profile(&mut self, label: &str, hover: bool) {
        self.measure(label, 200, |i, _| {
            if !hover {
                return vec![Event::PointerMoved(egui::pos2(700.0, 400.0))];
            }
            let t = i as f32 / 200.0;
            vec![Event::PointerMoved(egui::pos2(
                300.0 + 600.0 * t,
                150.0 + 400.0 * ((t * 7.0).sin() * 0.5 + 0.5),
            ))]
        });
    }

    /// Frames while a long markdown reply streams in (a few tokens per frame) at the bottom of
    /// the current transcript, the pointer resting over it.
    fn profile_streaming(&mut self, label: &str) {
        let chunk = "The retry loop keeps `attempt` bounded, and **each** failure is logged. ";
        {
            let app = self.app();
            let mut reply = message(
                MsgRole::Assistant,
                "",
                vec![AssistantBlock::Answer(String::new())],
            );
            reply.streaming = true;
            app.active_session_mut().messages.push(reply);
        }
        self.measure(label, 300, |i, app| {
            if let Some(AssistantBlock::Answer(text)) = app
                .active_session_mut()
                .messages
                .last_mut()
                .and_then(|m| m.blocks.last_mut())
            {
                text.push_str(chunk);
                if i % 12 == 11 {
                    text.push_str("\n\n");
                }
            }
            vec![Event::PointerMoved(egui::pos2(700.0, 400.0))]
        });
        self.app().active_session_mut().messages.pop();
    }

    /// Frames while typing one character per frame into the focused editor.
    fn profile_typing(&mut self, label: &str) {
        self.measure(label, 120, |i, app| {
            if i == 0 {
                app.conv.editor.focus_editor_next_frame = true;
                return Vec::new();
            }
            vec![Event::Text(if i % 20 == 19 { "\n" } else { "x" }.into())]
        });
    }

    /// Click into the composer, then type a character (and now and then a space) every frame.
    fn profile_composer_typing(&mut self, label: &str) {
        let composer = egui::pos2(620.0, 688.0);
        self.measure(label, 120, |i, _| match i {
            0 => vec![
                Event::PointerMoved(composer),
                Event::PointerButton {
                    pos: composer,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            1 => vec![Event::PointerButton {
                pos: composer,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
            _ => vec![Event::Text(if i % 6 == 5 { " " } else { "a" }.into())],
        });
    }

    /// Frames with the wheel scrolling up, then back down, over the middle of the window.
    fn profile_scroll(&mut self, label: &str) {
        self.measure(label, 240, |i, _| {
            let dy = if i < 120 { 120.0 } else { -120.0 };
            vec![
                Event::PointerMoved(egui::pos2(700.0, 400.0)),
                Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(0.0, dy),
                    modifiers: egui::Modifiers::NONE,
                    phase: egui::TouchPhase::Move,
                },
            ]
        });
    }

    /// Wheel-scroll whatever is under the middle of the window (negative = down).
    fn scroll_by(&mut self, dy: f32) {
        for _ in 0..10 {
            self.harness
                .event(Event::PointerMoved(egui::pos2(700.0, 400.0)));
            self.harness.event(Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, dy / 10.0),
                modifiers: egui::Modifiers::NONE,
                phase: egui::TouchPhase::Move,
            });
            self.harness.run_steps(1);
        }
        self.harness.run_steps(20);
    }

    /// Step frames until the git worker delivers the diff titled `title`.
    /// Drive go to definition (F12) and the Sublime editing commands with real key events.
    fn check_editor_commands(&mut self) {
        let key = |key, modifiers| Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        };
        let tests = std::fs::canonicalize(self.project.join("test_stats.py")).unwrap();
        let stats = std::fs::canonicalize(self.project.join("stats.py")).unwrap();
        {
            let app = self.app();
            app.open_editor_file(tests.clone());
            let content = &app.conv.editor.active_document().unwrap().content;
            let at = content.find("mean([1").unwrap() + 1;
            app.conv.editor.navigation_target = Some((tests.clone(), at..at));
            app.conv.editor.focus_editor_next_frame = true;
        }
        self.harness.run_steps(3);
        self.harness
            .event(key(egui::Key::F12, egui::Modifiers::NONE));
        let mut landed = false;
        for _ in 0..200 {
            self.harness.step();
            if self
                .app()
                .conv
                .editor
                .active_document()
                .is_some_and(|document| document.path == stats)
            {
                landed = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        self.harness.run_steps(3);
        println!("CHECK F12 from an import to the defining file: {landed}");
        assert!(landed, "go to definition did not open stats.py");

        // The caret now sits on `def mean`: Cmd+/ comments that line, Cmd+Z restores it.
        let saved = self
            .app()
            .conv
            .editor
            .active_document()
            .unwrap()
            .content
            .clone();
        self.harness
            .event(key(egui::Key::Slash, egui::Modifiers::COMMAND));
        self.harness.run_steps(2);
        let commented = self
            .app()
            .conv
            .editor
            .active_document()
            .unwrap()
            .content
            .clone();
        let ok = commented.contains("# def mean(values):");
        println!("CHECK Cmd+/ comments the caret line: {ok}");
        assert!(ok, "toggle comment failed");
        self.harness
            .event(key(egui::Key::Z, egui::Modifiers::COMMAND));
        self.harness.run_steps(2);
        let restored = self.app().conv.editor.active_document().unwrap().content == saved;
        println!("CHECK Cmd+Z undoes the command: {restored}");
        assert!(restored, "undo after toggle comment failed");
        self.harness.event(key(
            egui::Key::D,
            egui::Modifiers::COMMAND.plus(egui::Modifiers::SHIFT),
        ));
        self.harness.run_steps(2);
        // F12 selected the name `mean`; like Sublime, a selection is duplicated in place.
        let duplicated = self
            .app()
            .conv
            .editor
            .active_document()
            .unwrap()
            .content
            .contains("def meanmean(values):");
        println!("CHECK Cmd+Shift+D duplicates the selection: {duplicated}");

        assert!(duplicated, "duplicate failed");
        self.harness
            .event(key(egui::Key::Z, egui::Modifiers::COMMAND));
        self.harness.run_steps(2);
        let app = self.app();
        let path = app.conv.editor.active_document().unwrap().path.clone();
        if let Some(index) = app
            .conv
            .editor
            .documents
            .iter()
            .position(|d| d.path == tests)
        {
            app.conv.editor.remove_document(index);
        }
        if let Some(index) = app
            .conv
            .editor
            .documents
            .iter()
            .position(|d| d.path == path)
        {
            app.conv.editor.active = Some(index);
        }
        self.harness.run_steps(2);
    }

    /// Wait until the active document's diff has read its base and laid out its changes.
    fn wait_for_diff_editor(&mut self) {
        for _ in 0..400 {
            self.harness.step();
            let ready = self
                .app()
                .conv
                .editor
                .active_document()
                .and_then(|document| document.diff.as_ref())
                .is_some_and(|diff| diff.ready());
            if ready {
                self.harness.run_steps(6);
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the diff editor never loaded its base");
    }

    /// A point inside the first line of change `index` on the diff's new side.
    fn diff_change_point(&mut self, index: usize) -> egui::Pos2 {
        let diff = self
            .app()
            .conv
            .editor
            .active_document()
            .and_then(|document| document.diff.as_ref())
            .expect("diff mode");
        let (top, _) = diff.frame_new_span(index).expect("change on screen");
        let clip = diff.frame_new_clip();
        egui::pos2(clip.left() + 120.0, top + 6.0)
    }

    fn wait_for_git_diff(&mut self, title: &str) {
        for _ in 0..400 {
            self.harness.step();
            let ready = self
                .app()
                .conv
                .git
                .diff
                .as_ref()
                .is_some_and(|(t, _)| t == title);
            if ready {
                self.harness.run_steps(4);
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("git diff {title:?} never arrived");
    }

    fn hold(&mut self, seconds: f32) {
        for _ in 0..(seconds * FPS as f32).round() as usize {
            self.shot();
        }
    }

    fn type_text(&mut self, text: &str, chars_per_frame: usize) {
        let chars: Vec<char> = text.chars().collect();
        for chunk in chars.chunks(chars_per_frame) {
            self.harness
                .event(Event::Text(chunk.iter().collect::<String>()));
            self.shot();
        }
    }

    /// What `send_message` does, minus starting a real agent: the events come from us.
    fn begin_turn(&mut self, text: &str) {
        let (tx, rx) = mpsc::channel();
        let app = self.app();
        let key = app.active_session_key();
        app.active_session_mut().title = crate::model::make_session_title(text);
        app.conv.composer.input.clear();
        app.conv.transcript.scroll_to_bottom_once = true;
        {
            let run = app.run_state_mut(key);
            run.begin_waiting_response();
            run.stream_error = None;
            run.last_user_prompt = Some(text.to_owned());
        }
        app.materialize_prompt(key, text, &[]);
        app.run_state_mut(key).agent_rx = Some(rx);
        let _ = tx.send(AgentEvent::AgentStart);
        self.tx = Some(tx);
    }

    fn send(&self, event: AgentEvent) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(event);
        }
    }

    fn stream_thinking(&mut self, text: &str) {
        for chunk in chunks(text, 7) {
            self.send(AgentEvent::ThinkingDelta(chunk));
            self.shot();
        }
        self.hold(0.3);
    }

    fn stream_text(&mut self, text: &str) {
        self.send(AgentEvent::TextStart);
        for chunk in chunks(text, 5) {
            self.send(AgentEvent::TextDelta(chunk));
            self.shot();
        }
        self.hold(0.3);
    }

    fn tool(&mut self, id: &str, name: &str, args: Value, running_seconds: f32) {
        self.send(AgentEvent::ToolStart {
            name: name.to_owned(),
            tool_call_id: id.to_owned(),
            args: Some(args.clone()),
        });
        self.hold(running_seconds);
        let result = run_tool(&self.project, name, &args, &self.tool_env);
        self.send(AgentEvent::ToolOutput {
            tool_call_id: id.to_owned(),
            text: result.output,
            truncated: false,
        });
        self.send(AgentEvent::ToolEnd {
            tool_call_id: id.to_owned(),
            is_error: Some(result.is_error),
            full_output_path: None,
            diff: result.diff,
        });
        self.hold(0.4);
    }

    fn finish_turn(&mut self) {
        let mut usage = TokenUsage {
            input_tokens: 3_412,
            output_tokens: 186,
            ..Default::default()
        };
        usage.record_generation(Duration::from_millis(4_300));
        self.send(AgentEvent::Usage(usage));
        self.send(AgentEvent::AssistantMessageDone);
        self.send(AgentEvent::Finished(AgentOutcome::Success {
            wire_cache: None,
        }));
        self.tx = None;
    }
}

/// `turns` user/assistant pairs mixing prose, lists, tables, code blocks and tool calls.
fn heavy_transcript(turns: usize) -> Vec<ChatMessage> {
    let mut messages = Vec::with_capacity(turns * 2);
    for turn in 0..turns {
        messages.push(message(
            MsgRole::User,
            &format!("Question {turn}: how does the reporting pipeline handle retries?"),
            vec![],
        ));
        let answer = format!(
            "## Step {turn}\n\nThe pipeline **retries** failed uploads with `backoff()`; see \
             [the docs](https://example.com). It keeps a queue per target:\n\n\
             - first item with `inline code`\n- second item\n- third item\n\n\
             | column | value |\n|---|---|\n| retries | {turn} |\n| delay | 2s |\n\n\
             ```rust\nfn backoff(attempt: u32) -> Duration {{\n    let base = 250;\n    \
             Duration::from_millis(base * 2u64.pow(attempt))\n}}\n```\n\n\
             That is the whole story for turn {turn}."
        );
        messages.push(message(
            MsgRole::Assistant,
            "",
            vec![
                AssistantBlock::Thinking("Let me look at how retries are scheduled.".into()),
                AssistantBlock::Tool {
                    tool_call_id: format!("heavy_{turn}"),
                    name: "read".into(),
                    args_summary: Some(r#"{"path":"src/pipeline/retry.rs"}"#.into()),
                    output: "line\n".repeat(40),
                    diff: None,
                    is_error: Some(false),
                    full_output_path: None,
                    output_truncated: false,
                    metadata: None,
                },
                AssistantBlock::Answer(answer),
            ],
        ));
    }
    messages
}

fn chunks(text: &str, size: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    chars.chunks(size).map(|c| c.iter().collect()).collect()
}

/// The window controls macOS draws over the unified title bar (offscreen frames lack them).
fn paint_traffic_lights(image: &mut image::RgbaImage) {
    let scale = SCALE;
    let (cy, radius) = (16.0 * scale, 6.0 * scale);
    let colors = [[255, 95, 87], [254, 188, 46], [40, 200, 64]];
    for (i, color) in colors.iter().enumerate() {
        let cx = (16.0 + 22.0 * i as f32) * scale;
        let (x0, x1) = ((cx - radius - 1.0) as u32, (cx + radius + 1.0) as u32);
        let (y0, y1) = ((cy - radius - 1.0) as u32, (cy + radius + 1.0) as u32);
        for y in y0..=y1 {
            for x in x0..=x1 {
                let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
                let coverage = (radius + 0.5 - d).clamp(0.0, 1.0);
                if coverage > 0.0 {
                    let px = image.get_pixel_mut(x, y);
                    for c in 0..3 {
                        px[c] =
                            (px[c] as f32 * (1.0 - coverage) + color[c] as f32 * coverage) as u8;
                    }
                }
            }
        }
    }
}

fn create_project(project: &Path) {
    std::fs::create_dir_all(project).unwrap();
    std::fs::write(project.join("stats.py"), STATS_PY).unwrap();
    std::fs::write(project.join("test_stats.py"), TEST_STATS_PY).unwrap();
    std::fs::write(project.join(".gitignore"), "__pycache__/\n").unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["add", "."],
        &[
            "-c",
            "user.name=oxi",
            "-c",
            "user.email=oxi@example.invalid",
            "commit",
            "-q",
            "-m",
            "Add statistics helpers",
        ],
    ] {
        let status = Command::new("git")
            .args(args)
            .current_dir(project)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }
}

fn seed_settings() {
    let mut settings = crate::settings::AppSettings::load();
    settings.theme_id = "mariana".into();
    settings.active_provider = LlmProviderKind::LocalHf;
    settings.provider_mut(LlmProviderKind::LocalHf).model_id = MODEL.into();
    settings.save().unwrap();
}

/// A few earlier chats so the sidebar looks lived in, dated over the past days.
fn seed_chats(project: &Path) {
    let root = project.to_string_lossy().to_string();
    let chats = [
        ("Add CSV export to the weekly report", "202609221640"),
        ("Why is test_percentile flaky on CI?", "202609211015"),
        (
            "Explain how the reporting pipeline is structured",
            "202609180930",
        ),
        ("Rename the stats helpers to snake_case", "202609151120"),
    ];
    for (prompt, stamp) in chats {
        let mut session = OxiApp::blank_session(prompt);
        session.messages = vec![
            message(MsgRole::User, prompt, vec![]),
            message(
                MsgRole::Assistant,
                "",
                vec![AssistantBlock::Answer("Done.".into())],
            ),
        ];
        crate::session_store::save_session_messages(&root, &mut session).unwrap();
        if let Some(file) = &session.session_file {
            let _ = Command::new("touch").args(["-t", stamp, file]).status();
        }
    }
}

fn message(role: MsgRole, text: &str, blocks: Vec<AssistantBlock>) -> ChatMessage {
    ChatMessage {
        role,
        text: text.into(),
        is_summary: false,
        attachments: vec![],
        blocks,
        streaming: false,
        started_at: None,
        worked_duration: None,
        route: None,
        changes: None,
    }
}

/// Show the guided Local HF setup as finished: runtime installed, one model running.
fn seed_local_model(app: &mut OxiApp) {
    let id = MODEL.to_owned();
    app.conv.local_models.runtime_path = "/usr/bin/true".into();
    app.conv.local_models.downloaded = vec![crate::local_models::DownloadedModel {
        id: id.clone(),
        repo: "Qwen/Qwen2.5-Coder-7B-Instruct-GGUF".into(),
        filename: id.clone(),
        path: String::new(),
        bytes: 4_683_074_240,
    }];
    app.conv.local_models.running_model_id = Some(id);
}

/// A focused GPU review using the demo's isolated settings and workspace.
fn review_acp_ui(rec: &mut Recorder<'_>) {
    let first_root = rec.project.to_string_lossy().into_owned();
    let second_root = rec.project.join("second-workspace");
    std::fs::create_dir_all(&second_root).unwrap();
    {
        let app = rec.app();
        app.set_active_session_provider(LlmProviderKind::ClaudeCodeAcp);
        app.set_active_session_model("workspace-one-model".into());
        let key = app.active_session_key();
        app.run_state_mut(key).plan_mode = true;
        app.save_settings_quietly();
        app.conv.workspaces.push(super::Workspace {
            root_path: second_root.to_string_lossy().into_owned(),
            sessions: vec![OxiApp::blank_session("Second workspace")],
            active: 0,
            sidebar_folded: false,
            pinned: vec![],
            folded_groups: vec![],
            worktree: None,
        });
        let second = app.conv.workspaces.len() - 1;
        app.select_workspace(second);
        app.set_active_session_model("workspace-two-model".into());
        let key = app.active_session_key();
        app.run_state_mut(key).plan_mode = false;
        app.save_settings_quietly();
        app.select_workspace(0);
        assert_eq!(
            app.conv.settings.active_config().model_id,
            "workspace-one-model"
        );
        assert!(app.run_state(app.active_session_key()).unwrap().plan_mode);
        app.new_chat();
        assert!(app.run_state(app.active_session_key()).unwrap().plan_mode);
        assert_eq!(
            app.conv.settings.acp_workspace_preferences[&first_root]
                .config
                .model_id,
            "workspace-one-model"
        );
    }
    let mut command =
        portable_pty::CommandBuilder::new(if cfg!(windows) { "cmd.exe" } else { "/bin/sh" });
    if cfg!(windows) {
        command.args(["/C", "echo ACP terminal ready"]);
    } else {
        command.args([
            "-c",
            "printf 'ACP terminal ready\nLive output is shown here.\n'",
        ]);
    }
    let (terminal, _process) =
        crate::terminal::TerminalSession::spawn_command(&rec.harness.ctx, command, 1024).unwrap();
    let (tx, rx) = mpsc::channel();
    tx.send(AgentEvent::AcpTerminal(crate::terminal::PendingTerminal(
        std::sync::Arc::new(std::sync::Mutex::new(Some(terminal))),
    )))
    .unwrap();
    {
        let app = rec.app();
        let key = app.active_session_key();
        app.run_state_mut(key).agent_rx = Some(rx);
        app.conv.sidebar.open = false;
    }
    rec.harness.run_steps(5);
    assert!(rec.app().conv.terminal_panel.open);
    assert_eq!(rec.app().terminals.len(), 1);
    for scale in [1.0, 1.25, 1.5] {
        rec.harness.set_pixels_per_point(scale);
        for width in [760.0, 420.0] {
            rec.harness.set_size(egui::vec2(width, 620.0));
            rec.harness.run_steps(3);
            rec.still(&format!("acp-terminal-{width}-{scale}"));
        }
    }
}

/// Real sidebar rendering with linked checkouts, in the isolated gallery process.
fn review_worktree_sidebar(rec: &mut Recorder) {
    use super::state::Workspace;
    let root = rec.project.to_string_lossy().into_owned();
    for branch in [
        "oxi/fix-sidebar",
        "oxi/a-long-feature-branch-name-for-truncation",
    ] {
        rec.app().conv.workspaces.push(Workspace {
            root_path: format!("{root}-{branch}"),
            sessions: Vec::new(),
            active: 0,
            sidebar_folded: false,
            pinned: Vec::new(),
            folded_groups: Vec::new(),
            worktree: Some(crate::git::worktree::WorktreeInfo {
                branch: branch.into(),
                main_root: root.clone().into(),
                main_branch: "dev".into(),
            }),
        });
    }
    for scale in [1.0, 1.25, 1.5] {
        rec.harness.set_pixels_per_point(scale);
        for width in [180.0, 250.0] {
            rec.app().conv.sidebar.width = width;
            rec.app().conv.workspaces[0].sidebar_folded = false;
            rec.harness.run_steps(4);
            rec.still(&format!("worktrees-{width}-{scale}"));
            rec.app().conv.workspaces[0].sidebar_folded = true;
            rec.harness.run_steps(3);
            rec.still(&format!("worktrees-folded-{width}-{scale}"));
        }
    }
}

/// Composer pickers fed by an ACP agent's config options, their searchable lists, and a user
/// turn with many images.
fn review_composer_pickers(rec: &mut Recorder<'_>) {
    use crate::model::{ChatMessage, MsgRole, UserAttachment};
    let select = |id: &str, name: &str, category: &str, current: &str, values: &[(&str, &str)]| {
        serde_json::json!({
            "id": id, "name": name, "category": category, "type": "select",
            "currentValue": current,
            "options": values.iter().map(|(v, n)| serde_json::json!({"value": v, "name": n, "description": format!("{n} description")})).collect::<Vec<_>>(),
        })
    };
    let options = serde_json::json!([
        select("mode", "Mode", "mode", "auto", &[("default", "Manual"), ("acceptEdits", "Accept edits"), ("plan", "Plan"), ("auto", "Auto"), ("bypassPermissions", "Bypass permissions")]),
        select("model", "Model", "model", "opus", &[("default", "Default (recommended)"), ("opus", "Opus 5.5"), ("sonnet", "Sonnet 5.5"), ("fable", "Fable 5.1"), ("haiku", "Haiku 4.5"), ("sonnet5", "Sonnet 5"), ("opus5", "Opus 5"), ("fable5", "Fable 5"), ("opus48", "Opus 4.8"), ("opus47", "Opus 4.7"), ("opus46", "Opus 4.6"), ("sonnet46", "Sonnet 4.6")]),
        select("effort", "Effort", "thought_level", "high", &[("default", "Default"), ("low", "Low"), ("medium", "Medium"), ("high", "High"), ("xhigh", "Xhigh"), ("max", "Max")]),
        {"id": "fast", "name": "Fast mode", "category": "model_config", "type": "boolean", "currentValue": false, "description": "Faster responses on supported models"},
    ]);
    {
        let app = rec.app();
        app.set_active_session_provider(LlmProviderKind::ClaudeCodeAcp);
        let key = app.active_session_key();
        let session_key = app.acp_session_key(key);
        let command = app.conv.settings.active_config().effective_acp_command();
        crate::agent::acp::config_options::publish(&session_key, &command, &options);
    }
    rec.harness.run_steps(4);
    rec.still("pickers-row");
    rec.harness.set_size(egui::vec2(1000.0, 620.0));
    rec.harness.run_steps(4);
    rec.still("pickers-row-narrow");
    rec.harness.set_size(SIZE);
    rec.harness.run_steps(2);
    let popup = |salt: (&str, &str)| egui::Id::new(("select_menu", salt)).with("popup");
    for (name, id) in [("mode", "mode"), ("model", "model"), ("effort", "effort")] {
        egui::Popup::open_id(&rec.harness.ctx, popup(("agent_option", id)));
        rec.harness.run_steps(3);
        rec.still(&format!("pickers-{name}-open"));
        egui::Popup::close_id(&rec.harness.ctx, popup(("agent_option", id)));
        rec.harness.run_steps(2);
    }
    egui::Popup::open_id(&rec.harness.ctx, popup(("agent_option", "model")));
    rec.harness.run_steps(2);
    for c in "son".chars() {
        rec.harness.event(egui::Event::Text(c.to_string()));
        rec.harness.run_steps(1);
    }
    rec.harness.run_steps(2);
    rec.still("pickers-model-search");
    egui::Popup::close_id(&rec.harness.ctx, popup(("agent_option", "model")));

    let png = |w: u32, h: u32, shade: u8| {
        let img = image::RgbaImage::from_fn(w, h, |x, y| {
            image::Rgba([shade, (x * 255 / w) as u8, (y * 255 / h) as u8, 255])
        });
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        bytes
    };
    let attachments = (0..14)
        .map(|i| UserAttachment::Image {
            mime: "image/png".into(),
            data: png(if i % 3 == 0 { 1600 } else { 900 }, 700, (i * 17) as u8),
        })
        .collect();
    {
        let app = rec.app();
        let key = app.active_session_key();
        let session = app.session_mut_by_key(key);
        session.messages.push(ChatMessage {
            role: MsgRole::User,
            text: "Here are all the screenshots".into(),
            is_summary: false,
            attachments,
            blocks: Vec::new(),
            streaming: false,
            started_at: None,
            worked_duration: None,
            route: None,
            changes: None,
        });
        app.conv.composer.pending_images = (0..12)
            .map(|i| ("image/png".to_string(), png(800, 600, (i * 20) as u8)))
            .collect();
    }
    rec.harness.run_steps(6);
    rec.still("chat-many-images");
}

/// Compare the actual app's GPU pixels on the input frame and the following idle frame.
/// Run with OXI_COMPOSER_REVIEW=1 OXI_GALLERY=/tmp/oxi-composer cargo test
/// --locked --bin oxi render_gallery -- --ignored --nocapture.
fn review_composer_typing(rec: &mut Recorder<'_>) {
    let input_id = egui::Id::new("composer_input");
    for theme in ["dark", "light"] {
        rec.app().conv.settings.theme_id = theme.into();
        crate::theme::apply_theme(&rec.harness.ctx, theme);
        rec.harness
            .ctx
            .all_styles_mut(|s| s.visuals.text_cursor.blink = false);
        for scale in [1.0, 1.25, 1.5, 2.0] {
            rec.app().conv.composer.input.clear();
            rec.harness.set_pixels_per_point(scale);
            rec.harness.ctx.memory_mut(|m| m.request_focus(input_id));
            rec.harness.run_steps(4);
            for (i, ch) in "hello ăîșț\n世界".chars().enumerate() {
                if ch == '\n' {
                    rec.harness.input_mut().modifiers = egui::Modifiers::SHIFT;
                    rec.harness.event(Event::Key {
                        key: egui::Key::Enter,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::SHIFT,
                    });
                } else {
                    rec.harness.event(Event::Text(ch.to_string()));
                }
                rec.harness.step();
                rec.harness.input_mut().modifiers = egui::Modifiers::NONE;
                let input = &rec.harness.state().conv.composer.input;
                let text = rec
                    .harness
                    .output()
                    .shapes
                    .iter()
                    .find_map(|s| match &s.shape {
                        egui::Shape::Text(t) if t.galley.job.text == *input => Some(t),
                        _ => None,
                    })
                    .expect("composer text shape");
                let rect = egui::Rect::from_min_size(text.pos, text.galley.size());
                let typed = rec.harness.render().unwrap();
                rec.harness.step();
                let idle = rec.harness.render().unwrap();
                let x = (rect.left() * scale).ceil() as u32;
                let y = (rect.top() * scale).ceil() as u32;
                let w = (rect.width() * scale).floor() as u32;
                let h = (rect.height() * scale).floor() as u32;
                let typed = image::imageops::crop_imm(&typed, x, y, w, h).to_image();
                let idle = image::imageops::crop_imm(&idle, x, y, w, h).to_image();
                let changed = typed
                    .pixels()
                    .zip(idle.pixels())
                    .filter(|(a, b)| a != b)
                    .count();
                if let Some(dir) = &rec.stills {
                    typed
                        .save(dir.join(format!("composer-{theme}-{scale}-{i}-typed.png")))
                        .unwrap();
                    idle.save(dir.join(format!("composer-{theme}-{scale}-{i}-idle.png")))
                        .unwrap();
                }
                assert_eq!(
                    changed, 0,
                    "{theme}, scale {scale}, character {ch:?}: composer text changes brightness after input"
                );
            }
            println!("COMPOSER GPU stable: {theme}, scale {scale}");
        }
    }
}
