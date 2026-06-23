use crate::model::MsgRole;
use crate::session_store;

use super::{OxiApp, SessionKey};

impl OxiApp {
    /// Stop the local agent run for the active session.
    pub(crate) fn send_abort(&mut self) {
        let key = self.active_session_key();
        self.stop_agent_run(key);
    }

    pub(crate) fn stop_agent_run(&mut self, key: SessionKey) {
        if let Some(state) = self.flow.sessions.get_mut(&key) {
            state.clear_agent();
            state.reset_after_disconnect();
        }

        if let Some(last) = self.conv.workspaces[key.workspace_idx].sessions[key.session_idx]
            .messages
            .iter_mut()
            .rev()
            .find(|m| m.role == MsgRole::Assistant)
        {
            last.streaming = false;
        }

        let root_path = self.conv.workspaces[key.workspace_idx].root_path.clone();
        if let Err(e) =
            session_store::save_session_messages(&root_path, self.session_mut_by_key(key))
        {
            self.run_state_mut(key).stream_error = Some(format!("Save session: {e}"));
        }

        self.flow.sessions.retain(|_, state| {
            state.agent_rx.is_some() || state.waiting_response || state.stream_error.is_some()
        });
    }
}
