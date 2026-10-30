//! Short chat titles: after a chat's first successful reply, the commit-message helper model
//! condenses the opening prompt into a few words, replacing the verbatim first-prompt title.

use eframe::egui;

use super::{OxiApp, SessionKey};
use crate::model::{MsgRole, make_session_title};
use crate::session_store;

const TITLE_SYSTEM_PROMPT: &str = "You name chat conversations. Reply with a title of 2 to 6 \
words that summarizes the user's request. Use sentence case, no quotes, no trailing period, \
no emoji. Reply with the title only.";

/// Longest prompt excerpt sent for titling; the opening of a request carries its topic.
const TITLE_PROMPT_CHARS: usize = 1500;

impl OxiApp {
    /// Start a title completion when `key` just finished its first turn and still carries the
    /// automatic first-prompt title (a manual rename is never overwritten).
    pub(crate) fn maybe_start_title_gen(&mut self, key: SessionKey) {
        // A remote chat's server names it.
        if !self.conv.settings.auto_title_chats || self.is_remote_key(key) {
            return;
        }
        let config = crate::router::helper_config(
            &self.conv.settings,
            self.conv.settings.commit_msg_config(),
            TITLE_PROMPT_CHARS,
        );
        // Agents other than Claude Code and Codex have no one-shot CLI to title with.
        if (config.is_acp() && !crate::agent::cli_complete::supports(config.provider))
            || config.provider == crate::settings::LlmProviderKind::Router
        {
            return;
        }
        let root = self.conv.workspaces[key.workspace_idx].root_path.clone();
        let session = self.session_by_key(key);
        let Some(file) = session.session_file.clone() else {
            return;
        };
        let mut users = session
            .messages
            .iter()
            .filter(|m| m.role == MsgRole::User && !m.is_summary);
        let (Some(first), None) = (users.next(), users.next()) else {
            return;
        };
        let auto_title = make_session_title(&first.text);
        if session.title != auto_title || first.text.trim().is_empty() {
            return;
        }
        // Already short: a model call would not make it any clearer.
        if auto_title.split_whitespace().count() <= 5 && auto_title.chars().count() <= 40 {
            return;
        }
        if self
            .conv
            .title_gen
            .iter()
            .any(|(r, f, _, _)| r == &root && f == &file)
        {
            return;
        }
        let excerpt: String = first.text.chars().take(TITLE_PROMPT_CHARS).collect();
        let (rx, _handle) = crate::agent::spawn_completion(crate::agent::CompleteRequest {
            config,
            system_prompt: TITLE_SYSTEM_PROMPT.to_string(),
            user_prompt: format!("Title this request:\n\n{excerpt}"),
            max_chars: Some(120),
            effort_override: Some("low".to_string()),
        });
        self.conv.title_gen.push((root, file, auto_title, rx));
    }

    /// Apply finished title completions. Failures are silent: the chat keeps its prompt title.
    pub(crate) fn drain_title_gen(&mut self, ctx: &egui::Context) {
        if self.conv.title_gen.is_empty() {
            return;
        }
        let mut finished = Vec::new();
        for (index, (_, _, _, rx)) in self.conv.title_gen.iter().enumerate() {
            loop {
                match rx.try_recv() {
                    Ok(crate::agent::CompleteEvent::Delta(_)) => {}
                    Ok(crate::agent::CompleteEvent::Done(result)) => {
                        finished.push((index, result.ok()));
                        break;
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        finished.push((index, None));
                        break;
                    }
                }
            }
        }
        for (index, result) in finished.into_iter().rev() {
            let (root, file, auto_title, _) = self.conv.title_gen.remove(index);
            if let Some(title) = result.as_deref().and_then(clean_title) {
                self.apply_generated_title(&root, &file, &auto_title, title);
                ctx.request_repaint();
            }
        }
    }

    /// Name chats with the titles their ACP agents generated, while a chat still carries the
    /// automatic first-prompt title. A chat that is answering gets it once the turn is saved.
    pub(crate) fn apply_agent_titles(&mut self, ctx: &egui::Context) {
        for (acp_key, title) in crate::agent::acp::take_agent_titles() {
            let Some(key) = self.session_key_for_acp(&acp_key) else {
                continue;
            };
            if !self.conv.settings.auto_title_chats {
                continue;
            }
            if self.run_state(key).is_some_and(|r| r.waiting_response) {
                crate::agent::acp::defer_title(&acp_key, title);
                continue;
            }
            let session = self.session_by_key(key);
            let first = session
                .messages
                .iter()
                .find(|m| m.role == MsgRole::User && !m.is_summary);
            let Some(first) = first else {
                continue;
            };
            let auto_title = make_session_title(&first.text);
            let Some(title) = clean_title(&title) else {
                continue;
            };
            if session.title != auto_title || session.title == title {
                continue;
            }
            let (Some(file), root) = (
                session.session_file.clone(),
                self.conv.workspaces[key.workspace_idx].root_path.clone(),
            ) else {
                continue;
            };
            self.apply_generated_title(&root, &file, &auto_title, title);
            ctx.request_repaint();
        }
    }

    fn apply_generated_title(&mut self, root: &str, file: &str, auto_title: &str, title: String) {
        let Some(wi) = self
            .conv
            .workspaces
            .iter()
            .position(|w| w.root_path == root)
        else {
            return;
        };
        let Some(si) = self.conv.workspaces[wi]
            .sessions
            .iter()
            .position(|s| s.session_file.as_deref() == Some(file))
        else {
            return;
        };
        let key = self.session_key(wi, si);
        // Renamed by the user meanwhile, or mid-run (a save would race the streaming writer).
        if self.session_by_key(key).title != auto_title
            || self.run_state(key).is_some_and(|r| r.waiting_response)
        {
            return;
        }
        self.session_mut_by_key(key).title = title;
        if let Err(e) = session_store::save_session_messages(root, self.session_mut_by_key(key)) {
            self.run_state_mut(key).stream_error = Some(format!("Save chat title: {e}"));
        }
    }
}

/// First line of the model's reply, stripped of quotes, markdown and a trailing period.
fn clean_title(raw: &str) -> Option<String> {
    let line = raw.lines().map(str::trim).find(|l| !l.is_empty())?;
    let line = line
        .trim_start_matches(|c: char| c == '#' || c == '*' || c.is_whitespace())
        .trim_start_matches("Title:")
        .trim_start_matches("title:")
        .trim()
        .trim_matches(|c: char| matches!(c, '"' | '\'' | '`' | '*' | '“' | '”'))
        .trim_end_matches('.')
        .trim();
    let words = line.split_whitespace().count();
    (1..=10)
        .contains(&words)
        .then(|| line.chars().take(60).collect())
}

#[cfg(test)]
mod tests {
    use super::clean_title;

    #[test]
    fn clean_title_strips_decoration() {
        assert_eq!(
            clean_title("\"Fix failing median test.\"\n").as_deref(),
            Some("Fix failing median test")
        );
        assert_eq!(
            clean_title("Title: CSV export").as_deref(),
            Some("CSV export")
        );
        assert_eq!(clean_title("   \n"), None);
        assert_eq!(
            clean_title("one two three four five six seven eight nine ten eleven"),
            None
        );
    }
}
