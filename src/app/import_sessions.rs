//! "Import agent session…" in a workspace's menu: pick a session an ACP agent ran outside oxi
//! (Claude Code in a terminal, say) and continue it as an oxi chat. See
//! [`crate::agent::acp::list_sessions`].

use std::sync::mpsc::{Receiver, TryRecvError, channel};

use eframe::egui::{self, RichText};

use super::OxiApp;
use super::task_runner::spawn_async_task;
use crate::agent::acp::{AcpImport, ExternalSession};
use crate::model::{ChatMessage, MsgRole};
use crate::settings::LlmProviderKind;
use crate::theme::*;

pub(crate) struct ImportDialog {
    workspace_root: String,
    provider: LlmProviderKind,
    state: ImportState,
}

enum ImportState {
    Listing(Receiver<Result<Vec<ExternalSession>, String>>),
    Listed(Vec<ExternalSession>),
    Importing(Receiver<Result<(String, Vec<ChatMessage>), String>>),
    Ready(String, Vec<ChatMessage>),
    Failed(String),
}

impl OxiApp {
    /// Open the import dialog for workspace `wi`, listing the sessions of the chat's agent (or
    /// Claude Code's when the composer is not on an ACP agent).
    pub(crate) fn open_session_import(&mut self, ctx: &egui::Context, wi: usize) {
        let active = self.conv.settings.active_provider;
        let provider = if active.is_acp() {
            active
        } else {
            LlmProviderKind::ClaudeCodeAcp
        };
        self.conv.import_sessions = Some(ImportDialog {
            workspace_root: self.conv.workspaces[wi].root_path.clone(),
            provider,
            state: ImportState::Failed(String::new()),
        });
        self.list_importable_sessions(ctx);
    }

    fn import_request(&self, dialog: &ImportDialog) -> AcpImport {
        let cfg = self.conv.settings.provider(dialog.provider);
        AcpImport {
            command_line: cfg.effective_acp_command(),
            cwd: std::path::PathBuf::from(dialog.workspace_root.trim()),
            env: cfg.acp_env(),
        }
    }

    fn list_importable_sessions(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &self.conv.import_sessions else {
            return;
        };
        let req = self.import_request(dialog);
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        let error_tx = tx.clone();
        spawn_async_task(
            move |e| {
                let _ = error_tx.send(Err(e));
            },
            move |rt| {
                let _ = tx.send(rt.block_on(crate::agent::acp::list_sessions(&req)));
                ctx.request_repaint();
            },
        );
        if let Some(dialog) = &mut self.conv.import_sessions {
            dialog.state = ImportState::Listing(rx);
        }
    }

    fn import_session(&mut self, ctx: &egui::Context, session_id: String) {
        let Some(dialog) = &self.conv.import_sessions else {
            return;
        };
        let req = self.import_request(dialog);
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        let error_tx = tx.clone();
        spawn_async_task(
            move |e| {
                let _ = error_tx.send(Err(e));
            },
            move |rt| {
                let loaded = rt.block_on(crate::agent::acp::load_session(&req, &session_id));
                let _ = tx.send(loaded.map(|messages| (session_id, messages)));
                ctx.request_repaint();
            },
        );
        if let Some(dialog) = &mut self.conv.import_sessions {
            dialog.state = ImportState::Importing(rx);
        }
    }

    /// A new chat in the dialog's workspace with the imported messages, bound to the agent's
    /// session so the next prompt continues it.
    fn finish_import(&mut self, session_id: &str, mut messages: Vec<ChatMessage>) {
        let Some(mut dialog) = self.conv.import_sessions.take() else {
            return;
        };
        let Some(wi) = self
            .conv
            .workspaces
            .iter()
            .position(|ws| ws.root_path == dialog.workspace_root)
            .filter(|_| !self.conv.no_workspace)
        else {
            self.conv.sidebar.notice =
                Some("The workspace was removed. Open it again to import a session.".into());
            return;
        };
        if wi != self.conv.active_workspace {
            self.select_workspace(wi);
            if self.conv.active_workspace != wi {
                // Keep the loaded transcript until the user saves their editor changes.
                dialog.state = ImportState::Ready(session_id.to_owned(), messages);
                self.conv.import_sessions = Some(dialog);
                return;
            }
        }
        let req = self.import_request(&dialog);
        let cfg = self.conv.settings.provider(dialog.provider).clone();
        let route = crate::model::RouteNote::direct(dialog.provider, &cfg.model_id, &cfg.effort);
        for message in &mut messages {
            if message.role == MsgRole::Assistant {
                message.route = Some(Box::new(route.clone()));
            }
        }
        let title = messages
            .iter()
            .find(|m| m.role == MsgRole::User)
            .map(|m| crate::model::make_session_title(&m.text))
            .unwrap_or_else(|| "Imported session".into());

        self.new_chat();
        let key = self.active_session_key();
        {
            let session = self.session_mut_by_key(key);
            session.messages = messages;
            session.messages_loaded = true;
            session.title = title;
            session.config = Some(crate::model::SessionConfig::from_provider(&cfg));
        }
        self.restore_active_session_config();
        let root = self.conv.workspaces[key.workspace_idx].root_path.clone();
        if let Err(e) =
            crate::session_store::save_session_messages(&root, self.session_mut_by_key(key))
        {
            self.run_state_mut(key).stream_error = Some(format!("Save session: {e}"));
            return;
        }
        self.persist_active_session_selection();
        crate::agent::acp::bind_session(
            &self.acp_session_key(key),
            &req.command_line,
            &req.cwd,
            session_id,
        );
        self.conv.transcript.scroll_to_bottom_once = true;
        self.conv.composer.focus_next_frame = true;
    }

    pub(crate) fn render_session_import(&mut self, ctx: &egui::Context) {
        if self.conv.import_sessions.as_ref().is_some_and(|dialog| {
            self.conv.no_workspace
                || !self
                    .conv
                    .workspaces
                    .iter()
                    .any(|ws| ws.root_path == dialog.workspace_root)
        }) {
            self.conv.import_sessions = None;
            self.conv.sidebar.notice =
                Some("The workspace was removed. Open it again to import a session.".into());
            return;
        }
        let Some(dialog) = &mut self.conv.import_sessions else {
            return;
        };
        // Pick up finished background work.
        let mut finished = None;
        match &dialog.state {
            ImportState::Listing(rx) => match rx.try_recv() {
                Ok(Ok(sessions)) => dialog.state = ImportState::Listed(sessions),
                Ok(Err(e)) => dialog.state = ImportState::Failed(e),
                Err(TryRecvError::Disconnected) => {
                    dialog.state = ImportState::Failed("Listing stopped unexpectedly.".into())
                }
                Err(TryRecvError::Empty) => {}
            },
            ImportState::Importing(rx) => match rx.try_recv() {
                Ok(Ok(done)) => finished = Some(done),
                Ok(Err(e)) => dialog.state = ImportState::Failed(e),
                Err(TryRecvError::Disconnected) => {
                    dialog.state = ImportState::Failed("Import stopped unexpectedly.".into())
                }
                Err(TryRecvError::Empty) => {}
            },
            _ => {}
        }
        if let Some((session_id, messages)) = finished {
            self.finish_import(&session_id, messages);
            return;
        }

        let mut open = true;
        let mut provider = dialog.provider;
        let mut import = None;
        let mut retry = false;
        let mut finish_ready = false;
        let busy = matches!(
            dialog.state,
            ImportState::Listing(_) | ImportState::Importing(_) | ImportState::Ready(_, _)
        );
        let workspace = std::path::Path::new(dialog.workspace_root.trim())
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        egui::Window::new("Import agent session")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size(egui::vec2(520.0, 420.0))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Agent").size(FS_SMALL).color(c_text_muted()));
                    ui.add_enabled_ui(!busy, |ui| {
                        egui::ComboBox::from_id_salt("import_session_agent")
                            .selected_text(provider.label())
                            .show_ui(ui, |ui| {
                                for kind in LlmProviderKind::ALL.into_iter().filter(|k| k.is_acp())
                                {
                                    ui.selectable_value(&mut provider, kind, kind.label());
                                }
                            });
                    });
                    ui.label(
                        RichText::new(format!("in {workspace}"))
                            .size(FS_SMALL)
                            .color(c_text_muted()),
                    );
                });
                ui.label(
                    RichText::new(
                        "Sessions this agent ran in this folder outside oxi. The imported chat continues the agent's own session.",
                    )
                    .size(FS_SMALL)
                    .color(c_text_faint()),
                );
                ui.add_space(6.0);
                match &dialog.state {
                    ImportState::Listing(_) => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Asking the agent for its sessions…");
                        });
                    }
                    ImportState::Importing(_) => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Loading the session…");
                        });
                    }
                    ImportState::Ready(_, _) => {
                        ui.label("Save the open files before switching to the imported session's workspace.");
                        finish_ready = ui.button("Finish import").clicked();
                    }
                    ImportState::Failed(error) => {
                        ui.label(RichText::new(error).color(c_error_fg()));
                        retry = ui.button("Try again").clicked();
                    }
                    ImportState::Listed(sessions) if sessions.is_empty() => {
                        ui.label("No sessions to import: none were found, or oxi already has them all.");
                    }
                    ImportState::Listed(sessions) => {
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            for session in sessions {
                                ui.horizontal(|ui| {
                                    if ui.button("Import").clicked() {
                                        import = Some(session.session_id.clone());
                                    }
                                    ui.label(&session.title);
                                    if let Some(when) = session.updated_at.as_deref().and_then(age) {
                                        ui.label(
                                            RichText::new(when).size(FS_SMALL).color(c_text_faint()),
                                        );
                                    }
                                });
                            }
                        });
                    }
                }
            });
        let changed_provider = provider != dialog.provider;
        dialog.provider = provider;
        if !open {
            self.conv.import_sessions = None;
        } else if finish_ready {
            if let ImportState::Ready(session_id, messages) =
                std::mem::replace(&mut dialog.state, ImportState::Failed(String::new()))
            {
                self.finish_import(&session_id, messages);
            }
        } else if let Some(session_id) = import {
            self.import_session(ctx, session_id);
        } else if retry || changed_provider {
            self.list_importable_sessions(ctx);
        }
    }
}

/// "3d ago" for an RFC 3339 time.
fn age(rfc3339: &str) -> Option<String> {
    let at = chrono::DateTime::parse_from_rfc3339(rfc3339).ok()?;
    let secs = (chrono::Utc::now() - at.with_timezone(&chrono::Utc))
        .num_seconds()
        .max(0) as u64;
    Some(format!(
        "{} ago",
        crate::router::decide::short_duration(secs)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Workspace;

    fn workspace(root: &std::path::Path) -> Workspace {
        Workspace {
            root_path: root.to_string_lossy().into_owned(),
            sessions: vec![OxiApp::blank_session("Existing chat")],
            active: 0,
            sidebar_folded: false,
            pinned: vec![],
            archived: Vec::new(),
            folded_groups: vec![],
            worktree: None,
        }
    }

    fn dialog(root: &std::path::Path) -> ImportDialog {
        ImportDialog {
            workspace_root: root.to_string_lossy().into_owned(),
            provider: LlmProviderKind::ClaudeCodeAcp,
            state: ImportState::Failed(String::new()),
        }
    }

    #[test]
    fn imports_wait_for_unsaved_files_and_follow_workspace_identity() {
        crate::secrets::use_mock_store();
        let dir = std::env::temp_dir().join(format!("oxi-import-ui-{}", rand::random::<u64>()));
        let first = dir.join("first");
        let target = dir.join("target");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        let file = first.join("file.rs");
        std::fs::write(&file, "original").unwrap();
        let mut app = OxiApp::new();
        app.conv.workspaces = vec![workspace(&first), workspace(&target)];
        app.conv.active_workspace = 0;
        app.conv.no_workspace = false;
        app.flow.sessions.clear();
        app.open_editor_file(file);
        let document = &mut app.conv.editor.documents[0];
        document.content = "unsaved changes".into();
        document.dirty = true;
        app.conv.import_sessions = Some(dialog(&target));
        let messages = vec![ChatMessage::user("Imported request", &[])];
        app.finish_import("external-session", messages.clone());
        assert_eq!(app.conv.active_workspace, 0);
        assert_eq!(app.conv.workspaces[0].sessions.len(), 1);
        assert_eq!(app.conv.workspaces[1].sessions.len(), 1);
        assert_eq!(app.conv.editor.documents[0].content, "unsaved changes");
        assert!(matches!(
            app.conv.import_sessions.as_ref().unwrap().state,
            ImportState::Ready(_, _)
        ));

        // Saving lets the same loaded import finish in its intended workspace.
        app.conv.editor.documents[0].dirty = false;
        app.finish_import("external-session", messages.clone());
        assert_eq!(app.conv.active_workspace, 1);
        assert!(app.conv.import_sessions.is_none());
        assert_eq!(app.conv.workspaces[0].sessions.len(), 1);
        assert_eq!(app.active_session().messages[0].text, "Imported request");
        let imported_file = app.active_session().session_file.as_ref().unwrap();
        assert_eq!(
            crate::session_store::load_session(imported_file)
                .unwrap()
                .messages[0]
                .text,
            "Imported request"
        );

        // Removing an earlier workspace changes the index, but not the import target.
        app.conv.import_sessions = Some(dialog(&target));
        app.delete_workspace(0);
        app.finish_import("another-external-session", messages);
        assert_eq!(app.conv.active_workspace, 0);
        assert_eq!(app.conv.workspaces[0].root_path, target.to_string_lossy());
        assert_eq!(app.active_session().messages[0].text, "Imported request");

        // Removing the target closes the dialog, even when its old index is reused.
        app.conv.import_sessions = Some(dialog(&target));
        app.conv.workspaces.push(workspace(&first));
        app.delete_workspace(0);
        app.render_session_import(&egui::Context::default());
        assert!(app.conv.import_sessions.is_none());
        assert_eq!(app.conv.workspaces[0].sessions.len(), 1);
        assert!(
            app.conv
                .sidebar
                .notice
                .as_ref()
                .unwrap()
                .contains("removed")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
