//! Agent tools, MCP, approval, limit, and web-search settings.

use super::super::OxiApp;
use super::layout::tool_chip;
use crate::agent::mcp::McpServerStatus;
use crate::settings::{ALL_TOOL_NAMES, DEFAULT_MCP_TIMEOUT_SECS, McpServerConfig, McpTransport};
use crate::theme::*;
use crate::ui::chrome::{
    alert_banner, field_hint, field_label, ghost_button, ghost_button_icon, icon_button_plain,
    pill_tab, setting_block, setting_item, setting_number, setting_row, setting_select,
    setting_switch, settings_disclosure, settings_password_field, settings_section,
    settings_section_title, settings_text_area, settings_text_field, settings_text_field_width,
};
use crate::ui::select_menu::SelectOption;
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
    (
        "Other agents",
        &["delegate_task", "task_status", "task_cancel"],
    ),
    ("Notes", &["scratchpad"]),
    ("Git", &["git_status", "git_diff"]),
    ("Web", &["web_search", "web_fetch"]),
];

impl OxiApp {
    pub(super) fn render_settings_agent_panel(&mut self, ui: &mut Ui) {
        settings_section_title(
            ui,
            "Agent tools",
            Some("What oxi's own agent may do, and when it has to ask first."),
        );

        // ── Approvals ──────────────────────────────────────────────────────
        settings_section(
            ui,
            "Approvals",
            Some(
                "The agent pauses and asks before matching tool calls. Bash is not sandboxed, so this is the real safety boundary.",
            ),
            |ui| {
                setting_switch(
                    ui,
                    &mut self.conv.settings.require_write_edit_approval,
                    "Ask before file changes",
                    Some("write, edit, delete, move and mkdir."),
                );
                setting_switch(
                    ui,
                    &mut self.conv.settings.require_bash_approval,
                    "Ask before commands",
                    Some("bash, and diagnostics (it runs the project's build tools)."),
                );
                if self.conv.settings.require_bash_approval {
                    setting_block(
                        ui,
                        "Always allowed commands",
                        Some(
                            "Commands starting with one of these run without asking. Pipes, ;, &&, redirects, $, quotes and globs always ask.",
                        ),
                        |ui| {
                            let mut remove_idx: Option<usize> = None;
                            for (i, entry) in
                                self.conv.settings.bash_allowlist.iter_mut().enumerate()
                            {
                                ui.horizontal(|ui| {
                                    settings_text_field_width(ui, entry, "cargo test", 240.0);
                                    if icon_button_plain(ui, ICON_CLOSE, 22.0, false)
                                        .on_hover_text("Remove")
                                        .clicked()
                                    {
                                        remove_idx = Some(i);
                                    }
                                });
                            }
                            if let Some(i) = remove_idx {
                                self.conv.settings.bash_allowlist.remove(i);
                            }
                            if ghost_button_icon(ui, ICON_PLUS, "Add command", false).clicked() {
                                self.conv.settings.bash_allowlist.push(String::new());
                            }
                        },
                    );
                }
                if self
                    .conv
                    .settings
                    .workspace_tool_allowlist
                    .values()
                    .any(|tools| !tools.is_empty())
                {
                    setting_block(
                        ui,
                        "Always allowed tools",
                        Some(
                            "Allowed with \u{201c}Always allow here\u{201d} on an approval, per workspace.",
                        ),
                        |ui| {
                            let mut remove: Option<(String, usize)> = None;
                            for (root, tools) in &self.conv.settings.workspace_tool_allowlist {
                                if tools.is_empty() {
                                    continue;
                                }
                                ui.label(
                                    RichText::new(root.as_str())
                                        .size(FS_TINY)
                                        .color(c_text_muted()),
                                );
                                ui.horizontal_wrapped(|ui| {
                                    ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                                    for (i, tool) in tools.iter().enumerate() {
                                        if tool_chip(ui, tool, true)
                                            .on_hover_text("Click to stop allowing it")
                                            .clicked()
                                        {
                                            remove = Some((root.clone(), i));
                                        }
                                    }
                                });
                                ui.add_space(4.0);
                            }
                            if let Some((root, i)) = remove
                                && let Some(tools) =
                                    self.conv.settings.workspace_tool_allowlist.get_mut(&root)
                            {
                                tools.remove(i);
                                if tools.is_empty() {
                                    self.conv.settings.workspace_tool_allowlist.remove(&root);
                                }
                            }
                        },
                    );
                }
            },
        );

        // ── Tools ──────────────────────────────────────────────────────────
        settings_section(ui, "Tools", None, |ui| {
            let enabled_count = self
                .conv
                .settings
                .tools_enabled
                .iter()
                .take(ALL_TOOL_NAMES.len())
                .filter(|enabled| **enabled)
                .count();
            let summary = format!(
                "{enabled_count} of {} available to the agent.",
                ALL_TOOL_NAMES.len()
            );
            setting_row(ui, "Enabled tools", Some(&summary), |ui| {
                if ghost_button(ui, "Disable all", false).clicked() {
                    self.conv.settings.tools_enabled.fill(false);
                }
                if ghost_button(ui, "Enable all", false).clicked() {
                    self.conv.settings.tools_enabled.fill(true);
                }
            });
            settings_disclosure(ui, "Choose tools", &["Enabled tools"], |ui| {
                for (group, names) in TOOL_GROUPS {
                    setting_block(ui, group, None, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
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
                    });
                }
            });
        });

        // ── Web search ─────────────────────────────────────────────────────
        settings_section(ui, "Web search", None, |ui| {
            use crate::settings::WebSearchBackend;
            let current = self.conv.settings.web_search_backend;
            let help = match current {
                WebSearchBackend::Bing => "No setup. Uses Bing's RSS results feed.",
                WebSearchBackend::DuckDuckGo => {
                    "No setup. Falls back to Bing when DuckDuckGo rate-limits."
                }
                WebSearchBackend::SearXng => "Your own instance, with JSON output enabled.",
            };
            if let Some(backend) = setting_select(
                ui,
                "web_search_backend",
                "Search with",
                Some(help),
                current.label(),
                || {
                    WebSearchBackend::ALL
                        .iter()
                        .map(|backend| SelectOption::new(backend.label(), *backend == current))
                        .collect()
                },
            )
            .and_then(|i| WebSearchBackend::ALL.get(i))
            {
                self.conv.settings.web_search_backend = *backend;
            }
            if self.conv.settings.web_search_backend == WebSearchBackend::SearXng {
                setting_block(ui, "SearXNG instance URL", None, |ui| {
                    settings_text_field(
                        ui,
                        &mut self.conv.settings.searxng_url,
                        "https://searxng.example.com",
                    );
                    if self.conv.settings.searxng_url.trim().is_empty() {
                        field_hint(ui, "No URL set: web_search reports a configuration error.");
                    }
                });
            }
        });

        // ── MCP servers ────────────────────────────────────────────────────
        settings_section(
            ui,
            "MCP servers",
            Some("Local commands or remote HTTP servers. Their tools appear as mcp_<name>_<tool>."),
            |ui| {
                let statuses = self.mcp.statuses();
                let mut remove_idx: Option<usize> = None;
                let n = self.conv.settings.mcp_servers.len();
                for i in 0..n {
                    let server = &mut self.conv.settings.mcp_servers[i];
                    let status = statuses.iter().find(|s| s.name == server.name);
                    setting_item(ui, |ui| {
                        ui.horizontal(|ui| {
                            crate::ui::chrome::toggle(ui, &mut server.enabled, "");
                            settings_text_field_width(ui, &mut server.name, "name", 120.0);
                            for (transport, label) in [
                                (McpTransport::Stdio, "Command"),
                                (McpTransport::Http, "HTTP"),
                            ] {
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
                                field_label(
                                    ui,
                                    "Environment (KEY=VALUE per line, stored in the OS keychain)",
                                );
                                settings_text_area(ui, &mut server.env, "GITHUB_TOKEN=…", 2);
                            }
                            McpTransport::Http => {
                                settings_text_field(ui, &mut server.url, "https://example.com/mcp");
                                field_label(
                                    ui,
                                    "Bearer token (optional, stored in the OS keychain)",
                                );
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
                            server.timeout_secs =
                                timeout.trim().parse::<u32>().ok().filter(|t| *t > 0);
                        }
                    });
                }
                if let Some(i) = remove_idx {
                    self.conv.settings.mcp_servers.remove(i);
                }
                setting_item(ui, |ui| {
                    ui.horizontal(|ui| {
                if ghost_button_icon(ui, ICON_PLUS, "Add MCP server", false).clicked() {
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
            })
                });
                if statuses.iter().any(|s| s.connecting) {
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(250));
                }
            },
        );

        // ── Limits ─────────────────────────────────────────────────────────
        settings_section(ui, "Limits", None, |ui| {
            settings_disclosure(
                ui,
                "Runaway loops and background agents",
                &["Limits"],
                |ui| {
                    let settings = &mut self.conv.settings;
                    setting_number(
                        ui,
                        "Max tool calls per run",
                        Some("0 means no cap."),
                        &mut settings.max_tool_rounds,
                        0,
                        0..=u32::MAX,
                    );
                    setting_number(
                        ui,
                        "Bash timeout cap (seconds)",
                        Some(
                            "Longest one command may run, 5\u{2013}3600. The model's own timeout is clamped to it.",
                        ),
                        &mut settings.bash_timeout_cap_secs,
                        300,
                        5..=3600,
                    );
                    setting_number(
                        ui,
                        "Stop idle ACP agents after (minutes)",
                        Some(
                            "Claude Code, Codex and Copilot chats each keep an agent process. An idle one is stopped and resumed on the next message. 0 means never.",
                        ),
                        &mut settings.acp_idle_minutes,
                        0,
                        0..=24 * 60,
                    );
                    let live = crate::agent::acp::live_count();
                    let help = format!(
                        "Past this, the least recently used idle agent stops. Running now: {live}. 0 means no cap."
                    );
                    setting_number(
                        ui,
                        "Max ACP agents running",
                        Some(&help),
                        &mut settings.acp_max_live_agents,
                        0,
                        0..=u32::MAX,
                    );
                },
            );
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
