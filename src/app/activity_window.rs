//! Activity window: a floating inspector over [`crate::agent::activity_log`] — raw provider
//! requests and response streams, retries/errors, tool calls, and ACP/MCP traffic. Opened from
//! the status bar; recording is a persisted opt-in switch inside the window.

use eframe::egui::{self, Align, Layout, RichText, ScrollArea, TextEdit};

use crate::agent::activity_log::{self, ActivityEntry, ActivityKind};
use crate::theme::*;

use super::OxiApp;

/// View state that only lives as long as the app runs (not persisted).
#[derive(Clone)]
struct ActivityView {
    open: bool,
    query: String,
    hidden: Vec<ActivityKind>,
    selected: Option<u64>,
    follow: bool,
}

impl Default for ActivityView {
    fn default() -> Self {
        Self {
            open: false,
            query: String::new(),
            hidden: Vec::new(),
            selected: None,
            follow: true,
        }
    }
}

fn view_id() -> egui::Id {
    egui::Id::new("oxi_activity_window")
}

fn load_view(ctx: &egui::Context) -> ActivityView {
    ctx.data(|d| d.get_temp::<ActivityView>(view_id()))
        .unwrap_or_default()
}

fn store_view(ctx: &egui::Context, view: ActivityView) {
    ctx.data_mut(|d| d.insert_temp(view_id(), view));
}

pub(crate) fn activity_window_open(ctx: &egui::Context) -> bool {
    load_view(ctx).open
}

pub(crate) fn toggle_activity_window(ctx: &egui::Context) {
    let mut view = load_view(ctx);
    view.open = !view.open;
    store_view(ctx, view);
}

fn kind_color(kind: ActivityKind) -> egui::Color32 {
    match kind {
        ActivityKind::Request => c_accent(),
        ActivityKind::Response => c_text(),
        ActivityKind::Retry => c_warning_fg(),
        ActivityKind::Error => c_error_fg(),
        ActivityKind::Tool | ActivityKind::Acp | ActivityKind::Mcp => c_text_muted(),
    }
}

fn entry_matches(entry: &ActivityEntry, view: &ActivityView, query: &str) -> bool {
    if view.hidden.contains(&entry.kind) {
        return false;
    }
    query.is_empty()
        || entry.title.to_lowercase().contains(query)
        || entry.body.to_lowercase().contains(query)
}

fn entry_as_text(entry: &ActivityEntry) -> String {
    format!(
        "[{}] {} · {}\n{}\n",
        entry.at.format("%H:%M:%S%.3f"),
        entry.kind.label(),
        entry.title,
        entry.body
    )
}

impl OxiApp {
    /// Render the Activity window when it is open. Call once per frame from the top-level `ui`.
    pub(crate) fn render_activity_window(&mut self, ctx: &egui::Context) {
        let mut view = load_view(ctx);
        if !view.open {
            return;
        }
        let mut open = true;
        let mut recording = self.conv.settings.activity_log_enabled;
        let entries = activity_log::snapshot();
        let query = view.query.trim().to_lowercase();
        let visible: Vec<_> = entries
            .iter()
            .filter(|e| entry_matches(e, &view, &query))
            .cloned()
            .collect();

        let screen = ctx.content_rect();
        egui::Window::new("Activity")
            .id(view_id())
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size(egui::vec2(
                (screen.width() * 0.7).min(980.0),
                (screen.height() * 0.7).min(720.0),
            ))
            .min_size(egui::vec2(420.0, 260.0))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.checkbox(&mut recording, "Record").on_hover_text(
                        "Capture raw provider requests/responses, tool calls and ACP/MCP \
                             traffic. Credentials are redacted. Kept in memory only.",
                    );
                    ui.add(
                        TextEdit::singleline(&mut view.query)
                            .hint_text("Filter…")
                            .desired_width(180.0),
                    );
                    ui.checkbox(&mut view.follow, "Follow");
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("Clear").clicked() {
                            activity_log::clear();
                            view.selected = None;
                        }
                        if ui
                            .add_enabled(!visible.is_empty(), egui::Button::new("Copy shown"))
                            .clicked()
                        {
                            let all: String = visible.iter().map(|e| entry_as_text(e)).collect();
                            ui.ctx().copy_text(all);
                        }
                    });
                });
                ui.horizontal_wrapped(|ui| {
                    for kind in ActivityKind::ALL {
                        let shown = !view.hidden.contains(&kind);
                        let count = entries.iter().filter(|e| e.kind == kind).count();
                        let label = format!("{} {count}", kind.label());
                        if crate::ui::chrome::pill_tab(ui, &label, shown) {
                            if shown {
                                view.hidden.push(kind);
                            } else {
                                view.hidden.retain(|k| *k != kind);
                            }
                        }
                    }
                });
                ui.separator();

                if entries.is_empty() {
                    ui.add_space(12.0);
                    ui.label(
                        RichText::new(if recording {
                            "Recording. Send a message and the traffic shows up here."
                        } else {
                            "Recording is off. Turn on Record to capture what oxi sends to \
                             providers and agents, and what comes back."
                        })
                        .color(c_text_muted()),
                    );
                    return;
                }

                let list_h = (ui.available_height() * 0.42).max(120.0);
                let row_h = ui.spacing().interact_size.y;
                let selected = view.selected;
                let follow = view.follow;
                ScrollArea::vertical()
                    .id_salt("activity_list")
                    .max_height(list_h)
                    .auto_shrink([false, false])
                    .stick_to_bottom(follow)
                    .show_rows(ui, row_h, visible.len(), |ui, range| {
                        for entry in &visible[range] {
                            let text = RichText::new(format!(
                                "{}  {:<8}  {}",
                                entry.at.format("%H:%M:%S%.3f"),
                                entry.kind.label(),
                                entry.title
                            ))
                            .monospace()
                            .size(FS_SMALL)
                            .color(kind_color(entry.kind));
                            let resp = ui.add_sized(
                                [ui.available_width(), row_h],
                                egui::Button::selectable(selected == Some(entry.id), text)
                                    .right_text(()),
                            );
                            if resp.clicked() {
                                view.selected = Some(entry.id);
                                view.follow = false;
                            }
                        }
                    });
                ui.separator();

                let detail = view
                    .selected
                    .and_then(|id| visible.iter().find(|e| e.id == id))
                    .or(visible.last());
                if let Some(entry) = detail {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format!("{} · {}", entry.kind.label(), entry.title))
                                .strong()
                                .size(FS_SMALL),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.button("Copy").clicked() {
                                ui.ctx().copy_text(entry.body.clone());
                            }
                        });
                    });
                    ScrollArea::both()
                        .id_salt(("activity_detail", entry.id))
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(if entry.body.is_empty() {
                                        "(empty)"
                                    } else {
                                        entry.body.as_str()
                                    })
                                    .monospace()
                                    .size(FS_SMALL),
                                )
                                .selectable(true)
                                .extend(),
                            );
                        });
                }
            });

        if recording != self.conv.settings.activity_log_enabled {
            self.conv.settings.activity_log_enabled = recording;
            activity_log::set_enabled(recording);
            self.save_settings_quietly();
        }
        view.open = open;
        store_view(ctx, view);
        if recording {
            // New entries arrive from background threads; keep the list live while it's shown.
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }
}
