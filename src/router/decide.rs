//! Candidate generation and scoring: pick a provider, model and effort for one turn.
//!
//! Pure over its inputs ([`Signals`] carries the quota / spend state) so it is unit-testable;
//! [`super::resolve`] gathers the live state and applies the result.

use std::collections::{HashMap, HashSet};

use super::catalog;
use super::classify::{TaskProfile, Tier};
use super::quota::QuotaSnapshot;
use crate::model::RouteNote;
use crate::settings::{AppSettings, Billing, LlmProviderKind, RouterStrategy};

/// Live state the decision depends on.
#[derive(Default)]
pub struct Signals {
    pub quota: HashMap<LlmProviderKind, QuotaSnapshot>,
    /// Providers cooling down after a rate-limit / quota error, with seconds left.
    pub cooldown: HashMap<LlmProviderKind, u64>,
    /// Estimated pay-per-use spend this month (USD).
    pub month_spend: f64,
}

pub struct DecideInput<'a> {
    pub settings: &'a AppSettings,
    pub strategy: RouterStrategy,
    pub profile: &'a TaskProfile,
    /// Providers with usable credentials (the Router itself is ignored).
    pub configured: &'a [LlmProviderKind],
    /// The route of this chat's previous turn.
    pub previous: Option<&'a RouteNote>,
    /// Providers already tried this turn (failover).
    pub exclude: &'a [LlmProviderKind],
    /// Only HTTP providers (failover cannot hand a half-finished turn to an ACP agent).
    pub http_only: bool,
    pub signals: &'a Signals,
}

#[derive(Debug, Clone)]
pub struct Scored {
    pub kind: LlmProviderKind,
    pub model: String,
    pub effort: String,
    pub score: f64,
    /// Short facts for the reason line, most relevant first.
    pub notes: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Rejected {
    pub kind: LlmProviderKind,
    pub model: String,
    pub why: String,
}

/// Providers that accept a reasoning-effort setting (mirrors the composer's effort selector).
pub fn supports_effort(kind: LlmProviderKind) -> bool {
    matches!(
        kind,
        LlmProviderKind::CustomAnthropic
            | LlmProviderKind::ClaudeCodeAcp
            | LlmProviderKind::OpenAi
            | LlmProviderKind::GptCodex
            | LlmProviderKind::OpenCodeGo
            | LlmProviderKind::AzureOpenAi
            | LlmProviderKind::CodexAcp
            | LlmProviderKind::CursorAcp
    )
}

/// Model ids worth considering for a provider: one per tier (falling back to the provider's
/// selected model), deduplicated.
fn provider_models(settings: &AppSettings, kind: LlmProviderKind) -> Vec<String> {
    let prefs = settings.router.prefs(kind);
    let selected = settings.provider(kind).model_id.trim().to_string();
    let mut out: Vec<String> = Vec::new();
    for m in [
        prefs.light_model.trim(),
        prefs.standard_model.trim(),
        prefs.heavy_model.trim(),
        selected.as_str(),
    ] {
        if !m.is_empty() && !out.iter().any(|o| o == m) {
            out.push(m.to_string());
        }
    }
    out
}

/// Turns of tool use a task usually takes; prompt tokens are paid again every round (mostly
/// from cache).
fn rounds(tier: Tier) -> f64 {
    match tier {
        Tier::Light => 1.0,
        Tier::Standard => 4.0,
        Tier::Heavy => 10.0,
    }
}

pub fn rank(input: &DecideInput<'_>) -> (Vec<Scored>, Vec<Rejected>) {
    let s = input.settings;
    let p = input.profile;
    let strategy = input.strategy;
    let excluded: HashSet<_> = input.exclude.iter().copied().collect();
    let mut scored = Vec::new();
    let mut rejected = Vec::new();

    let required = (p.tier.required_quality()
        + u8::from(strategy == RouterStrategy::Best && p.tier != Tier::Light))
    .min(5);

    for &kind in input.configured {
        if kind == LlmProviderKind::Router || excluded.contains(&kind) {
            continue;
        }
        if input.http_only && kind.is_acp() {
            continue;
        }
        let prefs = s.router.prefs(kind);
        if !prefs.enabled {
            continue;
        }
        let billing = s.router.billing(kind);
        // The model the user assigned to this task tier (empty tiers fall back up/down).
        let designated = {
            let selected = s.provider(kind).model_id.trim().to_string();
            let pick = |m: &str| (!m.trim().is_empty()).then(|| m.trim().to_string());
            match p.tier {
                Tier::Light => pick(&prefs.light_model).or_else(|| pick(&prefs.standard_model)),
                Tier::Standard => pick(&prefs.standard_model),
                Tier::Heavy => pick(&prefs.heavy_model).or_else(|| pick(&prefs.standard_model)),
            }
            .unwrap_or(selected)
        };
        let reject = |rejected: &mut Vec<Rejected>, model: &str, why: String| {
            rejected.push(Rejected {
                kind,
                model: model.to_string(),
                why,
            })
        };
        for model in provider_models(s, kind) {
            if let Some(secs) = input.signals.cooldown.get(&kind) {
                reject(
                    &mut rejected,
                    &model,
                    format!("rate limited (retry in {})", short_duration(*secs)),
                );
                continue;
            }
            let mut score = 0.0;
            let mut notes = Vec::new();

            // Context must fit.
            let cfg = s.provider(kind);
            let window = if cfg.model_id.trim() == model {
                cfg.context_window
            } else {
                None
            }
            .or_else(|| crate::agent::models::context_window_for_model(&model));
            if let Some(cw) = window
                && p.est_input_tokens as f64 > cw as f64 * 0.9
            {
                reject(
                    &mut rejected,
                    &model,
                    format!("context too small ({}k)", cw / 1000),
                );
                continue;
            }

            // Quality against the bar.
            let q = catalog::quality(&model);
            if q < required {
                let per_step = match (strategy, billing) {
                    (RouterStrategy::Local, Billing::Local) => 12.0,
                    (RouterStrategy::Cheapest, _) => 18.0,
                    _ => 25.0,
                };
                score -= per_step * f64::from(required - q);
                notes.push(format!("weaker than this task needs ({q}/{required})"));
            } else if strategy == RouterStrategy::Best {
                score += 6.0 * f64::from(q - required);
                if q > required {
                    notes.push("strongest available".into());
                }
            } else {
                score -= 3.0 * f64::from(q - required);
                if q == required {
                    notes.push("right size for the task".into());
                }
            }

            if model == designated {
                score += 4.0;
                notes.push(format!("your {}-task model", p.tier.label()));
            }

            // What it costs.
            match billing {
                Billing::Local => {
                    score += 8.0
                        + match strategy {
                            RouterStrategy::Local => 25.0,
                            RouterStrategy::Cheapest => 6.0,
                            RouterStrategy::Best => -4.0,
                            _ => 0.0,
                        };
                    notes.push("local, free".into());
                    if p.has_images {
                        score -= 15.0;
                        notes.push("may not read images".into());
                    }
                }
                Billing::Subscription => {
                    score += 14.0
                        + match strategy {
                            RouterStrategy::Subscriptions => 12.0,
                            RouterStrategy::Balanced => 4.0,
                            RouterStrategy::Cheapest => 6.0,
                            RouterStrategy::Local => -6.0,
                            RouterStrategy::Best => 0.0,
                        };
                    let snap = input.signals.quota.get(&kind);
                    match snap.and_then(|q| q.used_pct_for(&model).map(|u| (q, u))) {
                        Some((snap, used)) => {
                            if snap.limit_reached || used >= 99.0 {
                                reject(&mut rejected, &model, "subscription limit reached".into());
                                continue;
                            }
                            let left = 100.0 - used;
                            if p.tier != Tier::Heavy && left < f64::from(s.router.reserve_pct) {
                                reject(
                                    &mut rejected,
                                    &model,
                                    format!("only {left:.0}% left, kept for heavy tasks"),
                                );
                                continue;
                            }
                            score -= 18.0 * (used / 100.0).powi(2);
                            notes.push(format!("subscription, {}", window_summary(snap, &model)));
                        }
                        None => {
                            score -= 3.0;
                            notes.push("subscription (usage unknown)".into());
                        }
                    }
                }
                Billing::PayPerUse => {
                    let price = (kind == LlmProviderKind::OpenRouter)
                        .then(|| super::quota::openrouter_price(&model))
                        .flatten()
                        .or_else(|| catalog::price(&model));
                    let known = price.is_some();
                    let price = price.unwrap_or(catalog::Price {
                        input: 2.0,
                        output: 8.0,
                    });
                    let input_tokens =
                        (p.est_input_tokens as f64 * (1.0 + 0.25 * (rounds(p.tier) - 1.0))) as u64;
                    let cost = catalog::request_cost(price, input_tokens, p.est_output_tokens);
                    let budget = s.router.monthly_budget_usd;
                    if budget > 0.0 && input.signals.month_spend + cost > budget {
                        reject(
                            &mut rejected,
                            &model,
                            format!("monthly budget reached (${budget:.0})"),
                        );
                        continue;
                    }
                    let weight = match strategy {
                        RouterStrategy::Cheapest => 60.0,
                        RouterStrategy::Subscriptions | RouterStrategy::Local => 45.0,
                        RouterStrategy::Balanced => 30.0,
                        RouterStrategy::Best => 6.0,
                    };
                    score -= weight * cost;
                    notes.push(if known {
                        format!("pay per use, ~{}", format_usd(cost))
                    } else {
                        format!("pay per use, ~{} (price guessed)", format_usd(cost))
                    });
                    if let Some(credits) =
                        input.signals.quota.get(&kind).and_then(|q| q.credits_left)
                        && credits < cost
                    {
                        reject(&mut rejected, &model, "out of credit".into());
                        continue;
                    }
                }
            }

            // Staying put keeps the provider's prompt cache warm; switching re-reads everything.
            if let Some(prev) = input.previous
                && prev.provider == kind
            {
                if prev.model == model {
                    score += 5.0 + (p.est_input_tokens as f64 / 25_000.0).min(8.0);
                    notes.push("same model as last turn (prompt cache)".into());
                } else {
                    score += 2.0;
                }
            }

            let effort = if supports_effort(kind) {
                p.tier.effort().to_string()
            } else {
                String::new()
            };
            scored.push(Scored {
                kind,
                model,
                effort,
                score,
                notes,
            });
        }
    }
    scored.sort_by(|a, b| b.score.total_cmp(&a.score));
    (scored, rejected)
}

pub fn decide(input: &DecideInput<'_>) -> Result<RouteNote, String> {
    let (scored, rejected) = rank(input);
    let Some(best) = scored.first() else {
        let why = if rejected.is_empty() {
            "no enabled provider is configured".to_string()
        } else {
            rejected
                .iter()
                .map(|r| format!("{} · {}: {}", r.kind.label(), r.model, r.why))
                .collect::<Vec<_>>()
                .join("; ")
        };
        return Err(format!("Router: no provider available ({why})."));
    };

    let p = input.profile;
    let mut reason = format!(
        "{} task ({}). {} because: {}.",
        capitalize(p.tier.label()),
        p.signals.join(", "),
        provider_model(best.kind, &best.model),
        best.notes.join("; ")
    );
    if let Some(next) = scored.get(1) {
        reason.push_str(&format!(
            " Next best: {} ({}).",
            provider_model(next.kind, &next.model),
            next.notes.first().cloned().unwrap_or_default()
        ));
    }

    let mut alternatives: Vec<String> = scored
        .iter()
        .skip(1)
        .take(4)
        .map(|c| {
            format!(
                "{} — score {:.0}: {}",
                provider_model(c.kind, &c.model),
                c.score,
                c.notes.join("; ")
            )
        })
        .collect();
    alternatives.extend(
        rejected
            .iter()
            .take(4)
            .map(|r| format!("{} — skipped: {}", provider_model(r.kind, &r.model), r.why)),
    );

    Ok(RouteNote {
        provider: best.kind,
        model: best.model.clone(),
        effort: best.effort.clone(),
        tier: p.tier.label().to_string(),
        strategy: input.strategy.label().to_string(),
        reason,
        alternatives,
        failover_from: None,
    })
}

fn provider_model(kind: LlmProviderKind, model: &str) -> String {
    format!("{} · {}", kind.label(), model)
}

fn window_summary(snap: &QuotaSnapshot, model: &str) -> String {
    let m = model.to_ascii_lowercase();
    let parts: Vec<String> = snap
        .windows
        .iter()
        .filter(|w| {
            w.model_scope
                .as_ref()
                .is_none_or(|s| m.contains(s.as_str()))
        })
        .map(|w| format!("{} {:.0}% used", w.label, w.used_pct))
        .collect();
    if parts.is_empty() {
        "usage unknown".into()
    } else {
        parts.join(", ")
    }
}

fn format_usd(v: f64) -> String {
    if v < 0.01 {
        "<$0.01".into()
    } else {
        format!("${v:.2}")
    }
}

pub fn short_duration(secs: u64) -> String {
    match secs {
        s if s >= 86_400 => format!("{}d {}h", s / 86_400, (s % 86_400) / 3_600),
        s if s >= 3_600 => format!("{}h {}m", s / 3_600, (s % 3_600) / 60),
        s if s >= 60 => format!("{}m", s / 60),
        s => format!("{s}s"),
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::router::quota::UsageWindow;

    fn profile(tier: Tier) -> TaskProfile {
        TaskProfile {
            tier,
            est_input_tokens: 20_000,
            est_output_tokens: 4_000,
            has_images: false,
            signals: vec!["test".into()],
        }
    }

    fn settings() -> AppSettings {
        let mut s = AppSettings::default();
        s.provider_mut(LlmProviderKind::Ollama).model_id = "qwen2.5-coder:7b".into();
        s.provider_mut(LlmProviderKind::OpenRouter).model_id = "anthropic/claude-sonnet-4.5".into();
        s
    }

    fn window(used: f64) -> QuotaSnapshot {
        QuotaSnapshot {
            windows: vec![UsageWindow {
                label: "5h".into(),
                used_pct: used,
                resets_at: None,
                model_scope: None,
            }],
            ..Default::default()
        }
    }

    fn pick(
        s: &AppSettings,
        strategy: RouterStrategy,
        tier: Tier,
        configured: &[LlmProviderKind],
        signals: &Signals,
    ) -> RouteNote {
        let p = profile(tier);
        decide(&DecideInput {
            settings: s,
            strategy,
            profile: &p,
            configured,
            previous: None,
            exclude: &[],
            http_only: false,
            signals,
        })
        .unwrap()
    }

    const ALL: &[LlmProviderKind] = &[
        LlmProviderKind::Ollama,
        LlmProviderKind::OpenRouter,
        LlmProviderKind::ClaudeCodeAcp,
    ];

    #[test]
    fn heavy_task_prefers_subscription_heavy_model() {
        let r = pick(
            &settings(),
            RouterStrategy::Balanced,
            Tier::Heavy,
            ALL,
            &Signals::default(),
        );
        assert_eq!(r.provider, LlmProviderKind::ClaudeCodeAcp);
        assert_eq!(r.model, "opus");
        assert_eq!(r.effort, "high");
        assert!(r.reason.contains("Heavy task"));
    }

    #[test]
    fn light_task_uses_light_model() {
        let r = pick(
            &settings(),
            RouterStrategy::Balanced,
            Tier::Light,
            ALL,
            &Signals::default(),
        );
        assert_eq!(r.provider, LlmProviderKind::ClaudeCodeAcp);
        assert_eq!(r.model, "haiku");
    }

    #[test]
    fn exhausted_subscription_falls_back_to_paid() {
        let mut sig = Signals::default();
        sig.quota
            .insert(LlmProviderKind::ClaudeCodeAcp, window(100.0));
        let r = pick(
            &settings(),
            RouterStrategy::Balanced,
            Tier::Standard,
            ALL,
            &sig,
        );
        assert_eq!(r.provider, LlmProviderKind::OpenRouter);
        assert!(r.alternatives.iter().any(|a| a.contains("limit reached")));
    }

    #[test]
    fn reserve_is_kept_for_heavy_tasks() {
        let mut sig = Signals::default();
        sig.quota
            .insert(LlmProviderKind::ClaudeCodeAcp, window(95.0));
        let s = settings();
        let light = pick(&s, RouterStrategy::Balanced, Tier::Standard, ALL, &sig);
        assert_ne!(light.provider, LlmProviderKind::ClaudeCodeAcp);
        let heavy = pick(&s, RouterStrategy::Balanced, Tier::Heavy, ALL, &sig);
        assert_eq!(heavy.provider, LlmProviderKind::ClaudeCodeAcp);
    }

    #[test]
    fn local_strategy_stays_local_for_light_work() {
        let r = pick(
            &settings(),
            RouterStrategy::Local,
            Tier::Light,
            ALL,
            &Signals::default(),
        );
        assert_eq!(r.provider, LlmProviderKind::Ollama);
    }

    #[test]
    fn cooldown_and_budget_reject() {
        let mut s = settings();
        s.router.monthly_budget_usd = 1.0;
        let mut sig = Signals {
            month_spend: 1.0,
            ..Default::default()
        };
        sig.cooldown.insert(LlmProviderKind::ClaudeCodeAcp, 600);
        let r = pick(&s, RouterStrategy::Balanced, Tier::Heavy, ALL, &sig);
        assert_eq!(r.provider, LlmProviderKind::Ollama);
    }

    #[test]
    fn http_only_skips_acp() {
        let p = profile(Tier::Heavy);
        let s = settings();
        let sig = Signals::default();
        let r = decide(&DecideInput {
            settings: &s,
            strategy: RouterStrategy::Balanced,
            profile: &p,
            configured: ALL,
            previous: None,
            exclude: &[LlmProviderKind::OpenRouter],
            http_only: true,
            signals: &sig,
        })
        .unwrap();
        assert_eq!(r.provider, LlmProviderKind::Ollama);
    }
}
