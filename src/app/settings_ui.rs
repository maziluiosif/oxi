//! Settings page UI split by page and supporting subsystem.
//!
//! - [`layout`] owns page scaffolding, navigation, and save/discard flow.
//! - Page modules render one top-level settings destination each.
//! - Provider modules cover forms, SSH, OAuth, and background model discovery.
//! - Local HF modules cover the managed runtime UI, tasks, and message handling.

mod about_panel;
mod agent_panel;
mod appearance_panel;
mod dictation_panel;
mod github_panel;
mod layout;
mod local_hf_messages;
mod local_hf_panel;
mod local_hf_runtime;
mod prompts_panel;
mod provider_catalog;
mod provider_models;
mod provider_oauth;
mod provider_panel;
mod provider_ssh;
mod providers_panel;
mod terminal_panel;
