use eframe::egui::Color32;

use crate::theme::{C_ACCENT, C_TEXT_MUTED};

use super::PiChatApp;

impl PiChatApp {
    /// Stop the local agent run (replaces pi `abort`).
    pub(crate) fn send_abort(&mut self) {
        if let Some(c) = self.conn.cancel_agent.as_ref() {
            c.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    pub(crate) fn drop_agent(&mut self) {
        self.conn.clear_agent();
        self.flow.reset_after_disconnect();
    }

    pub(crate) fn stop_agent_run(&mut self) {
        self.drop_agent();
    }

    pub(crate) fn connection_status(&self) -> (&'static str, Color32) {
        if self.conn.connect_error.is_some() {
            ("Error", Color32::from_rgb(0xff, 0x8a, 0x8a))
        } else if self.flow.waiting_response && !self.flow.agent_ack {
            ("Running", C_ACCENT)
        } else if self.flow.waiting_response {
            ("Running", C_ACCENT)
        } else {
            ("Ready", C_TEXT_MUTED)
        }
    }
}
