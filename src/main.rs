#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod agent;
mod app;
mod compute;
mod git;
mod hydrate;
mod local_models;
mod local_models_remote;
mod markdown;
mod model;
mod oauth;
mod secrets;
mod session_store;
mod settings;
mod terminal;
mod theme;
mod ui;
mod update;
mod voice_engine;
mod voice_models;

use app::OxiApp;
use eframe::egui::IconData;

/// Record panics to `<config_dir>/oxi/crash.log` before the default hook prints to stderr,
/// so a crash in a background thread (agent/network) leaves a trace the user can find and
/// report even if they never saw the terminal.
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let log_path = dirs::config_dir().map(|d| d.join("oxi").join("crash.log"));
        if let Some(path) = log_path {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let entry = format!("[{timestamp}] {info}\n");
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
            {
                use std::io::Write;
                let _ = f.write_all(entry.as_bytes());
            }
        }
        default_hook(info);
    }));
}

fn app_icon() -> IconData {
    let image = image::load_from_memory(include_bytes!("../assets/app-icon.png"))
        .expect("embedded app icon should be a valid PNG")
        .into_rgba8();
    let (width, height) = image.dimensions();
    IconData {
        rgba: image.into_raw(),
        width,
        height,
    }
}

fn main() -> eframe::Result<()> {
    install_panic_hook();
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1240.0, 820.0])
            .with_min_inner_size([980.0, 680.0])
            .with_title("oxi")
            .with_drag_and_drop(true)
            .with_icon(app_icon()),
        ..Default::default()
    };
    eframe::run_native(
        "oxi",
        options,
        Box::new(|cc| {
            let app = OxiApp::new();
            // Register the persisted font choice so the theme's font install picks it up.
            theme::set_active_fonts(theme::FontSelection {
                ui: app.conv.settings.ui_font.clone(),
                mono: app.conv.settings.mono_font.clone(),
            });
            // Apply the persisted theme (installs fonts + builds egui visuals).
            theme::apply_theme(&cc.egui_ctx, &app.conv.settings.theme_id);
            // Apply the persisted text/UI density (zoom).
            cc.egui_ctx
                .set_zoom_factor(app.conv.settings.ui_density.zoom_factor());
            egui_extras::install_image_loaders(&cc.egui_ctx);
            Ok(Box::new(app) as Box<dyn eframe::App>)
        }),
    )
}
