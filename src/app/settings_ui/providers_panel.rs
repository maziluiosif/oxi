//! Provider selection settings page.

use super::super::OxiApp;
use super::provider_catalog::{PROVIDER_GROUPS, provider_blurb};
use crate::settings::LlmProviderKind;
use crate::theme::*;
use crate::ui::chrome::{card_frame, settings_caption, settings_section_title};
use eframe::egui::{self, Align, Color32, FontId, Layout, RichText, Sense, Stroke, Ui};

impl OxiApp {
    pub(super) fn render_settings_providers_panel(&mut self, ui: &mut Ui) {
        settings_section_title(
            ui,
            "Models & providers",
            Some("Choose where your models run. The provider marked active is used for new chats."),
        );

        // Every provider is visible at once, grouped by where it runs; the active one keeps a dot.
        let active = self.conv.settings.active_provider;
        for (group_label, providers) in PROVIDER_GROUPS {
            settings_caption(ui, group_label);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                for &provider in *providers {
                    let selected = provider == self.conv.settings_provider_tab;
                    let response =
                        provider_chip(ui, provider.label(), selected, provider == active);
                    if response.clicked() {
                        self.conv.settings_provider_tab = provider;
                    }
                }
            });
            ui.add_space(10.0);
        }

        let provider = self.conv.settings_provider_tab;
        ui.add_space(4.0);
        card_frame().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new(provider.label())
                            .size(FS_H3)
                            .color(c_text_strong())
                            .strong(),
                    );
                    ui.add_space(2.0);
                    ui.label(
                        RichText::new(provider_blurb(provider))
                            .size(FS_TINY)
                            .color(c_text_muted()),
                    );
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if self.conv.settings.active_provider == provider {
                        super::layout::active_pill(ui, "Active");
                    } else if crate::ui::chrome::primary_button(ui, "Make active")
                        .on_hover_text("Use this provider for new chats")
                        .clicked()
                    {
                        self.conv.settings.active_provider = provider;
                    }
                });
            });
        });
        ui.add_space(12.0);

        self.render_provider_config(ui, provider);

        // Provider OAuth (single section below the config, for clarity)
        if provider == LlmProviderKind::GptCodex {
            ui.add_space(12.0);
            settings_caption(ui, "Sign-in");
            ui.add_space(6.0);
            self.render_codex_oauth_section(ui);
        }

        if !provider.is_managed_hf() && !provider.is_acp() {
            ui.add_space(10.0);
            ui.label(
                RichText::new(
                    "Empty API key falls back to environment variables. OAuth still takes precedence where available. Keys are stored in the OS keychain, not in settings.json.",
                )
                .size(FS_TINY)
                .color(c_text_faint()),
            );
        }
    }
}

/// Selectable provider pill; a green dot marks the provider new chats use.
fn provider_chip(ui: &mut Ui, label: &str, selected: bool, active: bool) -> egui::Response {
    const DOT: f32 = 6.0;
    let galley = ui.painter().layout_no_wrap(
        label.to_string(),
        FontId::proportional(FS_SMALL),
        Color32::PLACEHOLDER,
    );
    let pad_x = 12.0;
    let dot_w = if active { DOT + 6.0 } else { 0.0 };
    let size = egui::vec2(galley.size().x + pad_x * 2.0 + dot_w, 30.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let hovered = response.hovered();
    let (fill, stroke, text) = if selected {
        (
            c_pill_selected_bg(),
            c_pill_selected_border(),
            c_text_strong(),
        )
    } else if hovered {
        (c_row_hover(), c_border(), c_text())
    } else {
        (c_bg_elevated(), c_border_subtle(), c_text_muted())
    };
    ui.painter().rect(
        rect,
        egui::CornerRadius::same(RADIUS_CHIP),
        fill,
        Stroke::new(1.0, stroke),
        egui::StrokeKind::Inside,
    );
    let mut x = rect.left() + pad_x;
    if active {
        ui.painter().circle_filled(
            egui::pos2(x + DOT * 0.5, rect.center().y),
            DOT * 0.5,
            c_success(),
        );
        x += dot_w;
    }
    ui.painter().galley(
        egui::pos2(x, rect.center().y - galley.size().y * 0.5),
        galley,
        text,
    );
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if active {
        response.on_hover_text("Active for new chats")
    } else {
        response
    }
}
