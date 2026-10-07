//! Local usage ledger: one JSON line per provider round in `~/.config/oxi/usage.jsonl`.
//!
//! Providers without a usage API (Cursor, OpenCode Go, plain API keys) are only visible to the
//! router through this history: tokens per provider over the last 5 h / 7 d, and estimated
//! pay-per-use spend this month for the budget cap.

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use super::quota::now_secs;
use crate::agent::TokenUsage;
use crate::settings::{Billing, LlmProviderKind};

/// Entries older than this are not loaded (the file itself is left alone).
const KEEP_SECS: u64 = 35 * 86_400;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LedgerEntry {
    /// Unix seconds.
    pub ts: u64,
    pub provider: LlmProviderKind,
    pub model: String,
    #[serde(default)]
    pub input: u64,
    #[serde(default)]
    pub output: u64,
    #[serde(default)]
    pub cache_read: u64,
    #[serde(default)]
    pub cache_write: u64,
    /// Estimated USD; 0 for subscriptions and local models.
    #[serde(default)]
    pub cost: f64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Totals {
    pub tokens: u64,
    pub cost: f64,
    pub requests: u64,
}

struct Ledger {
    entries: Vec<LedgerEntry>,
    path: Option<PathBuf>,
}

fn ledger_path() -> Option<PathBuf> {
    Some(crate::app_dirs::config_dir().join("usage.jsonl"))
}

fn ledger() -> &'static Mutex<Ledger> {
    static LEDGER: OnceLock<Mutex<Ledger>> = OnceLock::new();
    LEDGER.get_or_init(|| {
        let path = if cfg!(test) { None } else { ledger_path() };
        let cutoff = now_secs().saturating_sub(KEEP_SECS);
        let entries = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|raw| {
                raw.lines()
                    .filter_map(|l| serde_json::from_str::<LedgerEntry>(l).ok())
                    .filter(|e| e.ts >= cutoff)
                    .collect()
            })
            .unwrap_or_default();
        Mutex::new(Ledger { entries, path })
    })
}

/// Estimated USD for one round on a pay-per-use provider. Cache reads bill at ~10% of the input
/// price and cache writes at ~125%, which is how Anthropic and OpenAI both price them.
pub fn estimate_cost(kind: LlmProviderKind, model: &str, usage: &TokenUsage) -> Option<f64> {
    let price = (kind == LlmProviderKind::OpenRouter)
        .then(|| super::quota::openrouter_price(model))
        .flatten()
        .or_else(|| super::catalog::price(model))?;
    let input = usage.input_tokens as f64
        + usage.cache_read_input_tokens as f64 * 0.1
        + usage.cache_creation_input_tokens as f64 * 1.25;
    Some((input * price.input + usage.output_tokens as f64 * price.output) / 1_000_000.0)
}

/// Append one round. Cheap enough for the UI thread (a single short line append).
pub fn record(kind: LlmProviderKind, model: &str, billing: Billing, usage: &TokenUsage) {
    if usage.is_zero() || kind == LlmProviderKind::Router {
        return;
    }
    let cost = match billing {
        Billing::PayPerUse => estimate_cost(kind, model, usage).unwrap_or(0.0),
        Billing::Subscription | Billing::Local => 0.0,
    };
    record_cost(kind, model, usage, cost);
}

/// Record an explicitly reported cost (e.g. an OpenRouter Decisions request).
pub(super) fn record_cost(kind: LlmProviderKind, model: &str, usage: &TokenUsage, cost: f64) {
    let entry = LedgerEntry {
        ts: now_secs(),
        provider: kind,
        model: model.to_string(),
        input: usage.input_tokens,
        output: usage.output_tokens,
        cache_read: usage.cache_read_input_tokens,
        cache_write: usage.cache_creation_input_tokens,
        cost,
    };
    let mut l = ledger().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(path) = &l.path
        && let Ok(line) = serde_json::to_string(&entry)
    {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(f, "{line}");
        }
    }
    l.entries.push(entry);
}

/// Totals for one provider (or all when `None`) since `since` (unix seconds).
pub fn totals(kind: Option<LlmProviderKind>, since: u64) -> Totals {
    let l = ledger().lock().unwrap_or_else(|e| e.into_inner());
    l.entries
        .iter()
        .filter(|e| e.ts >= since && kind.is_none_or(|k| e.provider == k))
        .fold(Totals::default(), |mut t, e| {
            t.tokens += e.input + e.output + e.cache_read + e.cache_write;
            t.cost += e.cost;
            t.requests += 1;
            t
        })
}

/// Unix seconds at the start of the current local month.
pub fn month_start() -> u64 {
    use chrono::{Datelike, Local, TimeZone};
    let now = Local::now();
    Local
        .with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
        .single()
        .map(|t| t.timestamp().max(0) as u64)
        .unwrap_or_else(|| now_secs().saturating_sub(30 * 86_400))
}

/// Estimated pay-per-use spend this month across providers.
pub fn spend_this_month() -> f64 {
    totals(None, month_start()).cost
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_estimate_discounts_cache_reads() {
        let usage = TokenUsage {
            input_tokens: 0,
            cache_read_input_tokens: 1_000_000,
            output_tokens: 0,
            ..Default::default()
        };
        // Sonnet input is $3/M; cached reads bill at 10%.
        let c = estimate_cost(
            LlmProviderKind::CustomAnthropic,
            "claude-sonnet-4-5",
            &usage,
        )
        .unwrap();
        assert!((c - 0.3).abs() < 1e-9);
    }

    #[test]
    fn records_and_totals_in_memory() {
        let usage = TokenUsage {
            input_tokens: 1000,
            output_tokens: 500,
            ..Default::default()
        };
        let before = totals(Some(LlmProviderKind::LlamaCpp), 0);
        record(
            LlmProviderKind::LlamaCpp,
            "qwen3:14b",
            Billing::Local,
            &usage,
        );
        let after = totals(Some(LlmProviderKind::LlamaCpp), 0);
        assert_eq!(after.tokens - before.tokens, 1500);
        assert_eq!(after.cost, before.cost);
    }
}
