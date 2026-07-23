//! Theme, font, density, and chat-width settings.

use eframe::egui::{self, RichText, Ui};

use crate::theme::*;
use crate::ui::chrome::{
    card_frame, field_hint, hairline, settings_caption, settings_card_header,
    settings_section_title,
};

use super::super::OxiApp;

impl OxiApp {
    /// Register the current font choice and rebuild egui's font atlas so the change shows
    /// immediately (theme stays as-is).
    fn apply_font_selection(&self, ctx: &egui::Context) {
        crate::theme::set_active_fonts(crate::theme::FontSelection {
            ui: self.conv.settings.ui_font.clone(),
            mono: self.conv.settings.mono_font.clone(),
        });
        crate::theme::setup_style(ctx);
    }

    pub(super) fn render_settings_appearance_panel(&mut self, ui: &mut Ui) {
        settings_section_title(
            ui,
            "Appearance",
            Some("Theme, text size, and chat column width."),
        );
        card_frame().show(ui, |ui| {
            let themes = crate::theme::available_themes();
            let current = self.conv.settings.theme_id.clone();
            settings_card_header(
                ui,
                "Theme",
                Some("Built-in themes plus any custom JSON themes."),
            );
            let selected_theme = themes
                .iter()
                .find(|theme| theme.id == current)
                .map(|theme| theme.name.as_str())
                .unwrap_or("Default");
            let mut next_theme = None;
            egui::ComboBox::from_id_salt("appearance_theme_combo")
                .selected_text(selected_theme)
                .width(320.0)
                .show_ui(ui, |ui| {
                    for theme in &themes {
                        if ui
                            .selectable_label(theme.id == current, &theme.name)
                            .clicked()
                        {
                            next_theme = Some(theme.id.clone());
                        }
                    }
                });
            if let Some(theme_id) = next_theme.filter(|id| id != &current) {
                self.conv.settings.theme_id = theme_id.clone();
                crate::theme::apply_theme(ui.ctx(), &theme_id);
            }

            ui.add_space(14.0);
            hairline(ui);
            ui.add_space(12.0);
            settings_card_header(
                ui,
                "Font",
                Some("Interface and code fonts. System fonts are detected automatically."),
            );
            settings_caption(ui, "Interface");
            ui.add_space(4.0);
            let current = self.conv.settings.ui_font.clone();
            let current_name = crate::theme::ui_font_options()
                .iter()
                .find(|opt| opt.id == current)
                .map(|opt| opt.name.as_str())
                .unwrap_or("Default");
            let mut selected = None;
            egui::ComboBox::from_id_salt("interface_font_combo")
                .selected_text(current_name)
                .width(320.0)
                .height(360.0)
                .show_ui(ui, |ui| {
                    let search_id = egui::Id::new("interface_font_search");
                    let mut search = ui
                        .ctx()
                        .data(|data| data.get_temp::<String>(search_id))
                        .unwrap_or_default();
                    ui.add(
                        egui::TextEdit::singleline(&mut search)
                            .hint_text("Search fonts…")
                            .desired_width(290.0),
                    );
                    ui.ctx()
                        .data_mut(|data| data.insert_temp(search_id, search.clone()));
                    ui.separator();
                    let needle = search.trim().to_lowercase();
                    for opt in crate::theme::ui_font_options().iter().filter(|opt| {
                        needle.is_empty() || opt.name.to_lowercase().contains(&needle)
                    }) {
                        if ui.selectable_label(opt.id == current, &opt.name).clicked() {
                            selected = Some(opt.id.clone());
                        }
                    }
                });
            if let Some(id) = selected.filter(|id| id != &current) {
                self.conv.settings.ui_font = id;
                self.apply_font_selection(ui.ctx());
            }

            ui.add_space(8.0);
            settings_caption(ui, "Code / monospace");
            ui.add_space(4.0);
            let current = self.conv.settings.mono_font.clone();
            let current_name = crate::theme::mono_font_options()
                .iter()
                .find(|opt| opt.id == current)
                .map(|opt| opt.name.as_str())
                .unwrap_or("Default");
            let mut selected = None;
            egui::ComboBox::from_id_salt("monospace_font_combo")
                .selected_text(current_name)
                .width(320.0)
                .height(360.0)
                .show_ui(ui, |ui| {
                    let search_id = egui::Id::new("monospace_font_search");
                    let mut search = ui
                        .ctx()
                        .data(|data| data.get_temp::<String>(search_id))
                        .unwrap_or_default();
                    ui.add(
                        egui::TextEdit::singleline(&mut search)
                            .hint_text("Search fonts…")
                            .desired_width(290.0),
                    );
                    ui.ctx()
                        .data_mut(|data| data.insert_temp(search_id, search.clone()));
                    ui.separator();
                    let needle = search.trim().to_lowercase();
                    for opt in crate::theme::mono_font_options().iter().filter(|opt| {
                        needle.is_empty() || opt.name.to_lowercase().contains(&needle)
                    }) {
                        if ui.selectable_label(opt.id == current, &opt.name).clicked() {
                            selected = Some(opt.id.clone());
                        }
                    }
                });
            if let Some(id) = selected.filter(|id| id != &current) {
                self.conv.settings.mono_font = id;
                self.apply_font_selection(ui.ctx());
            }

            ui.add_space(14.0);
            hairline(ui);
            ui.add_space(12.0);
            let current_density = self.conv.settings.ui_density;
            settings_card_header(
                ui,
                "Text size",
                Some("Scales the whole UI (density / zoom)."),
            );
            egui::ComboBox::from_id_salt("appearance_density_combo")
                .selected_text(current_density.label())
                .width(320.0)
                .show_ui(ui, |ui| {
                    for density in crate::settings::UiDensity::ALL {
                        if ui
                            .selectable_label(density == current_density, density.label())
                            .clicked()
                            && density != current_density
                        {
                            self.conv.settings.ui_density = density;
                            ui.ctx().set_zoom_factor(density.zoom_factor());
                        }
                    }
                });

            ui.add_space(14.0);
            hairline(ui);
            ui.add_space(12.0);
            settings_card_header(
                ui,
                "Chat width",
                Some("Max width of the message column on wide screens."),
            );
            ui.add(
                egui::Slider::new(
                    &mut self.conv.settings.chat_column_max_width,
                    CHAT_COLUMN_WIDTH_MIN..=CHAT_COLUMN_WIDTH_MAX,
                )
                .suffix("px"),
            );
            field_hint(ui, "Raise it when the sidebar or git panel is hidden.");
        });
        ui.add_space(10.0);
        ui.label(
            RichText::new(format!(
                "Add a custom theme by dropping a <name>.json file in {}.",
                crate::theme::custom_themes_dir().display()
            ))
            .size(FS_TINY)
            .color(c_text_faint()),
        );
    }
}
