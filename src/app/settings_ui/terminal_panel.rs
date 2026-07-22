//! Embedded terminal settings.

use eframe::egui::Ui;
#[cfg(not(windows))]
use eframe::egui::RichText;

#[cfg(not(windows))]
use crate::theme::{c_text_muted, FS_SMALL};
use crate::ui::chrome::{card_frame, settings_card_header, settings_section_title};

use super::super::OxiApp;

impl OxiApp {
    pub(super) fn render_settings_terminal_panel(&mut self, ui: &mut Ui) {
        settings_section_title(
            ui,
            "Terminal",
            Some("Choose the shell used by the embedded terminal."),
        );
        card_frame().show(ui, |ui| {
            settings_card_header(
                ui,
                "Windows shell",
                Some("The new choice is used the next time the terminal starts."),
            );

            #[cfg(windows)]
            {
                let current = self.conv.settings.windows_terminal;
                let wsl_available = crate::terminal::wsl_available();
                eframe::egui::ComboBox::from_id_salt("windows_terminal_combo")
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
                                self.terminal = None;
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
            ui.label(
                RichText::new("oxi uses your platform's default shell on this operating system.")
                    .size(FS_SMALL)
                    .color(c_text_muted()),
            );
        });
    }
}
