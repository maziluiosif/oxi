//! Persistent settings (`~/.config/oxi/settings.json`).
//!
//! Split into [`provider`] (provider/profile domain types) and [`app_settings`] (the
//! top-level [`AppSettings`] struct, defaults, load/save/migration) and re-exported here,
//! so every existing `crate::settings::X` call site keeps working unchanged.

mod app_settings;
mod provider;

pub use app_settings::*;
pub use provider::*;
