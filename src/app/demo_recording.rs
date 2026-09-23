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
            app.conv.sidebar_width = 250.0;
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
        },
    };

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
            rec.app().conv.settings_tab = tab;
            rec.harness.run_steps(3);
            rec.still(name);
        }
        for provider in [
            LlmProviderKind::OpenAi,
            LlmProviderKind::ClaudeCodeAcp,
            LlmProviderKind::Ollama,
        ] {
            let app = rec.app();
            app.conv.settings_tab = SettingsTab::Providers;
            app.conv.settings_provider_tab = provider;
            rec.harness.run_steps(3);
            rec.still(&format!("settings-provider-{provider:?}").to_lowercase());
        }
        rec.app().conv.settings_tab = SettingsTab::Providers;
        rec.app().conv.settings_open = false;
        rec.app().conv.settings_original = None;
        rec.harness.run_steps(3);
    }
    {
        let app = rec.app();
        app.open_settings_page();
        app.conv.settings_provider_tab = LlmProviderKind::LocalHf;
    }
    rec.hold(2.6);
    rec.still("local-models");
    {
        let app = rec.app();
        app.conv.settings_open = false;
        app.conv.settings_original = None;
        app.conv.focus_chat_input_next_frame = true;
    }

    // 2. Ask for a fix.
    rec.hold(1.0);
    rec.type_text("Run the tests and fix the failing one", 2);
    rec.hold(0.4);
    rec.begin_turn("Run the tests and fix the failing one");
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
        rec.app().conv.settings_open = false;
        rec.app().conv.settings_original = None;
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
            app.conv.scroll_to_bottom_once = true;
        }
        rec.harness.run_steps(4);
        rec.still("chat-heavy");
        rec.profile("heavy idle", false);
        rec.profile_scroll("heavy scroll");
        rec.profile("heavy hover", true);
        rec.profile_streaming("heavy stream");
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
        rec.app().conv.scroll_to_bottom_once = true;
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
        rec.app().conv.sidebar_search = "csv".into();
        rec.harness.run_steps(3);
        rec.still("sidebar-search");
        rec.app().conv.sidebar_search.clear();
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
            app.conv.scroll_to_bottom_once = true;
        }
        rec.harness.run_steps(4);
    }

    // 4. Review the change in the editor, with the Git panel open.
    {
        let app = rec.app();
        let path = std::fs::canonicalize(project.join("stats.py")).unwrap();
        app.open_editor_file(path.clone());
        app.conv.editor.git_full_highlight_path = Some(path);
        app.conv.git_open = true;
    }
    rec.hold(4.0);
    rec.still("editor-git");
    if gallery {
        use crate::app::git_panel::GitTab;
        rec.app().conv.git_tab = GitTab::History;
        rec.harness.run_steps(4);
        rec.still("git-history");
        rec.app().conv.git_tab = GitTab::Branches;
        rec.harness.run_steps(4);
        rec.still("git-branches");
        rec.app().conv.git_tab = GitTab::Changes;
        rec.app().conv.editor.find_open = true;
        rec.app().conv.editor.find_query = "ordered".into();
        rec.harness.run_steps(4);
        rec.still("editor-find");
        rec.app().conv.editor.find_open = false;
        rec.app().open_file_picker();
        rec.harness.run_steps(4);
        rec.still("file-picker");
        rec.app().cancel_file_picker();
        rec.harness.run_steps(2);
        rec.profile("editor idle", false);
        rec.profile("editor hover", true);
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
        rec.app().conv.terminal_open = true;
        rec.hold(1.5);
        rec.still("terminal");
        crate::theme::apply_theme(&rec.harness.ctx, "light");
        rec.harness.run_steps(3);
        rec.still("editor-light");
        {
            let app = rec.app();
            app.conv.terminal_open = false;
            app.conv.git_open = false;
            app.conv.editor.documents.clear();
            app.conv.editor.active = None;
        }
        rec.harness.run_steps(3);
        rec.still("chat-light");
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
            let _ = ctx.run_ui(raw, |ui| {
                eframe::App::logic(app, ui.ctx(), &mut frame);
                eframe::App::ui(app, ui, &mut frame);
            });
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
        use egui_kittest::kittest::Queryable;
        if streaming {
            let app = self.app();
            let mut reply = message(
                MsgRole::Assistant,
                "",
                vec![AssistantBlock::Answer(String::new())],
            );
            reply.streaming = true;
            app.active_session_mut().messages.push(reply);
            app.conv.scroll_to_bottom_once = true;
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
        app.conv.input.clear();
        app.conv.scroll_to_bottom_once = true;
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
