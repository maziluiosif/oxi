use super::{OxiApp, SessionKey};

impl OxiApp {
    /// Stop the local agent run for the active session.
    pub(crate) fn send_abort(&mut self) {
        let key = self.active_session_key();
        self.stop_agent_run(key);
    }

    pub(crate) fn drop_agent(&mut self, key: SessionKey) {
        if let Some(state) = self.flow.sessions.get_mut(&key) {
            state.clear_agent();
            state.reset_after_disconnect();
        }
        self.flow.sessions.retain(|_, state| {
            state.agent_rx.is_some() || state.waiting_response || state.stream_error.is_some()
        });
    }

    pub(crate) fn stop_agent_run(&mut self, key: SessionKey) {
        self.drop_agent(key);
    }

}
