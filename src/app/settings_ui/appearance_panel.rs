//! Settings → General: theme, fonts, text size, how the chat looks, and background behavior.

use eframe::egui::{self, Ui};

use crate::theme::*;
use crate::ui::chrome::{
    setting_row, setting_select, setting_switch, settings_section, settings_section_title,
};
use crate::ui::select_menu::SelectOption;

use super::super::OxiApp;

/// Theme id and font ids for the Zed look (see [`Palette::ZED`]).
const ZED_THEME_ID: &str = "zed";
const ZED_UI_FONT: &str = "bundled:ibm_plex_sans";
const ZED_MONO_FONT: &str = "bundled:lilex";

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
        settings_section_title(ui, "General", Some("How oxi looks and behaves."));

        settings_section(ui, "Appearance", None, |ui| {
            let themes = crate::theme::available_themes();
            let current = self.conv.settings.theme_id.clone();
            let selected_theme = themes
                .iter()
                .find(|theme| theme.id == current)
                .map(|theme| theme.name.as_str())
                .unwrap_or("Default");
            let picked = setting_select(
                ui,
                "appearance_theme",
                "Theme",
                Some("Your own .json themes in the config folder's themes/ are listed too."),
                selected_theme,
                || {
                    themes
                        .iter()
                        .map(|theme| SelectOption::new(theme.name.clone(), theme.id == current))
                        .collect()
                },
            );
            if let Some(theme_id) = picked
                .and_then(|i| themes.get(i))
                .map(|theme| theme.id.clone())
                .filter(|id| id != &current)
            {
                // The Zed theme brings Zed's fonts along, but only over untouched defaults;
                // leaving it restores the defaults when the fonts are still Zed's pair.
                let settings = &mut self.conv.settings;
                let fonts = (settings.ui_font.as_str(), settings.mono_font.as_str());
                if theme_id == ZED_THEME_ID && fonts == ("default", "default") {
                    settings.ui_font = ZED_UI_FONT.to_string();
                    settings.mono_font = ZED_MONO_FONT.to_string();
                } else if current == ZED_THEME_ID && fonts == (ZED_UI_FONT, ZED_MONO_FONT) {
                    settings.ui_font = "default".to_string();
                    settings.mono_font = "default".to_string();
                }
                crate::theme::set_active_fonts(crate::theme::FontSelection {
                    ui: settings.ui_font.clone(),
                    mono: settings.mono_font.clone(),
                });
                settings.theme_id = theme_id.clone();
                crate::theme::apply_theme(ui.ctx(), &theme_id);
            }

            for (mono, label, help) in [
                (
                    false,
                    "Interface font",
                    Some("Inter and IBM Plex Sans are included."),
                ),
                (
                    true,
                    "Code font",
                    Some("JetBrains Mono and Lilex are included."),
                ),
            ] {
                let options = if mono {
                    crate::theme::mono_font_options()
                } else {
                    crate::theme::ui_font_options()
                };
                let current = if mono {
                    self.conv.settings.mono_font.clone()
                } else {
                    self.conv.settings.ui_font.clone()
                };
                let current_name = options
                    .iter()
                    .find(|opt| opt.id == current)
                    .map(|opt| opt.name.as_str())
                    .unwrap_or("Default");
                let picked = setting_select(ui, ("font", mono), label, help, current_name, || {
                    options
                        .iter()
                        .map(|opt| SelectOption::new(opt.name.clone(), opt.id == current))
                        .collect()
                });
                if let Some(id) = picked
                    .and_then(|i| options.get(i))
                    .map(|opt| opt.id.clone())
                    .filter(|id| id != &current)
                {
                    if mono {
                        self.conv.settings.mono_font = id;
                    } else {
                        self.conv.settings.ui_font = id;
                    }
                    self.apply_font_selection(ui.ctx());
                }
            }

            let current_density = self.conv.settings.ui_density;
            let picked = setting_select(
                ui,
                "appearance_density",
                "Text size",
                Some("Scales the whole window."),
                current_density.label(),
                || {
                    crate::settings::UiDensity::ALL
                        .iter()
                        .map(|density| {
                            SelectOption::new(density.label(), *density == current_density)
                        })
                        .collect()
                },
            );
            if let Some(density) = picked.and_then(|i| crate::settings::UiDensity::ALL.get(i)) {
                self.conv.settings.ui_density = *density;
                ui.ctx().set_zoom_factor(density.zoom_factor());
            }
        });

        settings_section(ui, "Chat", None, |ui| {
            let current_detail = self.conv.settings.tool_detail;
            let picked = setting_select(
                ui,
                "appearance_tool_detail",
                "Tool calls",
                Some(current_detail.description()),
                current_detail.label(),
                || {
                    crate::settings::ToolDetail::ALL
                        .iter()
                        .map(|detail| {
                            SelectOption::new(detail.label(), *detail == current_detail)
                                .detail(detail.description())
                        })
                        .collect()
                },
            );
            if let Some(detail) = picked.and_then(|i| crate::settings::ToolDetail::ALL.get(i)) {
                self.conv.settings.tool_detail = *detail;
            }

            setting_row(
                ui,
                "Chat width",
                Some("Widest the message column gets on a wide window."),
                |ui| {
                    ui.spacing_mut().slider_width = 150.0;
                    ui.add(
                        egui::Slider::new(
                            &mut self.conv.settings.chat_column_max_width,
                            CHAT_COLUMN_WIDTH_MIN..=CHAT_COLUMN_WIDTH_MAX,
                        )
                        .suffix(" px"),
                    )
                },
            );
        });

        settings_section(ui, "While agents work", None, |ui| {
            setting_switch(
                ui,
                &mut self.conv.settings.notify_in_background,
                "Notify me in the background",
                Some(
                    "A desktop notification when a reply finishes or an agent waits for approval \
                     while oxi isn't the focused window.",
                ),
            );
            setting_switch(
                ui,
                &mut self.conv.settings.keep_awake_while_working,
                "Keep the computer awake while an agent works",
                Some("Holds off idle sleep until the turn ends. Closing the lid still sleeps."),
            );
            setting_switch(
                ui,
                &mut self.conv.settings.steer_follow_ups,
                "Send follow-ups into the running response",
                Some(
                    "A message you send while the agent answers reaches it at its next step, \
                     without stopping it (agents that support it). Off: it waits for the response \
                     to finish. Cmd/Ctrl+Enter does the other.",
                ),
            );
        });
    }
}
