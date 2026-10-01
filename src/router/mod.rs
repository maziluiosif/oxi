//! The Router pseudo-provider: per turn, pick one of the user's configured providers plus a
//! model and an effort level, weighing task difficulty, subscription quota, pay-per-use cost
//! and prompt-cache stickiness. The decision (and why) is shown above the reply.
//!
//! - `jev`: multilingual task classification through OpenRouter (default)
//! - [`classify`]: local fallback classification
//! - [`catalog`]: rough quality tier and list price per model family
//! - [`quota`]: subscription windows / credit per provider (probes + response headers)
//! - [`ledger`]: local usage history and pay-per-use spend
//! - [`decide`]: candidate scoring

pub mod catalog;
pub mod classify;
pub mod decide;
mod jev;
pub mod ledger;
pub mod preference;
pub mod quota;

use std::time::Duration;

use crate::model::{AssistantBlock, ChatMessage, MsgRole, RouteNote};
use crate::settings::{AppSettings, LlmProviderKind, RouterStrategy};

/// How long a turn waits for fresh quota probes before routing with what it has.
const PROBE_BUDGET: Duration = Duration::from_secs(4);

/// Classify once per turn; reuse the profile if the selected provider fails over.
pub async fn classify_turn(
    settings: &AppSettings,
    chat: &[ChatMessage],
    plan_mode: bool,
    chars_per_token: f32,
) -> classify::TaskProfile {
    let last_user_idx = chat.iter().rposition(|m| m.role == MsgRole::User);
    let last_user = last_user_idx.map(|i| &chat[i]);
    let earlier = &chat[..last_user_idx.unwrap_or(chat.len())];
    let previous = previous_route(earlier);
    let prior_tool_calls = earlier
        .iter()
        .rev()
        .take_while(|m| m.role == MsgRole::Assistant)
        .flat_map(|m| &m.blocks)
        .filter(|b| matches!(b, AssistantBlock::Tool { .. }))
        .count();
    let input = classify::ClassifyInput {
        text: last_user.map(|m| m.text.as_str()).unwrap_or_default(),
        has_images: last_user.is_some_and(|m| !m.attachments.is_empty()),
        plan_mode,
        history_chars: earlier.iter().map(message_chars).sum(),
        chars_per_token,
        prior_tool_calls,
        previous_tier: previous.and_then(|r| parse_tier(&r.tier)),
    };
    jev::classify(settings, earlier, &input).await
}

/// Choose from the configured providers using a previously classified task.
pub async fn resolve(
    settings: &AppSettings,
    chat: &[ChatMessage],
    profile: &classify::TaskProfile,
    exclude: &[LlmProviderKind],
    http_only: bool,
) -> Result<RouteNote, String> {
    let _ = tokio::time::timeout(PROBE_BUDGET, quota::refresh(settings, true)).await;
    let oauth = crate::oauth::load_oauth_store();
    let configured = settings.configured_provider_kinds(&oauth);
    let strategy = RouterStrategy::from_id(&settings.provider(LlmProviderKind::Router).model_id);
    let earlier = &chat[..chat
        .iter()
        .rposition(|m| m.role == MsgRole::User)
        .unwrap_or(chat.len())];
    let previous = previous_route(earlier);
    let signals = gather_signals(&configured);
    decide::decide(&decide::DecideInput {
        settings,
        strategy,
        profile,
        configured: &configured,
        previous,
        exclude,
        http_only,
        signals: &signals,
    })
}

/// Concrete config for a one-shot helper completion (chat title, commit message, compaction
/// summary) when `cfg` is the Router: a light task on an HTTP provider, decided from the quota
/// state already known (no probing, this runs on the UI thread). Other configs pass through.
pub fn helper_config(
    settings: &AppSettings,
    cfg: crate::settings::ProviderConfig,
    input_chars: usize,
) -> crate::settings::ProviderConfig {
    if cfg.provider != LlmProviderKind::Router {
        return cfg;
    }
    let oauth = crate::oauth::load_oauth_store();
    let configured = settings.configured_provider_kinds(&oauth);
    let profile = classify::TaskProfile {
        tier: classify::Tier::Light,
        est_input_tokens: (input_chars / 4) as u64 + 500,
        est_output_tokens: 500,
        has_images: false,
        signals: vec!["helper task".into()],
    };
    let signals = gather_signals(&configured);
    let picked = decide::decide(&decide::DecideInput {
        settings,
        strategy: RouterStrategy::from_id(&cfg.model_id),
        profile: &profile,
        configured: &configured,
        previous: None,
        exclude: &[],
        http_only: true,
        signals: &signals,
    });
    match picked {
        Ok(note) => {
            let mut routed = settings.clone();
            apply(&mut routed, &note);
            routed.active_config().clone()
        }
        Err(_) => cfg,
    }
}

/// Point `settings` at the routed provider/model/effort for this run.
pub fn apply(settings: &mut AppSettings, note: &RouteNote) {
    settings.active_provider = note.provider;
    let cfg = settings.provider_mut(note.provider);
    // The window configured for the provider's usual model may not match the routed one.
    if cfg.model_id.trim() != note.model {
        cfg.context_window = None;
    }
    cfg.model_id = note.model.clone();
    cfg.effort = note.effort.clone();
}

pub fn gather_signals(configured: &[LlmProviderKind]) -> decide::Signals {
    let mut signals = decide::Signals {
        month_spend: ledger::spend_this_month(),
        ..Default::default()
    };
    for &kind in configured {
        if let Some(snap) = quota::snapshot(kind) {
            signals.quota.insert(kind, snap);
        }
        if let Some(left) = quota::cooldown_left(kind) {
            signals.cooldown.insert(kind, left);
        }
    }
    signals
}

fn previous_route(earlier: &[ChatMessage]) -> Option<&RouteNote> {
    earlier
        .iter()
        .rev()
        .filter(|m| m.role == MsgRole::Assistant)
        .find_map(|m| m.route.as_deref())
}

fn parse_tier(s: &str) -> Option<classify::Tier> {
    match s {
        "light" => Some(classify::Tier::Light),
        "standard" => Some(classify::Tier::Standard),
        "heavy" => Some(classify::Tier::Heavy),
        _ => None,
    }
}

fn message_chars(m: &ChatMessage) -> usize {
    m.text.len()
        + m.blocks
            .iter()
            .map(|b| match b {
                AssistantBlock::Thinking(_) => 0,
                AssistantBlock::Answer(t) => t.len(),
                AssistantBlock::Tool {
                    args_summary,
                    output,
                    ..
                } => args_summary.as_deref().map_or(0, str::len) + output.len(),
            })
            .sum::<usize>()
}
