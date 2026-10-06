//! ACP command terminals, shared with the interactive terminal panel.
use super::PromptCtx;
use crate::agent::AgentEvent;
use crate::terminal::{TerminalProcess, TerminalSession};
use portable_pty::CommandBuilder;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub(super) struct Terminals(Arc<Mutex<HashMap<String, (String, TerminalProcess)>>>);

impl Terminals {
    pub(super) fn close(&self) {
        for (_, (_, process)) in self.0.lock().unwrap_or_else(|e| e.into_inner()).drain() {
            let _ = process.kill();
        }
    }

    pub(super) fn create(&self, params: &Value, ctx: &PromptCtx) -> Result<Value, String> {
        if params["sessionId"].as_str() != Some(&ctx.session_id) {
            return Err("Terminal session is not the active prompt".into());
        }
        if ctx.plan_mode {
            return Err(super::PLAN_MODE_REFUSAL.into());
        }
        let command = params["command"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("Missing command")?;
        let mut cmd = CommandBuilder::new(command);
        if let Some(args) = params.get("args") {
            for arg in args.as_array().ok_or("Invalid command arguments")? {
                cmd.arg(arg.as_str().ok_or("Invalid command argument")?);
            }
        }
        let cwd = params["cwd"]
            .as_str()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| ctx.cwd.clone());
        if !cwd.is_absolute() {
            return Err("Terminal cwd must be absolute".into());
        }
        cmd.cwd(cwd);
        cmd.env("TERM", "xterm-256color");
        if let Some(env) = params.get("env") {
            for variable in env.as_array().ok_or("Invalid terminal environment")? {
                cmd.env(
                    variable["name"]
                        .as_str()
                        .ok_or("Missing environment name")?,
                    variable["value"]
                        .as_str()
                        .ok_or("Missing environment value")?,
                );
            }
        }
        let limit = params["outputByteLimit"]
            .as_u64()
            .unwrap_or(1024 * 1024)
            .min(16 * 1024 * 1024) as usize;
        let (terminal, process) =
            TerminalSession::spawn_command(&eframe::egui::Context::default(), cmd, limit)?;
        let id = format!("oxi-{}", rand::random::<u64>());
        let pending = crate::terminal::PendingTerminal(Arc::new(Mutex::new(Some(terminal))));
        if ctx.event_tx.send(AgentEvent::AcpTerminal(pending)).is_err() {
            let _ = process.kill();
            return Err("Terminal UI disconnected".into());
        }
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.clone(), (ctx.session_id.clone(), process));
        Ok(json!({"terminalId": id}))
    }

    pub(super) async fn request(&self, method: &str, params: &Value) -> Result<Value, String> {
        let id = params["terminalId"].as_str().ok_or("Missing terminalId")?;
        let process = {
            let mut terminals = self.0.lock().unwrap_or_else(|e| e.into_inner());
            let (session, process) = terminals.get(id).ok_or("Unknown terminalId")?;
            if params["sessionId"].as_str() != Some(session) {
                return Err("Terminal belongs to another session".into());
            }
            let process = process.clone();
            if method == "terminal/release" {
                terminals.remove(id);
            }
            process
        };
        match method {
            "terminal/output" => Ok(process.output()),
            "terminal/kill" | "terminal/release" => {
                process.kill()?;
                Ok(json!({}))
            }
            "terminal/wait_for_exit" => loop {
                if let Some(status) = process.exit_status() {
                    return Ok(status);
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            },
            _ => Err(format!("Unsupported terminal method: {method}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn terminal_creation_refuses_plan_mode_and_foreign_sessions() {
        let (tx, rx) = std::sync::mpsc::channel();
        let (perm_tx, _) = tokio::sync::mpsc::unbounded_channel();
        let mut ctx = PromptCtx {
            session_id: "active".into(),
            cwd: std::env::temp_dir(),
            plan_mode: true,
            updates: super::super::UpdateState::default(),
            event_tx: tx,
            perm_tx,
        };
        let terminals = Terminals::default();
        let params = json!({"sessionId":"active", "command":"does-not-exist"});
        assert_eq!(
            terminals.create(&params, &ctx).unwrap_err(),
            super::super::PLAN_MODE_REFUSAL
        );
        ctx.plan_mode = false;
        let foreign = json!({"sessionId":"foreign", "command":"does-not-exist"});
        assert!(
            terminals
                .create(&foreign, &ctx)
                .unwrap_err()
                .contains("active prompt")
        );
        assert!(rx.try_recv().is_err());
    }
}
