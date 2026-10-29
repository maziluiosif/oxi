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

impl OxiApp {
    pub(crate) fn render_chat_status(&mut self, ui: &mut egui::Ui) {
        self.render_context_status(ui);
        if let Some(quota) = self.subscription_status(ui.ctx()) {
            status_sep(ui);
            let response = ui.add(
                egui::Label::new(RichText::new(quota.text).size(FS_TINY).color(quota.color))
                    .sense(Sense::hover()),
            );
            response.on_hover_ui(|ui| {
                for (text, color) in quota.lines {
                    ui.label(RichText::new(text).size(FS_SMALL).color(color));
                }
            });
        }
        if let Some(text) = self.tokens_per_sec_text() {
            status_sep(ui);
            ui.label(RichText::new(text).size(FS_TINY).color(c_text_faint()))
                .on_hover_text(
                    "Output speed of the latest response, measured while the model streams \
                     (tool runs and approvals excluded)",
                );
        }
    }

    /// `(used, window)` tokens for the active chat. An ACP agent keeps its own context (system
    /// prompt, tools, compaction), so its own reading beats anything estimated from oxi's
    /// transcript.
    fn active_context_tokens(&self) -> (usize, usize) {
        let cfg = self.conv.settings.active_config();
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

    /// A small ring and `used / window`, with the agent-reported cost on hover.
    fn render_context_status(&self, ui: &mut egui::Ui) {
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
            text,
            egui::FontId::proportional(FS_TINY),
            if pct >= 0.75 {
                context_indicator_color(pct)
            } else {
                c_text_muted()
            },
        );
        let ring = 12.0;
        let size = egui::vec2(ring + 5.0 + galley.size().x, ui.available_height());
        let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
        resp.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Other, ui.is_enabled(), "Context usage")
        });
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
        if !resp.hovered() {
            return;
        }
        let mut hover = format!("Context used ({:.0}% of the window)", pct * 100.0);
        // What the agent says this chat cost (on a subscription, what it would cost at API
        // prices).
        let cfg = self.conv.settings.active_config();
        if cfg.is_acp()
            && let Some((amount, currency)) =
                crate::agent::acp::reported_cost(&self.acp_session_key(self.active_session_key()))
            && amount > 0.0
        {
            let amount = if currency == "USD" {
                format!("${amount:.2}")
            } else {
                format!("{amount:.2} {currency}")
            };
            hover.push_str(&format!(
                "\nAgent-reported cost since oxi started: {amount}"
            ));
        }
        resp.show_tooltip_text(hover);
    }

    /// Subscription windows for providers whose usage oxi can read: `5h 34% · 7d 12%`, colored
    /// by the fullest window, with reset times on hover. Asks for fresh numbers every
    /// [`QUOTA_POLL_SECS`] while frames run; [`quota::refresh`] throttles the probes itself.
    ///
    /// [`quota::refresh`]: crate::router::quota::refresh
    fn subscription_status(&self, ctx: &egui::Context) -> Option<QuotaStatus> {
        use crate::router::quota;
        let kind = self.conv.settings.active_provider;
        let router = &self.conv.settings.router;
        let enabled = match kind {
            LlmProviderKind::ClaudeCodeAcp => router.read_claude_code_usage,
            LlmProviderKind::CodexAcp => router.read_codex_cli_usage,
            LlmProviderKind::GptCodex => true,
            _ => return None,
        };
        if !enabled {
            return Some(QuotaStatus {
                text: "usage off".into(),
                color: c_text_faint(),
                lines: vec![(
                    "Turn on subscription usage in Settings → Router to see the 5h and weekly limits"
                        .into(),
                    c_text_faint(),
                )],
            });
        }
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
        let Some(snap) = quota::snapshot(kind) else {
            return Some(QuotaStatus {
                text: "usage…".into(),
                color: c_text_faint(),
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
        let fullest = snap
            .windows
            .iter()
            .filter(|w| w.model_scope.is_none())
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
        Some(QuotaStatus {
            text: if text.is_empty() {
                "usage ?".into()
            } else {
                text
            },
            color,
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
    /// Hover: one line per window with its reset time, then the last probe error.
    lines: Vec<(String, Color32)>,
}

fn status_sep(ui: &mut egui::Ui) {
    ui.add_space(4.0);
    ui.label(RichText::new("·").size(FS_TINY).color(c_text_faint()));
    ui.add_space(4.0);
}
