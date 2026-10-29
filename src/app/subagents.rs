//! Navigation and transcripts for provider-owned child conversations.
use super::{OxiApp, SessionKey};
use crate::model::delegated::DelegatedChat;
use crate::theme::*;
use eframe::egui::{self, RichText, Ui};

fn selected_id(app: &OxiApp, key: SessionKey) -> egui::Id {
    egui::Id::new((
        "selected_subagent",
        &app.conv.workspaces[key.workspace_idx].root_path,
        &app.session_by_key(key).session_file,
    ))
}
fn color(c: &DelegatedChat) -> egui::Color32 {
    if c.running() && !c.live {
        return c_text_faint();
    }
    match c.status() {
        "failed" | "error" => c_error_fg(),
        "running" | "requires_action" => c_accent(),
        _ => c_text_muted(),
    }
}
fn status(c: &DelegatedChat) -> &str {
    if c.running() && !c.live {
        return "Saved";
    }
    match c.status() {
        "running" => "Running",
        "completed" => "Done",
        "failed" | "error" => "Failed",
        "cancelled" | "canceled" => "Stopped",
        "requires_action" => "Needs input",
        "idle" => "Idle",
        "paused" => "Paused",
        other => other,
    }
}

/// Same centered, bounded column as the parent transcript and composer.
fn column(ui: &mut Ui, content: impl FnOnce(&mut Ui)) {
    let available = chat_column_center_width(ui.available_width(), ui.style());
    let width = available.min(chat_column_max_width(ui.ctx()));
    let pad = ((available - width) * 0.5).max(0.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.add_space(pad);
        ui.vertical(|ui| {
            ui.set_width(width);
            ui.spacing_mut().item_spacing.x = 8.0;
            content(ui);
        });
    });
}
fn text_at(ui: &Ui, text: &str, pos: egui::Pos2, width: f32, size: f32, color: egui::Color32) {
    let mut job = egui::text::LayoutJob::simple(
        text.to_owned(),
        egui::FontId::proportional(size),
        color,
        width.max(1.0),
    );
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    let galley = ui.painter().layout_job(job);
    ui.painter().galley(pos, galley, color);
}
/// One aligned row; color belongs to the state marker, not the entire agent name.
fn agent_row(ui: &mut Ui, c: &DelegatedChat, selected: bool, compact: bool) -> egui::Response {
    let height = if compact { 26.0 } else { 48.0 };
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::click(),
    );
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, c.label())
    });
    if selected || response.hovered() {
        ui.painter().rect_filled(
            rect,
            RADIUS_ROW,
            if selected {
                c_accent().gamma_multiply(0.13)
            } else {
                c_bg_elevated()
            },
        );
    }
    let marker = egui::pos2(rect.left() + 10.0, rect.center().y);
    ui.painter().circle_filled(marker, 2.5, color(c));
    let title_y = if compact {
        rect.center().y - FS_SMALL * 0.6
    } else {
        rect.top() + 8.0
    };
    let right = if compact { 8.0 } else { 76.0 };
    text_at(
        ui,
        c.label(),
        egui::pos2(rect.left() + 22.0, title_y),
        rect.width() - 22.0 - right,
        FS_SMALL,
        if selected || !compact {
            c_text()
        } else {
            c_text_muted()
        },
    );
    if !compact {
        let detail = if !c.model.is_empty() && !c.model.starts_with('<') {
            c.model.as_str()
        } else if c.task {
            "Workflow"
        } else {
            c.description.as_str()
        };
        text_at(
            ui,
            detail,
            egui::pos2(rect.left() + 22.0, rect.top() + 29.0),
            rect.width() - 98.0,
            FS_TINY,
            c_text_faint(),
        );
        ui.painter().text(
            egui::pos2(rect.right() - 12.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            status(c),
            egui::FontId::proportional(FS_TINY),
            color(c),
        );
    }
    crate::ui::chrome::keyboard_focus_ring(ui, &response, egui::CornerRadius::same(RADIUS_ROW));
    response.on_hover_text(format!("{} · {}\n{}", c.label(), status(c), c.description))
}
impl OxiApp {
    pub(super) fn selected_subagent(&self, ctx: &egui::Context) -> Option<(String, String)> {
        let key = self.active_session_key();
        ctx.data(|d| d.get_temp(selected_id(self, key)))
    }
    pub(super) fn select_subagent(
        &self,
        ctx: &egui::Context,
        key: SessionKey,
        root: &str,
        id: &str,
    ) {
        ctx.data_mut(|d| d.insert_temp(selected_id(self, key), (root.to_owned(), id.to_owned())));
    }
    pub(super) fn close_subagent(&self, ctx: &egui::Context) {
        ctx.data_mut(|d| {
            d.remove::<(String, String)>(selected_id(self, self.active_session_key()))
        });
    }
    pub(super) fn render_subagent_roster(&mut self, ui: &mut Ui) {
        let key = self.active_session_key();
        let chats = &self.session_by_key(key).delegated.chats;
        let links: Vec<(String, String, String, Option<String>)> = chats
            .iter()
            .map(|c| {
                (
                    c.root.clone(),
                    c.id.clone(),
                    c.label().to_string(),
                    c.tool_call_id.clone(),
                )
            })
            .collect();
        ui.ctx()
            .data_mut(|d| d.insert_temp(egui::Id::new("subagent_links"), links));
        if chats.is_empty() {
            return;
        }
        let agents = chats.iter().filter(|c| !c.task).count();
        let active = chats
            .iter()
            .filter(|c| !c.task && c.running() && c.live)
            .count();
        column(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                ui.menu_button(
                    RichText::new(format!("Agents · {agents}  ▾"))
                        .size(FS_SMALL)
                        .color(c_text_muted()),
                    |ui| {
                        ui.set_width(280.0_f32.min(ui.ctx().content_rect().width() - 32.0));
                        ui.label(
                            RichText::new("Workflows & agents")
                                .size(FS_TINY)
                                .color(c_text_faint()),
                        );
                        egui::ScrollArea::vertical()
                            .max_height(320.0)
                            .show(ui, |ui| {
                                for c in chats.iter().filter(|c| {
                                    c.parent == c.root
                                        || !chats
                                            .iter()
                                            .any(|p| p.root == c.root && p.id == c.parent)
                                }) {
                                    if agent_row(ui, c, false, false).clicked() {
                                        self.select_subagent(ui.ctx(), key, &c.root, &c.id);
                                        ui.close();
                                    }
                                }
                            });
                    },
                );
                if active > 0 {
                    ui.label(
                        RichText::new(format!("{active} running"))
                            .size(FS_TINY)
                            .color(c_accent()),
                    );
                }
            });
            ui.add_space(4.0);
        });
    }
    pub(super) fn render_sidebar_subagents(&mut self, ui: &mut Ui, key: SessionKey) {
        if key != self.active_session_key() {
            return;
        }
        let chats = &self.session_by_key(key).delegated.chats;
        if chats.is_empty() {
            return;
        }
        let roots: Vec<_> = chats
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                c.parent == c.root || !chats.iter().any(|p| p.root == c.root && p.id == c.parent)
            })
            .map(|(i, _)| i)
            .collect();
        let mut visited = std::collections::HashSet::new();
        ui.indent(("subagents", key), |ui| {
            let mut rows = |ui: &mut Ui| {
                for index in &roots {
                    self.render_subagent_sidebar_node(ui, key, *index, &mut visited);
                }
            };
            // Direct Agent calls have no workflow parent. Give large batches the same
            // folded hierarchy so a burst of spawned agents cannot take over the sidebar.
            if roots.len() > 4 {
                egui::CollapsingHeader::new(
                    RichText::new(format!("Agents · {}", roots.len()))
                        .size(FS_SMALL)
                        .color(c_text_muted()),
                )
                .id_salt(("direct_agents", selected_id(self, key)))
                .default_open(false)
                .show(ui, rows);
            } else {
                rows(ui);
            }
        });
    }
    fn render_subagent_sidebar_node(
        &self,
        ui: &mut Ui,
        key: SessionKey,
        index: usize,
        visited: &mut std::collections::HashSet<usize>,
    ) {
        if !visited.insert(index) {
            return;
        }
        let chats = &self.session_by_key(key).delegated.chats;
        let c = &chats[index];
        let children: Vec<usize> = chats
            .iter()
            .enumerate()
            .filter(|(_, child)| child.root == c.root && child.parent == c.id)
            .map(|(i, _)| i)
            .collect();
        let selected = self.selected_subagent(ui.ctx());
        let selected_here = selected
            .as_ref()
            .is_some_and(|(r, id)| r == &c.root && id == &c.id);
        let selected_child = selected.as_ref().is_some_and(|(root, id)| {
            let mut id = id.as_str();
            for _ in 0..chats.len() {
                let Some(child) = chats.iter().find(|ch| ch.root == *root && ch.id == id) else {
                    return false;
                };
                if child.parent == c.id && child.root == c.root {
                    return true;
                }
                if child.parent == child.root {
                    return false;
                }
                id = &child.parent;
            }
            false
        });
        let fold_id = egui::Id::new(("subagent_branch", selected_id(self, key), &c.root, &c.id));
        let default_open = selected_child;
        let mut open = ui
            .ctx()
            .data(|d| d.get_temp::<bool>(fold_id))
            .unwrap_or(default_open);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            if !children.is_empty()
                && ui
                    .add(
                        egui::Button::new(
                            RichText::new(if open { "▾" } else { "▸" })
                                .size(FS_TINY)
                                .color(c_text_faint()),
                        )
                        .frame(false)
                        .min_size(egui::vec2(16.0, 26.0)),
                    )
                    .clicked()
            {
                open = !open;
                ui.ctx().data_mut(|d| d.insert_temp(fold_id, open));
            }
            if agent_row(ui, c, selected_here, true).clicked() {
                self.select_subagent(ui.ctx(), key, &c.root, &c.id);
            }
        });
        if open {
            ui.indent((&c.root, &c.id), |ui| {
                for child in children {
                    self.render_subagent_sidebar_node(ui, key, child, visited);
                }
            });
        }
    }
    /// Reuses ordinary message/tool rendering but offers no independent prompt loop: these
    /// are conversations owned by the orchestrating provider.
    pub(super) fn render_subagent_chat(&mut self, ui: &mut Ui, _height: f32) -> bool {
        let request = ui
            .ctx()
            .data(|d| d.get_temp::<(String, String)>(egui::Id::new("open_subagent")));
        if let Some((root, id)) = request {
            let key = self.active_session_key();
            if let Some(c) =
                self.session_by_key(key).delegated.chats.iter().find(|c| {
                    c.root == root && (c.id == id || c.tool_call_id.as_deref() == Some(&id))
                })
            {
                ui.ctx()
                    .data_mut(|d| d.remove::<(String, String)>(egui::Id::new("open_subagent")));
                self.select_subagent(ui.ctx(), key, &c.root, &c.id);
            }
        }
        let Some((root, id)) = self.selected_subagent(ui.ctx()) else {
            return false;
        };
        let key = self.active_session_key();
        let Some(c) = self
            .session_by_key(key)
            .delegated
            .chats
            .iter()
            .find(|c| c.root == root && c.id == id)
        else {
            self.close_subagent(ui.ctx());
            return false;
        };
        if let Some(error) = ui
            .ctx()
            .data(|d| d.get_temp::<String>(egui::Id::new("subagent_control_error")))
        {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(c_error_fg(), error);
                if ui.small_button("Dismiss").clicked() {
                    ui.ctx()
                        .data_mut(|d| d.remove::<String>(egui::Id::new("subagent_control_error")));
                }
            });
        }
        let mut back = false;
        let mut stop = false;
        let task = c.task;
        let parent = self
            .session_by_key(key)
            .delegated
            .chats
            .iter()
            .find(|p| p.root == root && p.id == c.parent);
        let children: Vec<_> = self
            .session_by_key(key)
            .delegated
            .chats
            .iter()
            .filter(|child| child.root == root && child.parent == id)
            .collect();
        column(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                back = ui
                    .add(
                        egui::Button::new(
                            RichText::new("← Chat").size(FS_SMALL).color(c_text_muted()),
                        )
                        .frame(false),
                    )
                    .on_hover_text("Back to parent conversation")
                    .clicked();
                if let Some(parent) = parent {
                    ui.label(RichText::new("/").color(c_text_faint()));
                    let width = (ui.available_width() - 60.0).max(20.0);
                    if ui
                        .add_sized(
                            [width.min(240.0), 24.0],
                            egui::Button::new(
                                RichText::new(parent.label())
                                    .size(FS_SMALL)
                                    .color(c_text_muted()),
                            )
                            .frame(false)
                            .wrap_mode(egui::TextWrapMode::Truncate),
                        )
                        .clicked()
                    {
                        self.select_subagent(ui.ctx(), key, &parent.root, &parent.id);
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if c.live && c.running() && c.capabilities["cancel"].is_object() {
                        stop = ui
                            .add(
                                egui::Button::new(RichText::new("Stop").size(FS_SMALL))
                                    .frame(false),
                            )
                            .on_hover_text(if task { "Stop workflow" } else { "Stop agent" })
                            .clicked();
                    }
                });
            });
            ui.add_space(8.0);
            ui.add(egui::Label::new(RichText::new(c.label()).size(FS_BODY).strong()).truncate())
                .on_hover_text(c.label());
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(
                    RichText::new(format!("● {}", status(c)))
                        .size(FS_TINY)
                        .color(color(c)),
                );
                if !c.model.is_empty() && !c.model.starts_with('<') {
                    ui.label(
                        RichText::new(format!("· {}", c.model))
                            .size(FS_TINY)
                            .color(c_text_faint()),
                    );
                }
                let tokens = c.progress["usage"]["tokens"].as_u64().unwrap_or(c.used);
                if tokens > 0 {
                    ui.label(
                        RichText::new(format!(
                            "· {} tokens",
                            super::composer_helpers::format_context_tokens(tokens)
                        ))
                        .size(FS_TINY)
                        .color(c_text_faint()),
                    );
                }
            });
            ui.add_space(10.0);
            ui.separator();
        });
        let approval_height = if self
            .active_run_state()
            .is_some_and(|s| s.pending_approval.is_some())
        {
            140.0
        } else {
            0.0
        };
        egui::ScrollArea::vertical()
            .id_salt(("subagent_transcript", &root, &id))
            .max_height((ui.available_height() - approval_height).max(40.0))
            .stick_to_bottom(!task)
            .show(ui, |ui| {
                column(ui, |ui| {
                    ui.push_id((&root, &id), |ui| {
                        ui.add_space(14.0);
                        if task {
                            if !c.description.is_empty() {
                                ui.label(
                                    RichText::new(&c.description)
                                        .size(FS_SMALL)
                                        .color(c_text_muted()),
                                );
                                ui.add_space(10.0);
                            }
                            if let Some(phases) = c.progress["phases"].as_array() {
                                ui.horizontal_wrapped(|ui| {
                                    for (i, phase) in phases.iter().enumerate() {
                                        if i > 0 {
                                            ui.label(RichText::new("›").color(c_text_faint()));
                                        }
                                        ui.label(
                                            RichText::new(
                                                phase["title"].as_str().unwrap_or("Phase"),
                                            )
                                            .size(FS_TINY)
                                            .color(c_text_muted()),
                                        );
                                    }
                                });
                                ui.add_space(12.0);
                            }
                            if let Some(summary) = c.progress["summary"].as_str() {
                                ui.label(
                                    RichText::new(summary).size(FS_SMALL).color(c_text_muted()),
                                );
                                ui.add_space(10.0);
                            }
                        }
                        if !children.is_empty() {
                            let done = children.iter().filter(|ch| !ch.running()).count();
                            let failed = children
                                .iter()
                                .filter(|ch| matches!(ch.status(), "failed" | "error"))
                                .count();
                            ui.horizontal_wrapped(|ui| {
                                ui.label(
                                    RichText::new(format!(
                                        "Agents · {done}/{} finished",
                                        children.len()
                                    ))
                                    .size(FS_SMALL)
                                    .color(c_text_muted()),
                                );
                                if failed > 0 {
                                    ui.label(
                                        RichText::new(format!("· {failed} failed"))
                                            .size(FS_TINY)
                                            .color(c_error_fg()),
                                    );
                                }
                            });
                            ui.add_space(6.0);
                            egui::Frame::new()
                                .fill(c_bg_sidebar())
                                .stroke(egui::Stroke::new(1.0, c_border_subtle()))
                                .corner_radius(RADIUS_CARD)
                                .inner_margin(4)
                                .show(ui, |ui| {
                                    ui.spacing_mut().item_spacing.y = 2.0;
                                    for child in &children {
                                        if agent_row(ui, child, false, false).clicked() {
                                            self.select_subagent(
                                                ui.ctx(),
                                                key,
                                                &child.root,
                                                &child.id,
                                            );
                                        }
                                    }
                                });
                            ui.add_space(16.0);
                        }
                        for (i, m) in c.messages.iter().enumerate() {
                            crate::ui::messages::render_message(ui, i, m);
                            ui.add_space(8.0);
                        }
                        if c.messages.is_empty() && children.is_empty() {
                            ui.label(
                                RichText::new(if task {
                                    "Agents will appear here when the workflow starts."
                                } else {
                                    "Waiting for the agent's transcript…"
                                })
                                .size(FS_SMALL)
                                .color(c_text_faint()),
                            );
                        }
                    });
                });
            });
        if back {
            self.close_subagent(ui.ctx());
        }
        if stop {
            self.stop_subagent(ui.ctx(), key, root, id, task);
        }
        // Permission requests still belong to the parent and use the parent's approval channel.
        if let Some(pa) = self
            .active_run_state()
            .and_then(|s| s.pending_approval.clone())
        {
            self.render_subagent_approval(ui, pa);
        }
        true
    }
    fn stop_subagent(
        &mut self,
        ctx: &egui::Context,
        key: SessionKey,
        root: String,
        child: String,
        task: bool,
    ) {
        if self.is_remote_key(key) {
            self.remote_stop_subagent(key, root, child, task);
            return;
        }
        let manager = self.acp.clone();
        let session_key = self.acp_session_key(key);
        let ctx = ctx.clone();
        if let Ok(rt) = crate::runtime::runtime() {
            rt.spawn(async move {
                if let Err(e) = manager.cancel_child(session_key, root, child, task).await {
                    log::warn!("stop subagent: {e}");
                    ctx.data_mut(|d| d.insert_temp(egui::Id::new("subagent_control_error"), e));
                }
                ctx.request_repaint();
            });
        }
    }
}
