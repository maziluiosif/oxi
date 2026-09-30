//! `/` command menu for the chat input.
//!
//! Lists oxi's own commands plus whatever the active ACP agent advertised (see
//! [`crate::agent::acp::available_commands`]), so new agent commands show up without changes
//! here. ↑/↓ move, Tab completes, Enter completes and runs a command that takes no input, Esc
//! hides the menu until the input changes. Agent commands are sent as ordinary `/name input`
//! prompts, which is how ACP agents expect to receive them.

use std::sync::Arc;

use crate::agent::acp::AcpSlashCommand;

use super::*;

/// Commands oxi handles itself, for every provider (see `compaction::parse_slash_command`).
const LOCAL_COMMANDS: &[(&str, &str)] = &[
    ("new", "Start a new chat"),
    ("compact", "Summarize older messages to free up context"),
    (
        "plan",
        "Toggle plan mode (read-only investigation, then a plan); /plan <task> plans it",
    ),
];

const ROW_H: f32 = 26.0;
const MENU_MAX_H: f32 = 300.0;

#[derive(Clone, Default)]
struct MenuState {
    /// Input the selection belongs to; typing resets it to the first match.
    input: String,
    selected: usize,
    /// Input on which the user pressed Esc; the menu stays hidden until it changes.
    dismissed: Option<String>,
    /// The selection moved by keyboard this frame and should be scrolled into view.
    scroll: bool,
}

fn state_id() -> Id {
    Id::new("composer_slash_menu_state")
}

fn load_state(ctx: &egui::Context) -> MenuState {
    ctx.data(|d| d.get_temp::<MenuState>(state_id()))
        .unwrap_or_default()
}

fn store_state(ctx: &egui::Context, state: MenuState) {
    ctx.data_mut(|d| d.insert_temp(state_id(), state));
}

/// Commands whose name contains `query` (case-insensitive), prefix matches first.
fn filter_commands(all: Vec<AcpSlashCommand>, query: &str) -> Vec<AcpSlashCommand> {
    let query = query.to_lowercase();
    let (mut prefix, mut rest): (Vec<_>, Vec<_>) = all
        .into_iter()
        .filter(|c| c.name.to_lowercase().contains(&query))
        .partition(|c| c.name.to_lowercase().starts_with(&query));
    prefix.append(&mut rest);
    prefix
}

/// Local commands an agent's command of the same name can't replace.
const ALWAYS_LOCAL: &[&str] = &["new", "plan"];

/// oxi's commands followed by the agent's. `/new` and `/plan` always stay oxi's; an agent
/// `/compact` replaces oxi's, which can't shrink the context an ACP agent keeps itself.
fn merge_commands(agent: &[AcpSlashCommand]) -> Vec<AcpSlashCommand> {
    let agent_has = |name: &str| agent.iter().any(|c| c.name == name);
    let mut all: Vec<AcpSlashCommand> = LOCAL_COMMANDS
        .iter()
        .filter(|(name, _)| ALWAYS_LOCAL.contains(name) || !agent_has(name))
        .map(|(name, description)| AcpSlashCommand {
            name: (*name).to_string(),
            description: (*description).to_string(),
            hint: None,
        })
        .collect();
    all.extend(
        agent
            .iter()
            .filter(|c| !ALWAYS_LOCAL.contains(&c.name.as_str()))
            .cloned(),
    );
    all
}

impl OxiApp {
    /// Slash commands advertised by the active ACP agent for this workspace (empty for HTTP
    /// providers, or before the agent has started once).
    pub(crate) fn active_acp_commands(&self) -> Arc<Vec<AcpSlashCommand>> {
        let cfg = self.conv.settings.active_config();
        if !cfg.is_acp() {
            return Arc::default();
        }
        crate::agent::acp::available_commands(
            &cfg.effective_acp_command(),
            std::path::Path::new(&self.active_workspace().root_path),
        )
    }

    /// Matches for the command name being typed, or `None` when the menu should be hidden.
    fn slash_menu_items(&self, ctx: &egui::Context) -> Option<Vec<AcpSlashCommand>> {
        let query = self.conv.input.strip_prefix('/')?;
        if query.contains(char::is_whitespace)
            || load_state(ctx).dismissed.as_deref() == Some(self.conv.input.as_str())
        {
            return None;
        }
        let items = filter_commands(merge_commands(&self.active_acp_commands()), query);
        (!items.is_empty()).then_some(items)
    }

    /// Handle the menu's keys before the `TextEdit` sees them, so Enter/↑/↓/Tab don't also
    /// send, walk input history or move focus. Returns whether the menu is open, in which case
    /// the input must lock focus so Tab stays in it.
    pub(super) fn slash_menu_keys(&mut self, ui: &mut Ui, input_id: Id, focused: bool) -> bool {
        if !focused || self.confirm_prompt_open() {
            return false;
        }
        let Some(items) = self.slash_menu_items(ui.ctx()) else {
            return false;
        };
        let mut state = load_state(ui.ctx());
        if state.input != self.conv.input {
            state.input = self.conv.input.clone();
            state.selected = 0;
        }
        state.selected = state.selected.min(items.len() - 1);
        state.scroll = false;
        let none = egui::Modifiers::NONE;
        let (down, up, tab, enter, escape) = ui.input_mut(|i| {
            (
                i.consume_key(none, egui::Key::ArrowDown),
                i.consume_key(none, egui::Key::ArrowUp),
                i.consume_key(none, egui::Key::Tab),
                i.consume_key(none, egui::Key::Enter),
                i.consume_key(none, egui::Key::Escape),
            )
        });
        if down {
            state.selected = (state.selected + 1) % items.len();
            state.scroll = true;
        }
        if up {
            state.selected = (state.selected + items.len() - 1) % items.len();
            state.scroll = true;
        }
        if escape {
            state.dismissed = Some(self.conv.input.clone());
        }
        let selected = items[state.selected].clone();
        store_state(ui.ctx(), state);
        if tab || enter {
            self.accept_slash_command(ui.ctx(), input_id, &selected, enter);
        }
        true
    }

    /// Put `/name ` in the input; with `run`, send right away when the command takes no input.
    fn accept_slash_command(
        &mut self,
        ctx: &egui::Context,
        input_id: Id,
        command: &AcpSlashCommand,
        run: bool,
    ) {
        self.conv.input = format!("/{} ", command.name);
        if run && command.hint.is_none() {
            self.send_message();
            return;
        }
        let end = CCursor::new(self.conv.input.chars().count());
        let mut te_state = TextEdit::load_state(ctx, input_id).unwrap_or_default();
        te_state.cursor.set_char_range(Some(CCursorRange::one(end)));
        te_state.store(ctx, input_id);
        ctx.memory_mut(|m| m.request_focus(input_id));
    }

    /// Draw the menu above the input. `anchor` is the input's rect.
    pub(super) fn render_slash_menu(
        &mut self,
        ui: &Ui,
        input_id: Id,
        anchor: egui::Rect,
        focused: bool,
    ) {
        if !focused {
            return;
        }
        let Some(items) = self.slash_menu_items(ui.ctx()) else {
            return;
        };
        let state = load_state(ui.ctx());
        let selected = if state.input == self.conv.input {
            state.selected.min(items.len() - 1)
        } else {
            0
        };
        let width = ui.max_rect().width() + 2.0 * COMPOSER_FRAME_MARGIN;
        let pos = egui::pos2(
            anchor.left() - COMPOSER_FRAME_MARGIN,
            anchor.top() - COMPOSER_FRAME_MARGIN - 6.0,
        );
        let mut clicked = None;
        egui::Area::new(Id::new("composer_slash_menu"))
            .order(Order::Foreground)
            .pivot(egui::Align2::LEFT_BOTTOM)
            .fixed_pos(pos)
            .show(ui.ctx(), |ui| {
                Frame::new()
                    .fill(c_bg_elevated())
                    .stroke(Stroke::new(1.0, c_border()))
                    .corner_radius(crate::theme::RADIUS_PANEL)
                    .inner_margin(Margin::same(4))
                    .show(ui, |ui| {
                        ui.set_width(width - 8.0);
                        // An Area lays its content out within last frame's size, so without a
                        // fixed height the list stays as short as it once was (e.g. after
                        // switching from a provider with 2 commands back to an ACP agent).
                        let list_h = (items.len() as f32 * ROW_H).min(MENU_MAX_H);
                        egui::ScrollArea::vertical()
                            .max_height(list_h)
                            .min_scrolled_height(list_h)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                ui.spacing_mut().item_spacing.y = 0.0;
                                for (i, item) in items.iter().enumerate() {
                                    let is_selected = i == selected;
                                    let resp = slash_menu_row(ui, item, is_selected);
                                    if is_selected && state.scroll {
                                        resp.scroll_to_me(None);
                                    }
                                    if resp.clicked() {
                                        clicked = Some(item.clone());
                                    }
                                }
                            });
                    });
            });
        if let Some(item) = clicked {
            self.accept_slash_command(ui.ctx(), input_id, &item, true);
        }
    }
}

/// One menu row: `/name`, its input hint, then the description truncated to the row.
fn slash_menu_row(ui: &mut Ui, item: &AcpSlashCommand, selected: bool) -> egui::Response {
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H), Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    let painter = ui.painter();
    if selected || resp.hovered() {
        painter.rect_filled(rect, CornerRadius::same(RADIUS_CHIP), c_row_hover());
    }
    let one_line = |text: String, size: f32, color: Color32, max_w: f32| {
        let mut job = egui::text::LayoutJob::single_section(
            text,
            egui::TextFormat::simple(egui::FontId::proportional(size), color),
        );
        job.wrap = egui::text::TextWrapping::truncate_at_width(max_w.max(0.0));
        painter.layout_job(job)
    };
    let mut x = rect.left() + 8.0;
    let right = rect.right() - 8.0;
    let name = one_line(format!("/{}", item.name), FS_SMALL, c_text(), right - x);
    painter.galley(
        egui::pos2(x, rect.center().y - name.size().y * 0.5),
        name.clone(),
        c_text(),
    );
    x += name.size().x + 6.0;
    if let Some(hint) = item.hint.as_deref().filter(|h| !h.is_empty()) {
        let hint = one_line(format!("<{hint}>"), FS_TINY, c_text_faint(), right - x);
        painter.galley(
            egui::pos2(x, rect.center().y - hint.size().y * 0.5),
            hint.clone(),
            c_text_faint(),
        );
        x += hint.size().x + 6.0;
    }
    if !item.description.is_empty() && right - x > 40.0 {
        x += 6.0;
        let desc = one_line(item.description.clone(), FS_TINY, c_text_muted(), right - x);
        painter.galley(
            egui::pos2(x, rect.center().y - desc.size().y * 0.5),
            desc,
            c_text_muted(),
        );
    }
    resp.on_hover_text(&item.description)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(name: &str) -> AcpSlashCommand {
        AcpSlashCommand {
            name: name.into(),
            description: String::new(),
            hint: None,
        }
    }

    fn names(list: &[AcpSlashCommand]) -> Vec<&str> {
        list.iter().map(|c| c.name.as_str()).collect()
    }

    #[test]
    fn prefix_matches_come_first() {
        let all = vec![cmd("pr-comments"), cmd("review"), cmd("security-review")];
        assert_eq!(
            names(&filter_commands(all.clone(), "REV")),
            ["review", "security-review"]
        );
        assert_eq!(names(&filter_commands(all, "")).len(), 3);
    }

    #[test]
    fn agent_compact_replaces_local_but_new_and_plan_stay_local() {
        let merged = merge_commands(&[cmd("compact"), cmd("init"), cmd("new"), cmd("plan")]);
        assert_eq!(names(&merged), ["new", "plan", "compact", "init"]);
        assert_eq!(merged[2].description, "");
        assert!(!merged[1].description.is_empty());
        let local_only = merge_commands(&[]);
        assert_eq!(names(&local_only), ["new", "compact", "plan"]);
    }
}
