mod agent;
mod app;
mod hydrate;
mod markdown;
mod model;
mod oauth;
mod session_store;
mod settings;
mod theme;
mod ui;

use crate::theme::setup_style;
use app::OxiApp;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1240.0, 820.0])
            .with_min_inner_size([980.0, 680.0])
            .with_title("oxi")
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(
        "oxi",
        options,
        Box::new(|cc| {
            setup_style(&cc.egui_ctx);
            egui_extras::install_image_loaders(&cc.egui_ctx);
            Ok(Box::new(OxiApp::new()) as Box<dyn eframe::App>)
        }),
    )
}
