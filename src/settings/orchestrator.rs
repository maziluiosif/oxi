//! Settings for the [`LlmProviderKind::Orchestrator`] pseudo-provider: the lead agent the user
//! talks to, and the pool of agents it may hand work to. The orchestration itself lives in
//! [`crate::orchestrator`].

use serde::{Deserialize, Serialize};

use super::provider::LlmProviderKind;

/// One agent the lead may start workers on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerSlot {
    pub provider: LlmProviderKind,
    /// Model id; empty means the model selected for the provider, and lets the lead pick any of
    /// the provider's models.
    #[serde(default)]
    pub model: String,
    /// What this worker is good at, in the user's words; shown to the lead.
    #[serde(default)]
    pub note: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrchestratorSettings {
    /// The agent the user talks to.
    #[serde(default = "default_lead")]
    pub lead_provider: LlmProviderKind,
    /// Its model; empty means the model selected for that provider.
    #[serde(default)]
    pub lead_model: String,
    /// Agents the lead may use. Empty means every configured agent with its selected model.
    #[serde(default)]
    pub workers: Vec<WorkerSlot>,
    /// Workers running at once, per lead chat.
    #[serde(default = "default_max_workers")]
    pub max_workers: u8,
}

fn default_true() -> bool {
    true
}

fn default_lead() -> LlmProviderKind {
    LlmProviderKind::AntigravityAcp
}

fn default_max_workers() -> u8 {
    5
}

pub const MAX_WORKERS_LIMIT: u8 = 8;

impl Default for OrchestratorSettings {
    fn default() -> Self {
        Self {
            lead_provider: default_lead(),
            lead_model: String::new(),
            workers: Vec::new(),
            max_workers: default_max_workers(),
        }
    }
}

impl OrchestratorSettings {
    pub fn max_workers(&self) -> usize {
        usize::from(self.max_workers.clamp(1, MAX_WORKERS_LIMIT))
    }

    /// The slot allowing `provider` with `model` (empty: the slot's own model), if the pool has
    /// one. `default_pool` is what an empty pool stands for.
    pub fn slot_for<'a>(
        pool: &'a [WorkerSlot],
        provider: LlmProviderKind,
        model: &str,
    ) -> Option<&'a WorkerSlot> {
        let model = model.trim();
        let mut slots = pool.iter().filter(|s| s.enabled && s.provider == provider);
        if model.is_empty() {
            return slots.next();
        }
        let slots: Vec<_> = slots.collect();
        slots
            .iter()
            .find(|s| s.model.trim().eq_ignore_ascii_case(model))
            .or_else(|| slots.iter().find(|s| s.model.trim().is_empty()))
            .copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(provider: LlmProviderKind, model: &str) -> WorkerSlot {
        WorkerSlot {
            provider,
            model: model.into(),
            note: String::new(),
            enabled: true,
        }
    }

    #[test]
    fn a_model_matches_its_slot_or_a_slot_open_to_any_model() {
        let pool = vec![
            slot(LlmProviderKind::CodexAcp, "gpt-6.2-sol"),
            slot(LlmProviderKind::ClaudeCodeAcp, ""),
        ];
        let find = |p, m| OrchestratorSettings::slot_for(&pool, p, m).map(|s| s.model.clone());
        assert_eq!(
            find(LlmProviderKind::CodexAcp, "GPT-6.2-sol"),
            Some("gpt-6.2-sol".into())
        );
        assert_eq!(
            find(LlmProviderKind::CodexAcp, ""),
            Some("gpt-6.2-sol".into())
        );
        assert_eq!(find(LlmProviderKind::CodexAcp, "gpt-4"), None);
        assert_eq!(
            find(LlmProviderKind::ClaudeCodeAcp, "opus"),
            Some(String::new())
        );
        assert_eq!(find(LlmProviderKind::CursorAcp, ""), None);
    }

    #[test]
    fn older_settings_get_the_defaults() {
        let s: OrchestratorSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(s, OrchestratorSettings::default());
        assert_eq!(s.max_workers(), 5);
    }
}
