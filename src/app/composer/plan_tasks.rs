//! Plan mode toggle, the "plan ready" bar, and the agent's live checklist (`todo_write`), all
//! shown inside the composer card.

use super::*;
use crate::agent::tools::{TodoItem, TodoStatus, parse_todos};
use crate::model::{AssistantBlock, MsgRole};

/// Checklist rows shown before collapsing the rest into "… N more".
const MAX_TASK_ROWS: usize = 8;

/// The most recent `todo_write` list in `messages`, if any.
pub(super) fn latest_todos(messages: &[crate::model::ChatMessage]) -> Option<Vec<TodoItem>> {
    messages
        .iter()
        .rev()
        .filter(|m| m.role == MsgRole::Assistant)
        .flat_map(|m| m.blocks.iter().rev())
        .find_map(|b| match b {
            AssistantBlock::Tool {
                name,
                args_summary: Some(args),
                is_error,
                ..
            } if name == "todo_write" && *is_error != Some(true) => serde_json::from_str(args)
                .ok()
                .and_then(|v| parse_todos(&v)),
            _ => None,
        })
}

/// The checklist worth showing. While a turn runs: the list it wrote, or the previous turn's
/// list if items were left open (the agent may carry on with it). Once the turn is over the
/// list is history, except after a failed turn, where it shows how far the work got. Agents
/// often end a turn without ticking the last item, so an open list alone is not a reason to
/// keep it on screen.
pub(super) fn visible_todos(
    messages: &[crate::model::ChatMessage],
    running: bool,
) -> Option<Vec<TodoItem>> {
    let turn_start = messages
        .iter()
        .rposition(|m| m.role == MsgRole::User)
        .map_or(0, |i| i + 1);
    let open = |t: &Vec<TodoItem>| t.iter().any(|t| t.status != TodoStatus::Completed);
    let this_turn = latest_todos(&messages[turn_start..]).filter(|t| !t.is_empty());
    if !running {
        let run = &messages[turn_start..];
        return this_turn.filter(|t| open(t) && crate::model::reply_failed(run));
    }
    if this_turn.is_some() {
        return this_turn;
    }
    let previous_start = messages[..turn_start.saturating_sub(1)]
        .iter()
        .rposition(|m| m.role == MsgRole::User)
        .map_or(0, |i| i + 1);
    latest_todos(&messages[previous_start..turn_start])
        .filter(|t| !t.is_empty())
        .filter(open)
}

/// Editing a previous prompt must keep the approved plan in the transcript.
fn render_plan_ready_actions(ui: &mut Ui, editing_prompt: bool) -> (bool, bool) {
    if editing_prompt {
        return (false, false);
    }
    let mut implement = false;
    let mut dismiss = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(
            RichText::new(ICON_PLAN)
                .font(egui::FontId::new(FS_SMALL, icon_font()))
                .color(c_accent()),
        );
        ui.label(
            RichText::new("Plan ready. Review it, then:")
                .size(FS_SMALL)
                .color(c_text_muted()),
        );
        if crate::ui::chrome::primary_button(ui, "Implement plan")
            .on_hover_text("Turn plan mode off and ask the agent to carry out the plan")
            .clicked()
        {
            implement = true;
        }
        if crate::ui::chrome::flat_button_icon(
            ui,
            "",
            "Keep planning",
            FS_SMALL,
            egui::vec2(0.0, CONTROL_HEIGHT),
            c_text_muted(),
        )
        .on_hover_text("Hide this; reply below to refine the plan")
        .clicked()
        {
            dismiss = true;
        }
    });
    ui.add_space(COMPOSER_GAP);
    (implement, dismiss)
}

impl OxiApp {
    pub(super) fn plan_mode_on(&self) -> bool {
        self.run_state(self.active_session_key())
            .is_some_and(|r| r.plan_mode)
    }

    /// Pill next to the model picker while plan mode is on (`/plan` turns it on). Plan mode
    /// keeps the agent read-only and asks for a plan; it stays on for the chat until switched
    /// off, by clicking the pill or with `/plan` again.
    pub(super) fn render_plan_toggle(&mut self, ui: &mut Ui, compact: bool) {
        if !self.plan_mode_on() || self.agent_shows_plan_mode() {
            return;
        }
        let color = c_accent();
        let text = if compact {
            crate::ui::chrome::icon_glyph_rich(ICON_PLAN, FS_SMALL, color).into()
        } else {
            crate::ui::chrome::icon_label_job(ICON_PLAN, "Plan", FS_SMALL, color)
        };
        let resp = ui
            .add(
                Button::new(text)
                    .fill(c_pill_selected_bg())
                    .stroke(Stroke::new(1.0, c_pill_selected_border()))
                    .corner_radius(CornerRadius::same(255))
                    .min_size(egui::vec2(0.0, CONTROL_HEIGHT)),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text(
                "Plan mode is on: the agent only investigates (read-only tools) and proposes a plan. Click (or send /plan) to turn it off.",
            );
        if resp.clicked() {
            let key = self.active_session_key();
            let run = self.run_state_mut(key);
            run.plan_mode = false;
            self.save_settings_quietly();
            self.conv.composer.focus_next_frame = true;
        }
    }

    /// After a plan-mode answer: offer to implement it (turns plan mode off and sends the go).
    pub(super) fn render_plan_ready_bar(&mut self, ui: &mut Ui) {
        let key = self.active_session_key();
        let Some(run) = self.run_state(key) else {
            return;
        };
        let last_is_answer = self
            .active_session()
            .messages
            .last()
            .is_some_and(|m| m.role == MsgRole::Assistant && !m.streaming);
        if !run.last_turn_planned
            || run.waiting_response
            || run.stream_error.is_some()
            || !last_is_answer
        {
            return;
        }
        let (implement, dismiss) =
            render_plan_ready_actions(ui, self.conv.composer.editing_last_prompt.is_some());
        if dismiss {
            self.run_state_mut(key).last_turn_planned = false;
        }
        if implement {
            let run = self.run_state_mut(key);
            run.plan_mode = false;
            run.last_turn_planned = false;
            self.save_settings_quietly();
            let draft = std::mem::take(&mut self.conv.composer.input);
            self.conv.composer.input = if draft.trim().is_empty() {
                "Implement the plan above.".to_string()
            } else {
                format!("Implement the plan above. {}", draft.trim())
            };
            self.send_message();
        }
    }

    /// The agent's current checklist, while it is still relevant (a run is going or items
    /// remain open). Collapsible; the fold state is remembered per chat.
    pub(super) fn render_task_panel(&mut self, ui: &mut Ui) {
        let running = self.active_waiting_response();
        let Some(todos) = visible_todos(&self.active_session().messages, running) else {
            return;
        };
        let done = todos
            .iter()
            .filter(|t| t.status == TodoStatus::Completed)
            .count();
        let key = self.active_session_key();
        let fold_id = Id::new(("composer_tasks_folded", key.workspace_idx, key.session_idx));
        let mut folded = ui
            .ctx()
            .data(|d| d.get_temp::<bool>(fold_id))
            .unwrap_or(false);

        let header_bg = ui.painter().add(egui::Shape::Noop);
        let header = ui
            .horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 5.0;
                ui.label(
                    RichText::new(ICON_TASKS)
                        .font(egui::FontId::new(FS_SMALL, icon_font()))
                        .color(c_text_muted()),
                );
                ui.label(
                    RichText::new(format!("Tasks {done}/{}", todos.len()))
                        .size(FS_SMALL)
                        .color(c_text_muted()),
                );
                if folded
                    && let Some(current) = todos.iter().find(|t| t.status == TodoStatus::InProgress)
                {
                    ui.label(
                        RichText::new(format!("· {}", current.content))
                            .size(FS_SMALL)
                            .color(c_text()),
                    );
                }
                ui.label(
                    RichText::new(if folded {
                        ICON_ANGLE_DOWN
                    } else {
                        ICON_ANGLE_UP
                    })
                    .font(egui::FontId::new(FS_TINY, icon_font()))
                    .color(c_text_faint()),
                );
            })
            .response
            .interact(Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if header.hovered() || header.has_focus() {
            ui.painter().set(
                header_bg,
                egui::Shape::rect_filled(
                    header.rect.expand2(egui::vec2(6.0, 2.0)),
                    CornerRadius::same(RADIUS_ROW),
                    c_row_hover(),
                ),
            );
        }
        crate::ui::chrome::keyboard_focus_ring(ui, &header, CornerRadius::same(RADIUS_ROW));
        if header.clicked() {
            folded = !folded;
            ui.ctx().data_mut(|d| d.insert_temp(fold_id, folded));
        }
        if !folded {
            for todo in todos.iter().take(MAX_TASK_ROWS) {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    ui.add_space(2.0);
                    let (icon, icon_color, text) = match todo.status {
                        TodoStatus::Completed => (
                            ICON_CHECK,
                            c_accent(),
                            RichText::new(&todo.content)
                                .size(FS_SMALL)
                                .color(c_text_faint())
                                .strikethrough(),
                        ),
                        TodoStatus::InProgress => (
                            ICON_DOT_CIRCLE,
                            c_accent(),
                            strong_rich(&todo.content, FS_SMALL, c_text_strong()),
                        ),
                        TodoStatus::Pending => (
                            ICON_CIRCLE,
                            c_text_faint(),
                            RichText::new(&todo.content)
                                .size(FS_SMALL)
                                .color(c_text_muted()),
                        ),
                    };
                    ui.label(
                        RichText::new(icon)
                            .font(egui::FontId::new(FS_TINY, icon_font()))
                            .color(icon_color),
                    );
                    ui.add(egui::Label::new(text).wrap());
                });
            }
            if todos.len() > MAX_TASK_ROWS {
                ui.label(
                    RichText::new(format!("… {} more", todos.len() - MAX_TASK_ROWS))
                        .size(FS_TINY)
                        .color(c_text_faint()),
                );
            }
        }
        ui.add_space(COMPOSER_GAP);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ChatMessage;
    use egui_kittest::kittest::Queryable;

    #[test]
    fn plan_handoff_is_hidden_while_editing_a_prompt() {
        let mut setup = false;
        let mut harness = egui_kittest::Harness::builder().build_ui_state(
            |ui, state: &mut (bool, (bool, bool))| {
                if !setup {
                    crate::theme::apply_theme(ui.ctx(), "dark");
                    setup = true;
                    return;
                }
                state.1 = render_plan_ready_actions(ui, state.0);
            },
            (false, (false, false)),
        );
        harness.run_steps(3);
        assert!(harness.query_by_label("Implement plan").is_some());
        harness.state_mut().0 = true;
        harness.run_steps(3);
        assert!(harness.query_by_label("Implement plan").is_none());
        assert!(harness.query_by_label("Keep planning").is_none());
        assert_eq!(harness.state().1, (false, false));
        harness.state_mut().0 = false;
        harness.run_steps(3);
        harness.get_by_label("Implement plan").click();
        harness.step();
        assert_eq!(harness.state().1, (true, false));
    }

    fn todo_block(args: &str) -> AssistantBlock {
        AssistantBlock::Tool {
            tool_call_id: "t".into(),
            name: "todo_write".into(),
            args_summary: Some(args.into()),
            output: String::new(),
            diff: None,
            is_error: None,
            full_output_path: None,
            output_truncated: false,
            metadata: None,
        }
    }

    fn assistant() -> ChatMessage {
        ChatMessage {
            role: MsgRole::Assistant,
            text: String::new(),
            is_summary: false,
            attachments: Vec::new(),
            blocks: Vec::new(),
            streaming: false,
            started_at: None,
            worked_duration: None,
            route: None,
            changes: None,
            evidence: None,
        }
    }

    #[test]
    fn latest_todo_list_wins() {
        let mut first = assistant();
        first.blocks.push(todo_block(
            r#"{"todos":[{"content":"a","status":"pending"}]}"#,
        ));
        let mut second = assistant();
        second.blocks.push(todo_block(
            r#"{"todos":[{"content":"a","status":"completed"},{"content":"b","status":"in_progress"}]}"#,
        ));
        let todos = latest_todos(&[first, second]).unwrap();
        assert_eq!(todos.len(), 2);
        assert_eq!(todos[1].status, TodoStatus::InProgress);
        assert!(latest_todos(&[]).is_none());
    }

    #[test]
    fn checklist_shows_while_its_turn_runs_and_after_a_failure() {
        let list = |status: &str| {
            let mut m = assistant();
            m.blocks.push(todo_block(&format!(
                r#"{{"todos":[{{"content":"a","status":"completed"}},{{"content":"b","status":"{status}"}}]}}"#
            )));
            m
        };
        let user = ChatMessage {
            role: MsgRole::User,
            ..assistant()
        };
        // Written in the running turn, finished or not: shown.
        assert!(visible_todos(&[user.clone(), list("completed")], true).is_some());
        assert!(visible_todos(&[user.clone(), list("in_progress")], true).is_some());
        // The turn ended without ticking the last item: hidden, not stuck at 1/2.
        assert!(visible_todos(&[user.clone(), list("in_progress")], false).is_none());
        assert!(visible_todos(&[user.clone(), list("completed")], false).is_none());
        // The turn failed: the open list stays to show where it stopped.
        let mut failed = list("in_progress");
        failed.append_answer("\n[Error] quota\n");
        assert!(visible_todos(&[user.clone(), failed], false).is_some());
        // The next turn picks up an open list from the previous one while it runs...
        let carried = [user.clone(), list("pending"), user.clone(), assistant()];
        assert!(visible_todos(&carried, true).is_some());
        assert!(visible_todos(&carried, false).is_none());
        // ...but not a finished one, nor one from two turns back.
        let finished = [user.clone(), list("completed"), user.clone(), assistant()];
        assert!(visible_todos(&finished, true).is_none());
        let old = [
            user.clone(),
            list("pending"),
            user.clone(),
            assistant(),
            user,
            assistant(),
        ];
        assert!(visible_todos(&old, true).is_none());
    }
}
