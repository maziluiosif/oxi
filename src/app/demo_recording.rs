//! Scripted product demo, rendered offscreen from the real UI.
//!
//! `scripts/render-demo.sh` runs the ignored `record_demo` test, a guided tour composed on a
//! stage (see `stage`), and turns it into the website video and the README GIF. The agent's
//! turns are scripted (no model is called), but their tools run for real against a generated
//! sample project, so the test output and the diffs are genuine.
//!
//! Isolation: the process uses a throwaway `HOME` and an in-memory credential store, so it
//! never touches the user's settings, chats or keychain.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Event};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use serde_json::{Value, json};

use super::OxiApp;
use crate::agent::tools::{ToolEnv, run_tool};
use crate::agent::{AgentEvent, AgentOutcome, TokenUsage};
use crate::model::{AssistantBlock, MsgRole};
use crate::settings::LlmProviderKind;

mod fixtures;
mod stage;
mod switch;

use fixtures::{
    chunks, create_project, heavy_transcript, message, paint_traffic_lights, seed_chats,
    seed_local_model, seed_settings,
};

const FPS: u64 = 30;
const FRAME: Duration = Duration::from_millis(1000 / FPS);
const SIZE: egui::Vec2 = egui::vec2(1240.0, 780.0);
const SCALE: f32 = 2.0;
const MODEL: &str = "qwen2.5-coder-7b-instruct-q4_k_m.gguf";

#[test]
#[ignore = "renders the website demo; run through scripts/render-demo.sh"]
fn record_demo() {
    let video = PathBuf::from(std::env::var("OXI_DEMO_VIDEO").expect("OXI_DEMO_VIDEO"));
    let stills = std::env::var_os("OXI_DEMO_STILLS").map(PathBuf::from);
    let mut rec = setup(stills, Some(&video), TOUR_CHAPTERS.to_vec());
    tour(&mut rec);
    if let Some(stage) = rec.stage.take() {
        stage.finish();
    }
}

/// Every key screen as a still, without the video frames or real-time pacing. Used to review UI
/// changes: `OXI_GALLERY=/some/dir cargo test --release render_gallery -- --ignored`.
#[test]
#[ignore = "renders UI review stills; set OXI_GALLERY to the output folder"]
fn render_gallery() {
    let stills = PathBuf::from(std::env::var("OXI_GALLERY").expect("OXI_GALLERY"));
    let mut rec = setup(Some(stills), None, Vec::new());
    run_gallery(&mut rec);
}

/// A fresh sample project, throwaway home and seeded app. With `video`, frames are composed on
/// the stage (with `chapters` on its rail) and encoded there, paced to `FPS`.
fn setup(
    stills: Option<PathBuf>,
    video: Option<&Path>,
    chapters: Vec<&'static str>,
) -> Recorder<'static> {
    let scratch = std::env::temp_dir().join(format!("oxi-demo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    let home = scratch.join("home");
    let project = scratch.join("Projects").join("stats-kit");
    std::fs::create_dir_all(&home).unwrap();
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

    let mut builder = Harness::builder()
        .with_size(SIZE)
        .with_pixels_per_point(SCALE);
    if video.is_some() {
        // Real-time animations: one step per video frame.
        builder = builder.with_step_dt(1.0 / FPS as f32);
    }
    let harness = builder
        .renderer(NoCursor(egui_kittest::wgpu::WgpuTestRenderer::new()))
        .build_eframe(|cc| {
            let mut app = OxiApp::new();
            crate::theme::set_active_fonts(crate::theme::FontSelection {
                ui: app.conv.settings.ui_font.clone(),
                mono: app.conv.settings.mono_font.clone(),
            });
            crate::theme::apply_theme(&cc.egui_ctx, &app.conv.settings.theme_id);
            egui_extras::install_image_loaders(&cc.egui_ctx);
            seed_local_model(&mut app);
            app.conv.sidebar.width = 250.0;
            app.new_chat();
            app
        });
    Recorder {
        harness,
        stage: video.map(|video| stage::Stage::new(video, FPS, chapters)),
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
            evidence: None,
        },
    }
}

fn run_gallery(rec: &mut Recorder<'_>) {
    let project = rec.project.clone();
    let rec = &mut *rec;
    if std::env::var_os("OXI_RESIZE_REVIEW").is_some() {
        review_live_resize(rec);
        return;
    }
    if std::env::var_os("OXI_POLISH_REVIEW").is_some() {
        review_ui_polish(rec);
        return;
    }
    if std::env::var_os("OXI_SPLIT_REVIEW").is_some() {
        review_split_view(rec);
        return;
    }
    if std::env::var_os("OXI_WORKTREE_REVIEW").is_some() {
        review_worktree_sidebar(rec);
        return;
    }

    if std::env::var_os("OXI_CHAT_NAV_REVIEW").is_some() {
        review_chat_navigation(rec);
        return;
    }

    if std::env::var_os("OXI_PICKER_REVIEW").is_some() {
        review_composer_pickers(rec);
        return;
    }

    if std::env::var_os("OXI_COMPOSER_REVIEW").is_some() {
        review_composer_typing(rec);
        return;
    }

    if std::env::var_os("OXI_SUBAGENT_REVIEW").is_some() {
        review_subagent_chats(rec);
        return;
    }
    if std::env::var_os("OXI_ACP_REVIEW").is_some() {
        review_acp_ui(rec);
        return;
    }

    if std::env::var_os("OXI_STREAM_REVIEW").is_some() {
        review_streaming_ui(rec);
        return;
    }

    if std::env::var_os("OXI_EVIDENCE_REVIEW").is_some() {
        review_evidence_ui(rec);
        return;
    }

    if std::env::var_os("OXI_SELECTION_REVIEW").is_some() {
        review_selection_flicker(rec);
        return;
    }

    if let Ok(root) = std::env::var("OXI_REMOTE_REVIEW") {
        review_remote_workspace(rec, &root);
        return;
    }

    let gallery = true;
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
            (SettingsTab::Editor, "settings-editor"),
            (SettingsTab::Appearance, "settings-appearance"),
            (SettingsTab::Server, "settings-server"),
            (SettingsTab::About, "settings-about"),
        ] {
            rec.app().conv.settings_page.tab = tab;
            rec.harness.run_steps(3);
            if tab == SettingsTab::Server {
                // Phone access on (in the throwaway config folder); the status comes from a worker.
                crate::headless::phone::save(&crate::headless::phone::PhoneConfig {
                    port: crate::headless::phone::DEFAULT_PORT,
                    token: "0123456789abcdef0123456789abcdef".into(),
                    lan: false,
                })
                .unwrap();
                rec.app().conv.settings_page.server = Default::default();
                for _ in 0..40 {
                    rec.harness.run_steps(1);
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
            rec.still(name);
            if matches!(
                tab,
                SettingsTab::Agent | SettingsTab::Editor | SettingsTab::Prompts
            ) {
                rec.scroll_by(-700.0);
                rec.still(&format!("{name}-more"));
            }
        }
        // Settings search: the result list, then a hit's card scrolled to and flashing.
        rec.app().conv.settings_page.search = "awake".into();
        rec.harness.run_steps(3);
        rec.still("settings-search");
        {
            let app = rec.app();
            app.conv.settings_page.search.clear();
            app.conv.settings_page.tab = SettingsTab::Agent;
        }
        crate::ui::chrome::request_settings_reveal(&rec.harness.ctx, "Approvals");
        rec.harness.run_steps(4);
        rec.still("settings-search-reveal");
        // A pending change shows the save bar.
        rec.app().conv.settings.editor.minimap = !rec.app().conv.settings.editor.minimap;
        rec.app().conv.settings_page.tab = SettingsTab::Editor;
        rec.harness.run_steps(3);
        rec.still("settings-unsaved");
        // A long save error truncates before the buttons instead of running under them.
        rec.app().conv.settings_page.save_error = Some(
            "Could not save settings: Could not save credentials to the OS keychain: access to \
             this item is restricted by the system policy for this application"
                .into(),
        );
        rec.harness.run_steps(3);
        rec.still("settings-save-error");
        rec.app().conv.settings_page.save_error = None;
        // Search into a provider form: "API key" opens the form and flashes Connection.
        rec.app().conv.settings_page.search = "api key".into();
        rec.harness.run_steps(3);
        rec.still("settings-search-provider");
        rec.app().conv.settings_page.search.clear();
        rec.app().conv.settings.editor.minimap = !rec.app().conv.settings.editor.minimap;
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
        // OXI_SETTINGS_REVIEW=1: only the Settings stills above.
        if std::env::var_os("OXI_SETTINGS_REVIEW").is_some() {
            return;
        }
    }
    {
        let app = rec.app();
        app.open_settings_page();
        app.conv.settings_page.provider_tab = LlmProviderKind::LocalHf;
    }
    rec.hold(2.6);
    rec.still("local-models");
    rec.framed_still("local-models");
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
        interrupted_providers: Vec::new(),
            direct: false,
        })));
        rec.harness.run_steps(4);
        rec.still("chat-routed");
    }
    rec.hold(0.5);

    // 3. The agent works through it with real tools.
    fix_turn_tools(rec);
    rec.still("agent-run");
    rec.framed_still("agent");
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
    for theme in ["mariana", "sublime", "midnight", "zed", "light", "dark"] {
        crate::theme::apply_theme(&rec.harness.ctx, theme);
        rec.harness.run_steps(3);
        rec.still(&format!("theme-{theme}"));
    }
    let theme = rec.app().conv.settings.theme_id.clone();
    crate::theme::apply_theme(&rec.harness.ctx, &theme);
    rec.harness.run_steps(3);
    fix_turn_finish(rec);
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
        // Claude Code implements, Codex reviews in the same chat: the handoff line.
        {
            let app = rec.app();
            let mut claude = message(
                MsgRole::Assistant,
                "",
                vec![AssistantBlock::Answer(
                    "Added retry with backoff to `fetch`.".into(),
                )],
            );
            claude.route = Some(Box::new(crate::model::RouteNote::direct(
                LlmProviderKind::ClaudeCodeAcp,
                "opus",
                "high",
            )));
            let mut codex = message(
                MsgRole::Assistant,
                "",
                vec![AssistantBlock::Answer(
                    "Reviewed it: the jitter can overflow for large attempts.".into(),
                )],
            );
            codex.route = Some(Box::new(crate::model::RouteNote::direct(
                LlmProviderKind::CodexAcp,
                "gpt-5.5",
                "high",
            )));
            app.active_session_mut().messages = vec![
                message(MsgRole::User, "Add retry with backoff to fetch", vec![]),
                claude,
                message(MsgRole::User, "Review what Claude did", vec![]),
                codex,
            ];
        }
        rec.harness.run_steps(4);
        rec.still("chat-handoff");

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
        rec.profile_resize("heavy resize");
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
        // Sublime-style editor preferences: bigger font, caret line, white space, no minimap.
        {
            let prefs = &mut rec.app().conv.settings.editor;
            prefs.font_size = 15.0;
            prefs.highlight_line = true;
            prefs.draw_white_space = crate::settings::DrawWhiteSpace::All;
            prefs.minimap = false;
        }
        rec.app().conv.editor.focus_editor_next_frame = true;
        rec.harness.run_steps(4);
        rec.still("editor-prefs");
        rec.app().conv.settings.editor = crate::settings::EditorSettings::default();
        rec.harness.run_steps(2);
        rec.check_sublime_keys();
        // Scroll past end: the last line can reach the top of the view.
        rec.scroll_by(-4000.0);
        rec.still("editor-scroll-past-end");
        rec.scroll_by(4000.0);
        // Find in Files, then Find References on `median` from stats.py.
        rec.app().show_project_search();
        rec.app().conv.project_search.query = "median".into();
        rec.app().conv.project_search.replace_open = true;
        for _ in 0..20 {
            rec.harness.run_steps(2);
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        rec.still("search-in-files");
        {
            let app = rec.app();
            let content = app.conv.editor.active_document().unwrap().content.clone();
            let byte = content.find("def median").unwrap() + 5;
            app.conv.editor.navigation_cursor_char = content[..byte].chars().count();
        }
        let ctx = rec.harness.ctx.clone();
        rec.app().find_references_at_caret(&ctx);
        for _ in 0..20 {
            rec.harness.run_steps(2);
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        rec.still("search-references");
        rec.app().conv.sidebar.mode = super::state::SidebarMode::Explorer;
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
        rec.framed_still("plan");
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

/// Compare the presented resize frame with the next frame, while a long answer is live.
fn review_live_resize(rec: &mut Recorder<'_>) {
    const MARKER: &str = "The live resize marker stays above the composer.";
    let tail_rect = |rec: &Recorder<'_>| {
        rec.harness
            .output()
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text.contains(MARKER) => {
                    Some(egui::Rect::from_min_size(text.pos, text.galley.size()))
                }
                _ => None,
            })
            .expect("live answer tail")
    };
    for theme in ["dark", "light"] {
        rec.app().conv.settings.theme_id = theme.into();
        crate::theme::apply_theme(&rec.harness.ctx, theme);
        rec.harness
            .ctx
            .all_styles_mut(|style| style.visuals.text_cursor.blink = false);
        for scale in [1.0, 1.25, 2.0] {
            rec.harness.set_pixels_per_point(scale);
            {
                let app = rec.app();
                app.conv.settings.dictation.enabled = true;
                app.conv.composer.input =
                    "A draft that wraps as the window gets narrower. ".repeat(3);
                app.active_session_mut().messages = heavy_transcript(100);
                let mut answer = message(MsgRole::Assistant, "", vec![AssistantBlock::Answer(
                    format!("{}\n\n{MARKER}", "I am reviewing the implementation and checking the behaviour during resizing.\n\n".repeat(30))
                )]);
                answer.streaming = true;
                app.active_session_mut().messages.push(answer);
                let key = app.active_session_key();
                app.run_state_mut(key).waiting_response = true;
                app.conv.transcript.scroll_to_bottom_once = true;
            }
            rec.harness.run_steps(6);
            for width in [
                1240.0, 1000.0, 850.0, 760.0, 680.0, 600.0, 520.0, 480.0, 520.0, 680.0, 850.0,
                1000.0, 1240.0,
            ] {
                if let Some(AssistantBlock::Answer(text)) = rec
                    .app()
                    .active_session_mut()
                    .messages
                    .last_mut()
                    .and_then(|message| message.blocks.last_mut())
                {
                    text.push_str(" Still working.");
                }
                rec.harness.set_size(egui::vec2(width, 680.0));
                rec.harness.step();
                let presented = tail_rect(rec);
                let input = rec
                    .harness
                    .ctx
                    .read_response(egui::Id::new("composer_input"))
                    .unwrap()
                    .rect;
                let resized = rec.harness.render().unwrap();
                let provider = rec
                    .label_center("Local HF")
                    .expect("provider remains visible");
                let send = rec.label_center("Queue message").expect("queue button");
                assert!(
                    (provider.y - send.y).abs() < 0.5,
                    "{theme} {scale} {width}: composer controls wrapped"
                );
                rec.harness.step();
                let settled = tail_rect(rec);
                assert_eq!(
                    presented, settled,
                    "{theme} {scale} {width}: live chat moves after resize"
                );
                assert_eq!(
                    input,
                    rec.harness
                        .ctx
                        .read_response(egui::Id::new("composer_input"))
                        .unwrap()
                        .rect,
                    "{theme} {scale} {width}: composer moves after resize"
                );
                let idle = rec.harness.render().unwrap();
                let x = (presented.left().max(0.0) * scale).ceil() as u32;
                let y = (presented.top().max(0.0) * scale).ceil() as u32;
                let w = (presented.width() * scale).floor() as u32;
                let h = (presented.height() * scale).floor() as u32;
                let resized_crop = image::imageops::crop_imm(&resized, x, y, w, h).to_image();
                let idle_crop = image::imageops::crop_imm(&idle, x, y, w, h).to_image();
                assert_eq!(
                    resized_crop, idle_crop,
                    "{theme} {scale} {width}: text pixels flicker after resize"
                );
                if let Some(dir) = &rec.stills {
                    resized
                        .save(dir.join(format!("resize-{theme}-{scale}-{width}.png")))
                        .unwrap();
                }
            }
            println!("LIVE RESIZE stable: {theme}, scale {scale}");
        }
    }
}

/// Visual review of the actual composer at three widths in every built-in palette.
/// Run with OXI_POLISH_REVIEW=1 OXI_GALLERY=/tmp/oxi-polish cargo test --release
/// --locked --lib render_gallery -- --ignored --nocapture.
fn review_ui_polish(rec: &mut Recorder<'_>) {
    let input_id = egui::Id::new("composer_input");
    rec.app().conv.settings.dictation.enabled = true;
    for theme in ["dark", "light", "midnight", "sublime", "mariana"] {
        rec.app().conv.settings.theme_id = theme.into();
        crate::theme::apply_theme(&rec.harness.ctx, theme);
        for width in [1240.0, 760.0, 520.0] {
            rec.harness.set_size(egui::vec2(width, 680.0));
            rec.app().conv.composer.input.clear();
            rec.harness.ctx.memory_mut(|m| m.request_focus(input_id));
            rec.harness.run_steps(5);
            rec.still(&format!("polish-{theme}-{width}-empty"));
            rec.app().conv.composer.input = "Review the changes and explain the tradeoffs.\nKeep the interface responsive on smaller windows.".into();
            rec.harness.run_steps(5);
            rec.still(&format!("polish-{theme}-{width}-draft"));
            assert!(rec.harness.query_by_label("Send message").is_some());
            let send = rec.label_center("Send message").expect("send button");
            let mic = rec.label_center("Dictation").expect("microphone button");
            assert!(
                (mic.y - send.y).abs() < 0.5,
                "{theme} {width}: microphone and Send are misaligned"
            );
            if width >= 760.0 {
                let provider = rec.label_center("Local HF").expect("provider picker");
                let scale = rec.harness.ctx.pixels_per_point();
                let model_y = rec
                    .harness
                    .query_all_by_label_contains("qwen2.5")
                    .next()
                    .expect("model picker")
                    .rect()
                    .center()
                    .y
                    / scale;
                assert!(
                    (provider.y - send.y).abs() < 0.5,
                    "{theme} {width}: provider and Send are misaligned"
                );
                assert!(
                    (model_y - send.y).abs() < 0.5,
                    "{theme} {width}: model and Send are misaligned"
                );
            }
            let text = rec
                .harness
                .ctx
                .read_response(input_id)
                .expect("composer input");
            assert!(
                text.rect.right() <= width,
                "{theme} {width}: composer overflows"
            );
            assert!(
                text.rect.bottom() < 680.0,
                "{theme} {width}: composer is clipped"
            );
            // A click on the provider must leave keyboard focus with the picker, rather
            // than the composer card immediately taking it back.
            let pos = rec.label_center("Local HF").expect("provider picker");
            rec.harness.event(Event::PointerMoved(pos));
            for pressed in [true, false] {
                rec.harness.event(Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                });
                rec.harness.step();
            }
            assert!(
                !rec.harness.ctx.memory(|m| m.has_focus(input_id)),
                "{theme} {width}: composer stole picker focus"
            );
            rec.still(&format!("polish-{theme}-{width}-picker"));
            egui::Popup::close_all(&rec.harness.ctx);
        }
    }
}

/// The scripted fix: run the suite, read the culprit, edit it.
fn fix_turn_tools(rec: &mut Recorder<'_>) {
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
}

/// The rest of the fix: the suite passes, the agent sums up.
fn fix_turn_finish(rec: &mut Recorder<'_>) {
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
}

const TOUR_CHAPTERS: [&str; 7] = [
    "Local models",
    "Any provider",
    "Agent",
    "Review",
    "Git",
    "Plan mode",
    "Themes",
];

/// The product video: one pass over the main features, a chapter each.
fn tour(rec: &mut Recorder<'_>) {
    {
        let app = rec.app();
        app.open_settings_page();
        app.conv.settings_page.provider_tab = LlmProviderKind::LocalHf;
    }
    rec.harness.run_steps(6);
    rec.card(Some(stage::Card::Intro), true);
    rec.hold(2.4);

    // 1. Local models.
    rec.card(Some(stage::Card::Intro), false);
    rec.chapter(
        0,
        "Run open models on your own machine",
        "Pick a GGUF from HuggingFace. oxi installs llama.cpp, downloads the model and runs it.",
    );
    rec.hold(4.2);
    rec.still("local-models");
    rec.framed_still("local-models");

    // 2. Providers: ACP agents and the Router.
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
    rec.crossfade(0.4);
    {
        let app = rec.app();
        app.conv.settings_page.tab = super::state::SettingsTab::Providers;
        app.conv.settings_page.provider_tab = LlmProviderKind::ClaudeCodeAcp;
    }
    rec.chapter(
        1,
        "Or use the subscription you already pay for",
        "Claude Code, Codex and Cursor over ACP, ChatGPT sign-in, or any OpenAI-compatible API.",
    );
    rec.hold(3.4);
    rec.still("providers-acp");
    rec.crossfade(0.4);
    rec.app().conv.settings_page.provider_tab = LlmProviderKind::Router;
    rec.chapter(
        1,
        "Let the Router pick for every task",
        "It sends each request to the right provider, and switches when a quota runs out.",
    );
    rec.hold(3.6);
    rec.still("providers-router");
    rec.framed_still("router");

    // 3. The agent fixes a failing test.
    rec.crossfade(0.4);
    {
        let app = rec.app();
        app.conv.settings_page.open = false;
        app.conv.settings_page.original = None;
        app.conv.composer.focus_next_frame = true;
    }
    rec.chapter(
        2,
        "Ask, and watch it work",
        "It runs your tests, reads the code and fixes it, with every step in view.",
    );
    rec.hold(1.2);
    rec.type_text("Run the tests and fix the failing one", 1);
    rec.hold(0.4);
    rec.begin_turn("Run the tests and fix the failing one");
    rec.hold(0.5);
    fix_turn_tools(rec);
    rec.still("agent-run");
    rec.framed_still("agent");
    fix_turn_finish(rec);
    rec.hold(2.0);
    rec.still("chat");

    // 4. Review the change in the diff editor.
    rec.crossfade(0.4);
    {
        let app = rec.app();
        // Wide enough for the split view.
        app.conv.git_ui.open = false;
        assert!(app.open_diff_editor(
            "stats.py",
            crate::app::file_explorer::DiffSource::WorkTree,
            None
        ));
    }
    rec.wait_for_diff_editor();
    rec.chapter(
        3,
        "Review every change side by side",
        "The diff is a real editor. Stage or discard any block with one click.",
    );
    rec.hold(1.0);
    // Over the end of the changed line, clear of its text.
    let change = rec.diff_change_point(0) + egui::vec2(250.0, 0.0);
    rec.move_pointer(change, 0.9);
    rec.hold(2.4);
    rec.still("diff-review");
    rec.framed_still("review");
    rec.crossfade(0.35);
    rec.app().conv.editor.diff_inline = true;
    rec.chapter(
        3,
        "Split or inline, your call",
        "Word-level highlights, an overview ruler, and F7 to jump between changes.",
    );
    rec.hold(2.8);
    rec.pointer(None);
    rec.crossfade(0.4);
    {
        let app = rec.app();
        app.conv.editor.diff_inline = false;
        app.conv.git_ui.open = true;
    }

    // 5. Stage and commit from the Git panel.
    rec.chapter(
        4,
        "Commit without leaving the window",
        "Stage, write a message (or let the model write it), then commit and push.",
    );
    rec.app()
        .request(crate::git::GitOp::Stage(vec!["stats.py".into()]));
    rec.hold(0.8);
    if let Some(heading) = rec.label_center("Message") {
        rec.move_pointer(heading + egui::vec2(60.0, 32.0), 0.7);
        rec.click();
    }
    for word in "Fix median() for even-length input".split_inclusive(' ') {
        rec.app().conv.git_ui.commit_message.push_str(word);
        rec.hold(0.08);
    }
    rec.hold(0.6);
    if let Some(button) = rec.label_center("Commit") {
        rec.move_pointer(button, 0.6);
        rec.click();
    }
    rec.hold(1.0);
    rec.crossfade(0.35);
    rec.app().conv.git_ui.tab = crate::app::git_panel::GitTab::History;
    rec.hold(2.2);
    rec.still("git-commit");
    rec.framed_still("git");

    // 6. Plan mode, with its live checklist.
    rec.pointer(None);
    rec.crossfade(0.4);
    {
        let app = rec.app();
        app.conv.git_ui.tab = crate::app::git_panel::GitTab::Changes;
        app.conv.git_ui.open = false;
        app.conv.editor.documents.clear();
        app.conv.editor.active = None;
        app.new_chat();
        let key = app.active_session_key();
        app.run_state_mut(key).plan_mode = true;
        app.conv.composer.focus_next_frame = true;
    }
    rec.chapter(
        5,
        "Plan first, then build",
        "/plan investigates without touching a file, with a live checklist. One click to implement.",
    );
    rec.hold(0.8);
    rec.type_text("Add CSV export to the stats report", 1);
    rec.hold(0.3);
    let key = rec.app().active_session_key();
    rec.app().run_state_mut(key).last_turn_planned = true;
    rec.begin_turn("Add CSV export to the stats report");
    rec.stream_thinking("I'll map how reports are produced before proposing changes.");
    let todos = |done: usize| {
        let items = [
            "Map how reports are rendered",
            "Find where output formats are chosen",
            "Draft the CSV writer and CLI flag",
            "List tests to add",
        ];
        json!({"todos": items.iter().enumerate().map(|(i, item)| json!({
            "content": item,
            "status": if i < done { "completed" } else if i == done { "in_progress" } else { "pending" },
        })).collect::<Vec<_>>()})
    };
    rec.tool("todo1", "todo_write", todos(0), 0.3);
    rec.tool("call_p1", "read", json!({ "path": "stats.py" }), 0.6);
    rec.tool("todo2", "todo_write", todos(2), 0.3);
    rec.tool("todo3", "todo_write", todos(4), 0.3);
    rec.stream_text(
        "## Plan\n\n1. Add `write_csv(report, path)` next to `mean`, `median` and \
         `percentile` in `stats.py`.\n2. Add a `--format csv` flag to the report command.\n\
         3. Tests: round-trip a small report through `csv.reader`.\n",
    );
    rec.finish_turn();
    rec.hold(0.8);
    if let Some(button) = rec.label_center("Implement plan") {
        rec.move_pointer(button, 0.8);
    }
    rec.hold(2.2);
    rec.still("plan-ready");
    rec.framed_still("plan");

    // 7. Themes.
    rec.pointer(None);
    rec.chapter(
        6,
        "Make it yours",
        "Five themes, your own MCP servers, keyless web search and local voice dictation.",
    );
    for theme in ["light", "midnight", "sublime", "dark", "mariana"] {
        rec.crossfade(0.3);
        crate::theme::apply_theme(&rec.harness.ctx, theme);
        rec.hold(1.25);
        rec.still(&format!("theme-{theme}"));
    }

    // Outro: how to get it.
    rec.card(Some(stage::Card::Outro), true);
    rec.hold(1.2);
    rec.framed_still("outro");
    rec.hold(3.4);
}

/// The wgpu renderer minus the triangle kittest paints at the pointer: the video draws its own
/// pointer, and stills are screenshots.
struct NoCursor(egui_kittest::wgpu::WgpuTestRenderer);

impl egui_kittest::TestRenderer for NoCursor {
    fn setup_eframe(&self, cc: &mut eframe::CreationContext<'_>, frame: &mut eframe::Frame) {
        self.0.setup_eframe(cc, frame);
    }

    fn handle_delta(&mut self, delta: &egui::TexturesDelta) {
        self.0.handle_delta(delta);
    }

    fn render(
        &mut self,
        ctx: &egui::Context,
        output: &egui::FullOutput,
    ) -> Result<image::RgbaImage, String> {
        let pointer = ctx.input(|i| i.pointer.hover_pos());
        let is_cursor = |shape: &egui::epaint::ClippedShape| {
            matches!(&shape.shape, egui::Shape::Path(path)
                if path.points.len() == 3 && Some(path.points[0]) == pointer)
        };
        if output.shapes.last().is_some_and(is_cursor) {
            let mut output = output.clone();
            output.shapes.pop();
            return self.0.render(ctx, &output);
        }
        self.0.render(ctx, output)
    }
}

struct Recorder<'a> {
    harness: Harness<'a, OxiApp>,
    /// Video frames are composed here; `None` renders stills only, as fast as possible.
    stage: Option<stage::Stage>,
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
        if self.stage.is_none() {
            return;
        }
        let mut image = self.harness.render().expect("render frame");
        paint_traffic_lights(&mut image);
        if let Some(stage) = &mut self.stage {
            stage.frame(image);
        }
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
        events: impl FnMut(usize, &mut OxiApp) -> Vec<Event>,
    ) {
        self.measure_sized(label, frames, |_| SIZE, events);
    }

    /// [`Self::measure`] with the window size of frame `i` given by `size` (warm-up frames use
    /// `size(0)`).
    fn measure_sized(
        &mut self,
        label: &str,
        frames: usize,
        size: impl Fn(usize) -> egui::Vec2,
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
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    size(i.saturating_sub(WARM_UP)),
                )),
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
        // The chat must stay instant: every frame inside one 60 Hz refresh, typical frames
        // with room to spare. Debug builds are an order of magnitude slower, so only release
        // runs hold the line.
        let chat = ["chat", "heavy", "composer", "400 chats"]
            .iter()
            .any(|prefix| label.starts_with(prefix));
        if chat && !cfg!(debug_assertions) {
            let (p50, max) = (times[frames / 2], times[frames - 1]);
            assert!(
                p50 < Duration::from_millis(4),
                "{label}: typical frame {p50:?} is over the 4 ms chat budget"
            );
            assert!(
                max < Duration::from_micros(16_600),
                "{label}: slowest frame {max:?} misses a 60 Hz refresh"
            );
        }
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

    /// Frames while the window is dragged narrower and back, a few points per frame, the
    /// pointer at the window edge like a real resize drag, then half a second at rest so the
    /// max includes the frame that re-measures the transcript once the width settles.
    fn profile_resize(&mut self, label: &str) {
        let width = |i: usize| SIZE.x - 4.0 * (i.min(200) as f32).min(240.0 - i.min(200) as f32);
        self.measure_sized(
            label,
            270,
            |i| egui::vec2(width(i), SIZE.y),
            |i, _| vec![Event::PointerMoved(egui::pos2(width(i) - 1.0, 400.0))],
        );
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

    /// Font size, Next Modification and tab keys, driven by real key events on stats.py.
    fn check_sublime_keys(&mut self) {
        use crate::settings::EDITOR_FONT_SIZE_DEFAULT;
        let key = |key, modifiers| Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        };
        self.harness
            .event(key(egui::Key::Equals, egui::Modifiers::COMMAND));
        self.harness.run_steps(2);
        let bigger = self.app().conv.settings.editor.font_size;
        self.harness
            .event(key(egui::Key::Num0, egui::Modifiers::COMMAND));
        self.harness.run_steps(2);
        let reset = self.app().conv.settings.editor.font_size;
        let ok = bigger == EDITOR_FONT_SIZE_DEFAULT + 1.0 && reset == EDITOR_FONT_SIZE_DEFAULT;
        println!("CHECK Cmd+= grows the editor font, Cmd+0 resets it: {ok} ({bigger} -> {reset})");
        assert!(ok, "editor font keys");

        // From the top, F7 lands on the first changed line (line 14, `mid = ...`).
        {
            let app = self.app();
            let path = app.conv.editor.active_document().unwrap().path.clone();
            app.conv.editor.navigation_target = Some((path, 0..0));
            app.conv.editor.focus_editor_next_frame = true;
        }
        self.harness.run_steps(3);
        self.harness
            .event(key(egui::Key::F7, egui::Modifiers::NONE));
        self.harness.run_steps(4);
        let caret_line = {
            let app = self.app();
            let caret = app.conv.editor.navigation_cursor_char;
            let content = &app.conv.editor.active_document().unwrap().content;
            content.chars().take(caret).filter(|c| *c == '\n').count()
        };
        println!(
            "CHECK F7 jumps to the next Git change: line {}",
            caret_line + 1
        );
        assert_eq!(caret_line + 1, 14, "next modification");

        // Multiple selections: Cmd+D thrice on `ordered`, type over all of them, undo.
        let original = {
            let app = self.app();
            let document = app.conv.editor.active_document().unwrap();
            let path = document.path.clone();
            let at = document.content.find("ordered = sorted").unwrap() + 2;
            let original = document.content.clone();
            app.conv.editor.navigation_target = Some((path, at..at));
            app.conv.editor.focus_editor_next_frame = true;
            original
        };
        self.harness.run_steps(3);
        for _ in 0..3 {
            self.harness
                .event(key(egui::Key::D, egui::Modifiers::COMMAND));
            self.harness.run_steps(2);
        }
        let selections = 1 + self
            .app()
            .conv
            .editor
            .active_document()
            .unwrap()
            .view
            .extra
            .len();
        self.still("editor-multi-cursor");
        self.harness.event(Event::Text("xs".into()));
        self.harness.run_steps(2);
        let typed = self
            .app()
            .conv
            .editor
            .active_document()
            .unwrap()
            .content
            .clone();
        let replaced = typed.matches("xs").count();
        println!("CHECK Cmd+D x3 then typing edits {selections} selections: {replaced} replaced");
        assert_eq!((selections, replaced), (3, 3), "multiple selections");
        self.harness.event(Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        self.harness.run_steps(2);
        let single = self
            .app()
            .conv
            .editor
            .active_document()
            .unwrap()
            .view
            .extra
            .is_empty();
        // egui groups undo by time; wait past its stable interval so one Cmd+Z undoes the typing.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        self.harness.run_steps(2);
        self.harness
            .event(key(egui::Key::Z, egui::Modifiers::COMMAND));
        self.harness.run_steps(3);
        let restored = self.app().conv.editor.active_document().unwrap().content == original;
        println!(
            "CHECK Escape collapses to one caret: {single}; Cmd+Z restores the text: {restored}"
        );
        assert!(single && restored, "collapse/undo");

        // Folding: Cmd+Alt+[ on `median`'s body hides it, Cmd+K Cmd+0 opens everything.
        {
            let app = self.app();
            let document = app.conv.editor.active_document().unwrap();
            let path = document.path.clone();
            let at = document.content.find("def median").unwrap() + 4;
            app.conv.editor.navigation_target = Some((path, at..at));
            app.conv.editor.focus_editor_next_frame = true;
        }
        self.harness.run_steps(3);
        let mac = cfg!(target_os = "macos");
        let cmd = if mac {
            egui::Modifiers::COMMAND.plus(egui::Modifiers::MAC_CMD)
        } else {
            egui::Modifiers::COMMAND
        };
        let (fold_mods, fold_key) = if mac {
            (cmd.plus(egui::Modifiers::ALT), egui::Key::OpenBracket)
        } else {
            (
                egui::Modifiers::CTRL.plus(egui::Modifiers::SHIFT),
                egui::Key::OpenBracket,
            )
        };
        self.harness.event(key(fold_key, fold_mods));
        self.harness.run_steps(4);
        let folds = self
            .app()
            .conv
            .editor
            .active_document()
            .unwrap()
            .view
            .folds
            .ranges
            .clone();
        self.still("editor-fold");
        println!("CHECK Cmd+Alt+[ folds the caret's block: {folds:?}");
        assert_eq!(folds, vec![9..17], "fold region");
        self.harness.event(key(egui::Key::K, cmd));
        self.harness.run_steps(1);
        self.harness.event(key(egui::Key::Num0, cmd));
        self.harness.run_steps(3);
        let open = self
            .app()
            .conv
            .editor
            .active_document()
            .unwrap()
            .view
            .folds
            .is_empty();
        let font = self.app().conv.settings.editor.font_size;
        println!("CHECK Cmd+K Cmd+0 unfolds all (font untouched {font}): {open}");
        assert!(
            open && font == crate::settings::EDITOR_FONT_SIZE_DEFAULT,
            "unfold all"
        );

        // Word completion: typing `ord` on the last line offers `ordered`; Enter takes it.
        let before = {
            let app = self.app();
            let document = app.conv.editor.active_document().unwrap();
            let (path, end, content) = (
                document.path.clone(),
                document.content.len(),
                document.content.clone(),
            );
            app.conv.editor.navigation_target = Some((path, end..end));
            app.conv.editor.focus_editor_next_frame = true;
            content
        };
        self.harness.run_steps(3);
        std::thread::sleep(std::time::Duration::from_millis(1100));
        self.harness.event(Event::Text("ord".into()));
        self.harness.run_steps(2);
        let offered = self
            .app()
            .conv
            .editor
            .active_document()
            .unwrap()
            .view
            .completion
            .as_ref()
            .map(|open| open.items.clone())
            .unwrap_or_default();
        self.still("editor-completion");
        self.harness
            .event(key(egui::Key::Enter, egui::Modifiers::NONE));
        self.harness.run_steps(2);
        let inserted = self
            .app()
            .conv
            .editor
            .active_document()
            .unwrap()
            .content
            .ends_with("ordered");
        println!("CHECK typing `ord` offers {offered:?}; Enter inserts it: {inserted}");
        assert!(
            offered.first().is_some_and(|word| word == "ordered") && inserted,
            "completion"
        );
        std::thread::sleep(std::time::Duration::from_millis(1100));
        self.harness.run_steps(1);
        self.harness
            .event(key(egui::Key::Z, egui::Modifiers::COMMAND));
        self.harness.run_steps(3);
        assert!(
            self.app().conv.editor.active_document().unwrap().content == before,
            "undo completion"
        );

        // Column selection: Alt+drag from line 13 col 4 to line 17 col 8 selects that block.
        let (from, to) = {
            let document = self.app().conv.editor.active_document().unwrap();
            (
                crate::app::file_explorer::text_point(document, 12, 4).unwrap(),
                crate::app::file_explorer::text_point(document, 16, 8).unwrap(),
            )
        };
        let alt = egui::Modifiers::ALT;
        self.harness.event(Event::PointerMoved(from));
        self.harness.run_steps(1);
        self.harness.event(Event::PointerButton {
            pos: from,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: alt,
        });
        self.harness.run_steps(1);
        for step in 1..=6 {
            let t = step as f32 / 6.0;
            self.harness
                .event(Event::PointerMoved(from + (to - from) * t));
            self.harness.run_steps(1);
        }
        self.harness.event(Event::PointerButton {
            pos: to,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: alt,
        });
        self.harness.run_steps(2);
        let (regions, texts) = {
            let document = self.app().conv.editor.active_document().unwrap();
            let texts: Vec<String> = document
                .view
                .extra
                .iter()
                .map(|sel| document.content[sel.range()].to_owned())
                .collect();
            (1 + document.view.extra.len(), texts)
        };
        self.still("editor-column-select");
        println!("CHECK Alt+drag makes {regions} column selections: {texts:?}");
        assert_eq!(regions, 5, "column selection");
        self.app()
            .conv
            .editor
            .active_document_mut()
            .unwrap()
            .view
            .extra
            .clear();
        self.harness.run_steps(1);

        // Bookmarks: Cmd+F2 marks the caret's line, F2 comes back to it from elsewhere.
        {
            let app = self.app();
            let document = app.conv.editor.active_document().unwrap();
            let path = document.path.clone();
            let at = document.content.find("def median").unwrap();
            app.conv.editor.navigation_target = Some((path, at..at));
            app.conv.editor.focus_editor_next_frame = true;
        }
        self.harness.run_steps(3);
        self.harness.event(key(egui::Key::F2, cmd));
        self.harness.run_steps(2);
        let marked = self
            .app()
            .conv
            .editor
            .active_document()
            .unwrap()
            .view
            .bookmarks
            .clone();
        {
            let app = self.app();
            let path = app.conv.editor.active_document().unwrap().path.clone();
            app.conv.editor.navigation_target = Some((path, 0..0));
        }
        self.harness.run_steps(3);
        self.harness
            .event(key(egui::Key::F2, egui::Modifiers::NONE));
        self.harness.run_steps(3);
        let line = self
            .app()
            .conv
            .editor
            .active_document()
            .unwrap()
            .view
            .last_caret_line;
        self.still("editor-bookmark");
        println!("CHECK Cmd+F2 bookmarks {marked:?}, F2 returns to line {line}");
        assert!(marked == vec![9] && line == 9, "bookmarks");
        self.app()
            .conv
            .editor
            .active_document_mut()
            .unwrap()
            .view
            .bookmarks
            .clear();

        // Cmd+Shift+] walks to the next tab when there is more than one.
        let (before, count) = {
            let app = self.app();
            (app.conv.editor.active, app.conv.editor.documents.len())
        };
        let modifiers = egui::Modifiers::COMMAND.plus(egui::Modifiers::SHIFT);
        self.harness.event(Event::Key {
            key: egui::Key::CloseCurlyBracket,
            physical_key: Some(egui::Key::CloseBracket),
            pressed: true,
            repeat: false,
            modifiers: if cfg!(target_os = "macos") {
                modifiers.plus(egui::Modifiers::MAC_CMD)
            } else {
                modifiers
            },
        });
        self.harness.run_steps(2);
        let after = self.app().conv.editor.active;
        if cfg!(target_os = "macos") && count > 1 {
            println!("CHECK Cmd+Shift+] selects the next tab: {before:?} -> {after:?}");
            assert_ne!(before, after, "next tab");
            if let Some(before) = before {
                self.app().conv.editor.active = Some(before);
            }
            self.harness.run_steps(2);
        }
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

    /// Move to `chapter` of the rail with a new headline (no-op without a stage).
    fn chapter(&mut self, chapter: usize, title: &str, subtitle: &str) {
        let Some(stage) = &mut self.stage else {
            return;
        };
        let state = stage.state();
        state.chapter = Some(chapter);
        state.previous = state.headline.take();
        state.headline = Some((title.to_owned(), subtitle.to_owned()));
        state.headline_t = 0.0;
    }

    /// Show (`on`) or hide a title card over the window; with nothing shown yet it starts
    /// fully covered.
    fn card(&mut self, card: Option<stage::Card>, on: bool) {
        let Some(stage) = &mut self.stage else {
            return;
        };
        let first = stage.state().headline.is_none() && stage.state().card.is_none();
        let state = stage.state();
        state.card = card;
        state.card_target = if on { 1.0 } else { 0.0 };
        if first {
            state.card_t = state.card_target;
        }
    }

    /// Also save the next composed video frame as `stage-{name}.png` among the stills.
    fn framed_still(&mut self, name: &str) {
        if let (Some(stage), Some(dir)) = (&mut self.stage, &self.stills) {
            stage.save_next = Some(dir.join(format!("stage-{name}.png")));
        }
        self.shot();
    }

    /// Blend the window from what is on screen now to what comes next.
    fn crossfade(&mut self, seconds: f32) {
        if let Some(stage) = &mut self.stage {
            stage.crossfade((seconds * FPS as f32).round() as usize);
        }
    }

    fn pointer(&mut self, at: Option<egui::Pos2>) {
        if at.is_none() {
            self.harness.event(Event::PointerGone);
        }
        if let Some(stage) = &mut self.stage {
            stage.pointer = at;
        }
    }

    /// Glide the pointer to `to` (window points), hovering what it passes over.
    fn move_pointer(&mut self, to: egui::Pos2, seconds: f32) {
        let from = self
            .stage
            .as_ref()
            .and_then(|stage| stage.pointer)
            .unwrap_or(egui::pos2(SIZE.x * 0.62, SIZE.y * 0.82));
        let frames = ((seconds * FPS as f32).round() as usize).max(1);
        for i in 1..=frames {
            let t = stage::ease(i as f32 / frames as f32);
            // A slight arc reads as a hand move rather than a robot's.
            let lift = (t * std::f32::consts::PI).sin() * 18.0;
            let at = from.lerp(to, t) - egui::vec2(0.0, lift);
            self.harness.event(Event::PointerMoved(at));
            self.pointer(Some(at));
            self.shot();
        }
    }

    /// Press and release the primary button where the pointer is.
    fn click(&mut self) {
        let Some(at) = self.stage.as_ref().and_then(|stage| stage.pointer) else {
            return;
        };
        let button = |pressed| Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        self.harness.event(button(true));
        if let Some(stage) = &mut self.stage {
            stage.pointer_down = true;
        }
        self.hold(0.12);
        self.harness.event(button(false));
        if let Some(stage) = &mut self.stage {
            stage.pointer_down = false;
        }
        self.hold(0.1);
    }

    /// Center of the first on-screen widget labeled `label` (after an icon, for icon
    /// buttons), in window points.
    fn label_center(&mut self, label: &str) -> Option<egui::Pos2> {
        let scale = self.harness.ctx.pixels_per_point();
        let rect = self
            .harness
            .query_all_by_label_contains(label)
            .filter(|node| {
                node.accesskit_node()
                    .label()
                    .is_some_and(|text| text == label || text.ends_with(&format!(" {label}")))
            })
            .map(|node| node.rect())
            .next()?;
        Some((rect.center().to_vec2() / scale).to_pos2())
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
        if app.active_session().messages.is_empty() {
            app.active_session_mut().title = crate::model::make_session_title(text);
        }
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

/// Stills of an evidence-mode turn: real before/after captures (headless Chrome), a check and
/// the agent's claims in the evidence card (`OXI_EVIDENCE_REVIEW=1`).
fn review_evidence_ui(rec: &mut Recorder<'_>) {
    use crate::evidence::{Claim, EvidenceItem, EvidenceReport, ItemKind, Origin, Phase};
    fn click_toggle(rec: &mut Recorder<'_>) {
        let at = rec.label_center("Evidence mode").unwrap();
        rec.harness.event(Event::PointerMoved(at));
        rec.harness.run_steps(2);
        for pressed in [true, false] {
            rec.harness.event(Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
            rec.harness.run_steps(2);
        }
    }
    rec.harness.run_steps(4);
    let dir = rec.project.join(".evidence-review");
    std::fs::create_dir_all(&dir).unwrap();
    let page = |error: &str| {
        format!(
            "<html><body style=\"font-family:-apple-system,sans-serif;background:#fafaf7;margin:0;padding:48px\">\
             <h2 style=\"margin:0 0 20px\">Sign in</h2>\
             <input placeholder=\"Email\" value=\"ana@example.com\" style=\"display:block;width:280px;padding:10px;margin-bottom:10px;border:1px solid #ccc;border-radius:8px\">\
             <input type=password value=\"hunter2\" style=\"display:block;width:280px;padding:10px;margin-bottom:10px;border:1px solid {};border-radius:8px\">{}\
             <button style=\"padding:10px 20px;border:0;border-radius:8px;background:#e26a2c;color:white\">Sign in</button></body></html>",
            if error.is_empty() { "#ccc" } else { "#d33" },
            error
        )
    };
    let shoot = |name: &str, html: String| {
        let file = dir.join(format!("{name}.html"));
        std::fs::write(&file, html).unwrap();
        let out = dir.join(format!("{name}.png"));
        crate::evidence::capture::url(&format!("file://{}", file.display()), &out, 640, 360, 500)
            .expect("headless Chrome capture")
            .file
    };
    let before = shoot("before", page(""));
    let after = shoot(
        "after",
        page(
            "<p style=\"color:#d33;margin:0 0 12px;font-size:14px\">Wrong password. Try again or reset it.</p>",
        ),
    );
    let item = |id: &str, kind, phase, label: &str, file: Option<&Path>| EvidenceItem {
        workspace: None,
        id: id.into(),
        kind,
        phase,
        label: label.into(),
        origin: Origin::Oxi,
        file: file.map(|f| f.to_string_lossy().into_owned()),
        preview: None,
        fresh: false,
        source: "Chrome headless · http://localhost:5173/login".into(),
        command: None,
        cwd: Some(rec.project.to_string_lossy().into_owned()),
        exit_code: None,
        timed_out: false,
        output: String::new(),
        duration_ms: 0,
    };
    let mut check = item("e3", ItemKind::Check, Phase::Check, "Unit tests", None);
    check.command = Some("npm test -- login".into());
    check.exit_code = Some(0);
    check.duration_ms = 4_210;
    check.output = "PASS src/login.test.ts\n  ✓ shows an inline error on a wrong password (41 ms)\n\nTests: 1 passed, 1 total".into();
    let report = EvidenceReport {
        stale: false,
        workspace: None,
        turn: "review".into(),
        requested: true,
        submitted: true,
        summary: "A wrong password now shows an inline error under the field instead of failing silently.".into(),
        items: vec![
            item("e1", ItemKind::Image, Phase::Before, "Login page, wrong password", Some(&before)),
            item("e2", ItemKind::Image, Phase::After, "Login page, wrong password", Some(&after)),
            check,
        ],
        claims: vec![
            Claim {
                text: "A wrong password shows an inline error".into(),
                evidence: vec!["e2".into(), "e3".into()],
            },
            Claim {
                text: "Nothing else on the page changed".into(),
                evidence: vec![],
            },
        ],
    };
    {
        let app = rec.app();
        app.set_active_session_provider(LlmProviderKind::ClaudeCodeAcp);
        let key = app.active_session_key();
        app.run_state_mut(key).evidence_mode.set(true);
        app.conv.sidebar.open = false;
    }
    rec.begin_turn("Show an error when the password is wrong");
    rec.send(AgentEvent::TextDelta(
        "Done. The login form now shows an inline error when the password is wrong.".into(),
    ));
    rec.send(AgentEvent::Evidence(Box::new(report)));
    rec.finish_turn();
    // Image decoding runs on loader threads; let it land.
    for _ in 0..30 {
        rec.harness.run_steps(2);
        std::thread::sleep(Duration::from_millis(50));
    }
    rec.harness.run_steps(4);
    rec.still("evidence-card");
    rec.harness.set_size(egui::vec2(560.0, 900.0));
    for _ in 0..10 {
        rec.harness.run_steps(2);
        std::thread::sleep(Duration::from_millis(30));
    }
    rec.still("evidence-card-narrow");
    for provider in [LlmProviderKind::OpenAi, LlmProviderKind::Ollama] {
        rec.app().set_active_session_provider(provider);
        for scale in [1.0, 1.5, 2.0] {
            rec.harness.set_pixels_per_point(scale);
            rec.harness.run_steps(4);
            for width in [1240.0, 560.0, 430.0] {
                rec.harness.set_size(egui::vec2(width, 900.0));
                rec.harness.run_steps(6);
                let rendered = rec.harness.render().unwrap();
                assert_eq!(
                    rendered.dimensions(),
                    ((width * scale) as u32, (900.0 * scale) as u32)
                );
                assert!(rec.harness.query_by_label("Evidence mode").is_some());
                assert!(rec.harness.query_by_label("Send message").is_some());
                rec.still(&format!("evidence-{provider:?}-{width}-{scale}"));
            }
        }
        for enabled in [false, true] {
            click_toggle(rec);
            let key = rec.app().active_session_key();
            assert_eq!(
                rec.app().run_state(key).unwrap().evidence_mode.enabled(),
                enabled
            );
        }
    }
    rec.app()
        .set_active_session_provider(LlmProviderKind::OpenAi);
    rec.harness.set_pixels_per_point(1.0);
    rec.harness.run_steps(4);
    rec.harness.set_size(egui::vec2(1240.0, 900.0));
    {
        let report = rec
            .app()
            .active_session_mut()
            .messages
            .iter_mut()
            .rev()
            .find_map(|m| m.evidence.as_deref_mut())
            .unwrap();
        report.stale = true;
    }
    rec.harness.run_steps(5);
    assert!(rec.harness.query_by_label("The workspace changed after this evidence was produced. Fresh verification is needed.").is_some());
    rec.still("evidence-stale");
    let key = rec.app().active_session_key();
    rec.app().run_state_mut(key).evidence_mode.set(false);
    let running_mode = rec.app().run_state(key).unwrap().evidence_mode.clone();
    rec.begin_turn("Explain how Evidence chooses whether verification is needed");
    rec.send(AgentEvent::TextDelta("Informational replies need no automatic verification. Enabling Evidence during an edit applies at the end of that response.".into()));
    rec.harness.run_steps(6);
    assert!(rec.harness.query_by_label("Stop generation").is_some());
    click_toggle(rec);
    assert!(
        running_mode.enabled(),
        "the in-flight response must see the composer toggle immediately"
    );
    rec.still("evidence-enabled-during-response");
    rec.app().conv.composer.input = "/evidence".into();
    rec.app().send_message();
    assert!(
        !running_mode.enabled(),
        "a bare /evidence must toggle the running turn immediately"
    );
    assert!(
        rec.app().run_state(key).unwrap().queued.is_empty(),
        "the toggle must not be queued as a new task"
    );
    rec.finish_turn();
}

/// Escape over an editor selection, and a right-press inside a transcript selection, must not
/// paint the selection differently for a frame (`OXI_SELECTION_REVIEW=1`).
fn review_selection_flicker(rec: &mut Recorder<'_>) {
    use egui::PointerButton::{Primary, Secondary};
    let button = |pos, button, pressed| Event::PointerButton {
        pos,
        button,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    let differing = |a: &image::RgbaImage, b: &image::RgbaImage| {
        a.pixels().zip(b.pixels()).filter(|(x, y)| x != y).count()
    };
    let selection = |rec: &mut Recorder<'_>| {
        rec.harness
            .ctx
            .plugin::<egui::text_selection::LabelSelectionState>()
            .lock()
            .has_selection()
    };

    {
        let app = rec.app();
        let reply = "Retries now back off between attempts, starting at a hundred milliseconds \
                     and doubling each time up to the configured cap. Every attempt is logged \
                     with its delay so a slow endpoint shows up in the output, and the final \
                     error keeps the last response body for debugging. That is the whole \
                     selection story for this review.";
        let messages = &mut app.active_session_mut().messages;
        messages.push(message(MsgRole::User, "Add retries to fetch", vec![]));
        messages.push(message(
            MsgRole::Assistant,
            "",
            vec![AssistantBlock::Answer(reply.into())],
        ));
    }
    rec.harness.run_steps(6);
    let rect = rec
        .harness
        .query_all_by_label_contains("Retries now back off")
        .map(|node| {
            let r = node.rect();
            egui::Rect::from_min_max(
                (r.min.to_vec2() / SCALE).to_pos2(),
                (r.max.to_vec2() / SCALE).to_pos2(),
            )
        })
        .next()
        .expect("reply on screen");
    let from = rect.left_top() + egui::vec2(3.0, 4.0);
    let to = rect.right_bottom() - egui::vec2(3.0, 4.0);
    rec.harness.event(Event::PointerMoved(from));
    rec.harness.run_steps(2);
    rec.harness.event(button(from, Primary, true));
    rec.harness.step();
    for i in 1..=8 {
        rec.harness
            .event(Event::PointerMoved(from.lerp(to, i as f32 / 8.0)));
        rec.harness.step();
    }
    rec.harness.event(button(to, Primary, false));
    rec.harness.run_steps(2);
    assert!(selection(rec), "the reply is selected");
    let middle = rect.center();
    rec.harness.event(Event::PointerMoved(middle));
    rec.harness.run_steps(3);
    let before = rec.harness.render().unwrap();
    // kittest never calls `raw_input_hook`; route the press through it as eframe does.
    let hooked = |rec: &mut Recorder<'_>, event| {
        let ctx = rec.harness.ctx.clone();
        let mut input = std::mem::take(rec.harness.input_mut());
        input.events.push(event);
        eframe::App::raw_input_hook(rec.app(), &ctx, &mut input);
        *rec.harness.input_mut() = input;
        rec.harness.step();
    };
    hooked(rec, button(middle, Secondary, true));
    let pressed = rec.harness.render().unwrap();
    let changed = differing(&before, &pressed);
    println!("CHECK right-press inside a chat selection repaints {changed} pixels");
    hooked(rec, button(middle, Secondary, false));
    rec.harness.run_steps(2);
    let menu = rec.app().conv.transcript.message_menu.is_some();
    let held = rec.app().conv.transcript.held_selection.is_some();
    println!(
        "CHECK the message menu opens with Copy selection: {}",
        menu && held
    );
    rec.still("chat-selection-menu");
    assert_eq!(changed, 0, "chat selection flickers on right-press");
    assert!(menu && held, "message menu with the held selection");
    rec.harness.key_press(egui::Key::Escape);
    rec.harness.run_steps(2);

    let project = rec.project.clone();
    rec.app()
        .open_editor_file(std::fs::canonicalize(project.join("stats.py")).unwrap());
    rec.harness.run_steps(4);
    {
        let app = rec.app();
        let document = app.conv.editor.active_document().unwrap();
        let path = document.path.clone();
        let at = document.content.find("ordered = sorted").unwrap();
        app.conv.editor.navigation_target = Some((path, at..at + 40));
        app.conv.editor.focus_editor_next_frame = true;
    }
    rec.harness.run_steps(4);
    let before = rec.harness.render().unwrap();
    rec.harness.key_press(egui::Key::Escape);
    let escaped = rec.harness.render().unwrap();
    rec.harness.step();
    let after = rec.harness.render().unwrap();
    let flicker = differing(&before, &escaped);
    let settled = differing(&before, &after);
    println!("CHECK Escape over an editor selection repaints {flicker} then {settled} pixels");
    assert_eq!(
        (flicker, settled),
        (0, 0),
        "editor selection flickers on Escape"
    );
}

/// Stills of a turn while it streams: live thinking, a running command, an explore run, a failed
/// tool, and the finished turn unfolded (`OXI_STREAM_REVIEW=1`; `OXI_TOOL_DETAIL=minimal` or
/// `expanded` renders the same turn in that tool-detail mode).
fn review_streaming_ui(rec: &mut Recorder<'_>) {
    rec.harness.run_steps(4);
    {
        let app = rec.app();
        app.conv.settings_page.open = false;
        app.conv.settings_page.original = None;
        if let Ok(mode) = std::env::var("OXI_TOOL_DETAIL") {
            app.conv.settings.tool_detail =
                serde_json::from_value(json!(mode)).expect("minimal, balanced or expanded");
        }
    }
    // ACP can pause after commentary without ending the turn. Exercise the real event
    // consumer so a message boundary never leaves the transcript looking idle.
    rec.begin_turn("Check the implementation and continue with the next file.");
    rec.send(AgentEvent::TextDelta(
        "I found the first issue. I will check the next file.".into(),
    ));
    rec.send(AgentEvent::AssistantMessageDone);
    rec.harness.run_steps(4);
    assert!(rec.app().session_row_is_running(0, 0));
    assert!(rec.app().active_waiting_response());
    assert_eq!(rec.harness.query_all_by_label("Working").count(), 1);
    rec.still("stream-commentary-waiting");
    crate::theme::apply_theme(&rec.harness.ctx, "light");
    rec.harness.run_steps(4);
    assert_eq!(rec.harness.query_all_by_label("Working").count(), 1);
    rec.still("stream-commentary-waiting-light");
    // A silent agent: the tail says so and offers a way out instead of spinning forever.
    assert_eq!(
        rec.harness.query_all_by_label("Stop and continue").count(),
        0
    );
    {
        let app = rec.app();
        let key = app.active_session_key();
        app.run_state_mut(key).last_event_at =
            Some(std::time::Instant::now() - std::time::Duration::from_secs(130));
    }
    rec.harness.run_steps(4);
    assert_eq!(rec.harness.query_all_by_label("Continue").count(), 1);
    assert_eq!(
        rec.harness.query_all_by_label("Working").count(),
        0,
        "the stall line replaces the Working line"
    );
    rec.still("stream-stalled-light");
    crate::theme::apply_theme(&rec.harness.ctx, "dark");
    rec.harness.run_steps(4);
    rec.still("stream-stalled");
    crate::theme::apply_theme(&rec.harness.ctx, "light");
    rec.finish_turn();
    rec.harness.run_steps(4);
    assert!(!rec.app().session_row_is_running(0, 0));
    assert!(!rec.app().active_waiting_response());
    assert_eq!(rec.harness.query_all_by_label("Working").count(), 0);
    rec.still("stream-commentary-finished-light");
    crate::theme::apply_theme(&rec.harness.ctx, "dark");
    rec.app().active_session_mut().messages.clear();
    rec.begin_turn("Why does the percentile test flake?");
    rec.stream_thinking(
        "The user says test_percentile is flaky. Flaky usually means ordering or floating point. \
         Let me think about what percentile() does with ties and whether the test compares \
         floats exactly. I should read the test and the implementation before guessing.",
    );
    rec.still("stream-thinking-live");
    rec.send(AgentEvent::ToolStart {
        name: "bash".into(),
        tool_call_id: "s_1".into(),
        args: Some(json!({ "command": "python3 -m unittest -q test_stats.TestPercentile" })),
    });
    rec.send(AgentEvent::ToolOutput {
        tool_call_id: "s_1".into(),
        text: "test_p50 ... ok\ntest_p90 ... ok\ntest_p99 ...".into(),
        truncated: false,
    });
    rec.harness.run_steps(4);
    rec.still("stream-bash-running");
    rec.send(AgentEvent::ToolEnd {
        tool_call_id: "s_1".into(),
        is_error: Some(true),
        full_output_path: None,
        diff: None,
    });
    for (id, name, args) in [
        ("s_2", "read", json!({ "path": "stats.py" })),
        (
            "s_3",
            "grep",
            json!({ "pattern": "def percentile", "path": "." }),
        ),
        ("s_4", "read", json!({ "path": "tests/test_stats.py" })),
    ] {
        rec.tool(id, name, args, 0.2);
    }
    rec.send(AgentEvent::ToolStart {
        name: "find".into(),
        tool_call_id: "s_5".into(),
        args: Some(json!({ "pattern": "**/conftest.py" })),
    });
    rec.harness.run_steps(4);
    rec.still("stream-explore-running");
    rec.send(AgentEvent::ToolEnd {
        tool_call_id: "s_5".into(),
        is_error: Some(false),
        full_output_path: None,
        diff: None,
    });
    rec.harness.run_steps(4);
    rec.still("stream-planning");
    rec.stream_text("`percentile()` interpolates with floats, and the test compares with `==`.");
    rec.harness.run_steps(4);
    rec.still("stream-answer");
    rec.finish_turn();
    rec.harness.run_steps(4);
    rec.still("stream-done");
    {
        let persist = crate::ui::preview_expand::expand_persist_id(egui::Id::new((
            "activity_summary",
            1usize,
        )));
        rec.harness
            .ctx
            .data_mut(|d| d.insert_persisted(persist, true));
    }
    rec.harness.run_steps(4);
    rec.still("stream-done-expanded");
    crate::theme::apply_theme(&rec.harness.ctx, "light");
    rec.harness.run_steps(4);
    rec.still("stream-done-expanded-light");
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
/// Split view: two chats side by side, "Send to both", swapping focus by clicking the other
/// pane, and a general chat.
fn review_split_view(rec: &mut Recorder<'_>) {
    rec.harness.set_size(egui::vec2(1500.0, 900.0));
    rec.harness.run_steps(4);
    let chats = rec.app().active_workspace().sessions.len();
    println!("SPLIT chats in workspace: {chats}");
    {
        let app = rec.app();
        // The newest seeded chat next to the blank one `setup` opened.
        let partner = app.session_key(app.conv.active_workspace, 1);
        app.ensure_session_loaded(partner);
        app.open_split_with(partner);
    }
    rec.harness.run_steps(6);
    rec.still("split-view");
    rec.app().toggle_split_mirror();
    rec.harness.run_steps(3);
    rec.still("split-mirror");
    // "Send to both": one prompt lands in both chats.
    let count = |rec: &mut Recorder<'_>, key| {
        rec.app()
            .session_by_key(key)
            .messages
            .iter()
            .filter(|m| m.role == crate::model::MsgRole::User)
            .count()
    };
    let active = rec.app().active_session_key();
    let partner = rec.app().split_partner_key().unwrap();
    let before = (count(rec, active), count(rec, partner));
    rec.app().conv.composer.input = "Summarize the README".into();
    rec.app().send_message();
    rec.harness.run_steps(4);
    let after = (count(rec, active), count(rec, partner));
    println!("SPLIT mirror user messages {before:?} -> {after:?}");
    assert_eq!(
        after,
        (before.0 + 1, before.1 + 1),
        "the prompt went to both chats"
    );
    assert_eq!(rec.app().active_session_key(), active);
    assert!(rec.app().conv.composer.input.is_empty());
    rec.still("split-mirror-sent");
    for key in [active, partner] {
        if let Some(run) = rec.app().flow.sessions.get_mut(&key) {
            run.clear_agent();
            run.waiting_response = false;
        }
    }
    rec.harness.run_steps(3);
    // Click the unfocused (right) pane: it becomes the active chat, the left one the preview.
    let before = rec.app().active_session_key();
    let click = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    // On the transcript itself, not just the footer.
    let pos = egui::pos2(1150.0, 400.0);
    rec.harness.hover_at(pos);
    rec.harness.step();
    rec.harness.event(click(pos, true));
    rec.harness.step();
    rec.harness.event(click(pos, false));
    rec.harness.run_steps(6);
    let after = rec.app().active_session_key();
    println!("SPLIT focus {before:?} -> {after:?}");
    assert_ne!(before, after, "clicking the other pane focuses it");
    rec.still("split-focus-swapped");
    rec.harness.set_size(egui::vec2(900.0, 700.0));
    rec.harness.run_steps(4);
    rec.still("split-narrow");
    rec.harness.set_size(egui::vec2(1500.0, 900.0));
    rec.app().close_split();
    rec.app().new_general_chat();
    rec.harness.run_steps(6);
    println!(
        "SPLIT general root: {}",
        rec.app().active_workspace().root_path
    );
    rec.still("general-chat");
}

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

/// Keyboard chat switching: `#` in Goto Anything (Cmd+Shift+O) and Ctrl+Tab, driven with real
/// key events while the composer has focus.
fn review_chat_navigation(rec: &mut Recorder<'_>) {
    use egui::{Key, Modifiers};
    let active = |rec: &mut Recorder<'_>| {
        let app = rec.app();
        (
            app.conv.active_workspace,
            app.conv.workspaces[app.conv.active_workspace].active,
        )
    };
    rec.app().conv.composer.focus_next_frame = true;
    rec.harness.run_steps(3);
    let start = active(rec);
    let titles: Vec<String> = rec.app().conv.workspaces[start.0]
        .sessions
        .iter()
        .map(|session| session.title.clone())
        .collect();
    assert!(titles.len() >= 3, "the demo seeds several chats");

    rec.harness
        .key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::O);
    rec.harness.run_steps(3);
    assert!(
        rec.app().conv.editor.file_picker_open,
        "Cmd+Shift+O opens the chat switcher"
    );
    assert_eq!(rec.app().conv.editor.file_picker_query, "#");
    rec.still("chat-switcher");
    rec.harness.key_press(Key::Enter);
    rec.harness.run_steps(3);
    assert!(!rec.app().conv.editor.file_picker_open);
    let switched = active(rec);
    assert_ne!(
        switched, start,
        "Enter on an empty query goes to the previous chat"
    );

    rec.app().conv.composer.focus_next_frame = true;
    rec.harness.run_steps(2);
    let draft_before = rec.app().conv.composer.input.clone();
    rec.harness.key_press_modifiers(Modifiers::CTRL, Key::Tab);
    rec.harness.run_steps(3);
    let next = active(rec);
    assert_ne!(next, switched, "Ctrl+Tab moves to another chat");
    rec.harness
        .key_press_modifiers(Modifiers::CTRL | Modifiers::SHIFT, Key::Tab);
    rec.harness.run_steps(3);
    assert_eq!(active(rec), switched, "Ctrl+Shift+Tab comes back");
    assert_eq!(
        rec.app().conv.composer.input,
        draft_before,
        "the composer never receives the Tab"
    );

    // Typing narrows the list by title.
    let wanted = titles
        .iter()
        .find(|title| title.to_ascii_lowercase().contains("csv"))
        .expect("a CSV chat in the demo")
        .clone();
    rec.harness
        .key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::O);
    rec.harness.run_steps(2);
    rec.app().conv.editor.file_picker_query = "#csv".into();
    rec.harness.run_steps(3);
    rec.still("chat-switcher-query");
    rec.harness.key_press(Key::Enter);
    rec.harness.run_steps(3);
    let (wi, si) = active(rec);
    assert_eq!(rec.app().conv.workspaces[wi].sessions[si].title, wanted);

    // Hot exit: an unsent draft reaches disk once typing pauses.
    rec.app().conv.composer.input = "An unsent idea about the CSV export".into();
    rec.harness.run_steps(2);
    let drafts = crate::app_dirs::data_dir().join("drafts.json");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let saved = loop {
        rec.harness.step();
        let saved = std::fs::read_to_string(&drafts).unwrap_or_default();
        if saved.contains("An unsent idea") || std::time::Instant::now() > deadline {
            break saved;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    assert!(saved.contains("An unsent idea"), "draft not saved: {saved}");

    // Cmd+F: type, step through matches with Enter, close with Escape.
    {
        let app = rec.app();
        app.conv.composer.input.clear();
        app.active_session_mut().messages = heavy_transcript(60);
        app.conv.transcript.scroll_to_bottom_once = true;
    }
    rec.harness.run_steps(6);
    let on_screen = |rec: &Recorder<'_>, text: &str| {
        rec.harness
            .query_all_by_label_contains(text)
            .any(|node| (0.0..SIZE.y * SCALE).contains(&node.rect().center().y))
    };
    assert!(
        !on_screen(rec, "Question 1:"),
        "the first turns start off screen"
    );
    rec.harness.key_press_modifiers(Modifiers::COMMAND, Key::F);
    rec.harness.run_steps(3);
    assert!(
        rec.app().conv.transcript.find.is_some(),
        "Cmd+F opens chat find"
    );
    rec.harness.event(egui::Event::Text("Question 1".into()));
    rec.harness.run_steps(6);
    assert!(
        on_screen(rec, "Question 1:"),
        "typing reveals the first match"
    );
    rec.still("chat-find");
    rec.harness.key_press(Key::Enter);
    rec.harness.run_steps(6);
    assert_eq!(rec.app().conv.transcript.find.as_ref().unwrap().current, 1);
    assert!(
        on_screen(rec, "Question 10:"),
        "Enter moves to the next match"
    );
    rec.still("chat-find-next");
    rec.harness.key_press(Key::Escape);
    rec.harness.run_steps(3);
    assert!(
        rec.app().conv.transcript.find.is_none(),
        "Escape closes chat find"
    );

    // Distraction Free: Sublime's binding hides the panels and brings back the same ones.
    let (mods, key) = if cfg!(target_os = "macos") {
        (
            Modifiers::COMMAND | Modifiers::SHIFT | Modifiers::CTRL,
            Key::F,
        )
    } else {
        (Modifiers::SHIFT, Key::F11)
    };
    assert!(rec.app().conv.sidebar.open);
    rec.harness.key_press_modifiers(mods, key);
    rec.harness.run_steps(4);
    assert!(
        rec.app().distraction_free(),
        "the shortcut enters Distraction Free"
    );
    assert!(!rec.app().conv.sidebar.open);
    rec.still("distraction-free");
    rec.harness.key_press_modifiers(mods, key);
    rec.harness.run_steps(4);
    assert!(!rec.app().distraction_free());
    assert!(rec.app().conv.sidebar.open, "the sidebar comes back");

    // In the editor, Ctrl+Tab walks the open files instead of leaving for another chat.
    let chat_before = active(rec);
    let project = rec.project.clone();
    {
        let app = rec.app();
        app.open_editor_file(std::fs::canonicalize(project.join("stats.py")).unwrap());
        app.open_editor_file(std::fs::canonicalize(project.join("test_stats.py")).unwrap());
    }
    rec.harness.run_steps(3);
    let tab_before = rec.app().conv.editor.active;
    rec.harness.key_press_modifiers(Modifiers::CTRL, Key::Tab);
    rec.harness.run_steps(3);
    assert_ne!(
        rec.app().conv.editor.active,
        tab_before,
        "Ctrl+Tab switches file"
    );
    assert!(rec.app().conv.editor.active_document().is_some());
    assert_eq!(active(rec), chat_before, "the chat stays the same");
    println!("CHAT NAV ok");
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
    for scale in [1.0, 1.25, 2.0] {
        rec.harness.set_pixels_per_point(scale);
        // Apply the DPI/zoom event before testing window resize frames. egui rescales
        // the previous viewport when the zoom changes, before consuming the next size.
        rec.harness.run_steps(4);
        let mut previous_count = usize::MAX;
        for width in (430..=1240).rev().step_by(13) {
            rec.harness.set_size(egui::vec2(width as f32, 680.0));
            rec.harness.step();
            let send = rec.label_center("Send message").expect("send button");
            let context = rec
                .label_center("Context usage")
                .expect("context usage ring");
            assert!(
                context.y > send.y,
                "{width} {scale}: context usage lives in the status bar, below the composer"
            );
            let controls: Vec<_> = rec
                .harness
                .query_all_by_role(egui::accesskit::Role::ComboBox)
                .map(|node| node.rect())
                .collect();
            assert!(!controls.is_empty(), "provider must remain visible");
            assert!(
                controls.len() <= previous_count,
                "a hidden selector reappeared while narrowing the composer"
            );
            previous_count = controls.len();
            for rect in controls {
                if rect.right() / scale > width as f32
                    && let Some(dir) = &rec.stills
                {
                    rec.harness
                        .render()
                        .unwrap()
                        .save(dir.join("picker-overflow.png"))
                        .unwrap();
                }
                assert!(
                    (rect.center().y / scale - send.y).abs() < 0.5,
                    "{width} {scale}: selector wrapped"
                );
                assert!(
                    rect.right() / scale <= width as f32,
                    "{width} {scale}: selector overflowed: {rect:?}, dpi {}",
                    rec.harness.ctx.pixels_per_point()
                );
            }
        }
    }
    rec.harness.set_pixels_per_point(SCALE);
    for width in [1240.0, 1000.0, 800.0, 600.0, 480.0, 430.0] {
        rec.harness.set_size(egui::vec2(width, 680.0));
        rec.harness.run_steps(2);
        rec.still(&format!("pickers-responsive-{width}"));
    }
    rec.harness.set_size(SIZE);
    rec.harness.run_steps(3);
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
            evidence: None,
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
            // Deleting the final character must preserve the single-line field's anchor
            // on the input frame itself, not only after an extra idle frame.
            rec.app().conv.composer.input = "x".into();
            rec.harness.run_steps(4);
            let before = rec.harness.ctx.read_response(input_id).unwrap().rect;
            let height = rec.app().conv.composer.measured_full_h;
            rec.harness.event(Event::Key {
                key: egui::Key::Backspace,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            });
            rec.harness.step();
            assert!(rec.app().conv.composer.input.is_empty());
            let deleted = rec.harness.ctx.read_response(input_id).unwrap().rect;
            assert_eq!(
                deleted, before,
                "{theme} {scale}: final deletion moves the composer"
            );
            assert_eq!(rec.app().conv.composer.measured_full_h, height);
            let placeholder = rec
                .harness
                .output()
                .shapes
                .iter()
                .find_map(|s| match &s.shape {
                    egui::Shape::Text(t) if t.galley.job.text == "Message oxi…" => Some(t),
                    _ => None,
                })
                .expect("empty composer placeholder");
            assert!(
                placeholder
                    .galley
                    .job
                    .sections
                    .iter()
                    .all(|s| s.format.color == crate::theme::c_text_faint()),
                "{theme} {scale}: placeholder uses the normal text color"
            );
            rec.still(&format!("composer-{theme}-{scale}-last-deleted"));
            rec.harness.event(Event::Key {
                key: egui::Key::Backspace,
                physical_key: None,
                pressed: false,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            });
            rec.harness.step();
            assert_eq!(
                rec.harness.ctx.read_response(input_id).unwrap().rect,
                deleted,
                "{theme} {scale}: empty composer moves on the next frame"
            );
            println!("COMPOSER GPU stable: {theme}, scale {scale}");
        }
    }
}

/// A remote workspace against a real host: `OXI_REMOTE_REVIEW=ssh://host/path` (the host runs
/// `oxi server`). Lists its chats, runs two agent turns there and deletes the chat again.
fn review_remote_workspace(rec: &mut Recorder<'_>, root: &str) {
    let wait_for =
        |rec: &mut Recorder<'_>, what: &str, secs: u64, done: &dyn Fn(&mut OxiApp) -> bool| {
            let started = Instant::now();
            while !done(rec.app()) {
                assert!(
                    started.elapsed() < Duration::from_secs(secs),
                    "timed out waiting for {what}"
                );
                rec.harness.step();
                std::thread::sleep(Duration::from_millis(30));
            }
            println!("REMOTE {what} after {:?}", started.elapsed());
        };
    rec.harness.set_size(egui::vec2(1400.0, 860.0));
    rec.app().open_remote_dialog();
    rec.harness.run_steps(3);
    rec.still("remote-dialog");
    rec.app().remote.dialog = None;
    rec.app().open_workspace_path(root.to_string());
    let host = crate::remote::host_of(root).unwrap().to_string();
    rec.harness.run_steps(2);
    rec.still("remote-connecting");
    let started = Instant::now();
    while rec.app().remote.state(&host) != Some(crate::remote::client::ConnState::Connected) {
        let state = rec.app().remote.state(&host);
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "no connection: {state:?}"
        );
        rec.harness.step();
        std::thread::sleep(Duration::from_millis(30));
    }
    rec.harness.run_steps(20);
    let chats = rec.app().active_workspace().sessions.len();
    println!("REMOTE chats listed: {chats}");
    rec.still("remote-listed");

    // File panels can be verified even while the host's agent quota is exhausted.
    if std::env::var_os("OXI_REMOTE_PANELS_ONLY").is_some() {
        review_remote_panels(rec, root);
        return;
    }

    let provider = std::env::var("OXI_REMOTE_REVIEW_PROVIDER")
        .map(|slug| serde_json::from_value::<LlmProviderKind>(json!(slug)).expect("provider slug"))
        .unwrap_or(LlmProviderKind::ClaudeCodeAcp);
    rec.app().new_chat();
    rec.app().set_active_session_provider(provider);
    rec.app().conv.composer.input = "Reply with exactly the word pong and nothing else.".into();
    rec.app().send_message();
    rec.harness.run_steps(2);
    rec.still("remote-turn-running");
    let key = rec.app().active_session_key();
    wait_for(rec, "turn", 180, &|app| {
        !app.run_state(key).is_some_and(|r| r.waiting_response)
    });
    rec.harness.run_steps(20);
    rec.still("remote-turn-done");
    let app = rec.app();
    let session = app.session_by_key(key);
    let answer = crate::model::last_answer(&session.messages)
        .unwrap_or("")
        .to_string();
    println!(
        "REMOTE file {:?} answer {answer:?} error {:?}",
        session.session_file,
        app.run_state(key).and_then(|r| r.stream_error.clone())
    );
    assert!(
        session
            .session_file
            .as_deref()
            .is_some_and(crate::remote::is_remote)
    );
    assert!(answer.to_lowercase().contains("pong"), "answer: {answer}");

    // A follow-up uses the same chat: the transcripts agree, so no history goes along.
    rec.app().conv.composer.input = "And now reply with exactly: ping".into();
    rec.app().send_message();
    wait_for(rec, "second turn", 180, &|app| {
        !app.run_state(key).is_some_and(|r| r.waiting_response)
    });
    rec.harness.run_steps(20);
    rec.still("remote-second-turn");
    let answer = crate::model::last_answer(&rec.app().session_by_key(key).messages)
        .unwrap_or("")
        .to_string();
    println!("REMOTE second answer {answer:?}");
    assert!(answer.to_lowercase().contains("ping"), "answer: {answer}");

    review_remote_panels(rec, root);

    let si = key.session_idx;
    rec.app().reveal_chat_view();
    rec.app().delete_session(si);
    rec.harness.run_steps(20);
    std::thread::sleep(Duration::from_millis(500));
    rec.harness.run_steps(5);
}

/// The editor side of a remote workspace: explorer, a file opened and saved, Cmd+P, Find in
/// Files, the Git panel and a terminal, all on the host. `root` holds a git repository with a
/// committed `README.md` that has one uncommitted line, and `src/main.rs`.
fn review_remote_panels(rec: &mut Recorder<'_>, root: &str) {
    let step_for = |rec: &mut Recorder<'_>, secs: f32| {
        let until = Instant::now() + Duration::from_secs_f32(secs);
        while Instant::now() < until {
            rec.harness.step();
            std::thread::sleep(Duration::from_millis(30));
        }
    };
    let root_path = PathBuf::from(root);
    {
        let app = rec.app();
        app.conv.explorer.expanded.insert(root_path.join("src"));
        app.conv.explorer.cache.invalidate();
        app.conv.sidebar.open = true;
        app.conv.sidebar.mode = crate::app::state::SidebarMode::Explorer;
        app.open_editor_file(root_path.join("README.md"));
        app.conv.git_ui.open = true;
        app.request(crate::git::GitOp::Refresh);
    }
    step_for(rec, 3.0);
    let app = rec.app();
    let doc = app
        .conv
        .editor
        .active_document()
        .map(|d| (d.path.clone(), d.content.clone()));
    println!("REMOTE editor {doc:?} error {:?}", app.conv.editor.error);
    assert!(app.conv.editor.error.is_none());
    let (opened_path, content) = doc.expect("remote README opened in the editor");
    assert_eq!(opened_path, root_path.join("README.md"));
    assert!(content.contains("Hello from the mac mini."));
    println!(
        "REMOTE git repo {} branch {:?} unstaged {:?} error {:?}",
        app.conv.git.repo,
        app.conv.git.branch,
        app.conv
            .git
            .unstaged
            .iter()
            .map(|e| (&e.path, e.status))
            .collect::<Vec<_>>(),
        app.conv.git.error
    );
    assert!(app.conv.git.repo, "remote Git repository was detected");
    assert!(app.conv.git.error.is_none());
    assert_eq!(app.conv.git.branch, "main");
    assert!(app.conv.git.unstaged.iter().any(|e| e.path == "README.md"));
    rec.still("remote-editor-git");

    // Edit and save: the file on the host changes.
    {
        let app = rec.app();
        if let Some(document) = app.conv.editor.active_document_mut() {
            document.content.push_str("Saved from the app.\n");
            document.dirty = true;
            document.content_revision += 1;
        }
        let ctx = rec.harness.ctx.clone();
        rec.app().save_editor_file(&ctx);
    }
    println!("REMOTE save error {:?}", rec.app().conv.editor.error);
    let (host, path) = crate::remote::split(root).unwrap();
    let on_host = std::process::Command::new("ssh")
        .args([host, &format!("tail -n 1 '{path}/README.md'")])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    println!("REMOTE README last line on host: {on_host:?}");
    assert_eq!(on_host, "Saved from the app.");

    rec.app().open_file_picker();
    step_for(rec, 2.0);
    rec.still("remote-file-picker");
    rec.app().cancel_file_picker();
    rec.app().show_project_search();
    rec.app().conv.project_search.query = "remote".into();
    step_for(rec, 2.5);
    let (files, hits) = rec.app().conv.project_search.total_hits();
    println!("REMOTE search: {hits} hits in {files} files");
    assert!(
        files >= 2 && hits >= 2,
        "search found both remote fixture files"
    );
    rec.still("remote-search");

    rec.app().conv.sidebar.mode = crate::app::state::SidebarMode::Explorer;
    rec.app().conv.terminal_panel.open = true;
    step_for(rec, 4.0);
    assert!(
        rec.app().terminals.first().is_some_and(|t| t.is_alive()),
        "remote terminal started"
    );
    if let Some(terminal) = rec.app().terminals.first_mut() {
        terminal.type_text("pwd; git status --short\r");
    }
    step_for(rec, 2.0);
    rec.still("remote-terminal");
    let app = rec.app();
    app.conv.terminal_panel.open = false;
    app.conv.git_ui.open = false;
    app.terminals.clear();
    // Leave the folder as it was.
    let _ = std::process::Command::new("ssh")
        .args([host, &format!("cd '{path}' && git checkout -q README.md && printf '\\nAn uncommitted line.\\n' >> README.md")])
        .status();
}

/// Child transcripts reuse the same message renderer, including narrow layouts.
fn review_subagent_chats(rec: &mut Recorder<'_>) {
    use serde_json::json;
    fn click_label(rec: &mut Recorder<'_>, label: &str) {
        // Accessibility bounds use physical pixels; pointer events use logical points.
        let rect = rec.harness.get_by_label(label).rect();
        let pos = (rect.center().to_vec2() / rec.harness.ctx.pixels_per_point()).to_pos2();
        rec.harness.hover_at(pos);
        rec.harness.step();
        for pressed in [true, false] {
            rec.harness.event(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
            rec.harness.step();
        }
    }
    {
        let app = rec.app();
        app.active_session_mut().title = "Audit the editor".into();
        app.active_session_mut().messages = vec![
            message(MsgRole::User,"Review the editor using two agents",vec![]),
            message(MsgRole::Assistant,"I started a workflow with two reviewers. Open their sub-chats to follow the analysis.",vec![AssistantBlock::Answer("I started a workflow with two reviewers. Open their sub-chats to follow the analysis.".into())]),
        ];
        for value in [
            json!({"sessionId":"root","update":{"sessionUpdate":"async_task_spawned","asyncTaskId":"workflow","name":"Editor review","description":"Inspect correctness and responsiveness","taskType":"workflow","canStop":true}}),
            json!({"sessionId":"root","update":{"sessionUpdate":"async_task_progress","asyncTaskId":"workflow","phases":[{"title":"Inspect"},{"title":"Review"}],"summary":"Inspect · 1 of 2 agents completed"}}),
            json!({"sessionId":"workflow","update":{"sessionUpdate":"subagent_update","sessionId":"reader","title":"Code inspection","description":"Inspect editor buffer updates and file saves.","capabilities":{"cancel":{}},"state":{"state":"running"},"model":"Claude Opus"}}),
            json!({"sessionId":"reader","update":{"sessionUpdate":"session_message","messageId":"p1","recipientSessionId":"reader","content":[{"type":"text","text":"Inspect the editor update path. Report concrete issues and validation."}]}}),
            json!({"sessionId":"reader","update":{"sessionUpdate":"tool_call","toolCallId":"read","kind":"read","title":"Read editor.rs","status":"completed","rawInput":{"path":"src/app/editor.rs"},"rawOutput":"Buffer writes are published after the file is saved."}}),
            json!({"sessionId":"reader","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"The save path preserves **buffer consistency**. I am checking the concurrent update path next.\n\n- File write finishes first.\n- The buffer revision is then published.\n- Other chats observe the updated state."}}}),
            json!({"sessionId":"reader","update":{"sessionUpdate":"usage_update","used":18400,"size":200000}}),
            json!({"sessionId":"workflow","update":{"sessionUpdate":"subagent_update","sessionId":"reviewer","title":"Responsiveness review","description":"Verified the editor stays responsive during tool updates.","state":{"state":"idle","outcome":"completed"},"model":"Codex"}}),
            json!({"sessionId":"reviewer","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"No responsiveness regressions found."}}}),
        ] {
            app.active_session_mut().delegated.apply("root", &value);
        }
    }
    for width in [1240.0, 760.0, 520.0] {
        rec.harness.set_size(egui::vec2(width, 720.0));
        rec.harness.run_steps(5);
        rec.still(&format!("subagents-parent-{width}"));
        click_label(rec, "Agents · 2  ▾");
        rec.harness.run_steps(3);
        rec.still(&format!("subagents-menu-{width}"));
        rec.harness.event(egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        rec.harness.run_steps(3);
        click_label(rec, "Editor review");
        rec.harness.run_steps(5);
        rec.still(&format!("subagents-workflow-{width}"));
        click_label(rec, "Code inspection");
        rec.harness.run_steps(5);
        rec.still(&format!("subagents-child-{width}"));
        assert!(rec.harness.query_by_label("Send message").is_none());
        click_label(rec, "← Chat");
        rec.harness.run_steps(3);
        assert!(rec.harness.query_by_label("Send message").is_some());
    }
    if let Ok(dir) = std::env::var("OXI_SUBAGENT_SAVED_ROOT") {
        rec.app().active_session_mut().delegated = crate::agent::acp::saved_workflow_snapshot(
            std::path::Path::new(&dir),
            "saved-workflow",
        );
        assert!(!rec.app().active_session().delegated.chats.is_empty());
        rec.harness.set_size(egui::vec2(1240.0, 780.0));
        rec.harness.run_steps(5);
        rec.still("subagents-real-saved-workflow");
        let ctx = rec.harness.ctx.clone();
        {
            let app = rec.app();
            let key = app.active_session_key();
            let c = app
                .active_session()
                .delegated
                .chats
                .iter()
                .find(|c| c.task)
                .unwrap();
            app.select_subagent(&ctx, key, &c.root, &c.id);
        }
        for width in [1240.0, 760.0, 520.0] {
            rec.harness.set_size(egui::vec2(width, 780.0));
            rec.harness.run_steps(5);
            rec.still(&format!("subagents-real-detail-{width}"));
        }
    }
}
