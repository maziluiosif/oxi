//! Opt-in frame timing log (`OXI_FRAME_STATS=1`) for judging smoothness in the real app.
//!
//! Headless profiles (see `demo_recording::Recorder::measure`) cover UI code but not the GPU,
//! presentation, or AccessKit, which a screen reader or window manager can switch on at any time.
//! Once a second this prints to stderr how many frames ran, their CPU time (eframe's figure,
//! excluding the vsync wait), the longest gap between frames, and whether AccessKit is on.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use eframe::egui;

struct Window {
    started: Instant,
    last_frame: Option<Instant>,
    frames: u32,
    cpu_total: f32,
    cpu_max: f32,
    gap_max: Duration,
    causes: std::collections::BTreeMap<String, u32>,
}

static STATS: Mutex<Option<Window>> = Mutex::new(None);

fn enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("OXI_FRAME_STATS").is_some_and(|v| v != "0"))
}

/// Record one frame; `cpu_usage` is the previous frame's CPU time from `frame.info()`.
pub(crate) fn record(ctx: &egui::Context, cpu_usage: Option<f32>) {
    if !enabled() {
        return;
    }
    let now = Instant::now();
    let Ok(mut stats) = STATS.lock() else {
        return;
    };
    let window = stats.get_or_insert_with(|| Window {
        started: now,
        last_frame: None,
        frames: 0,
        cpu_total: 0.0,
        cpu_max: 0.0,
        gap_max: Duration::ZERO,
        causes: Default::default(),
    });
    for cause in ctx.repaint_causes() {
        let file = cause.file.rsplit("/src/").next().unwrap_or(cause.file);
        *window
            .causes
            .entry(format!("{file}:{} {}", cause.line, cause.reason))
            .or_default() += 1;
    }
    if let Some(last) = window.last_frame {
        window.gap_max = window.gap_max.max(now - last);
    }
    window.last_frame = Some(now);
    window.frames += 1;
    let cpu = cpu_usage.unwrap_or(0.0);
    window.cpu_total += cpu;
    window.cpu_max = window.cpu_max.max(cpu);
    if now - window.started >= Duration::from_secs(1) {
        // The node builder only exists while AccessKit is generating a tree.
        let accesskit = ctx
            .accesskit_node_builder(egui::Id::new("oxi_frame_stats_probe"), |_| ())
            .is_some();
        eprintln!(
            "FRAMES {:>3}/s  cpu mean {:>6.2} ms  max {:>6.2} ms  longest gap {:>6.1} ms  accesskit {}",
            window.frames,
            window.cpu_total / window.frames as f32 * 1e3,
            window.cpu_max * 1e3,
            window.gap_max.as_secs_f64() * 1e3,
            if accesskit { "ON" } else { "off" },
        );
        for (cause, count) in &window.causes {
            eprintln!("       {count:>3}x repaint requested by {cause}");
        }
        *window = Window {
            started: now,
            last_frame: Some(now),
            frames: 0,
            cpu_total: 0.0,
            cpu_max: 0.0,
            gap_max: Duration::ZERO,
            causes: Default::default(),
        };
    }
}
