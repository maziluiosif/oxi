//! Theme registry: built-in themes, custom theme loading from disk, and resolving a
//! theme id to a concrete [`Palette`].

use std::collections::BTreeMap;
use std::path::PathBuf;

use eframe::egui::{self, Color32};
use serde::{Deserialize, Serialize};

use super::palette::{Palette, rgb, set_active_palette};
use super::style::setup_style;

// ─── Theme registry ──────────────────────────────────────────────────────────

/// A selectable theme: stable `id`, display `name`, and its resolved `palette`.
#[derive(Debug, Clone, PartialEq)]
pub struct ThemeChoice {
    pub id: String,
    pub name: String,
    pub palette: Palette,
}

/// The default theme id, used when settings carry no (or an unknown) theme.
pub const DEFAULT_THEME_ID: &str = "dark";

/// Built-in themes, in display order.
pub fn builtin_themes() -> Vec<ThemeChoice> {
    vec![
        ThemeChoice {
            id: "dark".to_string(),
            name: "Dark".to_string(),
            palette: Palette::DARK,
        },
        ThemeChoice {
            id: "light".to_string(),
            name: "Light".to_string(),
            palette: Palette::LIGHT,
        },
        ThemeChoice {
            id: "midnight".to_string(),
            name: "Midnight".to_string(),
            palette: Palette::MIDNIGHT,
        },
        ThemeChoice {
            id: "sublime".to_string(),
            name: "Sublime".to_string(),
            palette: Palette::SUBLIME,
        },
        ThemeChoice {
            id: "mariana".to_string(),
            name: "Sublime 4".to_string(),
            palette: Palette::MARIANA,
        },
    ]
}

/// Base scheme a custom theme builds on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ThemeBase {
    #[default]
    Dark,
    Light,
}

/// On-disk custom theme. Drop a `<id>.json` file in [`custom_themes_dir`] and it shows
/// up in the theme picker. `colors` maps palette field names (e.g. `"bg_main"`) to
/// `#rrggbb` strings; any field left out inherits from `base`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeSpec {
    pub name: String,
    #[serde(default)]
    pub base: ThemeBase,
    #[serde(default)]
    pub colors: BTreeMap<String, String>,
}

/// Parse a `#rrggbb` (or `rrggbb`) hex string into a [`Color32`].
pub fn parse_hex_color(s: &str) -> Option<Color32> {
    let h = s.trim().trim_start_matches('#');
    if h.len() != 6 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let r = u8::from_str_radix(&h[0..2], 16).ok()?;
    let g = u8::from_str_radix(&h[2..4], 16).ok()?;
    let b = u8::from_str_radix(&h[4..6], 16).ok()?;
    Some(rgb(r, g, b))
}

impl ThemeSpec {
    /// Resolve to a concrete palette, applying recognized color overrides over the base.
    pub fn into_palette(self) -> Palette {
        let mut p = match self.base {
            ThemeBase::Light => Palette::LIGHT,
            ThemeBase::Dark => Palette::DARK,
        };
        for (key, value) in &self.colors {
            let Some(c) = parse_hex_color(value) else {
                continue;
            };
            match key.as_str() {
                "bg_main" => p.bg_main = c,
                "bg_sidebar" => p.bg_sidebar = c,
                "bg_elevated" => p.bg_elevated = c,
                "bg_elevated_2" => p.bg_elevated_2 = c,
                "bg_input" => p.bg_input = c,
                "faint_bg" => p.faint_bg = c,
                "border" => p.border = c,
                "border_subtle" => p.border_subtle = c,
                "accent" => p.accent = c,
                "text" => p.text = c,
                "text_strong" => p.text_strong = c,
                "text_muted" => p.text_muted = c,
                "text_faint" => p.text_faint = c,
                "sidebar_section" => p.sidebar_section = c,
                "user_bubble" => p.user_bubble = c,
                "success" => p.success = c,
                "danger" => p.danger = c,
                "diff_add_fg" => p.diff_add_fg = c,
                "diff_add_bg" => p.diff_add_bg = c,
                "diff_del_fg" => p.diff_del_fg = c,
                "diff_del_bg" => p.diff_del_bg = c,
                "widget_inactive_bg" => p.widget_inactive_bg = c,
                "widget_hovered_bg" => p.widget_hovered_bg = c,
                "widget_active_bg" => p.widget_active_bg = c,
                "widget_open_bg" => p.widget_open_bg = c,
                "widget_active_border" => p.widget_active_border = c,
                "selection_bg" => p.selection_bg = c,
                "selection_stroke" => p.selection_stroke = c,
                "md_code_bg" => p.md_code_bg = c,
                "md_code_block_bg" => p.md_code_block_bg = c,
                "md_code_block_header_bg" => p.md_code_block_header_bg = c,
                "md_code_block_border" => p.md_code_block_border = c,
                "md_quote_accent" => p.md_quote_accent = c,
                "md_code_fg" => p.md_code_fg = c,
                _ => {}
            }
        }
        p
    }
}

/// Directory custom theme files are read from: `<config>/oxi/themes`.
pub fn custom_themes_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("oxi")
        .join("themes")
}

/// Load custom themes from disk. Malformed files are skipped. Custom ids are namespaced
/// `custom:<file-stem>` so they never collide with built-ins.
pub fn load_custom_themes() -> Vec<ThemeChoice> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(custom_themes_dir()) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Ok(spec) = serde_json::from_slice::<ThemeSpec>(&bytes) else {
            continue;
        };
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("custom")
            .to_string();
        let name = if spec.name.trim().is_empty() {
            stem.clone()
        } else {
            spec.name.clone()
        };
        out.push(ThemeChoice {
            id: format!("custom:{stem}"),
            name,
            palette: spec.into_palette(),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// All selectable themes: built-ins followed by any custom themes on disk.
pub fn available_themes() -> Vec<ThemeChoice> {
    let mut v = builtin_themes();
    v.extend(load_custom_themes());
    v
}

/// Resolve a theme id to its palette, falling back to [`Palette::DARK`] if unknown.
pub fn resolve_palette(id: &str) -> Palette {
    available_themes()
        .into_iter()
        .find(|t| t.id == id)
        .map(|t| t.palette)
        .unwrap_or(Palette::DARK)
}

/// Set the active theme by id and rebuild egui visuals on `ctx`.
pub fn apply_theme(ctx: &egui::Context, id: &str) {
    set_active_palette(resolve_palette(id));
    setup_style(ctx);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_themes_present_and_unique() {
        let themes = builtin_themes();
        assert!(themes.iter().any(|t| t.id == "dark"));
        assert!(themes.iter().any(|t| t.id == "light"));
        assert!(themes.iter().any(|t| t.id == "midnight"));
        let mut ids: Vec<&str> = themes.iter().map(|t| t.id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), themes.len(), "theme ids must be unique");
    }

    #[test]
    fn dark_and_light_differ_in_base() {
        let dark = resolve_palette("dark");
        let light = resolve_palette("light");
        assert!(dark.dark_base);
        assert!(!light.dark_base);
        assert_ne!(dark.bg_main, light.bg_main);
        assert_ne!(dark.text, light.text);
    }

    #[test]
    fn resolve_palette_known_and_unknown() {
        assert_eq!(resolve_palette("light"), Palette::LIGHT);
        assert_eq!(resolve_palette("midnight"), Palette::MIDNIGHT);
        // Unknown ids fall back to dark.
        assert_eq!(resolve_palette("does-not-exist"), Palette::DARK);
    }

    #[test]
    fn parse_hex_color_valid_and_invalid() {
        assert_eq!(parse_hex_color("#0f1012"), Some(rgb(0x0f, 0x10, 0x12)));
        assert_eq!(parse_hex_color("0f1012"), Some(rgb(0x0f, 0x10, 0x12)));
        assert_eq!(parse_hex_color("  #ffffff "), Some(rgb(0xff, 0xff, 0xff)));
        assert_eq!(parse_hex_color("#fff"), None);
        assert_eq!(parse_hex_color("#gggggg"), None);
        assert_eq!(parse_hex_color(""), None);
    }

    #[test]
    fn theme_spec_overrides_base() {
        let mut colors = BTreeMap::new();
        colors.insert("bg_main".to_string(), "#123456".to_string());
        colors.insert("accent".to_string(), "#abcdef".to_string());
        // Unknown key is ignored rather than erroring.
        colors.insert("not_a_field".to_string(), "#000000".to_string());
        let spec = ThemeSpec {
            name: "Custom".to_string(),
            base: ThemeBase::Light,
            colors,
        };
        let p = spec.into_palette();
        assert_eq!(p.bg_main, rgb(0x12, 0x34, 0x56));
        assert_eq!(p.accent, rgb(0xab, 0xcd, 0xef));
        // Unspecified fields inherit from the light base.
        assert_eq!(p.text, Palette::LIGHT.text);
        assert!(!p.dark_base);
    }

    #[test]
    fn theme_spec_parses_from_json() {
        let json = r##"{ "name": "Mine", "base": "dark", "colors": { "accent": "#ff0000" } }"##;
        let spec: ThemeSpec = serde_json::from_str(json).unwrap();
        assert_eq!(spec.name, "Mine");
        let p = spec.into_palette();
        assert_eq!(p.accent, rgb(0xff, 0x00, 0x00));
        assert!(p.dark_base);
    }
}
