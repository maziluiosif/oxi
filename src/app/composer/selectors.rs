//! The composer's pickers: provider, model (or Router strategy) and thinking level. An ACP
//! agent that advertises its own session config options (permission mode, model, effort, fast
//! mode, agent persona…) gets those instead, exactly as it lists them, so new agent options
//! show up without changes here.

use eframe::egui::Ui;
use serde_json::{Value, json};

use crate::agent::acp::config_options::{self, AcpConfigKind, AcpConfigOption};
use crate::app::composer_helpers::short_model_label;
use crate::app::task_runner::spawn_async_task;
use crate::settings::LlmProviderKind;
use crate::ui::select_menu::{SelectMenu, SelectOption, labeled_switch};

use super::super::OxiApp;
use super::{composer_provider_groups, composer_provider_label};

/// Widest the thinking-level dropdown gets.
const EFFORT_W: f32 = 72.0;
/// Widest an agent option other than model / effort gets ("Accept edits", "Bypass permissions").
const AGENT_OPTION_W: f32 = 110.0;
const GAP: f32 = 6.0;

enum ComposerSelector {
    Agent(usize),
    Model,
    Effort,
    Router,
}

/// Where a failed agent-option change leaves its error for the UI thread.
fn option_error_id() -> eframe::egui::Id {
    eframe::egui::Id::new("composer_agent_option_error")
}

/// The agent option that holds its plan mode, which oxi drives through its own plan toggle.
fn is_plan_switch(option: &AcpConfigOption) -> bool {
    let kind = if option.category.is_empty() {
        option.id.as_str()
    } else {
        option.category.as_str()
    };
    matches!(kind, "mode" | "collaboration_mode")
        && matches!(&option.kind, AcpConfigKind::Select { values, .. } if values.iter().any(|v| v.value == "plan"))
}

fn agent_option_width(option: &AcpConfigOption, model_w: f32) -> f32 {
    if option.is_model() {
        model_w
    } else if option.is_thought_level() {
        EFFORT_W
    } else {
        AGENT_OPTION_W
    }
}

fn agent_option_label(option: &AcpConfigOption, plan_on: bool) -> String {
    let AcpConfigKind::Select { current, values } = &option.kind else {
        return option.name.clone();
    };
    let current = if plan_on && is_plan_switch(option) {
        "plan"
    } else {
        current.as_str()
    };
    values
        .iter()
        .find(|value| value.value == current)
        .map(|value| value.name.clone())
        .unwrap_or_else(|| short_model_label(current, 18))
}

/// Show `value` as the current choice of the option `is_option` matches, when the agent offers
/// it — the launch sets it the same way (`set_matching_config_option`).
fn with_launch_choice(
    options: &mut std::sync::Arc<Vec<AcpConfigOption>>,
    is_option: fn(&AcpConfigOption) -> bool,
    value: &str,
) {
    let value = value.trim();
    let Some(i) = options.iter().position(|o| {
        is_option(o)
            && matches!(&o.kind, AcpConfigKind::Select { current, values }
                if current != value && values.iter().any(|v| v.value == value))
    }) else {
        return;
    };
    if let AcpConfigKind::Select { current, .. } = &mut std::sync::Arc::make_mut(options)[i].kind {
        *current = value.to_owned();
    }
}

impl OxiApp {
    /// Config options the active chat's ACP agent advertised, once it has been started.
    fn active_agent_options(&self) -> Option<std::sync::Arc<Vec<AcpConfigOption>>> {
        let cfg = self.conv.settings.active_config();
        if !cfg.is_acp() {
            return None;
        }
        let session_key = self.acp_session_key(self.active_session_key());
        let command_line = cfg.effective_acp_command();
        config_options::options(&session_key, &command_line)
            .or_else(|| {
                // Until the agent is up, what it offered last time, with the model and effort
                // it will be launched with.
                let mut options = config_options::before_launch(&session_key, &command_line)?;
                with_launch_choice(&mut options, AcpConfigOption::is_model, &cfg.model_id);
                let effort = if cfg.effort.trim().is_empty() {
                    "default"
                } else {
                    cfg.effort.trim()
                };
                with_launch_choice(&mut options, AcpConfigOption::is_thought_level, effort);
                Some(options)
            })
            .filter(|options| !options.is_empty())
    }

    /// The agent's mode picker already shows plan mode, so the separate pill would repeat it.
    pub(super) fn agent_shows_plan_mode(&self) -> bool {
        self.active_agent_options()
            .is_some_and(|options| options.iter().any(is_plan_switch))
    }

    /// Provider dropdown (only providers the user has configured), then either the agent's own
    /// options or model + thinking level. Each hugs its label up to a cap, so short names keep
    /// the chevron close and long ones truncate instead of growing the bar.
    pub(super) fn render_model_selector(&mut self, ui: &mut Ui) {
        if let Some(error) = ui
            .ctx()
            .data_mut(|d| d.remove_temp::<String>(option_error_id()))
        {
            self.notify_composer(error);
        }
        let budget = ui.available_width();
        if budget < 24.0 {
            return;
        }
        let options = self.active_agent_options().unwrap_or_default();
        let mut controls = Vec::new();
        for (index, option) in options.iter().enumerate() {
            let width = match &option.kind {
                AcpConfigKind::Select { .. } => crate::ui::select_menu::select_button_width(
                    ui,
                    &agent_option_label(option, self.plan_mode_on()),
                    agent_option_width(option, 130.0),
                ),
                AcpConfigKind::Boolean(_) => {
                    ui.painter()
                        .layout_no_wrap(
                            option.name.clone(),
                            eframe::egui::FontId::proportional(crate::theme::FS_SMALL),
                            crate::theme::c_text_muted(),
                        )
                        .size()
                        .x
                        + 34.0
                        + 2.0 * ui.spacing().button_padding.x
                }
            };
            let priority = if self.plan_mode_on() && is_plan_switch(option) {
                4
            } else if option.is_model() {
                3
            } else if is_plan_switch(option) || option.category == "mode" {
                2
            } else if option.is_thought_level() {
                1
            } else {
                0
            };
            controls.push((ComposerSelector::Agent(index), width, priority));
        }
        if self.conv.settings.active_provider == LlmProviderKind::Router {
            controls.push((ComposerSelector::Router, 130.0, 3));
        } else {
            if !options.iter().any(AcpConfigOption::is_model) {
                let label = short_model_label(&self.conv.settings.active_config().model_id, 18);
                let width = crate::ui::select_menu::select_button_width(ui, &label, 130.0);
                controls.push((ComposerSelector::Model, width, 3));
            }
            if self.active_provider_supports_effort()
                && !options.iter().any(AcpConfigOption::is_thought_level)
            {
                controls.push((ComposerSelector::Effort, EFFORT_W, 1));
            }
        }
        // Remove secondary options one by one. The provider is the last selector left;
        // its label truncates within the remaining width instead of wrapping the row.
        let provider_min = budget.min(48.0);
        while controls
            .iter()
            .map(|(_, width, _)| width + GAP)
            .sum::<f32>()
            + provider_min
            > budget
        {
            let Some(index) = controls
                .iter()
                .enumerate()
                .min_by_key(|(_, (_, _, priority))| *priority)
                .map(|(index, _)| index)
            else {
                break;
            };
            let (hidden, _, _) = controls.remove(index);
            let popup = match hidden {
                ComposerSelector::Agent(index) => {
                    eframe::egui::Id::new(("select_menu", ("agent_option", &options[index].id)))
                }
                ComposerSelector::Model => eframe::egui::Id::new(("select_menu", "model")),
                ComposerSelector::Effort => eframe::egui::Id::new(("select_menu", "effort")),
                ComposerSelector::Router => {
                    eframe::egui::Id::new(("select_menu", "router_strategy"))
                }
            };
            eframe::egui::Popup::close_id(ui.ctx(), popup.with("popup"));
        }
        let provider_width = (budget
            - controls
                .iter()
                .map(|(_, width, _)| width + GAP)
                .sum::<f32>())
        .min(104.0);
        self.render_provider_selector(ui, provider_width);
        for (control, width, _) in controls {
            match control {
                ComposerSelector::Agent(index) => {
                    self.render_agent_options(ui, std::slice::from_ref(&options[index]), width)
                }
                ComposerSelector::Model => self.render_model_list_selector(ui, width, 18),
                ComposerSelector::Effort => self.render_effort_selector(ui, false),
                ComposerSelector::Router => self.render_router_strategy_selector(ui, width),
            }
        }
    }

    fn render_provider_selector(&mut self, ui: &mut Ui, width: f32) {
        let active_provider = self.conv.settings.active_provider;
        let mut kinds = Vec::new();
        let picked = SelectMenu::new("provider", composer_provider_label(active_provider))
            .hover(active_provider.label())
            .max_width(width)
            .show(ui, || {
                // Only while the list is open: this clones the secrets blob and probes a
                // legacy file, far too much work for every frame.
                let oauth = crate::oauth::load_oauth_store();
                let configured = self.conv.settings.configured_provider_kinds(&oauth);
                let mut options = Vec::new();
                for (group, providers) in composer_provider_groups(&configured) {
                    for kind in providers {
                        kinds.push(kind);
                        options.push(
                            SelectOption::new(kind.label(), kind == active_provider)
                                .group(Some(group.to_string())),
                        );
                    }
                }
                options
            });
        let Some(kind) = picked.map(|i| kinds[i]) else {
            return;
        };
        if kind == active_provider {
            return;
        }
        self.set_active_session_provider(kind);
        self.save_settings_quietly();
        // Remote/local HF choices come from its downloaded-model list; `/v1/models` only
        // reports the one model currently loaded. The Router has strategies, not models.
        if matches!(kind, LlmProviderKind::LocalHf | LlmProviderKind::RemoteHf) {
            self.refresh_local_hf_model_choices();
        } else if kind != LlmProviderKind::Router {
            self.spawn_model_fetch(ui.ctx(), kind);
        }
    }

    /// Model within the active provider, from the fetched model list (falling back to just the
    /// current model id so it's never empty).
    fn render_model_list_selector(&mut self, ui: &mut Ui, width: f32, model_chars: usize) {
        let kind = self.conv.settings.active_provider;
        let current = self.conv.settings.active_config().model_id.clone();
        let is_hf = matches!(kind, LlmProviderKind::LocalHf | LlmProviderKind::RemoteHf);
        let label = short_model_label(&current, model_chars);
        let mut items: Vec<String> = Vec::new();
        let picked = SelectMenu::new("model", &label)
            .hover(&current)
            .max_width(width)
            .show(ui, || {
                // Local HF's runtime endpoint only exposes the model currently loaded, so its
                // list is every downloaded model; refreshing `/v1/models` would otherwise drop
                // all switch targets but the running one.
                items = if is_hf {
                    let downloaded = if kind == LlmProviderKind::RemoteHf {
                        &self.conv.local_models.remote_downloaded
                    } else {
                        &self.conv.local_models.downloaded
                    };
                    downloaded.iter().map(|m| m.id.clone()).collect()
                } else {
                    self.conv
                        .fetched_models
                        .get(&kind)
                        .map(|f| f.models.clone())
                        .unwrap_or_default()
                };
                if items.is_empty() && !current.is_empty() {
                    items.push(current.clone());
                }
                // Full ids in the list so near-duplicates can be told apart; the closed button
                // keeps the short parsed form.
                items
                    .iter()
                    .map(|m| SelectOption::new(m.clone(), m == &current))
                    .collect()
            });
        let Some(model_id) = picked.map(|i| items[i].clone()) else {
            return;
        };
        if model_id == current {
            return;
        }
        if is_hf {
            // HF selection is a runtime operation, not merely a config edit. Keep the active
            // id until llama-server confirms the replacement is healthy, so a failed remote
            // switch can be retried from this list.
            self.start_selected_local_hf_model(ui.ctx(), &model_id);
        } else {
            self.set_active_session_model(model_id);
            self.save_settings_quietly();
        }
    }

    /// Under the Router the model slot picks the routing strategy (stored as the Router's
    /// `model_id`, so it is per chat like a model choice).
    fn render_router_strategy_selector(&mut self, ui: &mut Ui, width: f32) {
        use crate::settings::RouterStrategy;
        let current = RouterStrategy::from_id(
            &self
                .conv
                .settings
                .provider(LlmProviderKind::Router)
                .model_id,
        );
        let picked = SelectMenu::new("router_strategy", current.label())
            .hover(current.description())
            .max_width(width)
            .show(ui, || {
                RouterStrategy::ALL
                    .iter()
                    .map(|s| SelectOption::new(s.label(), *s == current).detail(s.description()))
                    .collect()
            });
        if let Some(strategy) = picked.map(|i| RouterStrategy::ALL[i])
            && strategy != current
        {
            self.set_active_session_model(strategy.id().to_string());
            self.save_settings_quietly();
        }
    }

    /// Compact thinking/reasoning selector beside the active model. ACP adapters receive this
    /// through `session/set_config_option`; HTTP providers use their native effort field.
    fn active_provider_supports_effort(&self) -> bool {
        matches!(
            self.conv.settings.active_provider,
            LlmProviderKind::CustomAnthropic
                | LlmProviderKind::ClaudeCodeAcp
                | LlmProviderKind::OpenAi
                | LlmProviderKind::GptCodex
                | LlmProviderKind::OpenCodeGo
                | LlmProviderKind::AzureOpenAi
                | LlmProviderKind::CodexAcp
                | LlmProviderKind::CursorAcp
                | LlmProviderKind::MuseCodeAcp
        )
    }

    fn render_effort_selector(&mut self, ui: &mut Ui, compact: bool) {
        let kind = self.conv.settings.active_provider;
        if !self.active_provider_supports_effort() || compact {
            return;
        }
        let values: &[(&str, &str)] = if matches!(
            kind,
            LlmProviderKind::CustomAnthropic | LlmProviderKind::ClaudeCodeAcp
        ) {
            &[
                ("", "Auto"),
                ("low", "Low"),
                ("medium", "Medium"),
                ("high", "High"),
                ("xhigh", "XHigh"),
                ("max", "Max"),
            ]
        } else {
            &[
                ("", "Auto"),
                ("low", "Low"),
                ("medium", "Medium"),
                ("high", "High"),
            ]
        };
        let current = self.conv.settings.provider(kind).effort.clone();
        let selected = values
            .iter()
            .find(|(value, _)| *value == current)
            .map(|(_, label)| *label)
            .unwrap_or("Auto");
        // Short labels so this doesn't grow with "Thinking: …"; capped at EFFORT_W.
        let picked = SelectMenu::new("effort", selected)
            .hover("Thinking / reasoning level")
            .max_width(EFFORT_W)
            .show(ui, || {
                values
                    .iter()
                    .map(|(value, label)| SelectOption::new(*label, current == *value))
                    .collect()
            });
        if let Some((value, _)) = picked.map(|i| values[i]) {
            self.set_active_session_effort(value.to_string());
            self.save_settings_quietly();
        }
    }

    /// One control per option the agent advertised, in its order: a dropdown for a select, a
    /// switch for a boolean.
    fn render_agent_options(&mut self, ui: &mut Ui, options: &[AcpConfigOption], model_w: f32) {
        let plan_on = self.plan_mode_on();
        let mut picked: Option<(&AcpConfigOption, Value)> = None;
        for option in options {
            let hover = if option.description.is_empty() {
                option.name.clone()
            } else {
                format!("{} — {}", option.name, option.description)
            };
            match &option.kind {
                AcpConfigKind::Select { current, values } => {
                    // oxi's plan mode puts the agent in plan for the next turn; show that.
                    let current = if plan_on && is_plan_switch(option) {
                        "plan"
                    } else {
                        current.as_str()
                    };
                    let label = agent_option_label(option, plan_on);
                    let choice = SelectMenu::new(("agent_option", &option.id), &label)
                        .hover(&hover)
                        .max_width(model_w)
                        .show(ui, || {
                            values
                                .iter()
                                .map(|v| {
                                    SelectOption::new(v.name.clone(), v.value == current)
                                        .detail(v.description.clone())
                                        .group(v.group.clone())
                                })
                                .collect()
                        });
                    if let Some(value) = choice.map(|i| &values[i].value)
                        && value != current
                    {
                        picked = Some((option, json!(value)));
                    }
                }
                AcpConfigKind::Boolean(on) => {
                    if let Some(on) = labeled_switch(ui, &option.name, *on, &option.description) {
                        picked = Some((option, json!(on)));
                    }
                }
            }
        }
        if let Some((option, value)) = picked {
            self.set_agent_option(ui.ctx(), option, value);
        }
    }

    /// Apply a value the user picked for one of the agent's options: on the running agent right
    /// away, and in oxi's settings for the model and effort so a relaunch keeps them.
    fn set_agent_option(
        &mut self,
        ctx: &eframe::egui::Context,
        option: &AcpConfigOption,
        value: Value,
    ) {
        let key = self.active_session_key();
        if is_plan_switch(option) {
            // Plan goes through oxi's plan mode (the agent switches at the next turn, and the
            // plan gets its "Implement" bar); any other mode ends it.
            let plan = value.as_str() == Some("plan");
            if plan || self.plan_mode_on() {
                self.run_state_mut(key).plan_mode = plan;
                self.save_settings_quietly();
            }
            if plan {
                return;
            }
        }
        let mut model = None;
        let mut effort = None;
        if option.is_model()
            && let Some(id) = value.as_str()
        {
            self.set_active_session_model(id.to_string());
            model = Some(id.to_string());
        } else if option.is_thought_level()
            && let Some(level) = value.as_str()
        {
            // oxi spells the agent's "default" level as an empty effort.
            let level = if level == "default" { "" } else { level };
            self.set_active_session_effort(level.to_string());
            effort = Some(level.to_string());
        }
        if model.is_some() || effort.is_some() {
            self.save_settings_quietly();
        }

        let session_key = self.acp_session_key(key);
        let command_line = self.conv.settings.active_config().effective_acp_command();
        config_options::set_local(&session_key, &command_line, &option.id, &value);
        if self.is_remote_key(key) {
            // The agent runs on the server. The model and effort travel with each prompt.
            if model.is_none() && effort.is_none() {
                self.remote_set_agent_option(key, &option.id, &value);
            }
            return;
        }
        let req = crate::agent::acp::AcpSetOption {
            session_key,
            command_line,
            config_id: option.id.clone(),
            value,
            model,
            effort,
        };
        let acp = self.acp.clone();
        let ctx = ctx.clone();
        let option_name = option.name.clone();
        spawn_async_task(
            |e| log::warn!("could not set an ACP agent option: {e}"),
            move |rt| {
                let name = option_name;
                if let Err(e) = rt.block_on(acp.set_option(req)) {
                    log::warn!("could not set ACP agent option {name}: {e}");
                    // Shown under the composer by the next frame (e.g. fast mode refused by
                    // the account's plan).
                    ctx.data_mut(|d| {
                        d.insert_temp(option_error_id(), format!("Could not set {name}: {e}"))
                    });
                }
                ctx.request_repaint();
            },
        );
    }
}
