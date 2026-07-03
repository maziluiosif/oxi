//! Settings page: profiles panel, system prompt panel, OAuth sections.
//!
//! Split by responsibility: [`layout`] (page scaffolding: header/sidebar/body dispatch,
//! small pill/chip widgets), [`panels`] (the three top-level panels: providers, agent,
//! appearance), and [`profile`] (per-profile editing, compute-target/SSH, Codex OAuth,
//! and model-list fetching).

mod layout;
mod panels;
mod profile;
