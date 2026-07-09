//! Settings page scaffolding: the sidebar/header/body layout, tab dispatch, and the
//! small pill/chip widgets shared across the other settings submodules.

use eframe::egui::{
    self, Align, Color32, CornerRadius, Frame, Layout, Margin, RichText, ScrollArea, Stroke, Ui,
};

use crate::theme::*;
use crate::ui::chrome::{ghost_button, primary_button, settings_caption, settings_nav_row};

use super::super::OxiApp;
use super::super::state::{SettingsExitAction, SettingsTab};

const SETTINGS_CONTENT_MAX: f32 = 820.0;

/// One settings-sidebar section: a caption + the navigation rows under it.
struct SettingsNavGroup {
    caption: &'static str,
    items: &'static [(SettingsTab, &'static str, &'static str)],
}

/// Grouped navigation keeps related pages together instead of dumping every tab in one list.
const SETTINGS_NAV: &[SettingsNavGroup] = &[
    SettingsNavGroup {
        caption: "AI",
        items: &[
            (SettingsTab::Providers, ICON_PROVIDERS, "Models & providers"),
            (SettingsTab::Prompts, ICON_PROMPTS, "Prompts"),
        ],
    },
    SettingsNavGroup {
        caption: "Agent",
        items: &[(SettingsTab::Agent, ICON_AGENT, "Tools & safety")],
    },
    SettingsNavGroup {
        caption: "App",
        items: &[
            (SettingsTab::Voice, ICON_MIC, "Voice"),
            (SettingsTab::Appearance, ICON_APPEARANCE, "Appearance"),
            (SettingsTab::About, ICON_INFO, "About"),
        ],
    },
];

impl OxiApp {
    pub(crate) fn open_settings_page(&mut self) {
        if self.conv.settings_original.is_none() {
            self.conv.settings_original = Some(self.conv.settings.clone());
        }
        self.conv.settings_open = true;
    }

    fn settings_dirty(&self) -> bool {
        self.conv
            .settings_original
            .as_ref()
            .is_some_and(|original| original != &self.conv.settings)
    }

    fn continue_after_settings_exit(&mut self, action: SettingsExitAction) {
        match action {
            SettingsExitAction::BackToChat => {
                self.conv.focus_chat_input_next_frame = true;
            }
            SettingsExitAction::ToggleSidebar => {
                self.conv.sidebar_open = !self.conv.sidebar_open;
                self.conv.focus_chat_input_next_frame = true;
            }
            SettingsExitAction::ToggleTerminal => self.toggle_terminal(),
            SettingsExitAction::ToggleGitChanges => {
                self.toggle_git_panel_tab(crate::app::git_panel::GitTab::Changes)
            }
            SettingsExitAction::ToggleGitBranches => {
                self.toggle_git_panel_tab(crate::app::git_panel::GitTab::Branches)
            }
        }
    }

    pub(crate) fn request_settings_exit(&mut self, action: SettingsExitAction) {
        if self.conv.settings_open && self.settings_dirty() {
            self.conv.settings_exit_prompt = Some(action);
        } else {
            self.close_settings_page();
            self.continue_after_settings_exit(action);
        }
    }

    pub(crate) fn close_settings_page(&mut self) {
        self.conv.settings_original = None;
        self.conv.settings_exit_prompt = None;
        self.conv.settings_open = false;
        self.conv.focus_chat_input_next_frame = true;
    }

    pub(crate) fn save_settings_page(&mut self) {
        if let Err(e) = self.conv.settings.save() {
            self.run_state_mut(self.active_session_key()).stream_error =
                Some(format!("Save settings: {e}"));
            return;
        }
        self.conv.settings_original = Some(self.conv.settings.clone());
        self.conv.settings_exit_prompt = None;
        self.conv.settings_open = false;
        self.conv.focus_chat_input_next_frame = true;
    }

    fn save_settings_and_continue(&mut self, action: SettingsExitAction) {
        if let Err(e) = self.conv.settings.save() {
            self.run_state_mut(self.active_session_key()).stream_error =
                Some(format!("Save settings: {e}"));
            return;
        }
        self.conv.settings_original = Some(self.conv.settings.clone());
        self.close_settings_page();
        self.continue_after_settings_exit(action);
    }

    pub(crate) fn cancel_settings_page(&mut self) {
        if let Some(original) = self.conv.settings_original.take() {
            self.conv.settings = original;
        }
        self.conv.settings_exit_prompt = None;
        self.conv.settings_open = false;
        self.conv.focus_chat_input_next_frame = true;
    }

    fn discard_settings_and_continue(&mut self, action: SettingsExitAction) {
        if let Some(original) = self.conv.settings_original.take() {
            self.conv.settings = original;
        }
        self.conv.settings_exit_prompt = None;
        self.conv.settings_open = false;
        self.continue_after_settings_exit(action);
    }

    pub(crate) fn render_settings_page(&mut self, ui: &mut Ui) {
        if self.conv.settings_original.is_none() {
            self.conv.settings_original = Some(self.conv.settings.clone());
        }
        const SIDEBAR_W_MIN: f32 = 180.0;
        const SIDEBAR_W_MAX: f32 = 320.0;
        let full_h = ui.available_height();

        ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
            ui.set_min_height(full_h);
            ui.spacing_mut().item_spacing.x = 0.0;

            let w = self.conv.sidebar_width.clamp(SIDEBAR_W_MIN, SIDEBAR_W_MAX);
            ui.allocate_ui_with_layout(
                egui::vec2(w, full_h),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    Frame::new()
                        .fill(c_bg_sidebar())
                        .inner_margin(Margin {
                            left: 12,
                            right: 10,
                            top: 12,
                            bottom: 12,
                        })
                        .show(ui, |ui| {
                            ui.set_min_width(ui.max_rect().width());
                            ui.set_min_height(ui.max_rect().height());
                            self.render_settings_sidebar(ui);
                        });
                    ui.expand_to_include_rect(ui.max_rect());
                },
            );

            let boundary_x = ui.cursor().min.x;
            ui.painter().vline(
                boundary_x,
                egui::Rangef::new(ui.min_rect().top(), ui.min_rect().top() + full_h),
                Stroke::new(1.0, c_border_subtle()),
            );
            ui.add_space(SIDEBAR_RESIZE_SEP_W);

            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), full_h),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    Frame::new().fill(c_bg_main()).show(ui, |ui| {
                        self.render_settings_header(ui);
                        ScrollArea::vertical()
                            .id_salt("settings_page_scroll")
                            .auto_shrink([false, false])
                            .scroll_bar_visibility(
                                egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded,
                            )
                            .show(ui, |ui| {
                                Frame::new()
                                    .inner_margin(Margin {
                                        left: 36,
                                        // The reserved 10px scroll gutter adds to this;
                                        // 26 + 10 keeps optical symmetry with the left.
                                        right: 26,
                                        top: 24,
                                        bottom: 48,
                                    })
                                    .show(ui, |ui| {
                                        ui.set_max_width(SETTINGS_CONTENT_MAX);
                                        self.render_settings_body(ui);
                                    });
                            });
                        ui.expand_to_include_rect(ui.max_rect());
                    });
                    ui.expand_to_include_rect(ui.max_rect());
                },
            );
        });

        self.render_unsaved_settings_exit_prompt(ui.ctx());
    }

    fn render_unsaved_settings_exit_prompt(&mut self, ctx: &egui::Context) {
        let Some(action) = self.conv.settings_exit_prompt else {
            return;
        };

        // Dim the page so the dialog reads as a modal, not a floating card.
        let screen = ctx.content_rect();
        egui::Area::new(egui::Id::new("unsaved_settings_exit_backdrop"))
            .order(egui::Order::Foreground)
            .fixed_pos(screen.min)
            .interactable(true)
            .show(ctx, |ui| {
                let (rect, response) = ui.allocate_exact_size(screen.size(), egui::Sense::click());
                ui.painter()
                    .rect_filled(rect, 0.0, Color32::from_black_alpha(140));
                // Clicking the dimmed backdrop dismisses (Stay).
                if response.clicked() {
                    self.conv.settings_exit_prompt = None;
                }
            });

        egui::Area::new(egui::Id::new("unsaved_settings_exit_prompt"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                Frame::new()
                    .fill(c_bg_elevated())
                    .stroke(Stroke::new(1.0, c_border()))
                    .corner_radius(RADIUS_CHIP)
                    .inner_margin(Margin::same(16))
                    .show(ui, |ui| {
                        ui.set_width(300.0);
                        ui.label(
                            RichText::new("Unsaved changes")
                                .size(FS_BODY)
                                .color(c_text())
                                .strong(),
                        );
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new("Save changes before leaving Settings?")
                                .size(FS_SMALL)
                                .color(c_text_muted()),
                        );
                        ui.add_space(14.0);

                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if primary_button(ui, "Save").clicked() {
                                self.save_settings_and_continue(action);
                            }
                            if ghost_button(ui, "Discard", true).clicked() {
                                self.discard_settings_and_continue(action);
                            }
                            if ghost_button(ui, "Stay", false).clicked() {
                                self.conv.settings_exit_prompt = None;
                            }
                        });
                    });
            });
    }

    fn settings_tab_label(tab: SettingsTab) -> &'static str {
        match tab {
            SettingsTab::Providers => "Models & providers",
            SettingsTab::Agent => "Tools & safety",
            SettingsTab::Prompts => "Prompts",
            SettingsTab::Voice => "Voice",
            SettingsTab::Appearance => "Appearance",
            SettingsTab::About => "About",
        }
    }

    fn render_settings_header(&mut self, ui: &mut Ui) {
        Frame::new()
            .fill(c_bg_main())
            .inner_margin(Margin {
                left: 36,
                right: 24,
                top: 16,
                bottom: 14,
            })
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new("Settings")
                                .size(FS_H1)
                                .color(c_text())
                                .strong(),
                        );
                        ui.add_space(2.0);
                        ui.label(
                            RichText::new(format!(
                                "Settings › {}",
                                Self::settings_tab_label(self.conv.settings_tab)
                            ))
                            .size(FS_TINY)
                            .color(c_text_muted()),
                        );
                    });

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let dirty = self.settings_dirty();
                        if ui
                            .add_enabled(dirty, crate::ui::chrome::primary_button_widget("Save"))
                            .on_hover_text("Write settings.json and return to chat")
                            .on_hover_cursor(egui::CursorIcon::PointingHand)
                            .clicked()
                        {
                            self.save_settings_page();
                        }
                        if ghost_button(ui, "Cancel", false)
                            .on_hover_text("Discard unsaved changes and return to chat")
                            .clicked()
                        {
                            self.cancel_settings_page();
                        }
                        if dirty {
                            ui.label(
                                RichText::new("Unsaved")
                                    .size(FS_TINY)
                                    .color(c_accent())
                                    .strong(),
                            );
                        }
                    });
                });
            });
        ui.painter().hline(
            ui.min_rect().x_range(),
            ui.cursor().min.y,
            Stroke::new(1.0, c_border_subtle()),
        );
    }

    fn render_settings_sidebar(&mut self, ui: &mut Ui) {
        ui.set_min_width(ui.max_rect().width());

        ui.add_space(4.0);

        for (group_i, group) in SETTINGS_NAV.iter().enumerate() {
            if group_i > 0 {
                ui.add_space(12.0);
            }
            settings_caption(ui, group.caption);
            ui.add_space(4.0);
            for (tab, icon, label) in group.items {
                let selected = self.conv.settings_tab == *tab;
                let response = settings_nav_row(ui, icon, label, selected);
                if response.clicked() {
                    self.conv.settings_tab = *tab;
                }
                ui.add_space(2.0);
            }
        }

        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            ui.add_space(4.0);
            ui.label(
                RichText::new(format!(
                    "~/{}",
                    crate::settings::AppSettings::config_path()
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("settings.json")
                ))
                .size(FS_TINY)
                .color(c_text_faint())
                .monospace(),
            );
            ui.add_space(2.0);
            ui.label(
                RichText::new("Settings file")
                    .size(FS_TINY)
                    .color(c_text_faint()),
            );
            ui.add_space(2.0);
        });
    }

    fn render_settings_body(&mut self, ui: &mut Ui) {
        match self.conv.settings_tab {
            SettingsTab::Providers => self.render_settings_providers_panel(ui),
            SettingsTab::Agent => self.render_settings_agent_panel(ui),
            SettingsTab::Prompts => self.render_settings_prompts_panel(ui),
            SettingsTab::Voice => self.render_settings_voice_panel(ui),
            SettingsTab::Appearance => self.render_settings_appearance_panel(ui),
            SettingsTab::About => self.render_settings_about_panel(ui),
        }
    }
}

/// Small "Active" / "Signed in" pill.
pub(super) fn active_pill(ui: &mut Ui, text: &str) {
    Frame::new()
        .fill(Color32::from_rgba_unmultiplied(
            c_accent().r(),
            c_accent().g(),
            c_accent().b(),
            32,
        ))
        .stroke(Stroke::new(
            1.0,
            Color32::from_rgba_unmultiplied(c_accent().r(), c_accent().g(), c_accent().b(), 90),
        ))
        .corner_radius(999.0)
        .inner_margin(Margin::symmetric(10, 3))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(FS_TINY).color(c_accent()).strong());
        });
}

pub(super) fn inactive_pill(ui: &mut Ui, text: &str) {
    Frame::new()
        .fill(c_bg_elevated_2())
        .stroke(Stroke::new(1.0, c_border_subtle()))
        .corner_radius(999.0)
        .inner_margin(Margin::symmetric(10, 3))
        .show(ui, |ui| {
            ui.label(
                RichText::new(text)
                    .size(FS_TINY)
                    .color(c_text_muted())
                    .strong(),
            );
        });
}

pub(super) fn tool_chip(ui: &mut Ui, name: &str, enabled: bool) -> egui::Response {
    let icon = if enabled { ICON_CHECK } else { "·" };
    let label_fid = egui::FontId::proportional(FS_SMALL);
    let icon_fid = egui::FontId::new(FS_SMALL, icon_font());
    // PLACEHOLDER so the paint-time colors below actually apply — a galley laid out with a
    // concrete color ignores the fallback passed to `painter().galley()`.
    let label_galley =
        ui.painter()
            .layout_no_wrap(name.to_string(), label_fid.clone(), Color32::PLACEHOLDER);
    let icon_galley = ui
        .painter()
        .layout_no_wrap(icon.to_string(), icon_fid, Color32::PLACEHOLDER);

    let pad = egui::vec2(12.0, 6.0);
    let icon_gap = 8.0;
    let size = egui::vec2(
        icon_galley.rect.width() + icon_gap + label_galley.rect.width() + pad.x * 2.0,
        label_galley.rect.height().max(icon_galley.rect.height()) + pad.y * 2.0,
    );
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let hovered = response.hovered();
    let (fill, stroke_col, text_col) = if enabled && hovered {
        (c_row_active(), c_border(), c_text())
    } else if enabled {
        (c_row_active(), c_border_subtle(), c_text())
    } else if hovered {
        (c_row_hover(), c_border_subtle(), c_text_muted())
    } else {
        (c_bg_elevated_2(), c_border_subtle(), c_text_muted())
    };
    let r = CornerRadius::same(255);
    ui.painter().rect_filled(rect, r, fill);
    ui.painter().rect_stroke(
        rect,
        r,
        Stroke::new(1.0, stroke_col),
        egui::StrokeKind::Middle,
    );
    let icon_col = if enabled { c_accent() } else { c_text_faint() };
    let top = rect.center().y - label_galley.rect.height() * 0.5;
    let icon_x = rect.left() + pad.x;
    let label_x = icon_x + icon_galley.rect.width() + icon_gap;
    ui.painter()
        .galley(egui::pos2(icon_x, top), icon_galley, icon_col);
    ui.painter()
        .galley(egui::pos2(label_x, top), label_galley, text_col);
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}
