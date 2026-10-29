//! Preparing a new work tree before its agent starts. A repository can have a setup script
//! (install dependencies, copy `.env`, build once…), set from its sidebar menu. It runs in each
//! new work tree, with its output, elapsed time and result shown above the chat's composer, and
//! can be cancelled.
//!
//! Messages sent while it runs wait in the chat's queue and go out once it succeeds. If it fails
//! or is cancelled they keep waiting: the agent never starts in a half-prepared work tree unless
//! the user runs the script again or chooses to continue without it.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, Frame, Margin, RichText, Stroke, Ui};

use super::OxiApp;
use crate::theme::*;

/// Output kept per run; older output is dropped from the front.
const MAX_OUTPUT: usize = 64 * 1024;
/// How long the "ready" line stays after a successful setup.
const READY_SHOWN: Duration = Duration::from_secs(8);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SetupState {
    Running,
    Succeeded,
    /// Exit status, or why it could not run.
    Failed(String),
    Cancelled,
}

/// One work tree's setup run.
pub(crate) struct WorktreeSetup {
    /// Root of the work tree workspace.
    pub root: String,
    pub script: String,
    pub state: SetupState,
    output: Arc<Mutex<String>>,
    started: Instant,
    finished: Option<Instant>,
    cancel: Arc<AtomicBool>,
    rx: Option<Receiver<SetupState>>,
    pub show_output: bool,
    /// The user chose to start the agent despite a failed or cancelled setup.
    pub skipped: bool,
}

impl WorktreeSetup {
    /// Whether the chats of this work tree must not start their agent yet.
    pub(crate) fn holds_agent(&self) -> bool {
        match self.state {
            SetupState::Running => true,
            SetupState::Failed(_) | SetupState::Cancelled => !self.skipped,
            SetupState::Succeeded => false,
        }
    }

    pub(crate) fn output(&self) -> String {
        self.output
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn elapsed(&self) -> Duration {
        self.finished.unwrap_or_else(Instant::now) - self.started
    }
}

/// Run `script` in `dir` with the shell, the user's login environment and `env`, appending its
/// output to `output`. Stops it when `cancel` is set.
fn run_script(
    script: &str,
    dir: &Path,
    env: &[(&str, String)],
    output: &Arc<Mutex<String>>,
    cancel: &AtomicBool,
) -> SetupState {
    let mut cmd = if cfg!(windows) {
        let mut c = std::process::Command::new("cmd");
        c.arg("/C").arg(script);
        c
    } else {
        let mut c = std::process::Command::new("sh");
        c.arg("-c").arg(script);
        c
    };
    cmd.current_dir(dir);
    crate::shell_env::apply(&mut cmd);
    for (key, value) in env {
        cmd.env(key, value);
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // Its own process group, so cancelling stops what it started too (npm, make, …).
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => return SetupState::Failed(format!("could not start the shell: {e}")),
    };
    let readers: Vec<_> = [
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    ]
    .into_iter()
    .flatten()
    .map(|mut stream| {
        let output = output.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = stream.read(&mut buf) {
                if n == 0 {
                    break;
                }
                let mut out = output.lock().unwrap_or_else(|e| e.into_inner());
                out.push_str(&String::from_utf8_lossy(&buf[..n]));
                if out.len() > MAX_OUTPUT {
                    let mut cut = out.len() - MAX_OUTPUT;
                    while !out.is_char_boundary(cut) {
                        cut += 1;
                    }
                    out.drain(..cut);
                }
            }
        })
    })
    .collect();
    let state = loop {
        if cancel.load(Ordering::SeqCst) {
            stop(&mut child);
            break SetupState::Cancelled;
        }
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break SetupState::Succeeded,
            Ok(Some(status)) => {
                break SetupState::Failed(match status.code() {
                    Some(code) => format!("exit code {code}"),
                    None => "stopped by a signal".into(),
                });
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => break SetupState::Failed(e.to_string()),
        }
    };
    for reader in readers {
        let _ = reader.join();
    }
    state
}

/// Stop the script and everything it started.
fn stop(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let group = -(child.id() as i32);
        // SAFETY: plain kill(2) on the process group the script leads.
        unsafe {
            libc::kill(group, libc::SIGTERM);
        }
        for _ in 0..20 {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        unsafe {
            libc::kill(group, libc::SIGKILL);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

impl OxiApp {
    /// The setup script configured for the repository whose main checkout is `main_root`.
    fn setup_script_for(&self, main_root: &Path) -> Option<String> {
        let main = main_root.canonicalize().ok()?;
        self.conv
            .settings
            .worktree_setup_scripts
            .iter()
            .find(|(root, _)| Path::new(root).canonicalize().ok().as_ref() == Some(&main))
            .map(|(_, script)| script.trim().to_string())
            .filter(|script| !script.is_empty())
    }

    /// Start the repository's setup script in the work tree at `root`, if it has one.
    pub(crate) fn start_worktree_setup(&mut self, root: &str) {
        let Some(info) = crate::git::worktree::info(Path::new(root)) else {
            return;
        };
        let Some(script) = self.setup_script_for(&info.main_root) else {
            return;
        };
        self.run_worktree_setup(root, script, &info);
    }

    fn run_worktree_setup(
        &mut self,
        root: &str,
        script: String,
        info: &crate::git::worktree::WorktreeInfo,
    ) {
        self.conv.worktree_setups.retain(|s| s.root != root);
        let output = Arc::new(Mutex::new(String::new()));
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = std::sync::mpsc::channel();
        let env = vec![
            ("OXI_WORKTREE_PATH", root.to_string()),
            (
                "OXI_MAIN_CHECKOUT",
                info.main_root.to_string_lossy().into_owned(),
            ),
            ("OXI_WORKTREE_BRANCH", info.branch.clone()),
        ];
        {
            let (script, dir, output, cancel) = (
                script.clone(),
                PathBuf::from(root),
                output.clone(),
                cancel.clone(),
            );
            let ctx = self.conv.git_ctx.clone();
            std::thread::spawn(move || {
                let state = run_script(&script, &dir, &env, &output, &cancel);
                let _ = tx.send(state);
                ctx.request_repaint();
            });
        }
        self.conv.worktree_setups.push(WorktreeSetup {
            root: root.to_string(),
            script,
            state: SetupState::Running,
            output,
            started: Instant::now(),
            finished: None,
            cancel,
            rx: Some(rx),
            show_output: false,
            skipped: false,
        });
    }

    /// The setup of workspace `wi`'s work tree, while it holds back its agent.
    pub(crate) fn setup_holding_agent(&self, wi: usize) -> Option<&WorktreeSetup> {
        let root = &self.conv.workspaces.get(wi)?.root_path;
        self.conv
            .worktree_setups
            .iter()
            .find(|s| &s.root == root && s.holds_agent())
    }

    /// Pick up finished setups; a successful one releases the messages its chats queued.
    pub(crate) fn drain_worktree_setups(&mut self, ctx: &egui::Context) {
        let mut released = Vec::new();
        for setup in &mut self.conv.worktree_setups {
            let Some(rx) = setup.rx.as_ref() else {
                continue;
            };
            let state = match rx.try_recv() {
                Ok(state) => state,
                Err(TryRecvError::Empty) => {
                    ctx.request_repaint_after(Duration::from_millis(250));
                    continue;
                }
                Err(TryRecvError::Disconnected) => {
                    SetupState::Failed("the setup stopped unexpectedly".into())
                }
            };
            setup.rx = None;
            setup.finished = Some(Instant::now());
            if state == SetupState::Succeeded {
                released.push(setup.root.clone());
            } else {
                // Show what went wrong without a click.
                setup.show_output = true;
            }
            setup.state = state;
        }
        for root in released {
            self.release_queued(&root);
        }
        // A finished, successful setup leaves the composer after a while.
        self.conv.worktree_setups.retain(|s| {
            !(s.state == SetupState::Succeeded
                && s.finished.is_some_and(|at| at.elapsed() > READY_SHOWN))
        });
        if self
            .conv
            .worktree_setups
            .iter()
            .any(|s| s.state == SetupState::Succeeded)
        {
            ctx.request_repaint_after(Duration::from_secs(1));
        }
    }

    /// Send what the chats of the work tree at `root` queued while setup held their agent.
    fn release_queued(&mut self, root: &str) {
        let Some(wi) = self
            .conv
            .workspaces
            .iter()
            .position(|w| w.root_path == root)
        else {
            return;
        };
        for si in 0..self.conv.workspaces[wi].sessions.len() {
            let key = self.session_key(wi, si);
            if self.run_state(key).is_some_and(|r| !r.queued.is_empty()) {
                self.send_next_queued(key);
            }
        }
    }

    /// The setup card above the active chat's composer: progress, output, and what to do when
    /// it failed.
    pub(super) fn render_worktree_setup(&mut self, ui: &mut Ui) {
        let root = self.active_workspace().root_path.clone();
        let Some(index) = self
            .conv
            .worktree_setups
            .iter()
            .position(|s| s.root == root)
        else {
            return;
        };
        let mut action = None;
        {
            let setup = &self.conv.worktree_setups[index];
            let elapsed = crate::theme::format_stream_elapsed(setup.elapsed());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let (text, color) = match &setup.state {
                    SetupState::Running => {
                        crate::theme::small_spinner(ui);
                        (
                            format!("Setting up the work tree · {elapsed}"),
                            c_text_muted(),
                        )
                    }
                    SetupState::Succeeded => (
                        format!("Work tree ready · setup took {elapsed}"),
                        c_success(),
                    ),
                    SetupState::Failed(why) if setup.skipped => (
                        format!("Setup failed ({why}); continued without it"),
                        c_text_muted(),
                    ),
                    SetupState::Failed(why) => (
                        format!("Setup failed ({why}). The agent was not started."),
                        c_error_fg(),
                    ),
                    SetupState::Cancelled if setup.skipped => (
                        "Setup cancelled; continued without it".into(),
                        c_text_muted(),
                    ),
                    SetupState::Cancelled => (
                        "Setup cancelled. The agent was not started.".into(),
                        c_warning_fg(),
                    ),
                };
                ui.label(RichText::new(text).size(FS_SMALL).color(color))
                    .on_hover_text(format!("Setup script:\n{}", setup.script));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    match setup.state {
                        SetupState::Running => {
                            if ui.small_button("Cancel").clicked() {
                                action = Some(SetupAction::Cancel);
                            }
                        }
                        SetupState::Succeeded => {
                            if ui.small_button("×").on_hover_text("Dismiss").clicked() {
                                action = Some(SetupAction::Dismiss);
                            }
                        }
                        _ => {
                            if setup.skipped {
                                if ui.small_button("×").on_hover_text("Dismiss").clicked() {
                                    action = Some(SetupAction::Dismiss);
                                }
                            } else if ui
                                .small_button("Continue without it")
                                .on_hover_text("Start the agent in this work tree as it is")
                                .clicked()
                            {
                                action = Some(SetupAction::Skip);
                            }
                            if ui
                                .small_button("Run again")
                                .on_hover_text("Run the setup script again in this work tree")
                                .clicked()
                            {
                                action = Some(SetupAction::Retry);
                            }
                        }
                    }
                    let label = if setup.show_output {
                        "Hide output"
                    } else {
                        "Output"
                    };
                    if ui.small_button(label).clicked() {
                        action = Some(SetupAction::ToggleOutput);
                    }
                });
            });
            if setup.show_output {
                let output = setup.output();
                Frame::new()
                    .fill(c_bg_input())
                    .stroke(Stroke::new(1.0, c_border_subtle()))
                    .corner_radius(RADIUS_BUTTON)
                    .inner_margin(Margin::symmetric(8, 6))
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt(("worktree_setup_output", &setup.root))
                            .max_height(160.0)
                            .stick_to_bottom(true)
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.label(
                                    RichText::new(if output.is_empty() {
                                        "(no output yet)"
                                    } else {
                                        output.trim_end()
                                    })
                                    .font(egui::FontId::monospace(FS_TINY))
                                    .color(c_text_muted()),
                                );
                            });
                    });
            }
            ui.add_space(4.0);
        }
        match action {
            Some(SetupAction::Cancel) => {
                self.conv.worktree_setups[index]
                    .cancel
                    .store(true, Ordering::SeqCst);
            }
            Some(SetupAction::Dismiss) => {
                self.conv.worktree_setups.remove(index);
            }
            Some(SetupAction::Skip) => {
                self.conv.worktree_setups[index].skipped = true;
                self.release_queued(&root);
            }
            Some(SetupAction::Retry) => {
                let script = self.conv.worktree_setups[index].script.clone();
                if let Some(info) = crate::git::worktree::info(Path::new(&root)) {
                    self.run_worktree_setup(&root, script, &info);
                }
            }
            Some(SetupAction::ToggleOutput) => {
                let setup = &mut self.conv.worktree_setups[index];
                setup.show_output = !setup.show_output;
            }
            None => {}
        }
    }

    /// Open the setup script editor for the repository whose main checkout is workspace `wi`.
    pub(crate) fn edit_worktree_setup(&mut self, wi: usize) {
        let root = self.conv.workspaces[wi].root_path.clone();
        let draft = self
            .conv
            .settings
            .worktree_setup_scripts
            .get(&root)
            .cloned()
            .unwrap_or_default();
        self.conv.setup_script_editor = Some((root, draft));
    }

    /// The setup script editor, as a modal over everything.
    pub(crate) fn render_setup_script_editor(&mut self, ctx: &egui::Context) {
        let Some((root, mut draft)) = self.conv.setup_script_editor.take() else {
            return;
        };
        let mut done = crate::ui::chrome::modal_backdrop(ctx, "worktree_setup_backdrop");
        let mut save = false;
        egui::Area::new(egui::Id::new("worktree_setup_editor"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                Frame::new()
                    .fill(c_bg_elevated())
                    .stroke(Stroke::new(1.0, c_border()))
                    .corner_radius(RADIUS_CHIP)
                    .inner_margin(Margin::same(16))
                    .show(ui, |ui| {
                        ui.set_width(440.0_f32.min(ctx.content_rect().width() - 48.0));
                        ui.label(
                            RichText::new("Work tree setup")
                                .size(FS_BODY)
                                .color(c_text())
                                .strong(),
                        );
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(format!(
                                "Runs with the shell in every new work tree of {} before its \
                                 agent starts, e.g. installing dependencies. \
                                 $OXI_MAIN_CHECKOUT is this checkout and $OXI_WORKTREE_PATH \
                                 the new folder. Empty: no setup.",
                                crate::theme::workspace_sidebar_label(&root)
                            ))
                            .size(FS_SMALL)
                            .color(c_text_muted()),
                        );
                        ui.add_space(8.0);
                        ui.add(
                            egui::TextEdit::multiline(&mut draft)
                                .font(egui::FontId::monospace(FS_SMALL))
                                .desired_rows(6)
                                .desired_width(f32::INFINITY)
                                .hint_text("npm ci\ncp \"$OXI_MAIN_CHECKOUT/.env\" ."),
                        );
                        ui.add_space(14.0);
                        let row = egui::vec2(ui.available_width(), 28.0);
                        ui.allocate_ui_with_layout(
                            row,
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                if crate::ui::chrome::ghost_button(ui, "Save", false).clicked() {
                                    save = true;
                                }
                                if crate::ui::chrome::ghost_button(ui, "Cancel", false)
                                    .on_hover_text("Esc")
                                    .clicked()
                                {
                                    done = true;
                                }
                            },
                        );
                    });
            });
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            done = true;
        }
        if save {
            let scripts = &mut self.conv.settings.worktree_setup_scripts;
            if draft.trim().is_empty() {
                scripts.remove(&root);
            } else {
                scripts.insert(root, draft);
            }
            self.save_settings_quietly();
        } else if !done {
            self.conv.setup_script_editor = Some((root, draft));
        }
    }
}

enum SetupAction {
    Cancel,
    Dismiss,
    Skip,
    Retry,
    ToggleOutput,
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn a_setup_script_reports_output_success_failure_and_cancellation() {
        let dir = crate::fsutil::unique_temp_path(&std::env::temp_dir(), "oxi-setup", "d");
        std::fs::create_dir_all(&dir).unwrap();
        let output = Arc::new(Mutex::new(String::new()));
        let cancel = AtomicBool::new(false);
        let env = [("OXI_WORKTREE_BRANCH", "oxi/test".to_string())];
        let state = run_script(
            "echo \"on $OXI_WORKTREE_BRANCH\"; echo oops >&2; touch made",
            &dir,
            &env,
            &output,
            &cancel,
        );
        assert_eq!(state, SetupState::Succeeded);
        let text = output.lock().unwrap().clone();
        assert!(
            text.contains("on oxi/test") && text.contains("oops"),
            "{text}"
        );
        assert!(dir.join("made").exists(), "it runs in the work tree");

        let state = run_script("exit 3", &dir, &env, &output, &cancel);
        assert_eq!(state, SetupState::Failed("exit code 3".into()));

        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            flag.store(true, Ordering::SeqCst);
        });
        let started = Instant::now();
        let state = run_script("sleep 30 & wait", &dir, &env, &output, &cancel);
        assert_eq!(state, SetupState::Cancelled);
        assert!(started.elapsed() < Duration::from_secs(10));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
