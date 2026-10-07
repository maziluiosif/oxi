//! Full-size Markdown preview tab, rendered from the live editor buffer.

use eframe::egui::{self, Align, Layout, Margin, ScrollArea, Ui};

use super::super::OxiApp;

impl OxiApp {
    pub(super) fn render_editor_markdown_preview(&self, ui: &mut Ui) {
        let Some(document) = self.conv.editor.active_document() else {
            return;
        };
        render_preview(ui, &document.path, &document.content);
    }
}

const PREVIEW_COLUMN_MAX_WIDTH: f32 = 800.0;
const PREVIEW_PADDING: f32 = 16.0;

fn render_preview(ui: &mut Ui, path: &std::path::Path, content: &str) {
    // ScrollArea and Frame inherit their parent's layout. Always reset it here:
    // the editor's parent may be a horizontal row containing other app panes.
    ui.with_layout(Layout::top_down(Align::Min), |ui| {
        ScrollArea::vertical()
            .id_salt(("editor_markdown_preview", path))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let column_width = ui
                    .available_width()
                    .clamp(1.0, PREVIEW_COLUMN_MAX_WIDTH + PREVIEW_PADDING * 2.0);
                ui.with_layout(Layout::top_down(Align::Center), |ui| {
                    ui.allocate_ui_with_layout(
                        egui::vec2(column_width, 0.0),
                        Layout::top_down(Align::Min),
                        |ui| {
                            egui::Frame::new()
                                .inner_margin(Margin::same(PREVIEW_PADDING as i8))
                                .show(ui, |ui| {
                                    ui.set_width((column_width - PREVIEW_PADDING * 2.0).max(1.0));
                                    crate::markdown::render_markdown_for_file(ui, content, path);
                                });
                        },
                    );
                });
            });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(
        ctx: &egui::Context,
        size: egui::Vec2,
        path: &std::path::Path,
        text: &str,
    ) -> egui::FullOutput {
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                ..Default::default()
            },
            |ui| {
                ui.horizontal_top(|ui| render_preview(ui, path, text));
            },
        )
    }

    #[test]
    fn preview_stacks_paragraphs_even_in_a_horizontal_parent() {
        for (width, expected_left) in [(320.0, 16.0), (900.0, 50.0), (1400.0, 300.0)] {
            let ctx = egui::Context::default();
            crate::theme::apply_theme(&ctx, "dark");
            let path = std::path::Path::new("/tmp/readme.md");
            let text = "First paragraph.\n\nSecond paragraph.\n\nThird paragraph.";
            let _ = frame(&ctx, egui::vec2(width, 600.0), path, text);
            let output = frame(&ctx, egui::vec2(width, 600.0), path, text);
            let positions: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.text().contains("paragraph.") => {
                        Some(text.pos)
                    }
                    _ => None,
                })
                .collect();
            assert!(positions.len() >= 3);
            assert!(
                (positions[0].x - expected_left).abs() < 1.0,
                "the column must be centered without centering paragraph text: {positions:?}"
            );
            for pair in positions[..3].windows(2) {
                assert!(
                    pair[1].y > pair[0].y,
                    "paragraphs must flow vertically: {positions:?}"
                );
                assert!((pair[1].x - pair[0].x).abs() < 1.0);
            }
        }
    }

    #[test]
    fn preview_displays_relative_images_in_paragraphs_links_and_tables() {
        let ctx = egui::Context::default();
        crate::theme::apply_theme(&ctx, "dark");
        egui_extras::install_image_loaders(&ctx);
        let dir = std::env::temp_dir().join(format!("oxi markdown {}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        let image_path = dir.join("test imagine.png");
        image::RgbaImage::from_pixel(32, 24, image::Rgba([70, 150, 200, 255]))
            .save(&image_path)
            .unwrap();
        let path = dir.join("readme.md");
        let text = "![plain](test%20imagine.png)\n\n[![linked](test%20imagine.png)](https://example.com)\n\n| image | description |\n|---|---|\n| ![table](test%20imagine.png) | Table image |";
        // Match the loader's local-file URI form independently of the Markdown resolver.
        #[cfg(windows)]
        let uri = format!(
            "file:///{}",
            image_path.to_str().unwrap().replace('\\', "/")
        );
        #[cfg(not(windows))]
        let uri = format!("file://{}", image_path.display());
        let start = std::time::Instant::now();
        let mut image_count = 0;
        while start.elapsed() < std::time::Duration::from_secs(3) {
            let output = frame(&ctx, egui::vec2(900.0, 700.0), &path, text);
            if let Ok(egui::load::TexturePoll::Ready { texture }) = ctx.try_load_texture(
                &uri,
                egui::TextureOptions::default(),
                egui::load::SizeHint::default(),
            ) {
                image_count = output.shapes.iter().filter(|shape| {
                    matches!(&shape.shape, egui::Shape::Rect(rect) if rect.fill_texture_id() == texture.id)
                }).count();
                if image_count == 3 {
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        std::fs::remove_dir_all(dir).unwrap();
        assert_eq!(
            image_count, 3,
            "all three image placements must paint their texture"
        );
    }

    #[test]
    #[ignore = "UI review artifact; run with --ignored"]
    fn render_markdown_preview_review() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md");
        let text = "# Markdown preview\n\nFirst paragraph with **bold** and `inline code`.\n\nSecond paragraph stays below the first.\n\n![Editor](assets/screenshots/editor-git.png)\n\n| Feature | Status |\n|---|---|\n| Local images | Visible |\n| Live buffer | Supported |";
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(920.0, 840.0))
            .wgpu()
            .build_ui(|ui| {
                crate::theme::apply_theme(ui.ctx(), "dark");
                egui_extras::install_image_loaders(ui.ctx());
                ui.horizontal_top(|ui| render_preview(ui, &path, text));
            });
        harness.run_steps(10);
        harness
            .render()
            .unwrap()
            .save("/tmp/oxi-markdown-preview.png")
            .unwrap();
    }
}
