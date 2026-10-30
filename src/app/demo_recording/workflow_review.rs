//! Stills of the day-to-day agent workflow screens: removing a work tree with unsaved work,
//! rewinding a chat, steering a running turn, commenting on diff lines and the rest.
//! `OXI_WORKFLOW_REVIEW=1 OXI_GALLERY=/tmp/oxi-workflow cargo test --lib render_gallery -- --ignored`

use std::process::Command;

use super::Recorder;
use super::fixtures::message;
use crate::agent::AgentEvent;
use crate::app::compaction::QueuedSend;
use crate::app::state::{ConfirmAction, Workspace};
use crate::model::{AssistantBlock, MsgRole, TurnChanges};

pub(super) fn run(rec: &mut Recorder<'_>) {
    worktree_removal(rec);
    rewind(rec);
    steering(rec);
    worktree_setup(rec);
    diff_comments(rec);
    interrupted_turn(rec);
    delegation(rec);
}

fn git(dir: &std::path::Path, args: &[&str]) {
    let status = Command::new("git")
        .args([
            "-c",
            "user.name=oxi",
            "-c",
            "user.email=oxi@example.invalid",
        ])
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

/// A work tree workspace for a fresh work tree of the demo project.
fn add_worktree(rec: &mut Recorder<'_>) -> (usize, std::path::PathBuf) {
    let folder = crate::git::worktree::create(&rec.project).unwrap();
    let app = rec.app();
    app.conv.workspaces.push(Workspace {
        root_path: folder.to_string_lossy().into_owned(),
        sessions: Vec::new(),
        active: 0,
        sidebar_folded: false,
        pinned: Vec::new(),
        folded_groups: Vec::new(),
        worktree: crate::git::worktree::info(&folder),
    });
    (app.conv.workspaces.len() - 1, folder)
}

fn wait_for_risk(rec: &mut Recorder<'_>) {
    for _ in 0..200 {
        rec.harness.step();
        if rec
            .app()
            .conv
            .worktree_risk
            .as_ref()
            .is_some_and(|c| c.result.is_some())
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    rec.harness.run_steps(3);
}

fn worktree_removal(rec: &mut Recorder<'_>) {
    let (clean, _) = add_worktree(rec);
    rec.app()
        .request_confirm(ConfirmAction::RemoveWorktree { wi: clean });
    wait_for_risk(rec);
    rec.still("worktree-remove-clean");
    assert!(
        rec.app()
            .conv
            .worktree_risk
            .as_ref()
            .unwrap()
            .allows_removal()
    );
    rec.app().conv.confirm_prompt = None;

    let (dirty, folder) = add_worktree(rec);
    std::fs::write(folder.join("stats.py"), "def mean(xs):\n    return 0\n").unwrap();
    std::fs::write(folder.join("notes.md"), "# Notes\n").unwrap();
    std::fs::write(folder.join("report.csv"), "a,b\n").unwrap();
    git(&folder, &["add", "report.csv"]);
    git(
        &folder,
        &["commit", "-q", "-m", "Add the weekly report export"],
    );
    std::fs::write(folder.join("plot.py"), "import matplotlib\n").unwrap();
    rec.app()
        .request_confirm(ConfirmAction::RemoveWorktree { wi: dirty });
    wait_for_risk(rec);
    {
        let check = rec.app().conv.worktree_risk.as_ref().unwrap();
        assert!(
            check.keep_branch,
            "unmerged commits keep the branch by default"
        );
        assert!(
            !check.allows_removal(),
            "uncommitted files need an explicit choice"
        );
    }
    rec.still("worktree-remove-dirty");
    rec.app().conv.worktree_risk.as_mut().unwrap().keep_branch = false;
    rec.harness.run_steps(3);
    rec.still("worktree-remove-dirty-branch");
    rec.app().conv.worktree_risk.as_mut().unwrap().force = true;
    rec.harness.run_steps(3);
    assert!(
        rec.app()
            .conv
            .worktree_risk
            .as_ref()
            .unwrap()
            .allows_removal()
    );
    rec.still("worktree-remove-dirty-forced");
    rec.app().conv.confirm_prompt = None;
    rec.app().conv.worktree_risk = None;
}

fn wait_for_rewind_check(rec: &mut Recorder<'_>) {
    for _ in 0..200 {
        rec.harness.step();
        if rec
            .app()
            .conv
            .rewind_check
            .as_ref()
            .is_some_and(|c| c.plan.is_some())
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    rec.harness.run_steps(3);
}

/// Rewinding to an earlier message after a restart: the saved snapshots restore the files.
fn rewind(rec: &mut Recorder<'_>) {
    let project = rec.project.clone();
    let snap = |rec: &Recorder<'_>| crate::git::checkpoint::snapshot(&rec.project).unwrap();
    let t0 = snap(rec);
    std::fs::write(
        project.join("export.py"),
        "def export_csv(rows):\n    pass\n",
    )
    .unwrap();
    let t1 = snap(rec);
    std::fs::write(
        project.join("stats.py"),
        "def mean(xs):\n    return sum(xs) / len(xs)\n",
    )
    .unwrap();
    let t2 = snap(rec);
    let turn = |before: &str, after: &str| {
        Some(Box::new(TurnChanges {
            repo: t0.repo.to_string_lossy().into_owned(),
            before: before.into(),
            after: after.into(),
            files: None,
        }))
    };
    let reply = |text: &str| {
        message(
            MsgRole::Assistant,
            "",
            vec![AssistantBlock::Answer(text.into())],
        )
    };
    {
        let app = rec.app();
        app.select_workspace(0);
        app.new_chat();
        let session = app.active_session_mut();
        session.title = "Add CSV export".into();
        let mut first = reply("Added `export.py` with a CSV writer.");
        first.changes = turn(&t0.tree, &t1.tree);
        let mut second = reply("`mean` now divides by the length.");
        second.changes = turn(&t1.tree, &t2.tree);
        session.messages = vec![
            message(
                MsgRole::User,
                "Add a CSV export for the weekly report",
                vec![],
            ),
            first,
            message(MsgRole::User, "Also fix mean()", vec![]),
            second,
        ];
    }
    let key = rec.app().active_session_key();
    rec.app().request_confirm(ConfirmAction::Rewind {
        wi: key.workspace_idx,
        si: key.session_idx,
        index: 0,
        restore_files: false,
    });
    wait_for_rewind_check(rec);
    rec.still("rewind-confirm");
    // An edit made since blocks restoring the files (the chat alone can still rewind).
    std::fs::write(project.join("export.py"), "# my own edit\n").unwrap();
    rec.app().conv.confirm_prompt = None;
    rec.app().request_confirm(ConfirmAction::Rewind {
        wi: key.workspace_idx,
        si: key.session_idx,
        index: 0,
        restore_files: false,
    });
    wait_for_rewind_check(rec);
    rec.still("rewind-conflict");
    rec.app().conv.confirm_prompt = None;
    rec.app().conv.rewind_check = None;
    std::fs::remove_file(project.join("export.py")).unwrap();
    git(&project, &["checkout", "-q", "--", "stats.py"]);
}

/// Messages written while oxi's own agent answers: steer one into the running turn.
fn steering(rec: &mut Recorder<'_>) {
    rec.app().new_chat();
    rec.begin_turn("Refactor the CSV export into its own module");
    let key = rec.app().active_session_key();
    let inbox = crate::agent::steer::SteerInbox::default();
    {
        let run = rec.app().run_state_mut(key);
        run.steer = Some(inbox.clone());
        for text in [
            "Keep the old function name as an alias",
            "Then run the tests",
        ] {
            run.queued.push_back(QueuedSend {
                text: text.into(),
                images: Vec::new(),
                texts: Vec::new(),
            });
        }
    }
    rec.send(AgentEvent::TextStart);
    rec.send(AgentEvent::TextDelta(
        "I'll move `export_csv` and its helpers into `export.py`.".into(),
    ));
    rec.harness.run_steps(4);
    rec.still("steer-queue");
    rec.app().steer_into_turn(key, 0);
    rec.harness.run_steps(3);
    assert_eq!(rec.app().run_state(key).unwrap().steer_pending.len(), 1);
    rec.still("steer-pending");
    // The agent reaches its next step and reads it.
    for text in inbox.take() {
        rec.send(AgentEvent::Steered(text));
    }
    rec.send(AgentEvent::TextStart);
    rec.send(AgentEvent::TextDelta(
        "Got it: `export_csv` stays importable from `stats` as an alias.".into(),
    ));
    rec.harness.run_steps(4);
    {
        let app = rec.app();
        let messages = &app.active_session().messages;
        assert!(messages[messages.len() - 2].role == MsgRole::User);
        assert!(app.run_state(key).unwrap().steer_pending.is_empty());
    }
    rec.still("steer-delivered");
    rec.finish_turn();
    rec.harness.run_steps(4);
}

fn wait_until(rec: &mut Recorder<'_>, done: impl Fn(&mut crate::app::OxiApp) -> bool) {
    for _ in 0..400 {
        rec.harness.step();
        if done(rec.app()) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    rec.harness.run_steps(3);
}

/// A repository's setup script running in a new work tree, then failing.
fn worktree_setup(rec: &mut Recorder<'_>) {
    let project = rec.project.to_string_lossy().into_owned();
    rec.app().select_workspace(0);
    rec.app().edit_worktree_setup(0);
    rec.app().conv.setup_script_editor.as_mut().unwrap().1 =
        "echo \"Installing dependencies...\"\nsleep 1\nnpm ci".into();
    rec.harness.run_steps(3);
    rec.still("setup-editor");
    rec.app().conv.setup_script_editor = None;
    rec.app().conv.settings.worktree_setup_scripts.insert(
        project.clone(),
        "echo \"Installing dependencies...\"\nsleep 1\necho \"npm ERR! Missing script: ci\" >&2\nexit 1".into(),
    );
    let before = rec.app().conv.workspaces.len();
    let root = rec.project.clone();
    rec.app().start_worktree_chat_at(root);
    wait_until(rec, |app| {
        app.conv.workspaces.len() > before && !app.conv.worktree_setups.is_empty()
    });
    rec.app().conv.composer.input = "Add a --json flag to the report command".into();
    rec.app().send_message();
    rec.app().conv.worktree_setups[0].show_output = true;
    rec.harness.run_steps(4);
    {
        let app = rec.app();
        let key = app.active_session_key();
        assert_eq!(
            app.run_state(key).unwrap().queued.len(),
            1,
            "held until setup is done"
        );
        assert!(!app.active_waiting_response());
    }
    rec.still("setup-running");
    wait_until(rec, |app| {
        app.conv
            .worktree_setups
            .first()
            .is_some_and(|s| !matches!(s.state, crate::app::worktree_setup::SetupState::Running))
    });
    rec.still("setup-failed");
    assert!(
        !rec.app().active_waiting_response(),
        "a failed setup holds the agent"
    );
}

/// Move the pointer onto the widget labelled `label` and click it.
fn click(rec: &mut Recorder<'_>, label: &str) {
    use egui_kittest::kittest::Queryable;
    // Accessibility bounds are in physical pixels; pointer events in points.
    let rect = rec.harness.get_by_label(label).rect();
    let pos = (rect.center().to_vec2() / rec.harness.ctx.pixels_per_point()).to_pos2();
    rec.harness.hover_at(pos);
    rec.harness.step();
    for pressed in [true, false] {
        rec.harness.event(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
        rec.harness.step();
    }
    rec.harness.run_steps(3);
}

/// Comments on diff lines collected for the agent.
fn diff_comments(rec: &mut Recorder<'_>) {
    use egui_kittest::kittest::Queryable;
    let project = rec.project.clone();
    let stats = std::fs::read_to_string(project.join("stats.py")).unwrap();
    std::fs::write(
        project.join("stats.py"),
        stats.replacen(
            "def ",
            "# Summary statistics for the weekly report.\n\ndef ",
            1,
        ),
    )
    .unwrap();
    rec.app().select_workspace(0);
    rec.app().new_chat();
    assert!(rec.app().open_diff_editor(
        "stats.py",
        crate::app::file_explorer::DiffSource::WorkTree,
        None
    ));
    rec.harness.run_steps(10);
    std::thread::sleep(std::time::Duration::from_millis(300));
    rec.harness.run_steps(10);
    // Hover down the new side until the change offers its buttons.
    let rect = rec.harness.ctx.content_rect();
    let mut found = false;
    for step in 0..60 {
        let pos = egui::pos2(rect.center().x + 200.0, 50.0 + step as f32 * 10.0);
        rec.harness.hover_at(pos);
        rec.harness.run_steps(2);
        if rec.harness.query_by_label("Comment").is_some() {
            found = true;
            break;
        }
    }
    assert!(found, "a hovered change offers Comment");
    rec.still("diff-comment-button");
    // A real pointer click: the button only exists while its change is hovered.
    click(rec, "Comment");
    assert!(rec.app().conv.comment_draft.is_some());
    rec.app().conv.comment_draft.as_mut().unwrap().text =
        "Use a module docstring instead of a comment.".into();
    rec.harness.run_steps(3);
    rec.still("diff-comment-draft");
    click(rec, "Add comment");
    let comment = rec.app().conv.review_comments[0].clone();
    rec.app()
        .conv
        .review_comments
        .push(crate::app::review_comments::ReviewComment {
            new_lines: Some((14, 16)),
            old_lines: Some((12, 14)),
            note: "Guard against an empty list here.".into(),
            ..comment
        });
    rec.app().reveal_chat_view();
    rec.harness.run_steps(4);
    rec.still("diff-comments-composer");
    rec.app().attach_review_comments();
    let texts = &rec.app().conv.composer.pending_texts;
    assert!(
        matches!(texts.last(), Some(crate::model::UserAttachment::Text { name, text })
        if name == "Review comments (2)" && text.contains("Use a module docstring"))
    );
    rec.harness.run_steps(3);
    rec.still("diff-comments-attached");
    std::fs::write(project.join("stats.py"), stats).unwrap();
}

/// A chat whose turn an earlier oxi process was running when it closed.
fn interrupted_turn(rec: &mut Recorder<'_>) {
    use crate::session_store::run_journal::{Effect, TurnJournal};
    rec.app().select_workspace(0);
    rec.app().new_chat();
    let key = rec.app().active_session_key();
    let root = rec.project.to_string_lossy().into_owned();
    {
        let session = rec.app().active_session_mut();
        session.title = "Make the percentile test deterministic".into();
        session.messages = vec![message(
            MsgRole::User,
            "Make the percentile test deterministic",
            vec![],
        )];
        crate::session_store::save_session_messages(&root, session).unwrap();
    }
    let file = rec.app().active_session().session_file.clone().unwrap();
    let mut journal =
        TurnJournal::start(&file, "Make the percentile test deterministic", None).unwrap();
    journal.tool("t1", "read", "{\"path\":\"test_stats.py\"}", Effect::Read);
    journal.tool_done("t1", false);
    journal.tool("t2", "edit", "{\"path\":\"test_stats.py\"}", Effect::Write);
    journal.tool("t3", "bash", "{\"command\":\"pytest -q\"}", Effect::Shell);
    journal.queue(&["Then update the changelog".into()]);
    drop(journal);
    rec.app().recover_interrupted_run(key);
    rec.harness.run_steps(5);
    assert!(rec.app().run_state(key).unwrap().interrupted.is_some());
    rec.still("interrupted-turn");
}

/// A chat's agent delegating a review to another provider: the task shows above its composer.
fn delegation(rec: &mut Recorder<'_>) {
    rec.app().select_workspace(0);
    rec.app().new_chat();
    let parent = rec.app().active_session_key();
    {
        let session = rec.app().active_session_mut();
        session.title = "Speed up the report export".into();
        session.messages = vec![
            message(MsgRole::User, "Speed up the report export", vec![]),
            message(
                MsgRole::Assistant,
                "",
                vec![AssistantBlock::Answer(
                    "I batched the CSV writes. I asked another agent to review the change.".into(),
                )],
            ),
        ];
    }
    let dir = crate::delegation::dir_for(&rec.app().acp_session_key(parent));
    std::fs::create_dir_all(dir.join("requests")).unwrap();
    let request = crate::delegation::Request::Delegate {
        task: "d5f2a91c".into(),
        provider: "openai".into(),
        model: Some("gpt-5.1".into()),
        prompt: "Review the batched CSV writes in export.py for correctness.".into(),
        title: Some("Review the CSV batching".into()),
        worktree: false,
    };
    std::fs::write(
        dir.join("requests/1-d5f2a91c.json"),
        serde_json::to_vec(&request).unwrap(),
    )
    .unwrap();
    let ctx = rec.harness.ctx.clone();
    rec.app().poll_delegations_now(&ctx);
    rec.harness.run_steps(3);
    rec.still("delegation-running");
    let _ = std::fs::remove_dir_all(dir);
}
