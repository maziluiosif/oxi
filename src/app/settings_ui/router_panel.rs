//! Settings → Providers → Router: strategy, guard rails, live quota / spend, and the per-provider
//! candidate setup the router picks from.

use super::super::OxiApp;
use crate::router::{ledger, quota};
use crate::settings::{Billing, LlmProviderKind, RouterStrategy};
use crate::theme::*;
use crate::ui::chrome::{
    card_frame, field_hint, field_label, ghost_button_icon, hairline, settings_card_header,
    settings_text_field_width,
};
use eframe::egui::{self, Align, Layout, RichText, Ui};

/// Re-probe quota at most this often while the page is open.
const PAGE_REFRESH_SECS: f64 = 120.0;

impl OxiApp {
    pub(super) fn render_router_settings(&mut self, ui: &mut Ui) {
        let configured: Vec<LlmProviderKind> = self
            .cached_configured_providers(ui.ctx())
            .into_iter()
            .filter(|k| *k != LlmProviderKind::Router)
            .collect();
        self.maybe_auto_refresh_quota(ui.ctx());

        self.render_router_strategy_card(ui);
        ui.add_space(12.0);
        self.render_router_quota_card(ui, &configured);
        ui.add_space(12.0);
        self.render_router_candidates_card(ui, &configured);
    }

    fn render_router_strategy_card(&mut self, ui: &mut Ui) {
        card_frame().show(ui, |ui| {
            settings_card_header(
                ui,
                "How it decides",
                Some(
                    "Each turn is classified as light, standard or heavy; candidates are scored on quality for that tier, subscription quota left, estimated cost, and staying on the same model for prompt caching. The choice and the reason appear above every reply.",
                ),
            );

            ui.checkbox(&mut self.conv.settings.router.use_jev, "Use Jev for multilingual task classification (default)");
            field_hint(ui, "Uses your OpenRouter API key from Settings or OPENROUTER_API_KEY. Sends the current message and limited recent text context to Jev 1.13; classification is billed to OpenRouter and included in the monthly budget. Without a key, or on errors / uncertain answers, uses a conservative local fallback.");

            field_label(ui, "Default strategy (new chats; each chat can change it in the composer)");
            let router_cfg = self.conv.settings.provider_mut(LlmProviderKind::Router);
            let current = RouterStrategy::from_id(&router_cfg.model_id);
            egui::ComboBox::from_id_salt("router_default_strategy")
                .selected_text(current.label())
                .width(240.0)
                .show_ui(ui, |ui| {
                    for s in RouterStrategy::ALL {
                        if ui
                            .selectable_label(s == current, s.label())
                            .on_hover_text(s.description())
                            .clicked()
                        {
                            router_cfg.model_id = s.id().to_string();
                        }
                    }
                });
            field_hint(ui, current.description());

            let router = &mut self.conv.settings.router;
            field_label(ui, "Subscription reserve");
            ui.add(
                egui::Slider::new(&mut router.reserve_pct, 0..=50)
                    .suffix("%")
                    .text("kept for heavy tasks"),
            );
            field_hint(
                ui,
                "Light and standard tasks stop using a subscription once less than this share of its usage window is left.",
            );

            field_label(ui, "Monthly pay-per-use budget");
            ui.horizontal(|ui| {
                ui.add(
                    egui::DragValue::new(&mut router.monthly_budget_usd)
                        .range(0.0..=10_000.0)
                        .speed(1.0)
                        .prefix("$"),
                );
                ui.label(
                    RichText::new(format!(
                        "spent this month (est.): ${:.2}",
                        ledger::spend_this_month()
                    ))
                    .size(FS_TINY)
                    .color(c_text_muted()),
                );
            });
            field_hint(ui, "0 = no cap. Pay-per-use providers are skipped once the cap would be exceeded.");

            ui.add_space(8.0);
            ui.checkbox(
                &mut router.failover,
                RichText::new("Continue with another provider when quota is reached")
                    .size(FS_SMALL)
                    .color(c_text()),
            )
            .on_hover_text(
                "Continue with the next eligible API or ACP provider after a quota failure, preserving reported progress and approval policies.",
            );
            field_hint(ui, "Applies to API and ACP providers in Router mode. At most two switches per request; unfinished tools or pending approvals stop continuation. An explicit ‘only’ provider instruction disables switching.");
        });
    }

    fn render_router_quota_card(&mut self, ui: &mut Ui, configured: &[LlmProviderKind]) {
        card_frame().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("Usage & quota")
                        .size(FS_BODY)
                        .color(c_text())
                        .strong(),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if quota::is_refreshing() {
                        ui.spinner();
                    } else if ghost_button_icon(ui, ICON_REFRESH, "Refresh", false).clicked() {
                        self.spawn_quota_refresh(ui.ctx(), true);
                    }
                });
            });
            ui.add_space(6.0);

            let router = &mut self.conv.settings.router;
            ui.checkbox(
                &mut router.read_claude_code_usage,
                RichText::new("Read Claude Code subscription usage (/usage)")
                    .size(FS_SMALL)
                    .color(c_text()),
            )
            .on_hover_text(
                "Reads Claude Code's login (macOS keychain or ~/.claude/.credentials.json) only to query the usage windows. The token is never refreshed or used for requests. macOS asks for permission the first time.",
            );
            ui.checkbox(
                &mut router.read_codex_cli_usage,
                RichText::new("Read Codex CLI subscription usage (/status)")
                    .size(FS_SMALL)
                    .color(c_text()),
            )
            .on_hover_text(
                "Reads ~/.codex/auth.json only to query the ChatGPT usage windows for Codex (ACP).",
            );
            field_hint(
                ui,
                "GPT Codex (ChatGPT sign-in) and OpenRouter report usage without extra permissions. Other providers are tracked from oxi's own usage history.",
            );
            ui.add_space(8.0);

            let now = quota::now_secs();
            let week_ago = now.saturating_sub(7 * 86_400);
            for (i, &kind) in configured.iter().enumerate() {
                if i > 0 {
                    hairline(ui);
                }
                let billing = self.conv.settings.router.billing(kind);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    ui.label(
                        RichText::new(kind.label())
                            .size(FS_SMALL)
                            .color(c_text_strong())
                            .strong(),
                    );
                    ui.label(
                        RichText::new(billing.label())
                            .size(FS_TINY)
                            .color(c_text_faint()),
                    );
                    if let Some(left) = quota::cooldown_left(kind) {
                        ui.label(
                            RichText::new(format!(
                                "cooling down {}",
                                crate::router::decide::short_duration(left)
                            ))
                            .size(FS_TINY)
                            .color(c_warning_fg()),
                        )
                        .on_hover_text("Skipped after a rate-limit or quota error");
                        if ui.small_button("Clear").clicked() {
                            quota::clear_cooldown(kind);
                        }
                    }
                });
                if let Some(snap) = quota::snapshot(kind) {
                    let mut parts: Vec<String> = snap
                        .windows
                        .iter()
                        .map(|w| {
                            let reset = w
                                .resets_at
                                .filter(|t| *t > now)
                                .map(|t| {
                                    format!(
                                        " (resets in {})",
                                        crate::router::decide::short_duration(t - now)
                                    )
                                })
                                .unwrap_or_default();
                            format!("{} {:.0}% used{reset}", w.label, w.used_pct)
                        })
                        .collect();
                    if let Some(c) = snap.credits_left {
                        parts.push(format!("${c:.2} credit left"));
                    }
                    if let Some(plan) = &snap.plan {
                        parts.push(format!("plan: {plan}"));
                    }
                    if !parts.is_empty() {
                        ui.add(
                            egui::Label::new(
                                RichText::new(parts.join(" · "))
                                    .size(FS_TINY)
                                    .color(c_text_muted()),
                            )
                            .wrap(),
                        );
                    }
                    if let Some(err) = &snap.error {
                        ui.add(
                            egui::Label::new(
                                RichText::new(err).size(FS_TINY).color(c_error_fg()),
                            )
                            .wrap(),
                        );
                    } else if !snap.source.is_empty() {
                        ui.label(
                            RichText::new(format!(
                                "{} · updated {} ago",
                                snap.source,
                                crate::router::decide::short_duration(
                                    now.saturating_sub(snap.updated_at)
                                )
                            ))
                            .size(FS_TINY)
                            .color(c_text_faint()),
                        );
                    }
                }
                let week = ledger::totals(Some(kind), week_ago);
                if week.requests > 0 {
                    let mut line = format!(
                        "Last 7 days in oxi: {} requests, {} tokens",
                        week.requests,
                        compact_tokens(week.tokens)
                    );
                    if billing == Billing::PayPerUse {
                        line.push_str(&format!(", ~${:.2}", week.cost));
                    }
                    ui.label(RichText::new(line).size(FS_TINY).color(c_text_faint()));
                }
            }
        });
    }

    fn render_router_candidates_card(&mut self, ui: &mut Ui, configured: &[LlmProviderKind]) {
        card_frame().show(ui, |ui| {
            settings_card_header(
                ui,
                "Candidates",
                Some(
                    "Providers the router may use, how each is billed, and which model to run per task tier. Empty tiers use the provider's selected model.",
                ),
            );
            for (i, &kind) in configured.iter().enumerate() {
                if i > 0 {
                    hairline(ui);
                }
                let selected_model = self.conv.settings.provider(kind).model_id.clone();
                let router = &mut self.conv.settings.router;
                let default_billing = Billing::default_for(kind);
                let prefs = router.prefs_mut(kind);
                ui.horizontal(|ui| {
                    ui.checkbox(
                        &mut prefs.enabled,
                        RichText::new(kind.label())
                            .size(FS_SMALL)
                            .color(c_text_strong())
                            .strong(),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let current = prefs.billing.unwrap_or(default_billing);
                        egui::ComboBox::from_id_salt(("router_billing", kind.slug()))
                            .selected_text(current.label())
                            .width(130.0)
                            .show_ui(ui, |ui| {
                                for b in [Billing::Subscription, Billing::PayPerUse, Billing::Local]
                                {
                                    if ui.selectable_label(b == current, b.label()).clicked() {
                                        prefs.billing = (b != default_billing).then_some(b);
                                    }
                                }
                            });
                    });
                });
                if !prefs.enabled {
                    continue;
                }
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let width = ((ui.available_width() - 120.0) / 3.0).clamp(110.0, 200.0);
                    for (label, field) in [
                        ("Light", &mut prefs.light_model),
                        ("Standard", &mut prefs.standard_model),
                        ("Heavy", &mut prefs.heavy_model),
                    ] {
                        ui.vertical(|ui| {
                            ui.label(RichText::new(label).size(FS_TINY).color(c_text_muted()));
                            settings_text_field_width(ui, field, &selected_model, width);
                        });
                    }
                });
                let q = |m: &str| {
                    let m = if m.trim().is_empty() { &selected_model } else { m };
                    crate::router::catalog::quality(m)
                };
                ui.label(
                    RichText::new(format!(
                        "Estimated quality (1–5): light {} · standard {} · heavy {}",
                        q(&prefs.light_model),
                        q(&prefs.standard_model),
                        q(&prefs.heavy_model)
                    ))
                    .size(FS_TINY)
                    .color(c_text_faint()),
                );
            }
        });
    }

    fn maybe_auto_refresh_quota(&self, ctx: &egui::Context) {
        let id = egui::Id::new("router_quota_auto_refresh");
        let now = ctx.input(|i| i.time);
        let last = ctx.data(|d| d.get_temp::<f64>(id));
        if last.is_none_or(|t| now - t >= PAGE_REFRESH_SECS) {
            ctx.data_mut(|d| d.insert_temp(id, now));
            self.spawn_quota_refresh(ctx, last.is_none());
        }
    }

    fn spawn_quota_refresh(&self, ctx: &egui::Context, force: bool) {
        let settings = self.conv.settings.clone();
        let ctx = ctx.clone();
        if let Ok(rt) = crate::runtime::runtime() {
            rt.spawn(async move {
                quota::refresh(&settings, force).await;
                ctx.request_repaint();
            });
        }
    }
}

fn compact_tokens(n: u64) -> String {
    match n {
        n if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1e6),
        n if n >= 1_000 => format!("{:.1}k", n as f64 / 1e3),
        n => n.to_string(),
    }
}
