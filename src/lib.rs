//! Library form of the oxi binary crate.
//!
//! `main.rs` is a thin entry point that calls [`run`], which lets the integration tests in
//! `tests/` exercise the public settings and filesystem APIs without launching the GUI.
//! Only the modules forming that public API are `pub`; everything else keeps the
//! crate-private visibility it had when this was a binary-only crate, so internal call
//! sites keep using the same `crate::…` paths unchanged.

mod agent;
mod app;
mod app_dirs;
mod code_nav;
mod compute;
mod delegation;
mod desktop_entry;
mod evidence;
pub mod fsutil;
mod git;
mod headless;
mod hydrate;
mod keep_awake;
mod local_models;
mod local_models_remote;
mod logging;
mod markdown;
mod merge3;
mod model;
mod oauth;
mod open_buffers;
mod os_open;
mod remote;
mod router;
mod runtime;
mod scratchpad;
mod secrets;
mod session_store;
pub mod settings;
mod shell_env;
mod terminal;
mod text_diff;
mod theme;
mod ui;
mod update;
mod vfs;
mod voice_engine;
mod voice_models;

use app::OxiApp;
use eframe::egui::IconData;

/// The window icon. 256 px is the largest size taskbars use; on macOS eframe hands it to the Dock
/// as an `NSImage`, which AppKit backs at Retina scale, so the 1024 px source pinned 32 MB.
/// The Dock draws that image as is, so the macOS copy keeps Apple's grid margin; Windows and Linux
/// taskbars expect the tile to fill the canvas.
pub(crate) fn app_icon() -> IconData {
    #[cfg(target_os = "macos")]
    const PNG: &[u8] = include_bytes!("../assets/macos/app-icon-256.png");
    #[cfg(not(target_os = "macos"))]
    const PNG: &[u8] = include_bytes!("../assets/windows/app-icon-256.png");
    let image = image::load_from_memory(PNG)
        .expect("embedded app icon should be a valid PNG")
        .into_rgba8();
    let (width, height) = image.dimensions();
    IconData {
        rgba: image.into_raw(),
        width,
        height,
    }
}

/// Process entry point: handles the `update`, ACP todo-MCP and `headless` (editor plugin)
/// subcommands, then runs the GUI.
pub fn run() -> eframe::Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("update") => std::process::exit(update::run_cli_update(args.next())),
        Some(agent::acp::oxi_mcp::SUBCOMMAND) => {
            std::process::exit(agent::acp::oxi_mcp::run_stdio())
        }
        Some(headless::SUBCOMMAND) => std::process::exit(headless::run()),
        Some(remote::service::SUBCOMMAND) => std::process::exit(remote::service::run()),
        _ => {}
    }
    logging::init();
    update::cleanup_previous_install();
    shell_env::prefetch();
    desktop_entry::ensure_in_background();
    // Warm slow first-use work on background threads while the window is created.
    secrets::prefetch_unified();
    theme::prewarm_editor_highlighting();
    let options = eframe::NativeOptions {
        viewport: ui::window_chrome::configure_viewport(
            eframe::egui::ViewportBuilder::default()
                .with_inner_size([1240.0, 820.0])
                .with_min_inner_size([980.0, 680.0])
                .with_title("oxi")
                .with_drag_and_drop(true)
                .with_icon(app_icon()),
        ),
        wgpu_options: eframe::egui_wgpu::WgpuConfiguration {
            // eframe defaults to two queued frames (throughput), which adds a whole refresh
            // interval between a keystroke and the screen. oxi draws little per frame, so keep
            // one in flight: typing and scrolling land on the next vsync.
            surface: eframe::egui_wgpu::SurfaceConfig::LOW_LATENCY,
            ..Default::default()
        },
        ..Default::default()
    };
    let result = eframe::run_native(
        "oxi",
        options,
        Box::new(|cc| {
            let mut app = OxiApp::new();
            app.install_menu_bar(&cc.egui_ctx);
            // Register the persisted font choice so the theme's font install picks it up.
            theme::set_active_fonts(theme::FontSelection {
                ui: app.conv.settings.ui_font.clone(),
                mono: app.conv.settings.mono_font.clone(),
            });
            // Apply the persisted theme (installs fonts + builds egui visuals).
            theme::apply_theme(&cc.egui_ctx, &app.conv.settings.theme_id);
            theme::load_system_cjk_fonts(&cc.egui_ctx);
            // Apply the persisted text/UI density (zoom).
            cc.egui_ctx
                .set_zoom_factor(app.conv.settings.ui_density.zoom_factor());
            egui_extras::install_image_loaders(&cc.egui_ctx);
            Ok(Box::new(app) as Box<dyn eframe::App>)
        }),
    );
    update::relaunch_if_requested();
    result
}
