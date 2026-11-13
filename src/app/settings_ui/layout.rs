//! Settings page scaffolding: the sidebar/header/body layout, tab dispatch, and the
//! small pill/chip widgets shared across the other settings submodules.

use eframe::egui::{
    self, Align, Color32, CornerRadius, Frame, Layout, Margin, RichText, ScrollArea, Stroke, Ui,
};

use crate::theme::*;
use crate::ui::chrome::{ghost_button, primary_button, settings_nav_row};

use super::super::OxiApp;
use super::super::state::{SettingsExitAction, SettingsTab};
use crate::settings::LlmProviderKind;

const SETTINGS_CONTENT_MAX: f32 = 720.0;
const SAVE_BAR_H: f32 = 52.0;

/// One settings-sidebar section: a caption + the navigation rows under it.
struct SettingsNavGroup {
    caption: &'static str,
    items: &'static [(SettingsTab, &'static str, &'static str)],
}

/// Grouped navigation keeps related pages together instead of dumping every tab in one list.
const SETTINGS_NAV: &[SettingsNavGroup] = &[
    SettingsNavGroup {
        caption: "App",
        items: &[
            (SettingsTab::Appearance, ICON_APPEARANCE, "General"),
            (SettingsTab::Editor, ICON_FILE, "Editor & terminal"),
            (SettingsTab::Voice, ICON_MIC, "Voice"),
        ],
    },
    SettingsNavGroup {
        caption: "AI",
        items: &[
            (SettingsTab::Providers, ICON_PROVIDERS, "Models"),
            (SettingsTab::Agent, ICON_AGENT, "Agent tools"),
            (SettingsTab::Prompts, ICON_PROMPTS, "Prompts"),
        ],
    },
    SettingsNavGroup {
        caption: "Connections",
        items: &[
            (SettingsTab::GitHub, ICON_GIT, "Git & GitHub"),
            (SettingsTab::Server, ICON_SERVER, "Server & phone"),
        ],
    },
    SettingsNavGroup {
        caption: "",
        items: &[(SettingsTab::About, ICON_INFO, "About")],
    },
];

impl OxiApp {
    pub(crate) fn open_settings_page(&mut self) {
        if self.conv.settings_page.original.is_none() {
            self.conv.settings_page.original = Some(self.conv.settings.clone());
        }
        self.conv.settings_page.save_error = None;
        self.conv.settings_page.search.clear();
        self.conv.settings_page.open = true;
    }

    /// Open Settings, or leave it (asking about unsaved changes) when it is already open:
    /// the status-bar gear and Cmd/Ctrl+, both toggle.
    pub(crate) fn toggle_settings_page(&mut self) {
        if self.conv.settings_page.open {
            self.request_settings_exit(SettingsExitAction::BackToChat);
        } else {
            self.open_settings_page();
        }
    }

    pub(crate) fn settings_dirty(&self) -> bool {
        self.conv
            .settings_page
            .original
            .as_ref()
            .is_some_and(|original| original != &self.conv.settings)
    }

    fn continue_after_settings_exit(&mut self, action: SettingsExitAction) {
        match action {
            SettingsExitAction::NewChat => self.new_chat(),
            SettingsExitAction::BackToChat => {
                self.focus_active_view_next_frame();
            }
            SettingsExitAction::ToggleSidebarVisibility => {
                self.conv.sidebar.open = !self.conv.sidebar.open;
                self.focus_active_view_next_frame();
            }
            SettingsExitAction::ToggleSidebar => {
                let chats_on = self.conv.sidebar.open
                    && self.conv.sidebar.mode == crate::app::state::SidebarMode::Chats;
                let chat_covered = self.conv.editor.active.is_some() || self.conv.diff_view.open;
                if chats_on && chat_covered {
                    // The chat list is already showing but an editor/diff tab covers the chat:
                    // bring the chat forward instead of hiding the list.
                    self.reveal_chat_view();
                } else {
                    self.conv.sidebar.open = !chats_on;
                }
                if !chats_on {
                    self.conv.sidebar.mode = crate::app::state::SidebarMode::Chats;
                    self.reveal_chat_view();
                }
                self.focus_active_view_next_frame();
            }
            SettingsExitAction::ToggleExplorer => {
                let explorer_on = self.conv.sidebar.open
                    && self.conv.sidebar.mode == crate::app::state::SidebarMode::Explorer;
                self.conv.sidebar.open = !explorer_on;
                if !explorer_on {
                    self.conv.sidebar.mode = crate::app::state::SidebarMode::Explorer;
                    if self.conv.editor.active.is_none() {
                        self.conv.editor.active = self.conv.editor.hidden_active.take();
                    }
                }
                self.focus_active_view_next_frame();
            }
            SettingsExitAction::ShowSearch { toggle } => {
                let search_on = self.conv.sidebar.open
                    && self.conv.sidebar.mode == crate::app::state::SidebarMode::Search;
                if toggle && search_on {
                    self.conv.sidebar.open = false;
                    self.focus_active_view_next_frame();
                } else {
                    self.show_project_search();
                }
            }
            SettingsExitAction::ToggleTerminal => self.toggle_terminal(),
            SettingsExitAction::ToggleGitPanel => self.toggle_git_panel_tab(None),
            SettingsExitAction::ToggleGitBranches => {
                self.toggle_git_panel_tab(Some(crate::app::git_panel::GitTab::Branches))
            }
        }
    }

    pub(crate) fn request_settings_exit(&mut self, action: SettingsExitAction) {
        if self.conv.settings_page.open && self.settings_dirty() {
            self.conv.settings_page.exit_prompt = Some(action);
        } else {
            self.close_settings_page();
            self.continue_after_settings_exit(action);
        }
    }

    pub(crate) fn close_settings_page(&mut self) {
        self.conv.settings_page.original = None;
        self.conv.settings_page.exit_prompt = None;
        self.conv.settings_page.open = false;
        self.focus_active_view_next_frame();
    }

    pub(crate) fn save_settings_page(&mut self) {
        // Failures stay on the Settings page (inline banner) — routing them to the
        // chat's stream-error banner hid them from the view the user is looking at.
        if let Err(e) = self.conv.settings.save() {
            self.conv.settings_page.save_error = Some(format!("Could not save settings: {e}"));
            self.conv.settings_page.exit_prompt = None;
            return;
        }
        // Stay on the page: the save bar disappears, and Back/Esc leaves when done.
        self.conv.settings_page.save_error = None;
        self.conv.settings_page.original = Some(self.conv.settings.clone());
        self.conv.settings_page.exit_prompt = None;
    }

    /// Put back the settings as they were when the page opened (or last saved).
    fn discard_settings_changes(&mut self, ctx: &egui::Context) {
        let computer_use = self.conv.settings.computer_use;
        if let Some(original) = self.conv.settings_page.original.clone() {
            self.conv.settings = original;
        }
        self.conv.settings_page.save_error = None;
        self.reapply_appearance(ctx);
        // The switch took effect on connected servers as soon as it was flipped.
        if self.conv.settings.computer_use != computer_use {
            self.sync_computer_use(crate::app::remote::LOCAL_MACHINE, false);
            for host in self.connected_hosts() {
                self.sync_computer_use(&host, false);
            }
        }
    }

    fn save_settings_and_continue(&mut self, action: SettingsExitAction) {
        if let Err(e) = self.conv.settings.save() {
            self.conv.settings_page.save_error = Some(format!("Could not save settings: {e}"));
            self.conv.settings_page.exit_prompt = None;
            return;
        }
        self.conv.settings_page.save_error = None;
        self.conv.settings_page.original = Some(self.conv.settings.clone());
        self.close_settings_page();
        self.continue_after_settings_exit(action);
    }

    fn discard_settings_and_continue(&mut self, ctx: &egui::Context, action: SettingsExitAction) {
        if let Some(original) = self.conv.settings_page.original.take() {
            self.conv.settings = original;
        }
        self.reapply_appearance(ctx);
        self.conv.settings_page.exit_prompt = None;
        self.conv.settings_page.open = false;
        self.continue_after_settings_exit(action);
    }

    /// Re-apply theme + fonts + zoom after Cancel/Discard restores the settings snapshot.
    fn reapply_appearance(&mut self, ctx: &egui::Context) {
        crate::theme::set_active_fonts(crate::theme::FontSelection {
            ui: self.conv.settings.ui_font.clone(),
            mono: self.conv.settings.mono_font.clone(),
        });
        crate::theme::apply_theme(ctx, &self.conv.settings.theme_id);
        ctx.set_zoom_factor(self.conv.settings.ui_density.zoom_factor());
        // Keep Local HF UI fields in sync with restored settings.
        self.conv.local_models.runtime_port = self.conv.settings.local_hf.runtime_port;
        self.conv.local_models.context_size = self.conv.settings.local_hf.context_size;
        self.conv.local_models.gpu_layers = self.conv.settings.local_hf.gpu_layers;
    }

    pub(crate) fn render_settings_page(&mut self, ui: &mut Ui) {
        if self.conv.settings_page.original.is_none() {
            self.conv.settings_page.original = Some(self.conv.settings.clone());
        }
        // The chat sidebar's width (within reason), so opening Settings doesn't jump the
        // boundary between the sidebar and the page.
        const SIDEBAR_W_MIN: f32 = 200.0;
        const SIDEBAR_W_MAX: f32 = 360.0;
        let full_h = ui.available_height();

        ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
            ui.set_min_height(full_h);
            ui.spacing_mut().item_spacing.x = 0.0;

            let w = self.conv.sidebar.width.clamp(SIDEBAR_W_MIN, SIDEBAR_W_MAX);
            ui.allocate_ui_with_layout(
                egui::vec2(w, full_h),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    Frame::new()
                        .fill(c_bg_sidebar())
                        .inner_margin(Margin {
                            left: 12,
                            right: 10,
                            top: 12 + crate::ui::window_chrome::TITLEBAR_H as i8,
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
                        // The save bar only exists while there is something to save, so a
                        // clean page is all settings and no chrome.
                        let show_bar =
                            self.settings_dirty() || self.conv.settings_page.save_error.is_some();
                        let scroll_h = if show_bar {
                            (full_h - SAVE_BAR_H).max(80.0)
                        } else {
                            full_h
                        };
                        // One scroll position per tab: switching from a long page scrolled
                        // down used to open the next one halfway down.
                        ScrollArea::vertical()
                            .id_salt(("settings_page_scroll", self.conv.settings_page.tab))
                            .max_height(scroll_h)
                            .auto_shrink([false, false])
                            .scroll_bar_visibility(
                                egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded,
                            )
                            .show(ui, |ui| {
                                Frame::new()
                                    .inner_margin(Margin {
                                        left: 32,
                                        // The reserved 10px scroll gutter adds to this;
                                        // 22 + 10 keeps optical symmetry with the left.
                                        right: 22,
                                        top: 20 + crate::ui::window_chrome::TITLEBAR_H as i8,
                                        bottom: 48,
                                    })
                                    .show(ui, |ui| {
                                        // Centered like the chat column, and never wider than
                                        // the viewport.
                                        let w = SETTINGS_CONTENT_MAX.min(ui.available_width());
                                        let side = ((ui.available_width() - w) * 0.5).max(0.0);
                                        ui.horizontal(|ui| {
                                            ui.add_space(side);
                                            ui.vertical(|ui| {
                                                ui.set_width(w);
                                                // The page row above zeroes it for the
                                                // sidebar/body split; buttons need a gap.
                                                ui.spacing_mut().item_spacing.x = 8.0;
                                                self.render_settings_body(ui);
                                            });
                                        });
                                    });
                            });
                        if show_bar {
                            self.render_settings_save_bar(ui);
                        }
                        ui.expand_to_include_rect(ui.max_rect());
                    });
                    ui.expand_to_include_rect(ui.max_rect());
                },
            );
        });

        self.render_unsaved_settings_exit_prompt(ui.ctx());
    }

    fn render_unsaved_settings_exit_prompt(&mut self, ctx: &egui::Context) {
        let Some(action) = self.conv.settings_page.exit_prompt else {
            return;
        };

        // Dim the page so the dialog reads as a modal, not a floating card.
        // Clicking the dimmed backdrop dismisses (Stay); Escape is handled by the
        // global shortcut handler so it doesn't race the prompt being opened.
        if crate::ui::chrome::modal_backdrop(ctx, "unsaved_settings_exit_backdrop") {
            self.conv.settings_page.exit_prompt = None;
        }

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
                        ui.set_width(crate::ui::chrome::MODAL_W);
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
                            if primary_button(ui, "Save").on_hover_text("Enter").clicked() {
                                self.save_settings_and_continue(action);
                            }
                            if ghost_button(ui, "Discard", true).clicked() {
                                self.discard_settings_and_continue(ctx, action);
                            }
                            if ghost_button(ui, "Stay", false)
                                .on_hover_text("Esc")
                                .clicked()
                            {
                                self.conv.settings_page.exit_prompt = None;
                            }
                        });
                    });
            });

        if ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
            self.save_settings_and_continue(action);
        }
    }

    /// Bottom bar shown while settings differ from what is on disk: what changed, Discard,
    /// Save. A failed save shows its error here too, next to the button that retries it.
    fn render_settings_save_bar(&mut self, ui: &mut Ui) {
        let save_key = if cfg!(target_os = "macos") {
            "\u{2318}S"
        } else {
            "Ctrl+S"
        };
        let rect = egui::Rect::from_min_size(
            egui::pos2(ui.max_rect().left(), ui.cursor().min.y),
            egui::vec2(ui.max_rect().width(), SAVE_BAR_H),
        );
        ui.painter()
            .rect_filled(rect, CornerRadius::ZERO, c_bg_sidebar());
        ui.painter().hline(
            rect.x_range(),
            rect.top(),
            Stroke::new(1.0, c_border_subtle()),
        );
        // Lined up with the centered page content above it.
        let room = (rect.width() - 64.0).max(0.0);
        let w = SETTINGS_CONTENT_MAX.min(room);
        let left = rect.left() + 32.0 + (room - w) * 0.5;
        let inner = egui::Rect::from_min_max(
            egui::pos2(left, rect.top()),
            egui::pos2(left + w, rect.bottom()),
        );
        // Buttons first (right to left), then the message truncated to what is left: a long
        // save error laid out first ran under Save/Discard.
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(inner)
                .layout(Layout::right_to_left(Align::Center)),
            |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let error = self.conv.settings_page.save_error.clone();
                let dirty = self.settings_dirty();
                if ui
                    .add_enabled(dirty, crate::ui::chrome::primary_button_widget("Save"))
                    .on_hover_text(format!("Write settings.json ({save_key})"))
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
                    self.save_settings_page();
                }
                if dirty
                    && ghost_button(ui, "Discard", false)
                        .on_hover_text("Put back the settings as they were")
                        .clicked()
                {
                    self.discard_settings_changes(ui.ctx());
                }
                ui.add_space(8.0);
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    if error.is_some() {
                        ui.label(
                            RichText::new(ICON_WARNING)
                                .font(egui::FontId::new(FS_TINY, icon_font()))
                                .color(c_error_fg()),
                        );
                    } else {
                        let (dot, _) =
                            ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                        ui.painter().circle_filled(dot.center(), 3.5, c_accent());
                    }
                    let text = error.unwrap_or_else(|| "You have unsaved changes".to_owned());
                    ui.add(
                        egui::Label::new(RichText::new(text).size(FS_SMALL).color(c_text()))
                            .truncate(),
                    );
                });
            },
        );
        ui.advance_cursor_after_rect(rect);
    }

    fn render_settings_sidebar(&mut self, ui: &mut Ui) {
        ui.set_min_width(ui.max_rect().width());

        // Same spot and look as the chat sidebar's "New chat": the way out comes first.
        let back_label = if self.conv.editor.active.is_some() || self.conv.diff_view.open {
            "Back to editor"
        } else {
            "Back to chat"
        };
        let back = crate::ui::chrome::flat_button_icon(
            ui,
            ICON_CHEVRON_LEFT,
            back_label,
            FS_SMALL,
            egui::vec2(ui.available_width(), 30.0),
            c_text(),
        )
        .on_hover_text("Asks before discarding unsaved changes");
        if back.rect.width() >= 170.0 {
            ui.painter().text(
                back.rect.right_center() - egui::vec2(8.0, 0.0),
                egui::Align2::RIGHT_CENTER,
                "Esc",
                egui::FontId::proportional(FS_TINY),
                c_text_faint(),
            );
        }
        if back.clicked() {
            self.request_settings_exit(SettingsExitAction::BackToChat);
        }
        ui.add_space(SPACE_SM);

        let search = crate::ui::chrome::sidebar_text_field(
            ui,
            &mut self.conv.settings_page.search,
            "Search settings\u{2026}",
        );
        if std::mem::take(&mut self.conv.settings_page.focus_search) {
            search.request_focus();
        }
        let query = self.conv.settings_page.search.trim().to_owned();
        let enter = search.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        ui.add_space(SPACE_MD);

        if query.is_empty() {
            ScrollArea::vertical()
                .id_salt("settings_nav_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| self.render_settings_nav(ui));
        } else {
            let hits = super::search::search(&query);
            let mut chosen = enter.then(|| hits.first().map(|hit| hit.target)).flatten();
            ScrollArea::vertical()
                .id_salt("settings_search_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if hits.is_empty() {
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new("No matching settings")
                                .size(FS_SMALL)
                                .color(c_text_faint()),
                        );
                    }
                    for (i, hit) in hits.iter().enumerate() {
                        let crumb = match hit.card {
                            Some(card) => format!("{} \u{203a} {card}", hit.page),
                            None => hit.page.to_owned(),
                        };
                        if settings_search_row(ui, hit.label, &crumb, i == 0)
                            .on_hover_text(if i == 0 { "Enter" } else { "" })
                            .clicked()
                        {
                            chosen = Some(hit.target);
                        }
                    }
                });
            if let Some(target) = chosen {
                self.go_to_settings_target(ui.ctx(), target);
            }
        }
    }

    fn render_settings_nav(&mut self, ui: &mut Ui) {
        for (group_i, group) in SETTINGS_NAV.iter().enumerate() {
            if group_i > 0 {
                ui.add_space(SPACE_SM);
            }
            if !group.caption.is_empty() {
                crate::ui::chrome::sidebar_caption(ui, group.caption);
                ui.add_space(2.0);
            }
            for (tab, icon, label) in group.items {
                let selected = self.conv.settings_page.tab == *tab;
                let response = settings_nav_row(ui, icon, label, selected);
                if response.clicked() {
                    self.conv.settings_page.tab = *tab;
                }
                ui.add_space(1.0);
            }
        }
    }

    fn go_to_settings_target(
        &mut self,
        ctx: &egui::Context,
        target: super::search::SettingsTarget,
    ) {
        use super::search::SettingsTarget;
        match target {
            SettingsTarget::Page { tab, card } => {
                self.conv.settings_page.tab = tab;
                if let Some(card) = card {
                    crate::ui::chrome::request_settings_reveal(ctx, card);
                }
            }
            SettingsTarget::Provider { kind, card } => {
                // A field every provider form has ("API key", "Base URL") opens the form already
                // showing, or the active provider's when that is the Router, which has none.
                let kind = kind.unwrap_or_else(|| {
                    [
                        self.conv.settings_page.provider_tab,
                        self.conv.settings.active_provider,
                    ]
                    .into_iter()
                    .find(|kind| *kind != LlmProviderKind::Router)
                    .unwrap_or(LlmProviderKind::OpenAi)
                });
                self.conv.settings_page.tab = SettingsTab::Providers;
                self.conv.settings_page.provider_tab = kind;
                if let Some(card) = card {
                    crate::ui::chrome::request_settings_reveal(ctx, card);
                }
            }
        }
        self.conv.settings_page.search.clear();
    }

    fn render_settings_body(&mut self, ui: &mut Ui) {
        match self.conv.settings_page.tab {
            SettingsTab::Providers => self.render_settings_providers_panel(ui),
            SettingsTab::Agent => self.render_settings_agent_panel(ui),
            SettingsTab::GitHub => self.render_settings_github_panel(ui),
            SettingsTab::Prompts => self.render_settings_prompts_panel(ui),
            SettingsTab::Voice => self.render_settings_voice_panel(ui),
            SettingsTab::Editor => self.render_settings_editor_panel(ui),
            SettingsTab::Appearance => self.render_settings_appearance_panel(ui),
            SettingsTab::Server => self.render_settings_server_panel(ui),
            SettingsTab::About => self.render_settings_about_panel(ui),
        }
    }
}

/// Small "Active" / "Signed in" pill.
pub(super) fn active_pill(ui: &mut Ui, text: &str) {
    let tint = |alpha| {
        Color32::from_rgba_unmultiplied(c_accent().r(), c_accent().g(), c_accent().b(), alpha)
    };
    status_pill(ui, text, c_accent(), tint(32), tint(90));
}

pub(super) fn inactive_pill(ui: &mut Ui, text: &str) {
    status_pill(
        ui,
        text,
        c_text_muted(),
        c_bg_elevated_2(),
        c_border_subtle(),
    );
}

/// Painted at a fixed size: a Frame inside a right-to-left row stretches to the row height.
fn status_pill(ui: &mut Ui, text: &str, fg: Color32, fill: Color32, stroke: Color32) {
    let galley =
        ui.painter()
            .layout_no_wrap(text.to_string(), egui::FontId::proportional(FS_TINY), fg);
    let size = egui::vec2(galley.size().x + 18.0, 20.0);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    ui.painter().rect(
        rect,
        egui::CornerRadius::same(10),
        fill,
        Stroke::new(1.0, stroke),
        egui::StrokeKind::Inside,
    );
    ui.painter()
        .galley(rect.center() - galley.size() * 0.5, galley, fg);
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
    // Center each galley on its own: the icon font's line height differs from the UI font's,
    // so sharing the label's top left the check mark riding high.
    let icon_top = rect.center().y - icon_galley.rect.height() * 0.5;
    let label_top = rect.center().y - label_galley.rect.height() * 0.5;
    let icon_x = rect.left() + pad.x;
    let label_x = icon_x + icon_galley.rect.width() + icon_gap;
    ui.painter()
        .galley(egui::pos2(icon_x, icon_top), icon_galley, icon_col);
    ui.painter()
        .galley(egui::pos2(label_x, label_top), label_galley, text_col);
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

/// One Settings search result: the setting's name over a muted "Page › Card" breadcrumb.
fn settings_search_row(ui: &mut Ui, label: &str, crumb: &str, first: bool) -> egui::Response {
    let w = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(w, 42.0), egui::Sense::click());
    let hovered = response.hovered() || response.has_focus();
    if hovered || first {
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(RADIUS_ROW),
            if hovered {
                c_row_hover()
            } else {
                c_row_active()
            },
        );
    }
    crate::ui::chrome::keyboard_focus_ring(ui, &response, CornerRadius::same(RADIUS_ROW));
    let text_w = rect.width() - 20.0;
    let one_line = |text: &str, size: f32, color: Color32| {
        let mut job = egui::text::LayoutJob::simple_singleline(
            text.to_owned(),
            egui::FontId::proportional(size),
            color,
        );
        job.wrap = egui::text::TextWrapping::truncate_at_width(text_w);
        ui.painter().layout_job(job)
    };
    let label_galley = one_line(label, FS_SMALL, c_text());
    let crumb_galley = one_line(crumb, FS_TINY, c_text_faint());
    let left = rect.left() + 10.0;
    ui.painter()
        .galley(egui::pos2(left, rect.top() + 5.0), label_galley, c_text());
    ui.painter().galley(
        egui::pos2(left, rect.bottom() - 5.0 - crumb_galley.size().y),
        crumb_galley,
        c_text_faint(),
    );
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    ui.add_space(2.0);
    response
}
