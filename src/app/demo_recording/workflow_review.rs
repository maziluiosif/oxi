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
    std::fs::write(
        project.join("export.py"),
        "def export_csv(rows):\n    pass\n",
    )
    .unwrap();
    rec.app().conv.confirm_prompt = None;
    rec.app().conv.rewind_check = None;
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
