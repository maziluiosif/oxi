//! Grouped application state.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::Instant;

use eframe::egui;

use crate::agent::{AgentEvent, ApprovalDecision};
use crate::model::Session;
use crate::settings::AppSettings;

/// A mutating tool call awaiting the user's approve/deny decision.
#[derive(Clone)]
pub struct PendingApproval {
    pub name: String,
    /// Human-readable summary of what the tool will do (e.g. the bash command or target path).
    pub summary: String,
}

/// Active section in the settings window (sidebar).
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum SettingsTab {
    #[default]
    Providers,
    Agent,
    Appearance,
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

#[derive(Default)]
pub struct SessionRunState {
    pub agent_rx: Option<Receiver<AgentEvent>>,
    pub cancel_agent: Option<Arc<AtomicBool>>,
    /// Back-channel to approve/deny mutating tool calls for this run.
    pub approval_tx: Option<Sender<ApprovalDecision>>,
    /// A mutating tool call currently waiting on the user.
    pub pending_approval: Option<PendingApproval>,
    pub waiting_response: bool,
    pub stream_started_at: Option<Instant>,
    pub agent_ack: bool,
    pub stream_error: Option<String>,
}

impl SessionRunState {
    pub fn clear_agent(&mut self) {
        if let Some(c) = self.cancel_agent.take() {
            c.store(true, Ordering::SeqCst);
        }
        self.agent_rx = None;
        self.approval_tx = None;
        self.pending_approval = None;
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

/// Cached model list for one provider profile.
#[derive(Debug, Clone, Default)]
pub struct FetchedModels {
    /// Model ids returned by the provider's `/v1/models` endpoint.
    pub models: Vec<String>,
    /// True while a fetch is in flight for this profile.
    pub loading: bool,
    /// Last error message, if any.
    pub error: Option<String>,
}

/// Result message from a background model-list fetch.
#[derive(Debug)]
pub struct ModelFetchMsg {
    pub profile_id: String,
    pub result: Result<Vec<String>, String>,
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
    /// Bottom terminal panel visibility and height (persisted in settings).
    pub terminal_open: bool,
    pub terminal_height: f32,
    pub settings: AppSettings,
    pub settings_open: bool,
    pub settings_tab: SettingsTab,
    pub settings_provider_tab: crate::settings::LlmProviderKind,
    pub oauth_busy: bool,
    pub oauth_last_message: Option<String>,
    /// Measured height of the composer TextEdit from the previous frame.
    pub composer_measured_text_h: f32,
    /// Full height of the composer row (from the previous frame) for splitting transcript vs input.
    pub composer_measured_full_h: f32,
    /// Diff view replaces the chat window while a file/commit diff is open.
    pub diff_view_open: bool,
    /// Cached colorized diff job, keyed on (title+text hash, wrap width). Avoids
    /// rebuilding the (potentially huge) `LayoutJob` on every frame while the same
    /// diff stays open.
    pub diff_job_cache: Option<(u64, u32, egui::text::LayoutJob)>,
    /// Source-control (git) panel visibility and width (persisted in settings).
    pub git_open: bool,
    pub git_width: f32,
    pub git_tab: crate::app::git_panel::GitTab,
    pub git: crate::git::GitState,
    pub git_commit_message: String,
    pub git_new_branch: String,
    /// Set while a commit-message generation is in flight: we've asked the git worker for
    /// the diff and are waiting for it to come back so we can kick off the LLM completion.
    pub commit_gen_pending: bool,
    /// Receiver for the in-flight commit-message completion (deltas + terminal Done).
    /// `Some` while generating; cleared when the run finishes.
    pub commit_gen_rx: Option<std::sync::mpsc::Receiver<crate::agent::CompleteEvent>>,
    /// Last commit-generation error, shown inline under the composer until the next run.
    pub commit_gen_error: Option<String>,
    /// Git worker request channel. Responses arrive on `git_rx`; drained each frame.
    pub git_tx: Option<std::sync::mpsc::Sender<crate::git::GitOp>>,
    pub git_rx: Option<std::sync::mpsc::Receiver<crate::git::GitState>>,
    /// egui context used for the git worker so it can request repaints.
    pub git_ctx: eframe::egui::Context,
    /// Background model-list fetch results keyed by profile id.
    pub fetched_models: std::collections::HashMap<String, FetchedModels>,
    /// Channel for model-list fetch results (drained each frame).
    pub model_rx: Option<std::sync::mpsc::Receiver<ModelFetchMsg>>,
}
