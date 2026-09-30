//! Agent tools, MCP, approval, limit, and web-search settings.

use super::super::OxiApp;
use super::layout::tool_chip;
use crate::agent::mcp::McpServerStatus;
use crate::settings::{ALL_TOOL_NAMES, DEFAULT_MCP_TIMEOUT_SECS, McpServerConfig, McpTransport};
use crate::theme::*;
use crate::ui::chrome::{
    alert_banner, card_frame, field_hint, field_label, field_label_first, ghost_button,
    nested_card_frame, pill_tab, settings_card_header, settings_password_field,
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
    ("Run commands", &["bash", "diagnostics"]),
    ("Plan & delegate", &["todo_write", "task"]),
    ("Notes", &["scratchpad"]),
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
                    ui.spacing_mut().item_spacing.x = 8.0;
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
                Some("Local (stdio) or remote (Streamable HTTP) MCP servers. Tools appear as mcp_<name>_<tool>; servers with resources also get list_resources / read_resource."),
            );
            let statuses = self.mcp.statuses();
            let mut remove_idx: Option<usize> = None;
            let n = self.conv.settings.mcp_servers.len();
            for i in 0..n {
                let server = &mut self.conv.settings.mcp_servers[i];
                let status = statuses.iter().find(|s| s.name == server.name);
                nested_card_frame().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut server.enabled, "");
                        settings_text_field_width(ui, &mut server.name, "name", 120.0);
                        for (transport, label) in
                            [(McpTransport::Stdio, "Command"), (McpTransport::Http, "HTTP")]
                        {
                            if pill_tab(ui, label, server.transport == transport) {
                                server.transport = transport;
                            }
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ghost_button(ui, "Remove", true).clicked() {
                                remove_idx = Some(i);
                            }
                            mcp_status_label(ui, server, status);
                        });
                    });
                    if let Some(err) = status
                        .filter(|s| server.enabled && !s.connected && !s.connecting)
                        .and_then(|s| s.error.as_deref())
                    {
                        ui.add_space(4.0);
                        alert_banner(ui, err, true);
                    }
                    ui.add_space(4.0);
                    match server.transport {
                        McpTransport::Stdio => {
                            ui.horizontal(|ui| {
                                settings_text_field_width(
                                    ui,
                                    &mut server.command,
                                    "command (npx, uvx, …)",
                                    150.0,
                                );
                                let mut args = server.args.join(" ");
                                if settings_text_field(ui, &mut args, "args…").changed() {
                                    server.args =
                                        args.split_whitespace().map(str::to_string).collect();
                                }
                            });
                            field_label(ui, "Environment (KEY=VALUE per line, stored in the OS keychain)");
                            ui.add(
                                egui::TextEdit::multiline(&mut server.env)
                                    .desired_rows(2)
                                    .desired_width(f32::INFINITY)
                                    .font(egui::TextStyle::Monospace)
                                    .hint_text("GITHUB_TOKEN=…"),
                            );
                        }
                        McpTransport::Http => {
                            settings_text_field(
                                ui,
                                &mut server.url,
                                "https://example.com/mcp",
                            );
                            field_label(ui, "Bearer token (optional, stored in the OS keychain)");
                            settings_password_field(ui, &mut server.bearer_token, "token");
                        }
                    }
                    field_label(ui, "Call timeout (seconds)");
                    let mut timeout = server
                        .timeout_secs
                        .map(|t| t.to_string())
                        .unwrap_or_default();
                    if settings_text_field_width(
                        ui,
                        &mut timeout,
                        &DEFAULT_MCP_TIMEOUT_SECS.to_string(),
                        120.0,
                    )
                    .changed()
                    {
                        server.timeout_secs = timeout.trim().parse::<u32>().ok().filter(|t| *t > 0);
                    }
                });
                ui.add_space(6.0);
            }
            if let Some(i) = remove_idx {
                self.conv.settings.mcp_servers.remove(i);
            }
            ui.horizontal(|ui| {
                if ghost_button(ui, "Add MCP server", false).clicked() {
                    self.conv
                        .settings
                        .mcp_servers
                        .push(crate::settings::McpServerConfig::default());
                }
                if !self.conv.settings.mcp_servers.is_empty()
                    && ghost_button(ui, "Connect / test", false)
                        .on_hover_text("Restart every server with the settings above and list its tools.")
                        .clicked()
                {
                    let mcp = self.mcp.clone();
                    let servers = self.conv.settings.mcp_servers.clone();
                    let ctx = ui.ctx().clone();
                    std::thread::spawn(move || {
                        mcp.reconnect_all(&servers);
                        ctx.request_repaint();
                    });
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
                }
            });
            if statuses.iter().any(|s| s.connecting) {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(250));
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
                    "When on, the agent pauses for your approval before each bash or diagnostics call (diagnostics runs the project's build tooling).",
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
                .icon(crate::ui::chrome::combo_chevron_icon)
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

/// One-line connection state for an MCP server row.
fn mcp_status_label(ui: &mut Ui, server: &McpServerConfig, status: Option<&McpServerStatus>) {
    let (text, color, hover) = match status {
        _ if !server.enabled => ("disabled".to_string(), c_text_faint(), None),
        Some(s) if s.connecting => ("connecting…".to_string(), c_text_muted(), None),
        Some(s) if s.connected => {
            let mut text = format!("● connected · {} tools", s.tools);
            if s.resources {
                text.push_str(" · resources");
            }
            (text, c_accent(), None)
        }
        Some(McpServerStatus {
            error: Some(err), ..
        }) => ("● error".to_string(), c_error_fg(), Some(err.clone())),
        _ => ("not connected".to_string(), c_text_faint(), None),
    };
    let resp = ui.label(RichText::new(text).size(FS_TINY).color(color));
    if let Some(hover) = hover {
        resp.on_hover_text(hover);
    }
}
