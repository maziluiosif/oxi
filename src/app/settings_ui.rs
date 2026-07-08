//! Settings page: providers panel, agent/prompt panels, OAuth sections.
//!
//! Split by responsibility: [`layout`] (page scaffolding: header/sidebar/body dispatch,
//! small pill/chip widgets), [`panels`] (the top-level panels: providers, agent, prompts,
//! appearance, about), and [`provider_panel`] (per-provider config editing,
//! compute-target/SSH, Codex OAuth, and model-list fetching).

mod layout;
mod local_hf_panel;
mod panels;
mod provider_panel;
