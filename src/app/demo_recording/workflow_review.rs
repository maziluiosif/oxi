//! Stills of the day-to-day agent workflow screens: removing a work tree with unsaved work,
//! rewinding a chat, steering a running turn, commenting on diff lines and the rest.
//! `OXI_WORKFLOW_REVIEW=1 OXI_GALLERY=/tmp/oxi-workflow cargo test --lib render_gallery -- --ignored`

use std::process::Command;

use super::Recorder;
use crate::app::state::{ConfirmAction, Workspace};

pub(super) fn run(rec: &mut Recorder<'_>) {
    worktree_removal(rec);
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
