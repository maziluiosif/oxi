//! Settings for the [`LlmProviderKind::Router`] pseudo-provider: which configured providers it
//! may pick from, how each one is billed, which model to use per task tier, and the budget /
//! quota guard rails. The routing logic itself lives in [`crate::router`].

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use super::provider::LlmProviderKind;

/// How a provider is paid for, which decides what "cost" means when the router scores it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Billing {
    /// Flat-rate plan with usage windows (Claude Pro/Max, ChatGPT Plus/Pro, Cursor, OpenCode Go):
    /// the marginal cost of a request is ~0 until the window runs out.
    Subscription,
    /// Metered API: every token costs money.
    PayPerUse,
    /// Runs on hardware the user owns.
    Local,
}

impl Billing {
    pub fn label(self) -> &'static str {
        match self {
            Billing::Subscription => "Subscription",
            Billing::PayPerUse => "Pay per use",
            Billing::Local => "Local",
        }
    }

    pub fn default_for(kind: LlmProviderKind) -> Billing {
        match kind {
            LlmProviderKind::ClaudeCodeAcp
            | LlmProviderKind::CursorAcp
            | LlmProviderKind::CodexAcp
            | LlmProviderKind::GptCodex
            | LlmProviderKind::OpenCodeGo => Billing::Subscription,
            LlmProviderKind::Ollama
            | LlmProviderKind::LmStudio
            | LlmProviderKind::LlamaCpp
            | LlmProviderKind::LocalHf
            | LlmProviderKind::RemoteHf => Billing::Local,
            LlmProviderKind::OpenAi
            | LlmProviderKind::OpenRouter
            | LlmProviderKind::AzureOpenAi
            | LlmProviderKind::CustomAnthropic
            | LlmProviderKind::Router => Billing::PayPerUse,
        }
    }
}

/// Per-provider router preferences.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouterProviderPrefs {
    /// Whether the router may pick this provider at all.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Overrides [`Billing::default_for`] (e.g. GPT Codex with an API key instead of OAuth).
    #[serde(default)]
    pub billing: Option<Billing>,
    /// Model ids per task tier. Empty means "the model selected for this provider".
    #[serde(default)]
    pub light_model: String,
    #[serde(default)]
    pub standard_model: String,
    #[serde(default)]
    pub heavy_model: String,
}

impl RouterProviderPrefs {
    pub fn new(kind: LlmProviderKind) -> Self {
        // Claude Code accepts its model aliases, so the tiers can be filled in up front.
        let (light, standard, heavy) = match kind {
            LlmProviderKind::ClaudeCodeAcp => ("haiku", "sonnet", "opus"),
            _ => ("", "", ""),
        };
        Self {
            enabled: true,
            billing: None,
            light_model: light.to_string(),
            standard_model: standard.to_string(),
            heavy_model: heavy.to_string(),
        }
    }
}

/// The router's strategy. Stored as the Router provider's `model_id`, so every chat keeps its
/// own strategy through [`crate::model::SessionConfig`] just like a regular model choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouterStrategy {
    /// Subscriptions first, then the cheapest model that is good enough.
    Balanced,
    /// Use up subscription windows before spending any money.
    Subscriptions,
    /// Lowest cost that still clears the quality bar.
    Cheapest,
    /// Highest quality, cost is a tie-breaker.
    Best,
    /// Local models unless the task is clearly too hard for them.
    Local,
}

impl RouterStrategy {
    pub const ALL: [RouterStrategy; 5] = [
        RouterStrategy::Balanced,
        RouterStrategy::Subscriptions,
        RouterStrategy::Cheapest,
        RouterStrategy::Best,
        RouterStrategy::Local,
    ];

    pub fn id(self) -> &'static str {
        match self {
            RouterStrategy::Balanced => "balanced",
            RouterStrategy::Subscriptions => "subscriptions-first",
            RouterStrategy::Cheapest => "cheapest",
            RouterStrategy::Best => "best-quality",
            RouterStrategy::Local => "local-first",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            RouterStrategy::Balanced => "Balanced",
            RouterStrategy::Subscriptions => "Subscriptions first",
            RouterStrategy::Cheapest => "Cheapest",
            RouterStrategy::Best => "Best quality",
            RouterStrategy::Local => "Local first",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            RouterStrategy::Balanced => {
                "Use subscription quota first, then the cheapest model that fits the task."
            }
            RouterStrategy::Subscriptions => {
                "Spend subscription windows before any pay-per-use credit."
            }
            RouterStrategy::Cheapest => "Lowest cost that still clears the quality bar.",
            RouterStrategy::Best => "Strongest model available; cost only breaks ties.",
            RouterStrategy::Local => "Stay on local models unless the task needs more.",
        }
    }

    pub fn from_id(id: &str) -> RouterStrategy {
        RouterStrategy::ALL
            .into_iter()
            .find(|s| s.id() == id.trim())
            .unwrap_or(RouterStrategy::Balanced)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouterSettings {
    /// Use Jev through OpenRouter for multilingual task classification.
    #[serde(default = "default_true")]
    pub use_jev: bool,
    #[serde(default)]
    pub providers: BTreeMap<LlmProviderKind, RouterProviderPrefs>,
    /// Keep this share of each subscription window for heavy tasks: light and standard
    /// tasks stop using a subscription once less than this percentage is left.
    #[serde(default = "default_reserve_pct")]
    pub reserve_pct: u8,
    /// Monthly cap for pay-per-use spend across providers, in USD. `0` = no cap.
    #[serde(default)]
    pub monthly_budget_usd: f64,
    /// Read Claude Code's OAuth token (keychain / `~/.claude/.credentials.json`) to query the
    /// subscription usage that `/usage` shows. Opt-in: it reads another app's credential.
    #[serde(default)]
    pub read_claude_code_usage: bool,
    /// Read Codex CLI's token (`~/.codex/auth.json`) to query the usage `/status` shows, for
    /// Codex (ACP). Opt-in for the same reason.
    #[serde(default)]
    pub read_codex_cli_usage: bool,
    /// Retry on the next-best provider when the chosen one is rate limited or out of quota.
    #[serde(default = "default_true")]
    pub failover: bool,
}

impl Default for RouterSettings {
    fn default() -> Self {
        Self {
            use_jev: true,
            providers: BTreeMap::new(),
            reserve_pct: default_reserve_pct(),
            monthly_budget_usd: 0.0,
            read_claude_code_usage: false,
            read_codex_cli_usage: false,
            failover: true,
        }
    }
}

impl RouterSettings {
    pub fn prefs(&self, kind: LlmProviderKind) -> RouterProviderPrefs {
        self.providers
            .get(&kind)
            .cloned()
            .unwrap_or_else(|| RouterProviderPrefs::new(kind))
    }

    pub fn prefs_mut(&mut self, kind: LlmProviderKind) -> &mut RouterProviderPrefs {
        self.providers
            .entry(kind)
            .or_insert_with(|| RouterProviderPrefs::new(kind))
    }

    pub fn billing(&self, kind: LlmProviderKind) -> Billing {
        self.prefs(kind)
            .billing
            .unwrap_or_else(|| Billing::default_for(kind))
    }
}

fn default_true() -> bool {
    true
}

fn default_reserve_pct() -> u8 {
    10
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jev_defaults_on_for_existing_settings_and_can_be_disabled() {
        let old: RouterSettings = serde_json::from_str(r#"{"reserve_pct":20}"#).unwrap();
        assert!(old.use_jev);
        assert_eq!(old.reserve_pct, 20);
        let disabled: RouterSettings = serde_json::from_str(r#"{"use_jev":false}"#).unwrap();
        assert!(!disabled.use_jev);
        let roundtrip: RouterSettings =
            serde_json::from_str(&serde_json::to_string(&disabled).unwrap()).unwrap();
        assert_eq!(roundtrip, disabled);
    }
}
