//! Fixture data and seeding helpers for the scripted demo: the sample Python project, its
//! chats and the transcript the tour scrolls through. Split out of `demo_recording` so the
//! recorder and tour logic stay readable.

use std::path::Path;
use std::process::Command;

use super::super::OxiApp;
use super::{MODEL, SCALE};
use crate::model::{AssistantBlock, ChatMessage, MsgRole};
use crate::settings::LlmProviderKind;

pub(super) const STATS_PY: &str = r#""""Small statistics helpers used by the reporting pipeline."""


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

pub(super) const TEST_STATS_PY: &str = r#"import unittest

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

/// `turns` user/assistant pairs mixing prose, lists, tables, code blocks and tool calls.
pub(super) fn heavy_transcript(turns: usize) -> Vec<ChatMessage> {
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

pub(super) fn chunks(text: &str, size: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    chars.chunks(size).map(|c| c.iter().collect()).collect()
}

/// The window controls macOS draws over the unified title bar (offscreen frames lack them).
pub(super) fn paint_traffic_lights(image: &mut image::RgbaImage) {
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

pub(super) fn create_project(project: &Path) {
    std::fs::create_dir_all(project).unwrap();
    std::fs::write(project.join("stats.py"), STATS_PY).unwrap();
    std::fs::write(project.join("test_stats.py"), TEST_STATS_PY).unwrap();
    std::fs::write(project.join(".gitignore"), "__pycache__/\n").unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["config", "user.name", "Ana Ionescu"],
        &["config", "user.email", "ana@example.invalid"],
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

pub(super) fn seed_settings() {
    let mut settings = crate::settings::AppSettings::load();
    settings.theme_id = "antigravity".into();
    settings.ui_font = "bundled:ibm_plex_sans".into();
    settings.mono_font = "bundled:lilex".into();
    if let Ok(theme) = std::env::var("OXI_GALLERY_THEME") {
        settings.theme_id = theme;
    }
    settings.active_provider = LlmProviderKind::LocalHf;
    settings.provider_mut(LlmProviderKind::LocalHf).model_id = MODEL.into();
    settings.save().unwrap();
}

/// A few earlier chats so the sidebar looks lived in, dated over the past days.
pub(super) fn seed_chats(project: &Path) {
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

pub(super) fn message(role: MsgRole, text: &str, blocks: Vec<AssistantBlock>) -> ChatMessage {
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
        evidence: None,
    }
}

/// Show the guided Local HF setup as finished: runtime installed, one model running.
pub(super) fn seed_local_model(app: &mut OxiApp) {
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
