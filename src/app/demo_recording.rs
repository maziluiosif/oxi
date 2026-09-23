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
    let scratch = std::env::temp_dir().join(format!("oxi-demo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    let home = scratch.join("home");
    let project = scratch.join("Projects").join("stats-kit");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&out).unwrap();
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

    // 1. The model is already downloaded and running: the guided Local HF setup.
    rec.harness.run_steps(4);
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
}

struct Recorder<'a> {
    harness: Harness<'a, OxiApp>,
    out: PathBuf,
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
        let mut image = self.harness.render().expect("render frame");
        paint_traffic_lights(&mut image);
        image
            .save(self.out.join(format!("frame_{:05}.png", self.frame)))
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
