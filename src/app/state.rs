//! Grouped application state.

use std::collections::HashMap;
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
    Profiles,
    Prompt,
}

/// One project root and its chat tabs.
pub struct Workspace {
    pub root_path: String,
    pub sessions: Vec<Session>,
    pub active: usize,
    pub sidebar_folded: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SessionKey {
    pub workspace_idx: usize,
    pub session_idx: usize,
}

pub struct SessionRunState {
    pub agent_rx: Option<Receiver<AgentEvent>>,
    pub cancel_agent: Option<Arc<AtomicBool>>,
    pub waiting_response: bool,
    pub stream_started_at: Option<Instant>,
    pub agent_ack: bool,
    pub stream_error: Option<String>,
}

impl Default for SessionRunState {
    fn default() -> Self {
        Self {
            agent_rx: None,
            cancel_agent: None,
            waiting_response: false,
            stream_started_at: None,
            agent_ack: false,
            stream_error: None,
        }
    }
}

impl SessionRunState {
    pub fn clear_agent(&mut self) {
        if let Some(c) = self.cancel_agent.take() {
            c.store(true, Ordering::SeqCst);
        }
        self.agent_rx = None;
    }

    pub fn reset_after_disconnect(&mut self) {
        self.end_waiting_response();
        self.agent_ack = false;
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

/// Local agent connection (no pi subprocess).
pub struct ConnectionState {
    pub connect_error: Option<String>,
    /// When true, skip loading session files from the old pi format (standalone uses in-memory chats).
    pub no_session: bool,
    /// OAuth login progress (`OAuthUiMsg`) from background threads.
    pub oauth_rx: Option<std::sync::mpsc::Receiver<crate::oauth::OAuthUiMsg>>,
}

pub struct RunState {
    pub sessions: HashMap<SessionKey, SessionRunState>,
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
    pub sidebar_open: bool,
    pub sidebar_width: f32,
    pub settings: AppSettings,
    pub settings_open: bool,
    pub settings_tab: SettingsTab,
    pub settings_provider_tab: crate::settings::LlmProviderKind,
    /// GitHub Enterprise hostname (optional) for Copilot device login.
    pub copilot_enterprise_domain: String,
    pub oauth_busy: bool,
    pub oauth_device_copilot: Option<(String, String)>,
    pub oauth_last_message: Option<String>,
    /// Measured height of the composer TextEdit from the previous frame.
    pub composer_measured_text_h: f32,
    /// Full height of the composer row (from the previous frame) for splitting transcript vs input.
    pub composer_measured_full_h: f32,
}
