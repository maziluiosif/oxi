//! Settings page: providers panel, agent/prompt panels, OAuth sections.
//!
//! Split by responsibility:
//! - [`layout`] — page scaffolding: grouped sidebar nav, header, body dispatch, pills/chips
//! - [`panels`] — top-level pages: providers, agent tools/safety, prompts, appearance, about
//! - [`provider_panel`] — per-provider form (model / connection / compute target / OAuth)
//! - [`local_hf_panel`] — Local HF model search/download/runtime controls
//! - [`dictation_panel`] — voice dictation + Whisper models

mod dictation_panel;
mod layout;
mod local_hf_panel;
mod panels;
mod provider_panel;
