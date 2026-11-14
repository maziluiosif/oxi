//! Split view: two chats side by side, e.g. the same task given to two agents, one of them in a
//! work tree. The focused pane is the app's active chat, so everything else (composer, git panel,
//! terminal, shortcuts) keeps working on it unchanged. The other pane only draws its transcript;
//! clicking it swaps focus. "Send to both" mirrors each prompt into the other chat.

use eframe::egui::{self, Align2, Color32, CornerRadius, FontId, Rect, Sense, Stroke, Ui};

use crate::model::SessionConfig;
use crate::theme::*;

use super::OxiApp;
use super::state::{SessionKey, TranscriptState};

/// A chat that stays put while chats are added or removed around it: by session file once it
/// has one, by position until then (a chat gets its file with the first message).
#[derive(Clone, Debug)]
pub(crate) struct ChatRef {
    root_path: String,
    session_file: Option<String>,
    session_idx: usize,
}

pub(crate) struct SplitView {
    /// The chat in the unfocused pane.
    partner: ChatRef,
    /// The unfocused pane is the right one. Each chat keeps its side when focus moves.
    pub partner_on_right: bool,
    /// Prompts sent in the focused pane go to the other chat too.
    pub mirror: bool,
    /// The partner's scroll and transcript caches; swapped with the active ones on focus.
    transcript: TranscriptState,
}

const HEADER_H: f32 = 26.0;
const FOOTER_H: f32 = 46.0;
const GAP: f32 = 14.0;
/// Panes narrower than this can't hold a transcript; the split shows just the focused chat.
const PANE_MIN_W: f32 = 300.0;

impl OxiApp {
    fn chat_ref(&self, key: SessionKey) -> ChatRef {
        ChatRef {
            root_path: self.conv.workspaces[key.workspace_idx].root_path.clone(),
            session_file: self.session_by_key(key).session_file.clone(),
            session_idx: key.session_idx,
        }
    }

    fn resolve_chat_ref(&self, chat: &ChatRef) -> Option<SessionKey> {
        let wi = self
            .conv
            .workspaces
            .iter()
            .position(|w| w.root_path == chat.root_path)?;
        let sessions = &self.conv.workspaces[wi].sessions;
        let si = match &chat.session_file {
            Some(file) => sessions
                .iter()
                .position(|s| s.session_file.as_deref() == Some(file))?,
            None => chat.session_idx.min(sessions.len().checked_sub(1)?),
        };
        Some(self.session_key(wi, si))
    }

    /// The chat in the unfocused pane. Closes the split when that chat is gone or is the
    /// focused one now (picked in the sidebar).
    pub(crate) fn split_partner_key(&mut self) -> Option<SessionKey> {
        let partner = self.conv.split.as_ref()?.partner.clone();
        let key = self
            .resolve_chat_ref(&partner)
            .filter(|key| *key != self.active_session_key());
        let Some(key) = key else {
            self.close_split();
            return None;
        };
        // Learn the session file once the chat's first message created it.
        if partner.session_file.is_none()
            && let Some(file) = self.session_by_key(key).session_file.clone()
            && let Some(split) = self.conv.split.as_mut()
        {
            split.partner.session_file = Some(file);
            split.partner.session_idx = key.session_idx;
        }
        Some(key)
    }

    /// The chat in the unfocused pane, without closing a stale split (for drawing).
    pub(crate) fn split_partner_peek(&self) -> Option<SessionKey> {
        let key = self.resolve_chat_ref(&self.conv.split.as_ref()?.partner)?;
        (key != self.active_session_key()).then_some(key)
    }

    /// Show chat `key` next to the active one.
    pub(crate) fn open_split_with(&mut self, key: SessionKey) {
        if key == self.active_session_key() {
            return;
        }
        let partner = self.chat_ref(key);
        self.open_split(partner, true);
    }

    fn open_split(&mut self, partner: ChatRef, partner_on_right: bool) {
        let mirror = self.conv.split.as_ref().is_some_and(|split| split.mirror);
        self.conv.split = Some(SplitView {
            partner,
            partner_on_right,
            mirror,
            transcript: TranscriptState {
                scroll_id: egui::Id::new("split_chat_scroll"),
                ..Default::default()
            },
        });
        if let Some(key) = self.split_partner_key() {
            self.ensure_session_loaded(key);
        }
        // The focused chat moves into a pane of its own, with a new scroll area.
        self.conv.transcript.scroll_to_bottom_once = true;
        self.reveal_chat_view();
    }

    pub(crate) fn close_split(&mut self) {
        if self.conv.split.take().is_some() {
            self.conv.transcript.scroll_to_bottom_once = true;
        }
    }

    pub(crate) fn toggle_split_mirror(&mut self) {
        if let Some(split) = self.conv.split.as_mut() {
            split.mirror = !split.mirror;
        }
    }

    /// Make the unfocused pane's chat the active one; the previously active chat takes its place.
    pub(crate) fn focus_split_partner(&mut self) {
        let Some(key) = self.split_partner_key() else {
            return;
        };
        let previous = self.chat_ref(self.active_session_key());
        let scroll_to_bottom = self.conv.transcript.scroll_to_bottom_once;
        self.select_session_in_workspace(key.workspace_idx, key.session_idx);
        if self.active_session_key() != key {
            // Blocked, e.g. by an unsaved file in the old workspace.
            return;
        }
        // The chat keeps its own scroll position rather than jumping to the bottom.
        self.conv.transcript.scroll_to_bottom_once = scroll_to_bottom;
        let Some(split) = self.conv.split.as_mut() else {
            return;
        };
        std::mem::swap(&mut self.conv.transcript, &mut split.transcript);
        split.partner = previous;
        split.partner_on_right = !split.partner_on_right;
    }

    /// Start a new chat in the active workspace next to the current one.
    pub(crate) fn new_chat_split(&mut self) {
        if self.conv.no_workspace {
            self.new_chat();
            return;
        }
        let mut previous = self.chat_ref(self.active_session_key());
        self.new_chat();
        // The new chat went in at the top, ahead of a previous chat that has no file yet.
        if previous.session_file.is_none() {
            previous.session_idx += 1;
        }
        self.open_split(previous, false);
    }

    /// Start a work tree chat next to the active one: the same project, a branch of its own, and
    /// whichever agent the user picks for it.
    pub(crate) fn start_worktree_split(&mut self) {
        let active = self.active_session_key();
        let workspace = &self.conv.workspaces[active.workspace_idx];
        let root = match &workspace.worktree {
            Some(info) => info.main_root.clone(),
            None => std::path::PathBuf::from(&workspace.root_path),
        };
        if self.start_worktree_chat_at(root) {
            self.conv.pending_split = Some(self.chat_ref(active));
        }
    }

    /// Called once a work tree chat opened: put the chat it was started from next to it.
    pub(crate) fn finish_pending_split(&mut self) {
        let Some(partner) = self.conv.pending_split.take() else {
            return;
        };
        if self.resolve_chat_ref(&partner).is_some() {
            self.open_split(partner, false);
        }
    }

    /// Run `f` with `partner` standing in as the active chat: its indices, transcript view
    /// state and provider selection. All of it is put back afterwards, so `f` must not add,
    /// remove or switch chats.
    fn with_partner_active<R>(&mut self, partner: SessionKey, f: impl FnOnce(&mut Self) -> R) -> R {
        let active_wi = self.conv.active_workspace;
        let active_si = self.conv.workspaces[active_wi].active;
        let partner_ws_active = self.conv.workspaces[partner.workspace_idx].active;
        let active_config = SessionConfig::from_provider(self.conv.settings.active_config());
        let partner_config = self.ensure_session_config(partner);
        let partner_provider =
            SessionConfig::from_provider(self.conv.settings.provider(partner_config.provider));
        partner_config.apply_to_settings(&mut self.conv.settings);
        self.conv.active_workspace = partner.workspace_idx;
        self.conv.workspaces[partner.workspace_idx].active = partner.session_idx;
        if let Some(split) = self.conv.split.as_mut() {
            std::mem::swap(&mut self.conv.transcript, &mut split.transcript);
        }

        let result = f(self);

        if let Some(split) = self.conv.split.as_mut() {
            std::mem::swap(&mut self.conv.transcript, &mut split.transcript);
        }
        self.conv.workspaces[partner.workspace_idx].active = partner_ws_active;
        self.conv.active_workspace = active_wi;
        self.conv.workspaces[active_wi].active = active_si;
        partner_provider.apply_to_settings(&mut self.conv.settings);
        active_config.apply_to_settings(&mut self.conv.settings);
        result
    }

    /// The composer contents to send to the other pane too, taken before a send consumes them.
    pub(crate) fn split_mirror_payload(&self) -> Option<MirrorPayload> {
        let split = self.conv.split.as_ref()?;
        let composer = &self.conv.composer;
        let text = composer.input.trim();
        // Slash commands act on one chat (and `/new` adds one).
        if !split.mirror || text.starts_with('/') {
            return None;
        }
        if text.is_empty()
            && composer.pending_images.is_empty()
            && composer.pending_texts.is_empty()
        {
            return None;
        }
        Some(MirrorPayload {
            input: composer.input.clone(),
            images: composer.pending_images.clone(),
            texts: composer.pending_texts.clone(),
        })
    }

    /// Send `payload` into the other pane's chat, with that chat's own agent and model.
    pub(crate) fn send_to_split_partner(&mut self, payload: MirrorPayload) {
        let Some(key) = self.split_partner_key() else {
            return;
        };
        self.with_partner_active(key, |app| {
            let composer = &mut app.conv.composer;
            let input = std::mem::replace(&mut composer.input, payload.input);
            let images = std::mem::replace(&mut composer.pending_images, payload.images);
            let texts = std::mem::replace(&mut composer.pending_texts, payload.texts);
            app.send_message_opts(false);
            app.conv.transcript.scroll_to_bottom_once = true;
            let composer = &mut app.conv.composer;
            composer.input = input;
            composer.pending_images = images;
            composer.pending_texts = texts;
        });
        // The send may have saved the partner as the last active chat.
        self.sync_active_session_to_settings();
    }

    /// The chat area in split mode. Returns false when there's no room (or no split), so the
    /// caller draws the single chat as usual.
    pub(crate) fn render_split_chat(&mut self, ui: &mut Ui) -> bool {
        let area = ui.max_rect();
        if self.conv.split.is_none() || area.width() < PANE_MIN_W * 2.0 + GAP {
            return false;
        }
        let Some(partner) = self.split_partner_key() else {
            return false;
        };
        let partner_on_right = self.conv.split.as_ref().is_some_and(|s| s.partner_on_right);
        let half = (area.width() - GAP) * 0.5;
        let left = Rect::from_min_size(area.min, egui::vec2(half, area.height()));
        let right = Rect::from_min_max(egui::pos2(left.right() + GAP, area.top()), area.max);
        let (active_rect, partner_rect) = if partner_on_right {
            (left, right)
        } else {
            (right, left)
        };
        ui.painter().vline(
            left.right() + GAP * 0.5,
            area.y_range(),
            Stroke::new(1.0, c_border_subtle()),
        );

        // Each side keeps its id, so a chat's scroll position survives a focus swap.
        let side = |on_right: bool| {
            if on_right {
                "split_right"
            } else {
                "split_left"
            }
        };
        let mut active_ui = ui.new_child(
            egui::UiBuilder::new()
                .id_salt(side(!partner_on_right))
                .max_rect(active_rect),
        );
        active_ui.set_clip_rect(active_rect.intersect(ui.clip_rect()));
        self.render_active_pane(&mut active_ui);

        let mut partner_ui = ui.new_child(
            egui::UiBuilder::new()
                .id_salt(side(partner_on_right))
                .max_rect(partner_rect),
        );
        partner_ui.set_clip_rect(partner_rect.intersect(ui.clip_rect()));
        self.render_partner_pane(&mut partner_ui, partner);
        true
    }

    fn render_active_pane(&mut self, ui: &mut Ui) {
        let rect = ui.max_rect();
        let header = Rect::from_min_size(rect.min, egui::vec2(rect.width(), HEADER_H));
        let key = self.active_session_key();
        let label = self.pane_label(key);
        paint_pane_header(ui, header, &label, true);

        // "Send to both" sits at the header's right edge.
        let mirror = self.conv.split.as_ref().is_some_and(|s| s.mirror);
        let text = if mirror {
            "✓ Send to both"
        } else {
            "Send to both"
        };
        let galley = ui.painter().layout_no_wrap(
            text.to_owned(),
            FontId::proportional(FS_TINY),
            if mirror { c_accent() } else { c_text_muted() },
        );
        let toggle_rect = Rect::from_center_size(
            egui::pos2(
                header.right() - galley.size().x * 0.5 - 8.0,
                header.center().y,
            ),
            galley.size() + egui::vec2(10.0, 6.0),
        );
        let toggle = ui
            .interact(toggle_rect, ui.id().with("split_mirror"), Sense::click())
            .on_hover_text("Send each prompt to both chats, each with its own agent and model");
        if toggle.hovered() {
            ui.painter()
                .rect_filled(toggle_rect, CornerRadius::same(RADIUS_ROW), c_row_hover());
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        ui.painter().galley(
            toggle_rect.center() - galley.size() * 0.5,
            galley,
            Color32::PLACEHOLDER,
        );
        if toggle.clicked() {
            self.toggle_split_mirror();
        }

        let body = Rect::from_min_max(egui::pos2(rect.left(), header.bottom()), rect.max);
        let mut body_ui = ui.new_child(egui::UiBuilder::new().id_salt("body").max_rect(body));
        self.render_chat_column(&mut body_ui);
    }

    fn render_partner_pane(&mut self, ui: &mut Ui, partner: SessionKey) {
        let rect = ui.max_rect();
        let header = Rect::from_min_size(rect.min, egui::vec2(rect.width(), HEADER_H));
        let label = self.pane_label(partner);
        paint_pane_header(ui, header, &label, false);

        let body = Rect::from_min_max(
            egui::pos2(rect.left(), header.bottom()),
            egui::pos2(rect.right(), rect.bottom() - FOOTER_H),
        );
        let footer = Rect::from_min_max(egui::pos2(rect.left(), body.bottom()), rect.max);
        self.with_partner_active(partner, |app| {
            let mut body_ui = ui.new_child(egui::UiBuilder::new().id_salt("body").max_rect(body));
            body_ui.set_clip_rect(body.intersect(ui.clip_rect()));
            let style = (*body_ui.style()).clone();
            let column_w = crate::theme::chat_column_center_width(body.width(), &style);
            app.render_conversation(&mut body_ui, column_w, body.height(), 12.0);
        });

        // The transcript is a preview until focused: this catches clicks (and hover) above it.
        // The scroll bar strip is left out so the pane can still be dragged through.
        let catch = Rect::from_min_max(rect.min, egui::pos2(rect.right() - 12.0, rect.bottom()));
        let click = ui
            .interact(catch, ui.id().with("split_focus"), Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);

        let close_rect = Rect::from_center_size(
            egui::pos2(header.right() - 14.0, header.center().y),
            egui::vec2(20.0, 20.0),
        );
        let close = ui
            .interact(close_rect, ui.id().with("split_close"), Sense::click())
            .on_hover_text("Close split view");
        crate::ui::chrome::paint_close_mark(
            ui.painter(),
            close_rect.center(),
            3.0,
            if close.hovered() {
                c_text()
            } else {
                c_text_faint()
            },
        );

        // The pane shows the chat, so a finished turn there is seen.
        if let Some(run) = self.flow.sessions.get_mut(&partner) {
            run.completion_unseen = false;
        }
        let status = self.pane_status(partner);
        paint_partner_footer(ui, footer, &status, click.hovered());

        if close.clicked() {
            self.close_split();
        } else if click.clicked() {
            self.focus_split_partner();
        }
    }

    /// "workspace · Agent · model" for a pane header.
    fn pane_label(&self, key: SessionKey) -> String {
        let workspace = &self.conv.workspaces[key.workspace_idx];
        let place = match &workspace.worktree {
            Some(info) => crate::app::worktrees::worktree_label(info),
            None => workspace_sidebar_label(&workspace.root_path),
        };
        match &self.session_by_key(key).config {
            Some(config) => {
                let agent = super::composer::composer_provider_label(config.provider);
                if config.model_id.is_empty() {
                    format!("{place} · {agent}")
                } else {
                    format!("{place} · {agent} · {}", config.model_id)
                }
            }
            None => place,
        }
    }

    fn pane_status(&self, key: SessionKey) -> String {
        let run = self.run_state(key);
        if run.is_some_and(|run| run.pending_approval.is_some()) {
            "Waiting for your approval · click to answer".into()
        } else if run.is_some_and(|run| run.waiting_response) {
            "Working… · click to focus".into()
        } else if run.is_some_and(|run| run.stream_error.is_some()) {
            "Stopped with an error · click to see".into()
        } else {
            "Click to reply in this chat".into()
        }
    }
}

/// The composer contents mirrored into the other pane.
pub(crate) struct MirrorPayload {
    input: String,
    images: Vec<(String, Vec<u8>)>,
    texts: Vec<crate::model::UserAttachment>,
}

fn paint_pane_header(ui: &Ui, rect: Rect, label: &str, focused: bool) {
    let painter = ui.painter();
    painter.hline(
        rect.x_range(),
        rect.bottom() - 1.0,
        Stroke::new(
            if focused { 2.0 } else { 1.0 },
            if focused {
                c_accent()
            } else {
                c_border_subtle()
            },
        ),
    );
    let mut job = egui::text::LayoutJob::simple_singleline(
        label.to_owned(),
        FontId::proportional(FS_SMALL),
        if focused { c_text() } else { c_text_muted() },
    );
    // One line, cut short before the controls at the right edge.
    job.wrap = egui::text::TextWrapping::truncate_at_width((rect.width() - 120.0).max(40.0));
    let galley = painter.layout_job(job);
    painter.galley(
        egui::pos2(
            rect.left() + 4.0,
            rect.center().y - galley.size().y * 0.5 - 1.0,
        ),
        galley,
        Color32::PLACEHOLDER,
    );
}

fn paint_partner_footer(ui: &Ui, rect: Rect, status: &str, hovered: bool) {
    let card = rect.shrink2(egui::vec2(4.0, 6.0));
    let painter = ui.painter();
    painter.rect(
        card,
        CornerRadius::same(RADIUS_ROW),
        c_bg_elevated(),
        Stroke::new(1.0, if hovered { c_accent() } else { c_border() }),
        egui::StrokeKind::Inside,
    );
    painter.text(
        egui::pos2(card.left() + 12.0, card.center().y),
        Align2::LEFT_CENTER,
        status,
        FontId::proportional(FS_SMALL),
        c_text_muted(),
    );
}
