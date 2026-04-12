//! Application state (`OxiApp`) and egui integration.

use std::path::PathBuf;

use eframe::egui;

use crate::model::{MsgRole, Session};
use crate::session_store;
use crate::settings::AppSettings;

mod agent_handlers;
mod connection;
mod composer;
mod conversation;
mod eframe_app;
mod input_history;
mod sessions;
mod settings_ui;
mod sidebar;
mod state;
mod streaming;
mod task_runner;

pub use state::{
    ConnectionState, ConversationState, RunState, SessionKey, SessionRunState, Workspace,
};

pub struct OxiApp {
    pub conn: ConnectionState,
    pub flow: RunState,
    pub conv: ConversationState,
}

impl OxiApp {
    pub fn new() -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let root_path = cwd.to_string_lossy().to_string();
        let settings = AppSettings::load();
        let sessions = Self::initial_workspace_sessions(&root_path, false);
        let mut app = Self {
            conn: ConnectionState {
                connect_error: None,
                no_session: false,
                oauth_rx: None,
            },
            flow: RunState {
                sessions: std::collections::HashMap::new(),
                current_backend_session_file: None,
                pending_session_idx: None,
                pending_load_session_idx: None,
            },
            conv: ConversationState {
                workspaces: vec![Workspace {
                    root_path,
                    sessions,
                    active: 0,
                    sidebar_folded: false,
                }],
                active_workspace: 0,
                input: String::new(),
                sidebar_search: String::new(),
                chat_scroll_id: egui::Id::new("main_chat_scroll"),
                pending_images: Vec::new(),
                scroll_to_bottom_once: true,
                input_history: Vec::new(),
                input_history_index: None,
                input_history_draft: String::new(),
                sidebar_open: true,
                sidebar_width: 168.0,
                settings,
                settings_open: false,
                settings_tab: state::SettingsTab::default(),
                settings_provider_tab: crate::settings::LlmProviderKind::OpenAi,
                copilot_enterprise_domain: String::new(),
                oauth_busy: false,
                oauth_device_copilot: None,
                oauth_last_message: None,
                composer_measured_text_h: 0.0,
                composer_measured_full_h: 0.0,
            },
        };
        app.ensure_active_session_loaded();
        app
    }

    pub(crate) fn active_session_key(&self) -> SessionKey {
        SessionKey {
            workspace_idx: self.conv.active_workspace,
            session_idx: self.active_workspace().active,
        }
    }

    pub(crate) fn session_key(&self, workspace_idx: usize, session_idx: usize) -> SessionKey {
        SessionKey {
            workspace_idx,
            session_idx,
        }
    }

    pub(crate) fn active_run_state(&self) -> Option<&SessionRunState> {
        self.flow.sessions.get(&self.active_session_key())
    }

    pub(crate) fn run_state(&self, key: SessionKey) -> Option<&SessionRunState> {
        self.flow.sessions.get(&key)
    }

    pub(crate) fn run_state_mut(&mut self, key: SessionKey) -> &mut SessionRunState {
        self.flow.sessions.entry(key).or_default()
    }

    pub(crate) fn any_waiting_response(&self) -> bool {
        self.flow.sessions.values().any(|state| state.waiting_response)
    }

    pub(crate) fn active_agent_ack(&self) -> bool {
        self.active_run_state().is_some_and(|state| state.agent_ack)
    }

    pub(crate) fn active_stream_error(&self) -> Option<&str> {
        self.active_run_state()
            .and_then(|state| state.stream_error.as_deref())
    }

    pub(crate) fn active_waiting_response(&self) -> bool {
        self.active_run_state()
            .is_some_and(|state| state.waiting_response)
    }

    pub(crate) fn stream_started_at_for(
        &self,
        workspace_idx: usize,
        session_idx: usize,
    ) -> Option<std::time::Instant> {
        self.run_state(self.session_key(workspace_idx, session_idx))
            .and_then(|state| state.stream_started_at)
    }

    pub(crate) fn active_workspace(&self) -> &Workspace {
        &self.conv.workspaces[self.conv.active_workspace]
    }

    pub(crate) fn active_workspace_mut(&mut self) -> &mut Workspace {
        &mut self.conv.workspaces[self.conv.active_workspace]
    }

    /// Ground truth for "model still running" on a session row (`workspace_idx`, session index in that workspace).
    pub(crate) fn session_row_is_running(&self, workspace_idx: usize, session_idx: usize) -> bool {
        self.conv
            .workspaces
            .get(workspace_idx)
            .and_then(|w| w.sessions.get(session_idx))
            .and_then(|s| s.messages.last())
            .is_some_and(|m| m.role == MsgRole::Assistant && m.streaming)
    }

    pub(crate) fn active_session_mut(&mut self) -> &mut Session {
        let w = self.conv.active_workspace;
        let a = self.conv.workspaces[w].active;
        &mut self.conv.workspaces[w].sessions[a]
    }

    pub(crate) fn active_session(&self) -> &Session {
        let w = self.active_workspace();
        &w.sessions[w.active]
    }

    pub(crate) fn session_mut(&mut self, idx: usize) -> &mut Session {
        let w = self.conv.active_workspace;
        &mut self.conv.workspaces[w].sessions[idx]
    }

    pub(crate) fn session_mut_by_key(&mut self, key: SessionKey) -> &mut Session {
        &mut self.conv.workspaces[key.workspace_idx].sessions[key.session_idx]
    }

    /// Save the current composer input/images to the active session, load from the target.
    fn swap_session_input(&mut self, new_workspace: usize, new_session: usize) {
        let old_wi = self.conv.active_workspace;
        let old_si = self.conv.workspaces[old_wi].active;
        self.conv.workspaces[old_wi].sessions[old_si].input_text =
            std::mem::take(&mut self.conv.input);
        self.conv.workspaces[old_wi].sessions[old_si].pending_images =
            std::mem::take(&mut self.conv.pending_images);
        self.conv.input =
            std::mem::take(&mut self.conv.workspaces[new_workspace].sessions[new_session].input_text);
        self.conv.pending_images = std::mem::take(
            &mut self.conv.workspaces[new_workspace].sessions[new_session].pending_images,
        );
        self.conv.input_history_index = None;
        self.conv.input_history_draft.clear();
    }

    pub(crate) fn select_workspace(&mut self, workspace_idx: usize) {
        if workspace_idx >= self.conv.workspaces.len()
            || workspace_idx == self.conv.active_workspace
        {
            return;
        }
        let target_si = self.conv.workspaces[workspace_idx].active;
        self.swap_session_input(workspace_idx, target_si);
        self.conv.active_workspace = workspace_idx;
        self.conv.scroll_to_bottom_once = true;
        self.ensure_active_session_loaded();
    }

    pub(crate) fn select_session_in_workspace(&mut self, workspace_idx: usize, session_idx: usize) {
        if workspace_idx >= self.conv.workspaces.len() {
            return;
        }
        let n = self.conv.workspaces[workspace_idx].sessions.len();
        if session_idx >= n {
            return;
        }
        if workspace_idx == self.conv.active_workspace
            && session_idx == self.conv.workspaces[workspace_idx].active
        {
            self.ensure_active_session_loaded();
            return;
        }
        self.swap_session_input(workspace_idx, session_idx);
        self.conv.active_workspace = workspace_idx;
        self.conv.workspaces[workspace_idx].active = session_idx;
        self.conv.scroll_to_bottom_once = true;
        self.ensure_active_session_loaded();
    }

    pub(crate) fn blank_session(title: impl Into<String>) -> Session {
        Session {
            title: title.into(),
            messages: vec![],
            session_file: None,
            messages_loaded: true,
            input_text: String::new(),
            pending_images: Vec::new(),
        }
    }

    pub(crate) fn initial_workspace_sessions(
        root_path: &str,
        skip_disk_sessions: bool,
    ) -> Vec<Session> {
        if skip_disk_sessions {
            return vec![Self::blank_session("New chat")];
        }
        let sessions = session_store::load_workspace_sessions(root_path);
        if sessions.is_empty() {
            vec![Self::blank_session("New chat")]
        } else {
            sessions
        }
    }

    pub(crate) fn ensure_active_session_loaded(&mut self) {
        if self.conn.no_session {
            return;
        }

        let active = self.active_workspace().active;
        let active_key = self.active_session_key();

        if self
            .run_state(active_key)
            .is_some_and(|state| state.waiting_response)
        {
            return;
        }

        let session_file = {
            let session = &self.active_workspace().sessions[active];
            if session.session_file.is_none() || session.messages_loaded {
                return;
            }
            session.session_file.clone()
        };

        if let Some(session_file) = session_file {
            if let Some(messages) = session_store::load_session_messages(&session_file) {
                let session = self.session_mut(active);
                session.messages = messages;
                session.messages_loaded = true;
            }
        }
    }

    pub(crate) fn push_input_history(&mut self, text: &str) {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return;
        }
        if self.conv.input_history.first().map(|s| s.as_str()) == Some(trimmed) {
            return;
        }
        self.conv.input_history.insert(0, trimmed.to_string());
        if self.conv.input_history.len() > 100 {
            self.conv.input_history.pop();
        }
    }
}
