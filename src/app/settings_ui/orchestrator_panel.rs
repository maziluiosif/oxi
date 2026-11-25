//! Settings → Providers → Orchestrator: the lead agent the user talks to, and the team it may
//! hand work to (see `crate::orchestrator`).

use super::super::OxiApp;
use super::agent_panel::render_oxi_tool_confirm_group;
use crate::settings::{LlmProviderKind, MAX_WORKERS_LIMIT, OxiToolGroup, WorkerSlot};
use crate::theme::*;
use crate::ui::chrome::{
    card_frame, field_hint, hairline, setting_row, setting_select, settings_card_header,
    settings_section, settings_text_field_width,
};
use crate::ui::select_menu::{SelectMenu, SelectOption};
use eframe::egui::{self, Align, Layout, RichText, Ui};

impl OxiApp {
    pub(super) fn render_orchestrator_settings(&mut self, ui: &mut Ui) {
        let agents: Vec<LlmProviderKind> = self
            .cached_configured_providers(ui.ctx())
            .into_iter()
            .filter(|k| !k.is_pseudo())
            .collect();
        self.render_orchestrator_lead_card(ui, &agents);
        self.render_orchestrator_team_card(ui, &agents);
        self.render_orchestrator_confirmations_card(ui);
    }

    fn render_orchestrator_confirmations_card(&mut self, ui: &mut Ui) {
        settings_section(
            ui,
            "Tool confirmations",
            Some(
                "Controls when oxi asks before the lead runs orchestrator tools. Agents in auto-approve/bypass mode, or agents that do not ask permission for MCP tools, will not prompt. All oxi tools can also be configured in Agent tools.",
            ),
            |ui| {
                render_oxi_tool_confirm_group(
                    ui,
                    &mut self.conv.settings,
                    OxiToolGroup::Orchestrator,
                );
            },
        );
    }

    fn render_orchestrator_lead_card(&mut self, ui: &mut Ui, agents: &[LlmProviderKind]) {
        let mut leads: Vec<LlmProviderKind> =
            agents.iter().copied().filter(|k| k.is_acp()).collect();
        let current = self.conv.settings.orchestrator.lead_provider;
        if !leads.contains(&current) {
            leads.insert(0, current);
        }
        settings_section(
            ui,
            "Lead",
            Some(
                "The agent you talk to. It plans, hands work to the team below (in parallel work trees when several agents edit at once), follows them and merges their changes into the workspace as uncommitted changes.",
            ),
            |ui| {
                if let Some(i) = setting_select(
                    ui,
                    "orchestrator_lead",
                    "Lead agent",
                    None,
                    current.label(),
                    || {
                        leads
                            .iter()
                            .map(|k| SelectOption::new(k.label(), *k == current))
                            .collect()
                    },
                ) && leads[i] != current
                {
                    let orch = &mut self.conv.settings.orchestrator;
                    orch.lead_provider = leads[i];
                    orch.lead_model.clear();
                }
                let selected = self
                    .conv
                    .settings
                    .provider(self.conv.settings.orchestrator.lead_provider)
                    .model_id
                    .clone();
                setting_row(
                    ui,
                    "Lead model",
                    Some("Empty: the model selected for that agent."),
                    |ui| {
                        settings_text_field_width(
                            ui,
                            &mut self.conv.settings.orchestrator.lead_model,
                            &selected,
                            200.0,
                        )
                    },
                );
                setting_row(
                    ui,
                    "Workers at once",
                    Some("Per lead chat. Each one is an agent session of its own."),
                    |ui| {
                        ui.spacing_mut().slider_width = 120.0;
                        ui.add(egui::Slider::new(
                            &mut self.conv.settings.orchestrator.max_workers,
                            1..=MAX_WORKERS_LIMIT,
                        ))
                    },
                );
            },
        );
    }

    fn render_orchestrator_team_card(&mut self, ui: &mut Ui, agents: &[LlmProviderKind]) {
        card_frame().show(ui, |ui| {
            settings_card_header(
                ui,
                "Team",
                Some(
                    "Who the lead may start workers on. A note tells it what each is good at; a model limits it to that model (empty: the agent's selected model, others allowed). The lead sees each subscription's quota left and falls back to other available subscriptions when one runs out.",
                ),
            );
            let now = crate::router::quota::now_secs();
            let selected_models: Vec<String> = self
                .conv
                .settings
                .orchestrator
                .workers
                .iter()
                .map(|slot| {
                    let m = if slot.model.trim().is_empty() {
                        self.conv.settings.provider(slot.provider).model_id.trim()
                    } else {
                        slot.model.trim()
                    };
                    if m == "default" {
                        String::new()
                    } else {
                        m.to_string()
                    }
                })
                .collect();
            let orch = &mut self.conv.settings.orchestrator;
            if orch.workers.is_empty() {
                ui.label(
                    RichText::new(
                        "Every configured agent, with its selected model. Customize to pick agents, models and notes.",
                    )
                    .size(FS_SMALL)
                    .color(c_text_muted()),
                );
                ui.add_space(6.0);
                if crate::ui::chrome::mini_button(ui, "Customize").clicked() {
                    orch.workers = agents
                        .iter()
                        .copied()
                        .filter(|k| k.is_acp())
                        .map(|provider| WorkerSlot {
                            provider,
                            model: String::new(),
                            note: String::new(),
                            enabled: true,
                        })
                        .collect();
                }
                return;
            }
            let mut remove = None;
            for (i, slot) in orch.workers.iter_mut().enumerate() {
                if i > 0 {
                    hairline(ui);
                }
                ui.push_id(("orchestrator_slot", i), |ui| {
                    ui.horizontal(|ui| {
                        crate::ui::chrome::toggle(
                            ui,
                            &mut slot.enabled,
                            strong_rich(slot.provider.label(), FS_SMALL, c_text_strong()),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if crate::ui::chrome::dismiss_button(ui)
                                .on_hover_text("Remove from the team")
                                .clicked()
                            {
                                remove = Some(i);
                            }
                            if slot.enabled
                                && let Some((text, color, hover)) =
                                    slot_quota_badge(slot.provider, &selected_models[i], now)
                            {
                                ui.add_space(6.0);
                                ui.label(RichText::new(text).size(FS_TINY).color(color))
                                    .on_hover_text(hover);
                            }
                        });
                    });
                    if !slot.enabled {
                        return;
                    }
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        ui.vertical(|ui| {
                            ui.label(RichText::new("Model").size(FS_TINY).color(c_text_muted()));
                            settings_text_field_width(ui, &mut slot.model, "any (selected)", 170.0);
                        });
                        ui.vertical(|ui| {
                            ui.label(RichText::new("Note for the lead").size(FS_TINY).color(c_text_muted()));
                            let width = (ui.available_width() - 8.0).clamp(160.0, 420.0);
                            settings_text_field_width(
                                ui,
                                &mut slot.note,
                                "e.g. backend, tests, reviews",
                                width,
                            );
                        });
                    });
                });
            }
            if let Some(i) = remove {
                orch.workers.remove(i);
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let picked = SelectMenu::new("orchestrator_add_agent", "Add agent…")
                    .framed(180.0)
                    .show(ui, || {
                        agents
                            .iter()
                            .map(|k| SelectOption::new(k.label(), false))
                            .collect()
                    });
                if let Some(i) = picked {
                    self.conv.settings.orchestrator.workers.push(WorkerSlot {
                        provider: agents[i],
                        model: String::new(),
                        note: String::new(),
                        enabled: true,
                    });
                }
                if crate::ui::chrome::mini_button(ui, "Use every agent")
                    .on_hover_text("Back to every configured agent with its selected model")
                    .clicked()
                {
                    self.conv.settings.orchestrator.workers.clear();
                }
            });
            field_hint(
                ui,
                "Add an agent twice to offer it with two models (e.g. a fast one and a strong one).",
            );
        });
    }
}

fn slot_quota_badge(
    provider: LlmProviderKind,
    effective_model: &str,
    now: u64,
) -> Option<(String, egui::Color32, String)> {
    let snap = crate::router::quota::snapshot(provider);
    let cooldown = crate::router::quota::cooldown_left(provider);
    let snap_exhausted = snap
        .as_ref()
        .is_some_and(|s| s.is_exhausted_for(effective_model, now));
    if cooldown.is_some() || snap_exhausted {
        let mut hover_parts: Vec<String> = Vec::new();
        if let Some(secs) = cooldown {
            hover_parts.push(format!(
                "Cooling down ({} left)",
                crate::router::decide::short_duration(secs)
            ));
        }
        if let Some(snap) = &snap {
            let m = effective_model.to_ascii_lowercase();
            let shortest_reset = snap
                .windows
                .iter()
                .filter(|w| {
                    w.model_scope
                        .as_ref()
                        .is_none_or(|s| m.contains(s.as_str()))
                        && w.used_pct >= 99.0
                        && w.resets_at.is_some_and(|t| t > now)
                })
                .filter_map(|w| w.resets_at.map(|t| (t - now, w.summary(now))))
                .min_by_key(|(left, _)| *left);
            if let Some((_, summary)) = shortest_reset {
                hover_parts.push(summary);
            } else {
                for w in &snap.windows {
                    hover_parts.push(w.summary(now));
                }
            }
            if let Some(c) = snap.credits_left
                && c <= 0.0
            {
                hover_parts.push("$0.00 credit left".into());
            }
        }
        let hover = if hover_parts.is_empty() {
            "Out of quota — oxi will fall back to another enabled team member".to_string()
        } else {
            hover_parts.join(" · ")
        };
        return Some(("Out of quota".to_string(), c_error_fg(), hover));
    }

    let snap = snap?;
    if snap.windows.is_empty() {
        return None;
    }
    let m = effective_model.to_ascii_lowercase();
    let applicable: Vec<&crate::router::quota::UsageWindow> = snap
        .windows
        .iter()
        .filter(|w| {
            w.model_scope
                .as_ref()
                .is_none_or(|s| m.contains(s.as_str()))
        })
        .collect();
    let windows = if applicable.is_empty() {
        snap.windows.iter().collect::<Vec<_>>()
    } else {
        applicable
    };
    let mut parts = Vec::with_capacity(windows.len());
    let mut hover_parts = Vec::with_capacity(windows.len());
    let mut max_used = 0.0_f64;
    for w in &windows {
        let used = match w.resets_at {
            Some(t) if t <= now => 0.0,
            _ => w.used_pct,
        };
        if used > max_used {
            max_used = used;
        }
        parts.push(format!("{} {:.0}%", w.label, used));
        hover_parts.push(w.summary(now));
    }
    let color = if max_used >= 75.0 {
        c_warning_fg()
    } else {
        c_text_muted()
    };
    Some((parts.join(" · "), color, hover_parts.join("\n")))
}
