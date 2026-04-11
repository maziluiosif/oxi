//! Grouped application state.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::Instant;

use eframe::egui;

use crate::agent::AgentEvent;
use crate::model::Session;
use crate::settings::AppSettings;

/// Active section in the settings window (sidebar).
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum SettingsTab {
    #[default]
    Providers,
    SystemPrompt,
}

/// One project root and its chat tabs.
pub struct Workspace {
    pub root_path: String,
    pub sessions: Vec<Session>,
    pub active: usize,
    pub sidebar_folded: bool,
}

/// Local agent connection (no pi subprocess).
pub struct ConnectionState {
    pub agent_rx: Option<Receiver<AgentEvent>>,
    pub cancel_agent: Option<Arc<AtomicBool>>,
    pub connect_error: Option<String>,
    /// When true, skip loading session files from the old pi format (standalone uses in-memory chats).
    pub no_session: bool,
    /// OAuth login progress (`OAuthUiMsg`) from background threads.
    pub oauth_rx: Option<std::sync::mpsc::Receiver<crate::oauth::OAuthUiMsg>>,
}

pub struct RunState {
    pub waiting_response: bool,
    pub stream_started_at: Option<Instant>,
    pub agent_ack: bool,
    pub stream_session_idx: Option<usize>,
    pub stream_error: Option<String>,
    pub current_backend_session_file: Option<String>,
    pub pending_session_idx: Option<usize>,
    pub pending_load_session_idx: Option<usize>,
}

pub struct ConversationState {
    pub workspaces: Vec<Workspace>,
    pub active_workspace: usize,
    pub input: String,
    pub sidebar_search: String,
    pub chat_scroll_id: egui::Id,
    pub pending_images: Vec<(String, Vec<u8>)>,
    pub scroll_to_bottom_once: bool,
    pub input_history: Vec<String>,
    pub input_history_index: Option<usize>,
    pub input_history_draft: String,
    pub input_history_ignore_next_edit_change: bool,
    pub sidebar_open: bool,
    pub sidebar_width: f32,
    pub settings: AppSettings,
    pub settings_open: bool,
    pub settings_tab: SettingsTab,
    /// GitHub Enterprise hostname (optional) for Copilot device login.
    pub copilot_enterprise_domain: String,
    pub oauth_busy: bool,
    pub oauth_device_copilot: Option<(String, String)>,
    pub oauth_last_message: Option<String>,
}

impl ConnectionState {
    pub fn clear_agent(&mut self) {
        if let Some(c) = self.cancel_agent.take() {
            c.store(true, Ordering::SeqCst);
        }
        self.agent_rx = None;
    }
}

impl RunState {
    pub fn reset_after_disconnect(&mut self) {
        self.end_waiting_response();
        self.agent_ack = false;
        self.stream_session_idx = None;
        self.current_backend_session_file = None;
        self.pending_session_idx = None;
        self.pending_load_session_idx = None;
        self.stream_error = None;
    }

    pub fn begin_waiting_response(&mut self) {
        self.waiting_response = true;
        self.stream_started_at = Some(Instant::now());
    }

    pub fn end_waiting_response(&mut self) {
        self.waiting_response = false;
        self.stream_started_at = None;
    }
}
