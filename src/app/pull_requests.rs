//! The pull request of the open workspace's branch, next to the chat that works on it: a pill in
//! the status bar (number, state, checks) with Open, Review changes and Fix failing checks, a
//! row in the git panel's Compare tab, and a dialog that pushes the branch and opens a pull
//! request with a title and description drafted from the chat. Read through the GitHub CLI
//! (see [`crate::git::pull_request`]) on a background thread, again every minute (more often
//! while checks run) and whenever the branch changes.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use eframe::egui::{self, Frame, Margin, RichText, Stroke, Ui};

use super::OxiApp;
use crate::git::pull_request::{NewPullRequest, PrState, PullRequest};
use crate::theme::*;

/// How often a branch's pull request is read again.
const REFRESH: Duration = Duration::from_secs(60);
/// … while its checks are still running.
const REFRESH_PENDING: Duration = Duration::from_secs(20);

type ViewResult = Result<Option<PullRequest>, String>;

/// What is known about one workspace's pull request.
#[derive(Default)]
struct Slot {
    /// Branch the result is for.
    branch: String,
    result: Option<ViewResult>,
    checked: Option<Instant>,
    rx: Option<Receiver<ViewResult>>,
}

/// The "Open a pull request" dialog.
pub(crate) struct PrDialog {
    root: String,
    new: NewPullRequest,
    rx: Option<Receiver<Result<PullRequest, String>>>,
    error: Option<String>,
}

#[derive(Default)]
pub(crate) struct PullRequests {
    slots: HashMap<String, Slot>,
    pub dialog: Option<PrDialog>,
}

/// One line for the checks: "✓ 5", "✗ 1 · ✓ 4", "… 2".
fn checks_label(pr: &PullRequest) -> Option<(String, egui::Color32)> {
    let checks = &pr.checks;
    if checks.total() == 0 {
        return None;
    }
    Some(if checks.failed > 0 {
        (format!("✗ {} failing", checks.failed), c_error_fg())
    } else if checks.pending > 0 {
        (format!("… {} running", checks.pending), c_text_muted())
    } else {
        (format!("✓ {}", checks.passed), c_success())
    })
}

fn state_color(state: PrState) -> egui::Color32 {
    match state {
        PrState::Open => c_success(),
        PrState::Draft | PrState::Closed => c_text_muted(),
        PrState::Merged => c_accent(),
    }
}

/// The message "Fix failing checks" puts in the composer.
fn fix_checks_prompt(pr: &PullRequest) -> String {
    format!(
        "The CI checks of pull request #{} ({}) are failing: {}. Find out why (`gh pr checks {}` \
         and `gh run view --log-failed` show the logs), fix the cause, and tell me what you \
         changed.",
        pr.number,
        pr.url,
        pr.checks.failing.join(", "),
        pr.number
    )
}

impl OxiApp {
    /// The open workspace's pull request, once read.
    pub(crate) fn active_pull_request(&self) -> Option<&PullRequest> {
        let slot = self
            .pull_requests
            .slots
            .get(&self.active_workspace().root_path)?;
        (slot.branch == self.conv.git.branch)
            .then_some(slot.result.as_ref()?.as_ref().ok()?.as_ref())
            .flatten()
    }

    /// Why the open workspace's pull request could not be read (no `gh`, not GitHub…).
    fn pull_request_error(&self) -> Option<&str> {
        let slot = self
            .pull_requests
            .slots
            .get(&self.active_workspace().root_path)?;
        slot.result.as_ref()?.as_ref().err().map(String::as_str)
    }

    /// Once per frame: take finished reads, start the next one when it is due.
    pub(crate) fn poll_pull_requests(&mut self, ctx: &egui::Context) {
        self.poll_pr_dialog();
        let root = self.active_workspace().root_path.clone();
        // Tests and gallery stills must not call the real GitHub CLI.
        if cfg!(test)
            || !self.conv.git.repo
            || self.conv.git.branch.is_empty()
            || crate::remote::is_remote(&root)
            || self.conn.no_session
        {
            return;
        }
        let branch = self.conv.git.branch.clone();
        let slot = self.pull_requests.slots.entry(root.clone()).or_default();
        if let Some(rx) = slot.rx.as_ref() {
            match rx.try_recv() {
                Ok(result) => {
                    slot.result = Some(result);
                    slot.rx = None;
                }
                Err(TryRecvError::Disconnected) => slot.rx = None,
                Err(TryRecvError::Empty) => {
                    ctx.request_repaint_after(Duration::from_millis(500));
                    return;
                }
            }
        }
        let pending = matches!(
            &slot.result,
            Some(Ok(Some(pr))) if pr.checks.pending > 0
        );
        let every = if pending { REFRESH_PENDING } else { REFRESH };
        let due = slot.branch != branch || slot.checked.is_none_or(|at| at.elapsed() >= every);
        if !due {
            if let Some(at) = slot.checked {
                ctx.request_repaint_after(every.saturating_sub(at.elapsed()));
            }
            return;
        }
        if slot.branch != branch {
            slot.result = None;
        }
        slot.branch = branch;
        slot.checked = Some(Instant::now());
        let (tx, rx) = std::sync::mpsc::channel();
        slot.rx = Some(rx);
        let ctx = ctx.clone();
        let _ = std::thread::Builder::new()
            .name("oxi-pull-request".into())
            .spawn(move || {
                let _ = tx.send(crate::git::pull_request::view(&PathBuf::from(root)));
                ctx.request_repaint();
            });
    }

    /// Stand in a pull request for the open workspace's branch (gallery stills).
    #[cfg(test)]
    pub(crate) fn demo_pull_request(&mut self, pr: PullRequest) {
        let root = self.active_workspace().root_path.clone();
        let slot = self.pull_requests.slots.entry(root).or_default();
        slot.branch = self.conv.git.branch.clone();
        slot.checked = Some(Instant::now() + Duration::from_secs(3600));
        slot.rx = None;
        slot.result = Some(Ok(Some(pr)));
    }

    /// Read the open workspace's pull request again now.
    fn refresh_pull_request(&mut self) {
        let root = self.active_workspace().root_path.clone();
        if let Some(slot) = self.pull_requests.slots.get_mut(&root) {
            slot.checked = None;
        }
    }

    /// Show the git panel's Compare tab: the branch's changes against its base.
    fn review_branch_changes(&mut self) {
        self.conv.git_ui.open = true;
        self.conv.git_ui.tab = super::git_panel::GitTab::Compare;
    }

    /// Put `text` in the composer (ahead of a draft) and focus it.
    fn compose(&mut self, text: String) {
        let draft = self.conv.composer.input.trim().to_owned();
        self.conv.composer.input = if draft.is_empty() {
            text
        } else {
            format!("{text}\n\n{draft}")
        };
        self.conv.composer.focus_next_frame = true;
    }

    /// The status bar's pull request pill, when the branch has one.
    pub(crate) fn render_pr_pill(&mut self, ui: &mut Ui) {
        let Some(pr) = self.active_pull_request().cloned() else {
            return;
        };
        let checks = checks_label(&pr);
        let mut job = egui::text::LayoutJob::default();
        let font = egui::FontId::proportional(FS_TINY);
        job.append(
            &format!("#{} ", pr.number),
            0.0,
            egui::TextFormat::simple(font.clone(), c_text_muted()),
        );
        job.append(
            pr.state.label(),
            0.0,
            egui::TextFormat::simple(font.clone(), state_color(pr.state)),
        );
        if let Some((label, color)) = &checks {
            job.append(
                &format!("  {label}"),
                0.0,
                egui::TextFormat::simple(font, *color),
            );
        }
        let response = ui
            .add(egui::Label::new(job).sense(egui::Sense::click()))
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text(format!(
                "Pull request #{}: {}\n{}",
                pr.number, pr.title, pr.url
            ));
        let mut action = None;
        egui::Popup::menu(&response).show(|ui| {
            ui.set_min_width(220.0);
            if ui.button("Open on GitHub").clicked() {
                action = Some(0);
            }
            if ui.button("Review changes").clicked() {
                action = Some(1);
            }
            if pr.checks.failed > 0
                && ui
                    .button("Fix failing checks")
                    .on_hover_text("Ask the agent of this chat to fix them")
                    .clicked()
            {
                action = Some(2);
            }
            if ui.button("Copy link").clicked() {
                action = Some(3);
            }
            if ui.button("Refresh").clicked() {
                action = Some(4);
            }
        });
        match action {
            Some(0) => {
                let _ = webbrowser::open(&pr.url);
            }
            Some(1) => self.review_branch_changes(),
            Some(2) => self.compose(fix_checks_prompt(&pr)),
            Some(3) => ui.ctx().copy_text(pr.url.clone()),
            Some(4) => self.refresh_pull_request(),
            _ => {}
        }
        ui.add_space(8.0);
    }

    /// The Compare tab's pull request row: the branch's pull request, or a button opening one.
    pub(crate) fn render_compare_pr_row(&mut self, ui: &mut Ui, base: &str, ahead: usize) {
        if let Some(pr) = self.active_pull_request().cloned() {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(
                    RichText::new(format!("#{}", pr.number))
                        .size(FS_SMALL)
                        .color(c_text_muted()),
                );
                ui.label(
                    RichText::new(pr.state.label())
                        .size(FS_SMALL)
                        .color(state_color(pr.state)),
                );
                if let Some((label, color)) = checks_label(&pr) {
                    ui.label(RichText::new(label).size(FS_SMALL).color(color))
                        .on_hover_text(if pr.checks.failing.is_empty() {
                            String::new()
                        } else {
                            format!("Failing: {}", pr.checks.failing.join(", "))
                        });
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("Open").clicked() {
                        let _ = webbrowser::open(&pr.url);
                    }
                    if pr.checks.failed > 0
                        && ui
                            .small_button("Fix checks")
                            .on_hover_text("Ask the agent of this chat to fix the failing checks")
                            .clicked()
                    {
                        self.compose(fix_checks_prompt(&pr));
                    }
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.add(
                            egui::Label::new(RichText::new(&pr.title).size(FS_SMALL))
                                .truncate()
                                .selectable(false),
                        )
                        .on_hover_text(&pr.title);
                    });
                });
            });
            ui.add_space(4.0);
            return;
        }
        let error = self.pull_request_error().map(str::to_owned);
        let on_base = self.conv.git.branch == base || base.is_empty();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if on_base || ahead == 0 {
                return;
            }
            if let Some(error) = &error {
                ui.add(
                    egui::Label::new(
                        RichText::new(format!("Pull requests: {error}"))
                            .size(FS_TINY)
                            .color(c_text_faint()),
                    )
                    .truncate(),
                )
                .on_hover_text(error);
                return;
            }
            if ui
                .button(RichText::new("Open a pull request…").size(FS_SMALL))
                .on_hover_text(format!(
                    "Push {} and open a pull request into {base} (GitHub CLI)",
                    self.conv.git.branch
                ))
                .clicked()
            {
                self.open_pr_dialog(base);
            }
        });
        ui.add_space(4.0);
    }

    /// Start the dialog, drafted from the open chat: its title, its first request and the
    /// branch's commits.
    fn open_pr_dialog(&mut self, base: &str) {
        let session = self.session_by_key(self.active_session_key());
        let chat_title = session.title.trim().to_owned();
        let request = session
            .messages
            .iter()
            .find(|m| m.role == crate::model::MsgRole::User)
            .map(|m| {
                crate::app::mentions::strip_mention_context(&m.text)
                    .trim()
                    .to_owned()
            })
            .unwrap_or_default();
        let commits: Vec<String> = self
            .conv
            .git_ui
            .compare
            .data
            .as_ref()
            .map(|data| {
                data.commits
                    .iter()
                    .map(|c| c.message.lines().next().unwrap_or_default().to_owned())
                    .collect()
            })
            .unwrap_or_default();
        let title = if !chat_title.is_empty() && chat_title != "New chat" {
            chat_title
        } else {
            commits.last().cloned().unwrap_or_default()
        };
        let mut body = String::new();
        if !request.is_empty() {
            body.push_str(&request);
            body.push_str("\n\n");
        }
        if !commits.is_empty() {
            body.push_str("Commits:\n");
            for commit in commits.iter().rev() {
                body.push_str(&format!("- {commit}\n"));
            }
        }
        self.pull_requests.dialog = Some(PrDialog {
            root: self.active_workspace().root_path.clone(),
            new: NewPullRequest {
                title,
                body: body.trim_end().to_owned(),
                base: base.to_owned(),
                draft: false,
            },
            rx: None,
            error: None,
        });
    }

    fn poll_pr_dialog(&mut self) {
        let Some(dialog) = self.pull_requests.dialog.as_mut() else {
            return;
        };
        let Some(rx) = dialog.rx.as_ref() else {
            return;
        };
        let result = match rx.try_recv() {
            Err(TryRecvError::Empty) => return,
            Ok(result) => result,
            Err(TryRecvError::Disconnected) => Err("The pull request was not opened.".into()),
        };
        dialog.rx = None;
        match result {
            Ok(pr) => {
                let root = dialog.root.clone();
                self.pull_requests.dialog = None;
                let slot = self.pull_requests.slots.entry(root).or_default();
                slot.branch = pr.head.clone();
                slot.checked = Some(Instant::now());
                let number = pr.number;
                slot.result = Some(Ok(Some(pr)));
                self.notify_composer(format!("Opened pull request #{number}."));
            }
            Err(e) => dialog.error = Some(e),
        }
    }

    /// The "Open a pull request" dialog, while open.
    pub(crate) fn render_pr_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.pull_requests.dialog.take() else {
            return;
        };
        let mut close = false;
        let mut submit = false;
        let busy = dialog.rx.is_some();
        let branch = self.conv.git.branch.clone();
        egui::Window::new("Open a pull request")
            .id(egui::Id::new("pr_dialog"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .frame(
                Frame::new()
                    .fill(c_bg_elevated())
                    .stroke(Stroke::new(1.0, c_border()))
                    .corner_radius(RADIUS_PANEL)
                    .inner_margin(Margin::same(16)),
            )
            .title_bar(false)
            .show(ctx, |ui| {
                let width = 460.0_f32.min(ctx.content_rect().width() - 64.0);
                ui.set_width(width);
                ui.label(
                    RichText::new("Open a pull request")
                        .size(FS_BODY)
                        .color(c_text_strong()),
                );
                ui.label(
                    RichText::new(format!(
                        "Pushes {branch} and opens it against {}.",
                        if dialog.new.base.is_empty() {
                            "the default branch"
                        } else {
                            &dialog.new.base
                        }
                    ))
                    .size(FS_SMALL)
                    .color(c_text_muted()),
                );
                ui.add_space(10.0);
                ui.add_enabled_ui(!busy, |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut dialog.new.title)
                            .hint_text("Title")
                            .desired_width(f32::INFINITY),
                    );
                    ui.add_space(6.0);
                    egui::ScrollArea::vertical()
                        .max_height(220.0)
                        .show(ui, |ui| {
                            ui.add(
                                egui::TextEdit::multiline(&mut dialog.new.body)
                                    .hint_text("Description")
                                    .desired_rows(8)
                                    .desired_width(f32::INFINITY),
                            );
                        });
                    ui.add_space(6.0);
                    ui.checkbox(&mut dialog.new.draft, "Open as a draft");
                });
                if let Some(error) = &dialog.error {
                    ui.add_space(6.0);
                    ui.label(RichText::new(error).size(FS_SMALL).color(c_error_fg()));
                }
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if busy {
                        small_spinner(ui);
                        ui.label(
                            RichText::new("Pushing and opening…")
                                .size(FS_SMALL)
                                .color(c_text_muted()),
                        );
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let ready = !busy && !dialog.new.title.trim().is_empty();
                        if ui
                            .add_enabled(ready, egui::Button::new("Open pull request"))
                            .clicked()
                        {
                            submit = true;
                        }
                        if ui.add_enabled(!busy, egui::Button::new("Cancel")).clicked() {
                            close = true;
                        }
                    });
                });
            });
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) && !busy {
            close = true;
        }
        if submit {
            let (tx, rx) = std::sync::mpsc::channel();
            let root = PathBuf::from(&dialog.root);
            let new = dialog.new.clone();
            let ctx = ctx.clone();
            let _ = std::thread::Builder::new()
                .name("oxi-pr-create".into())
                .spawn(move || {
                    let _ = tx.send(crate::git::pull_request::create(&root, &new));
                    ctx.request_repaint();
                });
            dialog.rx = Some(rx);
            dialog.error = None;
        }
        if !close {
            self.pull_requests.dialog = Some(dialog);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::pull_request::Checks;

    #[test]
    fn the_fix_prompt_names_the_failing_checks() {
        let pr = PullRequest {
            number: 7,
            title: "t".into(),
            url: "https://github.com/o/r/pull/7".into(),
            state: PrState::Open,
            base: "main".into(),
            head: "feat".into(),
            checks: Checks {
                passed: 1,
                failed: 2,
                pending: 0,
                failing: vec!["lint".into(), "test".into()],
            },
        };
        let prompt = fix_checks_prompt(&pr);
        assert!(
            prompt.contains("#7") && prompt.contains("lint, test"),
            "{prompt}"
        );
        assert_eq!(checks_label(&pr).unwrap().0, "✗ 2 failing");
    }
}
