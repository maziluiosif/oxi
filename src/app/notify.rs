//! Desktop notifications for chats that need the user while oxi is in the background: a
//! finished response or an approval prompt. Notification Center on macOS, `notify-send` on
//! Linux, plus egui's dock/taskbar attention request everywhere.

use eframe::egui;

use super::{OxiApp, SessionKey};

/// Why a chat wants the user's attention.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Attention {
    Finished,
    Failed,
    Approval,
}

impl OxiApp {
    /// Notify about `key` if oxi is not the focused window and the user opted in.
    pub(crate) fn notify_attention(&self, ctx: &egui::Context, key: SessionKey, kind: Attention) {
        if !self.conv.settings.notify_in_background || ctx.input(|i| i.focused) {
            return;
        }
        let chat = self
            .conv
            .workspaces
            .get(key.workspace_idx)
            .and_then(|w| w.sessions.get(key.session_idx))
            .map(|s| s.title.clone())
            .unwrap_or_default();
        let body = match kind {
            Attention::Finished => "Response finished",
            Attention::Failed => "Response failed",
            Attention::Approval => "Waiting for your approval",
        };
        let title = if chat.trim().is_empty() {
            "oxi".to_string()
        } else {
            chat
        };
        let attention = if kind == Attention::Approval {
            egui::UserAttentionType::Critical
        } else {
            egui::UserAttentionType::Informational
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(attention));
        show_desktop_notification(title, body.to_string());
    }
}

/// Fire-and-forget: a missing notifier must never block or break the UI thread.
fn show_desktop_notification(title: String, body: String) {
    let _ = std::thread::Builder::new()
        .name("oxi-notify".into())
        .spawn(move || {
            #[cfg(target_os = "macos")]
            {
                use mac_notification_sys::{Notification, set_application};
                static APP: std::sync::Once = std::sync::Once::new();
                // Posted as oxi rather than through `osascript`, whose notifications belong to
                // Script Editor: clicking one opened Script Editor instead of oxi.
                APP.call_once(|| {
                    let _ = set_application(&notification_bundle_id());
                });
                let _ = Notification::new()
                    .title("oxi")
                    .subtitle(&title)
                    .message(&body)
                    .asynchronous(true)
                    .send();
            }
            #[cfg(all(unix, not(target_os = "macos")))]
            {
                let _ = std::process::Command::new("notify-send")
                    .args(["--app-name=oxi", &title, &body])
                    .output();
            }
            #[cfg(windows)]
            {
                // The taskbar flash from RequestUserAttention is the notification on Windows.
                let _ = (title, body);
            }
        });
}

/// The app a click on the notification brings forward: oxi.app when running from the bundle;
/// otherwise (`cargo run`) the terminal that launched it, since there is no oxi app to open.
#[cfg(target_os = "macos")]
fn notification_bundle_id() -> String {
    const OXI: &str = "com.maziluiosif.oxi";
    let bundled = std::env::current_exe()
        .is_ok_and(|exe| exe.to_string_lossy().contains(".app/Contents/MacOS/"));
    if bundled {
        return OXI.to_string();
    }
    std::env::var("__CFBundleIdentifier")
        .ok()
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| OXI.to_string())
}
