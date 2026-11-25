//! The active chat's status in the status bar: context used, the agent's subscription windows
//! (5h / weekly) and output speed. Always visible there, where the composer only had room for a
//! ring with the rest behind a hover.

use eframe::egui::{self, Color32, RichText, Sense, Stroke};

use crate::settings::LlmProviderKind;
use crate::theme::*;

use super::OxiApp;
use super::composer_helpers::{
    context_indicator_color, format_context_tokens, format_tokens_per_sec, paint_arc,
};

/// The quota line stops reading usage after this long without a frame asking for it.
const QUOTA_POLL_SECS: f64 = 30.0;
/// The context card's popup id; the gallery opens it by this name.
pub(crate) const CONTEXT_CARD_ID: &str = "status_bar_context_popup";
/// Hover before the context card opens: just enough that sweeping the pointer across the
/// status bar doesn't flash it, far below egui's tooltip delay.
const CARD_HOVER_DELAY: f64 = 0.08;
/// How long a hover-opened card outlives the pointer leaving, so it can travel into the card.
const CARD_LEAVE_GRACE: f64 = 0.3;

/// Hover state of the context card. `open` means opened by hover; a pinned card lives in
/// egui's popup memory instead.
#[derive(Clone, Copy, Default)]
struct CardHover {
    open: bool,
    /// Closed by a click or Escape while hovered: wait for the pointer to leave before a hover
    /// may open it again.
    suppressed: bool,
    hover_since: Option<f64>,
    left_at: Option<f64>,
    /// The card's rect last frame, to keep it open while the pointer is over it.
    rect: Option<egui::Rect>,
}

/// What the context trigger computed, for the card.
struct ContextFigures {
    pct: f32,
    text: String,
    cost_line: Option<String>,
}

/// Whether the context card shows, pinned or hover-opened.
fn context_card_open(ctx: &egui::Context) -> bool {
    let id = egui::Id::new(CONTEXT_CARD_ID);
    egui::Popup::is_id_open(ctx, id)
        || ctx
            .data(|d| d.get_temp::<CardHover>(id.with("hover")))
            .is_some_and(|s| s.open)
}

impl OxiApp {
    pub(crate) fn render_chat_status(&mut self, ui: &mut egui::Ui) {
        let quota = self.subscription_status(ui.ctx());
        let card_open = context_card_open(ui.ctx());
        let (context_resp, figures) = self.render_context_trigger(ui, card_open);
        let mut quota_resp = None;
        if let Some(quota) = &quota {
            status_sep(ui);
            let bg_slot = ui.painter().add(egui::Shape::Noop);
            let response = ui
                .add(
                    egui::Label::new(RichText::new(&quota.text).size(FS_TINY).color(quota.color))
                        .sense(Sense::click())
                        .selectable(false),
                )
                .on_hover_cursor(egui::CursorIcon::PointingHand);
            crate::ui::chrome::paint_status_bar_hover(
                ui,
                bg_slot,
                response.rect,
                &response,
                card_open,
            );
            if response.clicked() {
                self.quota_action(ui.ctx(), quota.usage_disabled);
            }
            quota_resp = Some(response);
        }
        self.context_card(
            ui,
            &context_resp,
            quota_resp.as_ref(),
            quota.as_ref(),
            &figures,
        );
        if let Some(text) = self.tokens_per_sec_text() {
            status_sep(ui);
            ui.label(RichText::new(text).size(FS_TINY).color(c_text_faint()))
                .on_hover_text(
                    "Output speed of the latest response, measured while the model streams \
                     (tool runs and approvals excluded)",
                );
        }
    }

    /// Usage reading off: open Router settings. On: probe the subscription windows now.
    fn quota_action(&mut self, ctx: &egui::Context, usage_disabled: bool) {
        if usage_disabled {
            self.open_settings_page();
            self.conv.settings_page.tab = super::state::SettingsTab::Providers;
            self.conv.settings_page.provider_tab = LlmProviderKind::Router;
            return;
        }
        let settings = self.conv.settings.clone();
        let repaint = ctx.clone();
        if let Ok(rt) = crate::runtime::runtime() {
            rt.spawn(async move {
                crate::router::quota::refresh(&settings, true).await;
                repaint.request_repaint();
            });
            crate::ui::chrome::show_toast(ctx, "Refreshing usage limits…");
        }
    }

    /// Active provider config, resolving the Orchestrator pseudo-provider to its lead agent so
    /// context window, live ACP context/cost readings, and subscription quota follow the lead.
    fn effective_active_config(&self) -> crate::settings::ProviderConfig {
        if self.conv.settings.active_provider == LlmProviderKind::Orchestrator {
            crate::orchestrator::lead_config(&self.conv.settings)
        } else {
            self.conv.settings.active_config().clone()
        }
    }

    /// `(used, window)` tokens for the active chat. An ACP agent keeps its own context (system
    /// prompt, tools, compaction), so its own reading beats anything estimated from oxi's
    /// transcript.
    fn active_context_tokens(&self) -> (usize, usize) {
        let cfg = self.effective_active_config();
        let key = self.active_session_key();
        let reported = self
            .session_by_key(key)
            .agent_context
            .filter(|(provider, _, size)| cfg.is_acp() && *provider == cfg.provider && *size > 0);
        match reported {
            Some((_, used, size)) => (used as usize, size as usize),
            None => {
                let max_tokens =
                    cfg.effective_context_window(self.conv.settings.context_window_default);
                let used_chars = self.estimated_active_context_chars();
                let cpt = self.calibrated_chars_per_token(key) as f64;
                let used_tokens = ((used_chars as f64) / cpt).ceil().max(0.0) as usize;
                (used_tokens, max_tokens)
            }
        }
    }

    /// A small ring and `used / window`. Hovering it (or the usage figures next to it) opens
    /// the context card; a click pins the card open.
    fn render_context_trigger(
        &mut self,
        ui: &mut egui::Ui,
        card_open: bool,
    ) -> (egui::Response, ContextFigures) {
        let (used_tokens, max_tokens) = self.active_context_tokens();
        let pct = if max_tokens == 0 {
            0.0
        } else {
            used_tokens as f32 / max_tokens as f32
        }
        .clamp(0.0, 1.0);
        let text = format!(
            "{} / {}",
            format_context_tokens(used_tokens as u64),
            format_context_tokens(max_tokens as u64),
        );
        let galley = ui.painter().layout_no_wrap(
            text.clone(),
            egui::FontId::proportional(FS_TINY),
            if pct >= 0.75 {
                context_indicator_color(pct)
            } else {
                c_text_muted()
            },
        );
        let ring = 12.0;
        let size = egui::vec2(ring + 5.0 + galley.size().x, ui.available_height());
        let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
        let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
        resp.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), "Context usage")
        });
        let pill = rect
            .shrink2(egui::vec2(0.0, 2.0))
            .expand2(egui::vec2(4.0, 0.0));
        if card_open {
            ui.painter()
                .rect_filled(pill, egui::CornerRadius::same(RADIUS_ROW), c_row_active());
        } else if ui.is_enabled() && (resp.hovered() || resp.has_focus()) {
            ui.painter()
                .rect_filled(pill, egui::CornerRadius::same(RADIUS_ROW), c_row_hover());
        }
        crate::ui::chrome::keyboard_focus_ring(ui, &resp, egui::CornerRadius::same(RADIUS_ROW));
        let center = egui::pos2(rect.left() + ring * 0.5, rect.center().y);
        let radius = ring * 0.5 - 1.0;
        ui.painter()
            .circle_stroke(center, radius, Stroke::new(2.0, c_border_subtle()));
        paint_arc(
            ui,
            center,
            radius,
            -std::f32::consts::FRAC_PI_2,
            std::f32::consts::TAU * pct,
            Stroke::new(2.0, context_indicator_color(pct)),
        );
        let text_pos = egui::pos2(
            rect.left() + ring + 5.0,
            rect.center().y - galley.size().y * 0.5,
        );
        ui.painter().galley(text_pos, galley, c_text_muted());

        let cfg = self.effective_active_config();
        let cost_line = if cfg.is_acp()
            && let Some((amount, currency)) =
                crate::agent::acp::reported_cost(&self.acp_session_key(self.active_session_key()))
            && amount > 0.0
        {
            let formatted = if currency == "USD" {
                format!("${amount:.2}")
            } else {
                format!("{amount:.2} {currency}")
            };
            Some(format!("Agent-reported cost: {formatted}"))
        } else {
            None
        };
        (
            resp,
            ContextFigures {
                pct,
                text,
                cost_line,
            },
        )
    }

    /// The context card: context used, the subscription windows and one-click `/compact`.
    /// Opens after a short hover over the context or usage figures (much sooner than a
    /// tooltip) and stays while the pointer travels into it; a click pins it until the next
    /// click outside or Escape. Hover-opened, it stays out of egui's popup memory, so it never
    /// closes a menu that is open elsewhere.
    fn context_card(
        &mut self,
        ui: &mut egui::Ui,
        context: &egui::Response,
        quota_resp: Option<&egui::Response>,
        quota: Option<&QuotaStatus>,
        figures: &ContextFigures,
    ) {
        let ctx = ui.ctx().clone();
        let id = egui::Id::new(CONTEXT_CARD_ID);
        let state_id = id.with("hover");
        let mut st: CardHover = ctx.data(|d| d.get_temp(state_id)).unwrap_or_default();
        let now = ctx.input(|i| i.time);
        let mut pinned = egui::Popup::is_id_open(&ctx, id);
        let over_card = (pinned || st.open)
            && st
                .rect
                .zip(ctx.pointer_hover_pos())
                .is_some_and(|(r, p)| r.expand(4.0).contains(p));
        let hovering = context.hovered() || quota_resp.is_some_and(|r| r.hovered());

        if context.clicked() {
            if pinned {
                pinned = false;
                st.suppressed = true;
            } else {
                pinned = true;
            }
            st.open = false;
        } else if ctx.input(|i| i.pointer.any_click()) {
            if over_card {
                // Working inside a hover-opened card keeps it open.
                if st.open {
                    pinned = true;
                    st.open = false;
                }
            } else if !hovering {
                pinned = false;
                st.open = false;
            }
        }
        if (pinned || st.open) && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            pinned = false;
            st.open = false;
            st.suppressed = hovering;
        }

        if hovering {
            st.left_at = None;
            if !pinned && !st.open && !st.suppressed {
                let since = *st.hover_since.get_or_insert(now);
                let wait = CARD_HOVER_DELAY - (now - since);
                if wait <= 0.0 {
                    st.open = true;
                } else {
                    ctx.request_repaint_after_secs(wait as f32);
                }
            }
        } else {
            st.hover_since = None;
            st.suppressed = false;
            if st.open && !pinned {
                if over_card {
                    st.left_at = None;
                } else {
                    let left = *st.left_at.get_or_insert(now);
                    let wait = CARD_LEAVE_GRACE - (now - left);
                    if wait <= 0.0 {
                        st.open = false;
                    } else {
                        ctx.request_repaint_after_secs(wait as f32);
                    }
                }
            }
        }

        let was_pinned = egui::Popup::is_id_open(&ctx, id);
        if pinned && !was_pinned {
            egui::Popup::open_id(&ctx, id);
        } else if !pinned && was_pinned {
            egui::Popup::close_id(&ctx, id);
        }

        let key = self.active_session_key();
        let busy = self.active_waiting_response();
        let compacting = self.compaction_active_for(key);
        let can_compact = !busy
            && !compacting
            && self.compactable_turns(key) > super::compaction::COMPACT_KEEP_RECENT_TURNS;
        let mut trigger_compact = false;
        let mut trigger_quota = false;

        let anchor = quota_resp.map_or(context.rect, |q| context.rect.union(q.rect));
        let popup = egui::Popup::new(id, ctx.clone(), anchor, ui.layer_id())
            .align(egui::RectAlign::TOP_START)
            .gap(6.0)
            .close_behavior(egui::PopupCloseBehavior::IgnoreClicks);
        let popup = if pinned {
            popup.open_memory(None)
        } else {
            popup.open(st.open)
        };
        let ContextFigures {
            pct,
            text,
            cost_line,
        } = figures;
        let pct = *pct;
        let shown = popup.show(|ui| {
            ui.set_min_width(180.0);
            ui.set_max_width(220.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 3.0;
                popup_heading(ui, "Context", Some((pct, format!("{:.0}%", pct * 100.0))));
                mini_bar(ui, pct, context_indicator_color(pct));
                ui.label(
                    RichText::new(format!("{text} tokens"))
                        .size(FS_TINY)
                        .color(c_text_muted()),
                );
                if let Some(cost) = cost_line {
                    ui.label(RichText::new(cost).size(FS_TINY).color(c_text_muted()));
                }
                if let Some(quota) = quota {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label(strong_rich("Usage limits", FS_TINY, c_text_strong()));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let (label, hover) = if quota.usage_disabled {
                                ("Settings", "Open Router settings")
                            } else {
                                ("Refresh", "Read the usage limits now")
                            };
                            if card_link(ui, label).on_hover_text(hover).clicked() {
                                trigger_quota = true;
                            }
                        });
                    });
                    if quota.rows.is_empty() {
                        for (line, color) in &quota.lines {
                            ui.add(
                                egui::Label::new(RichText::new(line).size(FS_TINY).color(*color))
                                    .wrap(),
                            );
                        }
                    } else {
                        for row in &quota.rows {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(&row.label)
                                        .size(FS_TINY)
                                        .color(c_text_muted()),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.label(strong_rich(
                                            format!("{:.0}%", row.pct),
                                            FS_TINY,
                                            row.color,
                                        ));
                                        if let Some(reset) = &row.resets_in {
                                            ui.label(
                                                RichText::new(format!("resets in {reset}"))
                                                    .size(FS_TINY)
                                                    .color(c_text_faint()),
                                            );
                                        }
                                    },
                                );
                            });
                            ui.add_space(-2.0);
                            mini_bar(ui, (row.pct / 100.0) as f32, row.color);
                            ui.add_space(2.0);
                        }
                        if let Some(extra) = &quota.extra {
                            ui.label(RichText::new(extra).size(FS_TINY).color(c_text_faint()));
                        }
                        if let Some(err) = &quota.error {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(err).size(FS_TINY).color(c_error_fg()),
                                )
                                .wrap(),
                            );
                        }
                    }
                }
                ui.add_space(4.0);
                crate::ui::chrome::hairline(ui);
                ui.add_space(4.0);
                let label = if compacting {
                    "Compacting…"
                } else {
                    "Compact conversation"
                };
                let btn = crate::ui::chrome::ghost_button_icon_enabled(
                    ui,
                    ICON_MAGIC,
                    label,
                    false,
                    can_compact,
                )
                .on_hover_text("Summarize older turns to free context window (/compact)")
                .on_disabled_hover_text(if compacting {
                    "Compaction is already running"
                } else if busy {
                    "Wait for the current response to finish"
                } else {
                    "Need more turns before compacting"
                });
                if btn.clicked() {
                    trigger_compact = true;
                }
            });
        });
        st.rect = shown.map(|r| r.response.rect);

        let usage_disabled = quota.is_some_and(|q| q.usage_disabled);
        if trigger_compact || (trigger_quota && usage_disabled) {
            egui::Popup::close_id(&ctx, id);
            st.open = false;
            st.rect = None;
        }
        ctx.data_mut(|d| d.insert_temp(state_id, st));

        if trigger_quota {
            self.quota_action(&ctx, usage_disabled);
        }
        if trigger_compact {
            self.start_compaction(key, None);
            crate::ui::chrome::show_toast(&ctx, "Compacting conversation…");
        }
    }

    /// Whether this chat should keep subscription quota snapshots warm in the background.
    /// Runs for providers with their own usage probe and for `Orchestrator` / `Router` chats
    /// whenever any team usage probe is enabled (even if the lead itself has no usage probe).
    fn should_poll_quota(&self) -> bool {
        let router = &self.conv.settings.router;
        let any_probe = router.read_claude_code_usage
            || router.read_codex_cli_usage
            || router.read_antigravity_usage;
        if matches!(
            self.conv.settings.active_provider,
            LlmProviderKind::Orchestrator | LlmProviderKind::Router
        ) && any_probe
        {
            return true;
        }
        match self.effective_active_config().provider {
            LlmProviderKind::ClaudeCodeAcp => router.read_claude_code_usage,
            LlmProviderKind::CodexAcp => router.read_codex_cli_usage,
            LlmProviderKind::AntigravityAcp => router.read_antigravity_usage,
            LlmProviderKind::GptCodex => true,
            _ => false,
        }
    }

    /// Asks for fresh subscription usage every [`QUOTA_POLL_SECS`] while frames run;
    /// [`crate::router::quota::refresh`] throttles the probes itself.
    fn maybe_refresh_quota_in_background(&self, ctx: &egui::Context) {
        use crate::router::quota;
        let id = egui::Id::new("status_bar_quota_refresh");
        let t = ctx.input(|i| i.time);
        let last = ctx.data(|d| d.get_temp::<f64>(id));
        if !quota::is_refreshing() && last.is_none_or(|l| t - l >= QUOTA_POLL_SECS) {
            ctx.data_mut(|d| d.insert_temp(id, t));
            let settings = self.conv.settings.clone();
            let ctx = ctx.clone();
            if let Ok(rt) = crate::runtime::runtime() {
                rt.spawn(async move {
                    quota::refresh(&settings, false).await;
                    ctx.request_repaint();
                });
            }
        }
    }

    /// Subscription windows for providers whose usage oxi can read: `5h 34% · 7d 12%`, colored
    /// by the fullest window, with reset times on hover. Asks for fresh numbers every
    /// [`QUOTA_POLL_SECS`] while frames run; [`quota::refresh`] throttles the probes itself.
    ///
    /// [`quota::refresh`]: crate::router::quota::refresh
    fn subscription_status(&self, ctx: &egui::Context) -> Option<QuotaStatus> {
        use crate::router::quota;
        if self.should_poll_quota() {
            self.maybe_refresh_quota_in_background(ctx);
        }
        let kind = self.effective_active_config().provider;
        let router = &self.conv.settings.router;
        let enabled = match kind {
            LlmProviderKind::ClaudeCodeAcp => router.read_claude_code_usage,
            LlmProviderKind::CodexAcp => router.read_codex_cli_usage,
            LlmProviderKind::AntigravityAcp => router.read_antigravity_usage,
            LlmProviderKind::GptCodex => true,
            _ => return None,
        };
        if !enabled {
            return Some(QuotaStatus {
                text: "usage off".into(),
                color: c_text_faint(),
                usage_disabled: true,
                rows: Vec::new(),
                extra: None,
                error: None,
                lines: vec![(
                    "Turn on subscription usage in Settings → Router to see usage limits".into(),
                    c_text_faint(),
                )],
            });
        }
        let Some(snap) = quota::snapshot(kind) else {
            return Some(QuotaStatus {
                text: "usage…".into(),
                color: c_text_faint(),
                usage_disabled: false,
                rows: Vec::new(),
                extra: None,
                error: None,
                lines: vec![("Reading subscription usage…".into(), c_text_faint())],
            });
        };
        let now = quota::now_secs();
        let used = |w: &quota::UsageWindow| match w.resets_at {
            Some(reset) if reset <= now => 0.0,
            _ => w.used_pct,
        };
        let text = snap
            .windows
            .iter()
            .map(|w| format!("{} {:.0}%", w.label, used(w)))
            .collect::<Vec<_>>()
            .join(" · ");
        let unscoped_any = snap.windows.iter().any(|w| w.model_scope.is_none());
        let fullest = snap
            .windows
            .iter()
            .filter(|w| !unscoped_any || w.model_scope.is_none())
            .map(used)
            .fold(0.0, f64::max);
        let color = if snap.limit_reached || fullest >= 90.0 {
            c_error_fg()
        } else if fullest >= 75.0 {
            c_warning_fg()
        } else {
            c_text_muted()
        };
        let mut lines: Vec<_> = snap
            .windows
            .iter()
            .map(|w| {
                let color = if snap.limit_reached && w.model_scope.is_none() {
                    c_error_fg()
                } else {
                    c_text_muted()
                };
                (w.summary(now), color)
            })
            .collect();
        if let Some(err) = &snap.error {
            lines.push((format!("Usage: {err}"), c_error_fg()));
        }
        let rows = snap
            .windows
            .iter()
            .map(|w| {
                let pct = used(w).clamp(0.0, 100.0);
                let color = if (snap.limit_reached && w.model_scope.is_none()) || pct >= 90.0 {
                    c_error_fg()
                } else if pct >= 75.0 {
                    c_warning_fg()
                } else {
                    c_accent()
                };
                QuotaRow {
                    label: w.label.clone(),
                    pct,
                    resets_in: w
                        .resets_at
                        .filter(|&r| r > now)
                        .map(|r| crate::router::decide::short_duration(r - now)),
                    color,
                }
            })
            .collect();
        let mut extra = Vec::new();
        if let Some(plan) = &snap.plan {
            extra.push(format!("Plan: {plan}"));
        }
        if let Some(c) = snap.credits_left {
            extra.push(format!("${c:.2} credits left"));
        }
        Some(QuotaStatus {
            rows,
            extra: (!extra.is_empty()).then(|| extra.join(" · ")),
            error: snap.error.as_ref().map(|e| format!("Usage: {e}")),
            text: if text.is_empty() {
                "usage ?".into()
            } else {
                text
            },
            color,
            usage_disabled: false,
            lines,
        })
    }

    /// Output speed of the running turn (or the last finished one). Hidden until a provider has
    /// streamed long enough to measure a meaningful rate.
    fn tokens_per_sec_text(&self) -> Option<String> {
        self.active_run_state()
            .map(|s| {
                if s.turn_usage.output_tokens_per_sec().is_some() {
                    s.turn_usage
                } else {
                    s.last_turn_usage
                }
            })
            .and_then(|usage| usage.output_tokens_per_sec())
            .map(format_tokens_per_sec)
    }
}

struct QuotaStatus {
    text: String,
    color: Color32,
    usage_disabled: bool,
    /// One line per window with its reset time, then the last probe error. The card shows
    /// these while `rows` is empty.
    lines: Vec<(String, Color32)>,
    /// Context card: one bar per window. Empty while usage is off or not read yet.
    rows: Vec<QuotaRow>,
    /// Plan and remaining credits, when the provider reports them.
    extra: Option<String>,
    error: Option<String>,
}

struct QuotaRow {
    label: String,
    /// 0–100, already zeroed when the window has reset.
    pct: f64,
    resets_in: Option<String>,
    color: Color32,
}

/// Popup section title with an optional right-aligned, colored figure.
fn popup_heading(ui: &mut egui::Ui, title: &str, figure: Option<(f32, String)>) {
    ui.horizontal(|ui| {
        ui.label(strong_rich(title, FS_TINY, c_text_strong()));
        if let Some((pct, figure)) = figure {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(strong_rich(figure, FS_TINY, context_indicator_color(pct)));
            });
        }
    });
}

/// A small text link in the card's section headings.
fn card_link(ui: &mut egui::Ui, label: &str) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(
        label.to_owned(),
        egui::FontId::proportional(FS_TINY),
        Color32::PLACEHOLDER,
    );
    let (rect, response) = ui.allocate_exact_size(galley.size(), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
    });
    let hovered = response.hovered() || response.has_focus();
    ui.painter().galley(
        rect.min,
        galley,
        if hovered { c_accent() } else { c_text_faint() },
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A thin full-width progress bar.
fn mini_bar(ui: &mut egui::Ui, frac: f32, color: Color32) {
    let frac = frac.clamp(0.0, 1.0);
    let (bar_rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 4.0), Sense::hover());
    let r = egui::CornerRadius::same(2);
    ui.painter().rect_filled(bar_rect, r, c_border_subtle());
    if frac > 0.0 {
        let w = (bar_rect.width() * frac).max(3.0);
        ui.painter().rect_filled(
            egui::Rect::from_min_size(bar_rect.min, egui::vec2(w, bar_rect.height())),
            r,
            color,
        );
    }
}

fn status_sep(ui: &mut egui::Ui) {
    ui.add_space(4.0);
    ui.label(RichText::new("·").size(FS_TINY).color(c_text_faint()));
    ui.add_space(4.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orchestrator_context_window_follows_lead_model_and_live_agent_context() {
        crate::secrets::use_mock_store();
        let mut app = OxiApp::new();
        app.conn.no_session = true;
        app.conv.settings = crate::settings::AppSettings::default();
        let key = app.active_session_key();
        app.session_mut_by_key(key).agent_context = None;
        app.conv.settings.active_provider = LlmProviderKind::Orchestrator;
        app.conv.settings.orchestrator.lead_provider = LlmProviderKind::AntigravityAcp;
        app.conv.settings.orchestrator.lead_model = "gemini-4-neon".into();

        // ACP lead gets ACP_CONTEXT_WINDOW (1M) before live usage_update arrives, not 128k.
        let (_, window) = app.active_context_tokens();
        assert_eq!(window, crate::settings::ProviderConfig::ACP_CONTEXT_WINDOW);

        // Live agent_context reported by the lead ACP agent takes precedence.
        app.session_mut_by_key(key).agent_context =
            Some((LlmProviderKind::AntigravityAcp, 42_000, 500_000));
        assert_eq!(app.active_context_tokens(), (42_000, 500_000));

        // Non-ACP lead uses its model's catalog context window dynamically.
        app.session_mut_by_key(key).agent_context = None;
        app.conv.settings.orchestrator.lead_provider = LlmProviderKind::CustomAnthropic;
        app.conv.settings.orchestrator.lead_model = "claude-opus-4-6".into();
        let (_, window) = app.active_context_tokens();
        assert_eq!(window, 200_000);
    }

    #[test]
    fn orchestrator_and_router_poll_team_quota_regardless_of_lead() {
        crate::secrets::use_mock_store();
        let mut app = OxiApp::new();
        app.conn.no_session = true;
        app.conv.settings = crate::settings::AppSettings::default();
        app.conv.settings.router.read_claude_code_usage = false;
        app.conv.settings.router.read_codex_cli_usage = false;
        app.conv.settings.router.read_antigravity_usage = false;

        // Orchestrator with a lead that has no usage probe (CursorAcp):
        // no probes enabled -> should not poll.
        app.conv.settings.active_provider = LlmProviderKind::Orchestrator;
        app.conv.settings.orchestrator.lead_provider = LlmProviderKind::CursorAcp;
        assert!(!app.should_poll_quota());

        // Enabling any team usage probe triggers background polling even for Cursor/Copilot/OpenCode leads.
        app.conv.settings.router.read_claude_code_usage = true;
        for lead in [
            LlmProviderKind::CursorAcp,
            LlmProviderKind::CopilotAcp,
            LlmProviderKind::OpenCodeAcp,
        ] {
            app.conv.settings.orchestrator.lead_provider = lead;
            assert!(app.should_poll_quota(), "lead {lead:?} should poll team quota");
        }

        // Router also polls when any probe is enabled.
        app.conv.settings.active_provider = LlmProviderKind::Router;
        assert!(app.should_poll_quota());

        // Direct CursorAcp chat does not poll unrelated subscription probes.
        app.conv.settings.active_provider = LlmProviderKind::CursorAcp;
        assert!(!app.should_poll_quota());
    }
}
