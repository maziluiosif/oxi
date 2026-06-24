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
use eframe::egui::IconData;

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
            setup_style(&cc.egui_ctx);
            egui_extras::install_image_loaders(&cc.egui_ctx);
            Ok(Box::new(OxiApp::new()) as Box<dyn eframe::App>)
        }),
    )
}
