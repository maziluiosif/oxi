//! Desktop notifications for chats that need the user while oxi is in the background: a
//! finished response or an approval prompt. Uses what each OS already ships (`osascript` on
//! macOS, `notify-send` on Linux) plus egui's dock/taskbar attention request everywhere.

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
                let script = format!(
                    "display notification {} with title \"oxi\" subtitle {}",
                    applescript_string(&body),
                    applescript_string(&title)
                );
                let _ = std::process::Command::new("osascript")
                    .arg("-e")
                    .arg(script)
                    .output();
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

/// Quote `s` as an AppleScript string literal.
#[cfg(any(target_os = "macos", test))]
fn applescript_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' | '\r' => out.push(' '),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::applescript_string;

    #[test]
    fn applescript_strings_escape_quotes_and_newlines() {
        assert_eq!(applescript_string(r#"fix "x" \ y"#), r#""fix \"x\" \\ y""#);
        assert_eq!(applescript_string("a\nb"), "\"a b\"");
    }
}
