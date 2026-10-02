//! Embedded terminal settings.

#[cfg(not(windows))]
use eframe::egui::RichText;
use eframe::egui::Ui;

#[cfg(not(windows))]
use crate::theme::{FS_SMALL, c_text_muted};
use crate::ui::chrome::{card_frame, settings_card_header, settings_section_title};

use super::super::OxiApp;

impl OxiApp {
    pub(super) fn render_settings_terminal_panel(&mut self, ui: &mut Ui) {
        #[cfg(windows)]
        let (subtitle, card_title, card_hint) = (
            "Choose the shell used by the embedded terminal.",
            "Windows shell",
            "The new choice is used the next time the terminal starts.",
        );
        // Only Windows has a choice to make; elsewhere this page just says which shell runs.
        #[cfg(not(windows))]
        let (subtitle, card_title, card_hint) = (
            "The shell used by the embedded terminal.",
            "Shell",
            "Change your login shell to use a different one.",
        );
        settings_section_title(ui, "Terminal", Some(subtitle));
        card_frame().show(ui, |ui| {
            settings_card_header(ui, card_title, Some(card_hint));

            #[cfg(windows)]
            {
                let current = self.conv.settings.windows_terminal;
                let wsl_available = crate::terminal::wsl_available();
                eframe::egui::ComboBox::from_id_salt("windows_terminal_combo")
                    .icon(crate::ui::chrome::combo_chevron_icon)
                    .selected_text(current.label())
                    .width(320.0)
                    .show_ui(ui, |ui| {
                        for terminal in crate::settings::WindowsTerminal::ALL {
                            let available =
                                terminal != crate::settings::WindowsTerminal::Wsl || wsl_available;
                            let response = ui
                                .add_enabled_ui(available, |ui| {
                                    ui.selectable_label(terminal == current, terminal.label())
                                })
                                .inner;
                            if response.clicked() && terminal != current {
                                self.conv.settings.windows_terminal = terminal;
                                // Ensure the next opened/restarted panel uses the selected shell.
                                self.terminals.clear();
                                self.active_terminal = 0;
                            }
                            if terminal == crate::settings::WindowsTerminal::Wsl && !available {
                                response
                                    .on_disabled_hover_text("WSL is not installed or unavailable");
                            }
                        }
                    });
                crate::ui::chrome::field_hint(
                    ui,
                    if wsl_available {
                        "Command Prompt, Windows PowerShell, and WSL are available."
                    } else {
                        "Install and initialize WSL to enable the WSL option."
                    },
                );
            }

            #[cfg(not(windows))]
            {
                let shell = std::env::var("SHELL").unwrap_or_default();
                let text = if shell.is_empty() {
                    "oxi starts your login shell.".to_owned()
                } else {
                    format!("oxi starts your login shell: {shell}")
                };
                ui.label(RichText::new(text).size(FS_SMALL).color(c_text_muted()));
            }
        });
    }
}
