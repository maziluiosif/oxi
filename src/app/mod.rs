//! Application state (`PiChatApp`) and egui integration.

use std::path::PathBuf;

use eframe::egui;

use crate::model::{MsgRole, Session};
use crate::session_store;
use crate::settings::AppSettings;

mod agent_handlers;
mod connection;
mod eframe_app;
mod layout;
mod sessions;
mod state;
mod streaming;
mod task_runner;

pub use state::{ConnectionState, ConversationState, RunState, SettingsTab, Workspace};

pub struct PiChatApp {
    pub conn: ConnectionState,
    pub flow: RunState,
    pub conv: ConversationState,
}

impl PiChatApp {
    pub fn new() -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let root_path = cwd.to_string_lossy().to_string();
        let settings = AppSettings::load();
        let sessions = Self::initial_workspace_sessions(&root_path, false);
        let mut app = Self {
            conn: ConnectionState {
                agent_rx: None,
                cancel_agent: None,
                connect_error: None,
                no_session: false,
                oauth_rx: None,
            },
            flow: RunState {
                waiting_response: false,
                stream_started_at: None,
                agent_ack: false,
                stream_session_idx: None,
                stream_error: None,
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
                input_history_ignore_next_edit_change: false,
                sidebar_open: true,
                sidebar_width: 168.0,
                settings,
                settings_open: false,
                settings_tab: state::SettingsTab::default(),
                copilot_enterprise_domain: String::new(),
                oauth_busy: false,
                oauth_device_copilot: None,
                oauth_last_message: None,
            },
        };
        app.ensure_active_session_loaded();
        app
    }

    pub(crate) fn begin_waiting_response(&mut self) {
        self.flow.begin_waiting_response();
    }

    pub(crate) fn end_waiting_response(&mut self) {
        self.flow.end_waiting_response();
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

    pub(crate) fn stream_session_index(&self) -> usize {
        let a = self.active_workspace().active;
        self.flow.stream_session_idx.unwrap_or(a)
    }

    pub(crate) fn stream_session_mut(&mut self) -> &mut Session {
        let idx = self.stream_session_index();
        self.session_mut(idx)
    }

    /// Switch project root (disconnects pi if connected; cwd follows the workspace).
    pub(crate) fn select_workspace(&mut self, workspace_idx: usize) {
        if workspace_idx >= self.conv.workspaces.len()
            || workspace_idx == self.conv.active_workspace
        {
            return;
        }
        if self.conn.agent_rx.is_some() {
            self.stop_agent_run();
        }
        self.conv.active_workspace = workspace_idx;
        self.conv.scroll_to_bottom_once = true;
        self.ensure_active_session_loaded();
    }

    /// Focus a chat tab; may switch workspace.
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
        if workspace_idx != self.conv.active_workspace && self.conn.agent_rx.is_some() {
            self.stop_agent_run();
        }
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
        }
    }

    /// When `skip_disk_sessions` is true (standalone agent), start with one in-memory chat only.
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
        if self.conn.no_session || self.flow.waiting_response {
            return;
        }

        let active = self.active_workspace().active;
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

    /// Append a trimmed user prompt to the composer history (matches TUI: no consecutive duplicates, cap 100).
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
