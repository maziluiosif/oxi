use super::*;
use std::path::PathBuf;

struct TestRepo {
    root: PathBuf,
    repo: Repository,
}

impl TestRepo {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "oxi-git-refresh-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let repo = Repository::init(&root).unwrap();
        std::fs::write(root.join("file.txt"), "original\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("file.txt")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let sig = git2::Signature::now("Oxi Test", "oxi@example.com").unwrap();
        repo.commit(
            Some("HEAD"),
            &sig,
            &sig,
            "initial",
            &repo.find_tree(tree_id).unwrap(),
            &[],
        )
        .unwrap();
        Self { root, repo }
    }

    fn cwd(&self) -> &str {
        self.root.to_str().unwrap()
    }
}

impl Drop for TestRepo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn auto_refresh_updates_status_and_the_selected_file_diff() {
    let repo = TestRepo::new();
    let view = GitOp::ShowDiff {
        path: "file.txt".into(),
        staged: false,
    };
    std::fs::write(repo.root.join("file.txt"), "external update\n").unwrap();
    let state = auto_refresh(repo.cwd(), Some(&view));
    assert!(!state.busy);
    assert_eq!(state.last_op.as_deref(), Some("auto refresh"));
    assert_eq!(state.current_diff_path.as_deref(), Some("file.txt"));
    assert_eq!(state.current_diff_staged, Some(false));
    assert!(state.unstaged.iter().any(|entry| entry.path == "file.txt"));
    assert!(state.diff.unwrap().1.contains("+external update"));
    assert!(!state.line_changes["file.txt"].is_empty());
}

#[test]
fn auto_refresh_preserves_commit_diff_instead_of_treating_the_hash_as_a_file() {
    let repo = TestRepo::new();
    let hash = repo.repo.head().unwrap().target().unwrap().to_string();
    let view = GitOp::ShowCommit(hash.clone());
    let before = handle_op(repo.cwd(), view.clone());
    std::fs::write(repo.root.join("file.txt"), "external update\n").unwrap();
    let state = auto_refresh(repo.cwd(), Some(&view));
    assert_eq!(state.diff, before.diff);
    assert_eq!(state.current_diff_path, Some(hash));
    assert_eq!(state.unstaged.len(), 1);
}

#[test]
fn auto_refresh_detects_unopened_files_without_opening_a_diff() {
    let repo = TestRepo::new();
    std::fs::write(repo.root.join("new.txt"), "new file\n").unwrap();
    let state = auto_refresh(repo.cwd(), None);
    assert!(state.diff.is_none());
    assert!(state.current_diff_path.is_none());
    assert!(state.unstaged.iter().any(|entry| entry.path == "new.txt"));
}

#[test]
fn worker_distinguishes_auto_refresh_from_commit_diff_collection() {
    let repo = TestRepo::new();
    std::fs::write(repo.root.join("file.txt"), "external update\n").unwrap();
    let channels = GitChannels::new(repo.cwd().into(), egui::Context::default());
    channels.tx.send(GitOp::AutoRefresh).unwrap();
    channels.tx.send(GitOp::CollectCommitDiff).unwrap();
    let mut saw_auto_refresh = false;
    loop {
        let state = channels
            .rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        match state.last_op.as_deref() {
            Some("auto refresh") => {
                saw_auto_refresh = true;
                assert!(state.commit_diff.is_none());
            }
            Some("collect commit diff") => {
                assert!(saw_auto_refresh);
                assert!(!state.busy);
                assert!(state.commit_diff.unwrap().contains("external update"));
                break;
            }
            _ => {}
        }
    }
}

#[test]
fn worker_auto_refresh_preserves_diff_and_resets_it_on_workspace_change() {
    let first = TestRepo::new();
    let second = TestRepo::new();
    let channels = GitChannels::new(first.cwd().into(), egui::Context::default());
    let view = GitOp::ShowDiff {
        path: "file.txt".into(),
        staged: false,
    };
    channels.tx.send(view).unwrap();
    channels.tx.send(GitOp::AutoRefresh).unwrap();
    let receive_auto = || loop {
        let state = channels
            .rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        if state.last_op.as_deref() == Some("auto refresh") {
            break state;
        }
    };
    assert_eq!(
        receive_auto().current_diff_path.as_deref(),
        Some("file.txt")
    );
    channels
        .tx
        .send(GitOp::SetCwd(second.cwd().into()))
        .unwrap();
    channels.tx.send(GitOp::AutoRefresh).unwrap();
    let state = receive_auto();
    assert!(state.diff.is_none());
    assert!(state.current_diff_path.is_none());
}

#[test]
fn diff_worker_answers_views_alone_and_supersedes_stale_ones() {
    let repo = TestRepo::new();
    std::fs::write(repo.root.join("file.txt"), "external update\n").unwrap();
    std::fs::write(repo.root.join("other.txt"), "untracked\n").unwrap();
    let channels = GitChannels::new(repo.cwd().into(), egui::Context::default());
    let hash = repo.repo.head().unwrap().target().unwrap().to_string();
    channels.tx.send(GitOp::ShowCommit(hash)).unwrap();
    channels
        .tx
        .send(GitOp::ShowDiff {
            path: "file.txt".into(),
            staged: false,
        })
        .unwrap();
    let state = loop {
        let state = channels
            .rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        if state.diff_only && state.view_generation == 2 {
            break state;
        }
    };
    assert_eq!(state.current_diff_path.as_deref(), Some("file.txt"));
    let (title, text) = state.diff.unwrap();
    assert_eq!(title, "file.txt");
    assert!(text.contains("+external update"));
    // A literal pathspec: the untracked neighbour is not part of this file's diff.
    assert!(!text.contains("untracked"));
}

#[test]
fn line_changes_cover_only_changed_files() {
    let repo = TestRepo::new();
    std::fs::write(repo.root.join("file.txt"), "original\nadded\n").unwrap();
    std::fs::write(repo.root.join("new.txt"), "a\nb\n").unwrap();
    let state = handle_op(repo.cwd(), GitOp::Refresh);
    assert_eq!(
        state.line_changes["file.txt"],
        [GitLineChange {
            line: 1,
            kind: GitLineKind::Added
        }]
    );
    assert_eq!(state.line_changes["new.txt"].len(), 3);
}

#[test]
fn system_git_command_is_non_interactive_and_runs_in_the_workdir() {
    let git = system::resolve_executable("/custom/bin/git");
    assert_eq!(git, PathBuf::from("/custom/bin/git"));
    let workdir = std::env::temp_dir();
    let cmd = system::command(&git, &workdir, &system::push_args("feat/x"));
    assert_eq!(cmd.get_program(), "/custom/bin/git");
    assert_eq!(cmd.get_current_dir(), Some(workdir.as_path()));
    let args = cmd.get_args().collect::<Vec<_>>();
    assert_eq!(
        args,
        [
            "push",
            "--porcelain",
            "origin",
            "refs/heads/feat/x:refs/heads/feat/x"
        ]
    );
    let envs = cmd.get_envs().collect::<Vec<_>>();
    assert!(envs.contains(&("GIT_TERMINAL_PROMPT".as_ref(), Some("0".as_ref()))));
    assert_eq!(system::fetch_args(), ["fetch", "origin"]);
}

#[test]
fn system_git_resolves_a_default_executable_when_unconfigured() {
    let git = system::resolve_executable("  ");
    let name = git.file_name().unwrap().to_string_lossy().into_owned();
    assert!(name == "git" || name == "git.exe", "{git:?}");
}

#[test]
fn system_git_detects_non_fast_forward_push_rejections() {
    assert!(system::push_rejected_non_fast_forward(
        "To github.com:o/r.git\n!\trefs/heads/main:refs/heads/main\t[rejected] (fetch first)\nDone\n"
    ));
    assert!(system::push_rejected_non_fast_forward(
        "!\trefs/heads/main:refs/heads/main\t[rejected] (non-fast-forward)\n"
    ));
    assert!(!system::push_rejected_non_fast_forward(
        "!\trefs/heads/main:refs/heads/main\t[remote rejected] (pre-receive hook declined)\n"
    ));
    assert!(!system::push_rejected_non_fast_forward(
        " \trefs/heads/main:refs/heads/main\t1111111..2222222\nDone\n"
    ));
}

#[test]
fn system_git_pushes_and_fetches_through_a_local_remote() {
    // Needs a real `git`; skip quietly on machines without one.
    if system::version("").is_err() {
        return;
    }
    let git = system::resolve_executable("");
    let repo = TestRepo::new();
    let remote_dir = repo.root.with_extension("remote.git");
    let remote = Repository::init_bare(&remote_dir).unwrap();
    repo.repo
        .remote("origin", remote_dir.to_str().unwrap())
        .unwrap();
    let branch = current_branch(&repo.repo);
    let head = repo.repo.head().unwrap().target().unwrap();

    let pushed = network::system_push(&repo.repo, &git, &branch);
    let fetched = network::system_fetch(&repo.repo, &git);
    let remote_head = remote
        .find_reference(&format!("refs/heads/{branch}"))
        .ok()
        .and_then(|r| r.target());
    let tracking = repo
        .repo
        .find_reference(&format!("refs/remotes/origin/{branch}"))
        .ok()
        .and_then(|r| r.target());
    let _ = std::fs::remove_dir_all(&remote_dir);

    assert_eq!(pushed, Ok(()));
    assert_eq!(fetched, Ok(()));
    assert_eq!(remote_head, Some(head));
    assert_eq!(tracking, Some(head));
}

#[test]
fn compare_lists_branch_commits_and_work_tree_changes_since_the_merge_base() {
    let repo = TestRepo::new();
    let base = current_branch(&repo.repo);
    checkout_branch(&repo.repo, "feature", true).unwrap();
    std::fs::write(repo.root.join("added.txt"), "one\ntwo\n").unwrap();
    stage(&repo.repo, &["added.txt".into()]).unwrap();
    commit(&repo.repo, "add file").unwrap();
    // Uncommitted work counts too, so the compared files stay editable in place.
    std::fs::write(repo.root.join("file.txt"), "changed\n").unwrap();

    let result = compare(repo.cwd(), "");
    assert_eq!(result.error, None);
    assert_eq!(result.base, base);
    assert_eq!(result.bases, vec![base.clone()]);
    assert_eq!(result.commits.len(), 1);
    assert_eq!(result.commits[0].message, "add file");
    let files: Vec<_> = result
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.status, f.added, f.deleted))
        .collect();
    assert_eq!(files, [("added.txt", 'A', 2, 0), ("file.txt", 'M', 1, 1)]);
    assert_eq!(result.line_changes["added.txt"].len(), 2);
    assert_eq!(
        result.line_changes["file.txt"][0].kind,
        GitLineKind::Modified
    );

    let text = compare::compare_file_diff(&repo.repo, &base, "file.txt", None).unwrap();
    assert!(text.contains("-original") && text.contains("+changed"));
    let state = view_diff(
        repo.cwd(),
        GitOp::ShowCompareDiff {
            base: base.clone(),
            path: "added.txt".into(),
            old_path: None,
        },
    );
    assert_eq!(
        state.diff.unwrap().0,
        compare_diff_title(&base, "added.txt")
    );
    assert_eq!(state.current_diff_path.as_deref(), Some("added.txt"));
}

#[test]
fn block_edits_stage_and_revert_single_changes() {
    let repo = TestRepo::new();
    std::fs::write(repo.root.join("file.txt"), "one\ntwo\nthree\n").unwrap();
    stage(&repo.repo, &["file.txt".into()]).unwrap();
    commit(&repo.repo, "three lines").unwrap();
    std::fs::write(repo.root.join("file.txt"), "ONE\ntwo\nTHREE\n").unwrap();

    // Stage only the first change.
    hunk::apply_block(
        repo.cwd(),
        &BlockEdit {
            path: "file.txt".into(),
            target: BlockTarget::Index,
            start: 1,
            expected: vec!["one".into()],
            replacement: vec!["ONE".into()],
        },
    )
    .unwrap();
    let staged = show_diff(&repo.repo, "file.txt", true);
    assert!(staged.contains("+ONE") && !staged.contains("+THREE"));

    // Revert the other one in the work tree.
    hunk::apply_block(
        repo.cwd(),
        &BlockEdit {
            path: "file.txt".into(),
            target: BlockTarget::WorkTree,
            start: 3,
            expected: vec!["THREE".into()],
            replacement: vec!["three".into()],
        },
    )
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(repo.root.join("file.txt")).unwrap(),
        "ONE\ntwo\nthree\n"
    );
    assert!(
        show_diff(&repo.repo, "file.txt", false)
            .lines()
            .all(|l| !l.starts_with('+'))
    );
}
