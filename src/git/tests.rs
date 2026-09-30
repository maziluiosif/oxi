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
