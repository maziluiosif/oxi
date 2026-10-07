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

        // Every provider is visible at once, grouped by where it runs; the active one keeps a dot
        // and hosted APIs that already have credentials get a check. In a narrow window the chip
        // wall would push the provider's own settings below the fold, so it becomes a dropdown.
        let active = self.conv.settings.active_provider;
        let configured = self.cached_configured_providers(ui.ctx());
        if ui.available_width() < 640.0 {
            self.render_provider_dropdown(ui, active, &configured);
        } else {
            for (group_label, providers) in PROVIDER_GROUPS {
                settings_caption(ui, group_label);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                    for &provider in *providers {
                        let selected = provider == self.conv.settings_page.provider_tab;
                        let state = chip_state(provider, &configured);
                        let response = provider_chip(
                            ui,
                            provider.label(),
                            selected,
                            provider == active,
                            state,
                        );
                        if response.clicked() {
                            self.conv.settings_page.provider_tab = provider;
                        }
                    }
                });
                ui.add_space(10.0);
            }
        }

        let provider = self.conv.settings_page.provider_tab;
        ui.add_space(4.0);
        card_frame().show(ui, |ui| {
            // Action first (right-aligned), so the description wraps in whatever is left
            // instead of running underneath it in a narrow window.
            let width = ui.available_width();
            ui.allocate_ui_with_layout(
                egui::vec2(width, 0.0),
                Layout::right_to_left(Align::Center),
                |ui| {
                    if self.conv.settings.active_provider == provider {
                        super::layout::active_pill(ui, "Active");
                    } else if crate::ui::chrome::primary_button(ui, "Make active")
                        .on_hover_text("Use this provider for new chats")
                        .clicked()
                    {
                        self.conv.settings.active_provider = provider;
                    }
                    ui.add_space(12.0);
                    ui.with_layout(Layout::top_down(Align::Min), |ui| {
                        ui.label(
                            RichText::new(provider.label())
                                .size(FS_H3)
                                .color(c_text_strong())
                                .strong(),
                        );
                        ui.add_space(2.0);
                        ui.add(
                            egui::Label::new(
                                RichText::new(provider_blurb(provider))
                                    .size(FS_TINY)
                                    .color(c_text_muted()),
                            )
                            .wrap(),
                        );
                    });
                },
            );
        });
        ui.add_space(12.0);

        if provider == LlmProviderKind::Router {
            self.render_router_settings(ui);
            return;
        }
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

impl OxiApp {
    /// Providers with usable credentials, refreshed at most every 2 s: the check reads the
    /// OAuth store (keychain-backed), far too slow to repeat every frame.
    pub(super) fn cached_configured_providers(&self, ctx: &egui::Context) -> Vec<LlmProviderKind> {
        let id = egui::Id::new("settings_configured_providers");
        let now = ctx.input(|i| i.time);
        if let Some((at, kinds)) = ctx.data(|d| d.get_temp::<(f64, Vec<LlmProviderKind>)>(id))
            && now - at < 2.0
        {
            return kinds;
        }
        let oauth = crate::oauth::load_oauth_store();
        let kinds = self.conv.settings.configured_provider_kinds(&oauth);
        ctx.data_mut(|d| d.insert_temp(id, (now, kinds.clone())));
        kinds
    }

    fn render_provider_dropdown(
        &mut self,
        ui: &mut Ui,
        active: LlmProviderKind,
        configured: &[LlmProviderKind],
    ) {
        settings_caption(ui, "Provider");
        let current = self.conv.settings_page.provider_tab;
        egui::ComboBox::from_id_salt("settings_provider_dropdown")
            .selected_text(dropdown_label(current, active, configured))
            .width(ui.available_width().min(360.0))
            .height(420.0)
            .show_ui(ui, |ui| {
                for (index, (group_label, providers)) in PROVIDER_GROUPS.iter().enumerate() {
                    if index > 0 {
                        ui.separator();
                    }
                    ui.label(
                        RichText::new(*group_label)
                            .size(FS_TINY)
                            .color(c_text_faint()),
                    );
                    for &provider in *providers {
                        ui.selectable_value(
                            &mut self.conv.settings_page.provider_tab,
                            provider,
                            dropdown_label(provider, active, configured),
                        );
                    }
                }
            });
        ui.add_space(10.0);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ChipState {
    /// Local servers and agents that handle their own sign-in: nothing to show.
    Neutral,
    /// A hosted API with a key / OAuth token available.
    Ready,
    /// A hosted API with no credentials yet.
    NeedsKey,
}

fn chip_state(provider: LlmProviderKind, configured: &[LlmProviderKind]) -> ChipState {
    let hosted = PROVIDER_GROUPS
        .iter()
        .find(|(label, _)| *label == "Hosted APIs")
        .is_some_and(|(_, kinds)| kinds.contains(&provider));
    // Azure has no env fallback to detect, so it always reports configured; don't claim it.
    if !hosted || provider == LlmProviderKind::AzureOpenAi {
        ChipState::Neutral
    } else if configured.contains(&provider) {
        ChipState::Ready
    } else {
        ChipState::NeedsKey
    }
}

fn dropdown_label(
    provider: LlmProviderKind,
    active: LlmProviderKind,
    configured: &[LlmProviderKind],
) -> String {
    let mut label = provider.label().to_string();
    if provider == active {
        label.push_str("  · active");
    } else if chip_state(provider, configured) == ChipState::NeedsKey {
        label.push_str("  · no key");
    }
    label
}

/// Selectable provider pill; a green dot marks the provider new chats use, a check a hosted API
/// that has credentials, and a hosted API without them is dimmed.
fn provider_chip(
    ui: &mut Ui,
    label: &str,
    selected: bool,
    active: bool,
    state: ChipState,
) -> egui::Response {
    const DOT: f32 = 6.0;
    let galley = ui.painter().layout_no_wrap(
        label.to_string(),
        FontId::proportional(FS_SMALL),
        Color32::PLACEHOLDER,
    );
    let check = (state == ChipState::Ready).then(|| {
        ui.painter().layout_no_wrap(
            ICON_CHECK.to_string(),
            FontId::new(FS_TINY, icon_font()),
            Color32::PLACEHOLDER,
        )
    });
    let pad_x = 12.0;
    let dot_w = if active { DOT + 6.0 } else { 0.0 };
    let check_w = check.as_ref().map_or(0.0, |g| g.size().x + 6.0);
    let size = egui::vec2(galley.size().x + pad_x * 2.0 + dot_w + check_w, 30.0);
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
    } else if state == ChipState::NeedsKey {
        (c_bg_elevated(), c_border_subtle(), c_text_faint())
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
    let label_w = galley.size().x;
    ui.painter().galley(
        egui::pos2(x, rect.center().y - galley.size().y * 0.5),
        galley,
        text,
    );
    if let Some(check) = check {
        let cx = x + label_w + 6.0;
        ui.painter().galley(
            egui::pos2(cx, rect.center().y - check.size().y * 0.5),
            check,
            c_success(),
        );
    }
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let hint = match (active, state) {
        (true, _) => Some("Active for new chats"),
        (false, ChipState::Ready) => Some("Credentials found"),
        (false, ChipState::NeedsKey) => Some("No API key yet — select it to add one"),
        (false, ChipState::Neutral) => None,
    };
    match hint {
        Some(hint) => response.on_hover_text(hint),
        None => response,
    }
}
