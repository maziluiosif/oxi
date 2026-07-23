//! Agent tools, MCP, approval, limit, and web-search settings.

use super::super::OxiApp;
use super::layout::tool_chip;
use crate::settings::ALL_TOOL_NAMES;
use crate::theme::*;
use crate::ui::chrome::{
    card_frame, field_hint, field_label, field_label_first, ghost_button, settings_card_header,
    settings_section_title, settings_text_field, settings_text_field_width,
};
use eframe::egui::{self, Align, Layout, RichText, Ui};

/// Tool chips grouped by intent. Keep this list in sync with `ALL_TOOL_NAMES`.
const TOOL_GROUPS: &[(&str, &[&str])] = &[
    (
        "Explore workspace",
        &["read", "grep", "find", "ls", "codebase_search"],
    ),
    (
        "Change files",
        &["write", "edit", "delete", "move", "mkdir"],
    ),
    ("Run commands", &["bash"]),
    ("Git", &["git_status", "git_diff"]),
    ("Web", &["web_search", "web_fetch"]),
];

impl OxiApp {
    pub(super) fn render_settings_agent_panel(&mut self, ui: &mut Ui) {
        settings_section_title(
            ui,
            "Tools & safety",
            Some(
                "Control which tools the agent can call, when it must ask first, and how web search works.",
            ),
        );

        // ── Tools ──────────────────────────────────────────────────────────
        card_frame().show(ui, |ui| {
            let enabled_count = self
                .conv
                .settings
                .tools_enabled
                .iter()
                .take(ALL_TOOL_NAMES.len())
                .filter(|enabled| **enabled)
                .count();
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new("Enabled tools")
                            .size(FS_BODY)
                            .color(c_text())
                            .strong(),
                    );
                    ui.add_space(2.0);
                    ui.label(
                        RichText::new(format!(
                            "{enabled_count} of {} available to the agent",
                            ALL_TOOL_NAMES.len()
                        ))
                        .size(FS_TINY)
                        .color(c_text_muted()),
                    );
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ghost_button(ui, "Disable all", false).clicked() {
                        self.conv.settings.tools_enabled.fill(false);
                    }
                    if ghost_button(ui, "Enable all", false).clicked() {
                        self.conv.settings.tools_enabled.fill(true);
                    }
                });
            });
            ui.add_space(10.0);
            for (gi, (group, names)) in TOOL_GROUPS.iter().enumerate() {
                if gi > 0 {
                    ui.add_space(10.0);
                }
                ui.label(RichText::new(*group).size(FS_TINY).color(c_text_muted()));
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(8.0, 6.0);
                    for name in *names {
                        let Some(i) = ALL_TOOL_NAMES.iter().position(|n| n == name) else {
                            continue;
                        };
                        let enabled = self.conv.settings.tools_enabled[i];
                        if tool_chip(ui, name, enabled).clicked() {
                            self.conv.settings.tools_enabled[i] = !enabled;
                        }
                    }
                });
            }
        });

        // ── MCP servers ────────────────────────────────────────────────────
        ui.add_space(12.0);
        card_frame().show(ui, |ui| {
            settings_card_header(
                ui,
                "MCP servers",
                Some("Stdio MCP servers. Tools appear as mcp_<name>_<tool> in the agent."),
            );
            let mut remove_idx: Option<usize> = None;
            let n = self.conv.settings.mcp_servers.len();
            for i in 0..n {
                let server = &mut self.conv.settings.mcp_servers[i];
                ui.horizontal(|ui| {
                    ui.checkbox(&mut server.enabled, "");
                    settings_text_field_width(ui, &mut server.name, "name", 100.0);
                    settings_text_field_width(ui, &mut server.command, "command", 120.0);
                    let mut args = server.args.join(" ");
                    if settings_text_field_width(ui, &mut args, "args…", 180.0).changed() {
                        server.args = args.split_whitespace().map(str::to_string).collect();
                    }
                    if ghost_button(ui, "Remove", true).clicked() {
                        remove_idx = Some(i);
                    }
                });
                ui.add_space(4.0);
            }
            if let Some(i) = remove_idx {
                self.conv.settings.mcp_servers.remove(i);
            }
            if ghost_button(ui, "Add MCP server", false).clicked() {
                self.conv
                    .settings
                    .mcp_servers
                    .push(crate::settings::McpServerConfig::default());
            }
        });

        // ── Approvals ──────────────────────────────────────────────────────
        ui.add_space(12.0);
        card_frame().show(ui, |ui| {
            settings_card_header(
                ui,
                "Approvals",
                Some("When on, the agent pauses and asks before each matching tool call."),
            );
            let mut require_write_edit_approval = self.conv.settings.require_write_edit_approval;
            if ui
                .checkbox(
                    &mut require_write_edit_approval,
                    RichText::new("Ask before file changes")
                        .size(FS_SMALL)
                        .color(c_text()),
                )
                .on_hover_text(
                    "When on, the agent pauses before write, edit, delete, move, or mkdir tool calls.",
                )
                .changed()
            {
                self.conv.settings.require_write_edit_approval = require_write_edit_approval;
            }
            let mut require_bash_approval = self.conv.settings.require_bash_approval;
            if ui
                .checkbox(
                    &mut require_bash_approval,
                    RichText::new("Ask before bash").size(FS_SMALL).color(c_text()),
                )
                .on_hover_text(
                    "When on, the agent pauses for your approval before each bash tool call.",
                )
                .changed()
            {
                self.conv.settings.require_bash_approval = require_bash_approval;
            }
            ui.add_space(4.0);
            ui.label(
                RichText::new(
                    "Bash is not sandboxed; the approval prompt is the real safety boundary. Read-only tools never require approval.",
                )
                .size(FS_TINY)
                .color(c_text_faint()),
            );
        });

        // ── Limits ─────────────────────────────────────────────────────────
        ui.add_space(12.0);
        card_frame().show(ui, |ui| {
            settings_card_header(
                ui,
                "Limits",
                Some("Caps that keep a runaway agent loop from going forever."),
            );

            field_label_first(ui, "Max tool calls per run (0 = unlimited)");
            let mut max_rounds = self.conv.settings.max_tool_rounds.to_string();
            if settings_text_field_width(ui, &mut max_rounds, "0", 180.0).changed() {
                let trimmed = max_rounds.trim();
                if trimmed.is_empty() {
                    self.conv.settings.max_tool_rounds = 0;
                } else if let Ok(n) = trimmed.parse::<u32>() {
                    self.conv.settings.max_tool_rounds = n;
                }
            }
            field_hint(
                ui,
                "Caps tool-call rounds in a single agent run. 0 disables the cap.",
            );

            field_label(ui, "Bash timeout cap (seconds)");
            let mut bash_cap = self.conv.settings.bash_timeout_cap_secs.to_string();
            if settings_text_field_width(ui, &mut bash_cap, "300", 180.0).changed()
                && let Ok(n) = bash_cap.trim().parse::<u32>()
                && n >= 1
            {
                self.conv.settings.bash_timeout_cap_secs = n.clamp(5, 3600);
            }
            field_hint(
                ui,
                "Upper bound for one bash call (5–3600s). The model's own timeout is clamped to this.",
            );
        });

        // ── Web search ─────────────────────────────────────────────────────
        ui.add_space(12.0);
        card_frame().show(ui, |ui| {
            settings_card_header(
                ui,
                "Web search",
                Some("Backend used by the web_search tool."),
            );
            let current = self.conv.settings.web_search_backend;
            egui::ComboBox::from_id_salt("web_search_backend_combo")
                .selected_text(current.label())
                .width(220.0)
                .show_ui(ui, |ui| {
                    for backend in crate::settings::WebSearchBackend::ALL {
                        if ui
                            .selectable_label(backend == current, backend.label())
                            .clicked()
                        {
                            self.conv.settings.web_search_backend = backend;
                        }
                    }
                });
            ui.add_space(6.0);
            match self.conv.settings.web_search_backend {
                crate::settings::WebSearchBackend::Bing => {
                    ui.label(
                        RichText::new(
                            "Zero-config. Uses Bing's RSS results feed. No fallback if Bing fails.",
                        )
                        .size(FS_TINY)
                        .color(c_text_muted()),
                    );
                }
                crate::settings::WebSearchBackend::DuckDuckGo => {
                    ui.label(
                        RichText::new(
                            "Zero-config. DuckDuckGo HTML endpoint — may serve a bot challenge; Bing is usually more reliable.",
                        )
                        .size(FS_TINY)
                        .color(c_text_muted()),
                    );
                }
                crate::settings::WebSearchBackend::SearXng => {
                    field_label_first(ui, "SearXNG instance URL");
                    settings_text_field(
                        ui,
                        &mut self.conv.settings.searxng_url,
                        "https://searxng.example.com",
                    )
                    .on_hover_text(
                        "Base URL of your SearXNG instance. JSON output must be enabled (search.formats: [html, json]).",
                    );
                    if self.conv.settings.searxng_url.trim().is_empty() {
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(
                                "No URL set — web_search will report a configuration error.",
                            )
                            .size(FS_TINY)
                            .color(c_text_faint()),
                        );
                    } else {
                        field_hint(ui, "Requires JSON format enabled on the instance.");
                    }
                }
            }
        });
    }
}
