//! Moving an orchestrator worker's changes around (see `crate::orchestrator`): a snapshot of a
//! checkout's state for a worker to start from, uncommitted and untracked files included; the
//! worker's changes since then; and applying them to the main checkout as uncommitted changes.
//!
//! Everything goes through the `git` executable with a throwaway index (`GIT_INDEX_FILE`), so
//! neither the user's index nor the worker's is touched, applying included: a three-way merge
//! works on a copy of the user's index, and only the files change. Patches stay bytes from end
//! to end (files need not be UTF-8) and reach git on its standard input, never a temp file.

use std::ffi::OsStr;
use std::io::Write;
use std::path::{Path, PathBuf};

use super::system;

/// The changes of a worker beyond this size are cut when shown to the lead.
pub const MAX_DIFF_BYTES: usize = 200_000;

fn git_path() -> PathBuf {
    system::resolve_executable("")
}

/// Run git in `dir` (with index file `index`, and `input` on its standard input), returning
/// its standard output as bytes.
fn run_bytes(
    dir: &Path,
    args: &[impl AsRef<OsStr>],
    index: Option<&Path>,
    input: Option<&[u8]>,
) -> Result<Vec<u8>, String> {
    let mut cmd = system::command(&git_path(), dir, args);
    if let Some(index) = index {
        cmd.env("GIT_INDEX_FILE", index);
    }
    let not_run =
        |e: std::io::Error| format!("Could not run git: {e}. Install Git to use the orchestrator.");
    let output = match input {
        None => cmd.output().map_err(not_run)?,
        Some(input) => {
            cmd.stdin(std::process::Stdio::piped());
            let mut child = cmd.spawn().map_err(not_run)?;
            let stdin = child.stdin.take();
            // Written from another thread: git may fill its output pipes before reading all
            // of a large patch.
            let input = input.to_vec();
            let writer = std::thread::spawn(move || {
                if let Some(mut stdin) = stdin {
                    let _ = stdin.write_all(&input);
                }
            });
            let output = child.wait_with_output().map_err(not_run)?;
            let _ = writer.join();
            output
        }
    };
    if output.status.success() {
        Ok(output.stdout)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        Err([stderr.trim(), stdout.trim()]
            .into_iter()
            .find(|t| !t.is_empty())
            .unwrap_or("git failed")
            .to_owned())
    }
}

fn run(dir: &Path, args: &[impl AsRef<OsStr>], index: Option<&Path>) -> Result<String, String> {
    run_bytes(dir, args, index, None).map(|out| String::from_utf8_lossy(&out).into_owned())
}

/// A throwaway copy of `dir`'s index, removed on drop. Starting from the real index keeps git's
/// file stat cache, so `add -A` only reads changed files.
struct TempIndex(PathBuf);

impl TempIndex {
    fn of(dir: &Path) -> Result<Self, String> {
        let path = crate::fsutil::unique_temp_path(&std::env::temp_dir(), "oxi-index", "idx");
        let real = run(
            dir,
            &["rev-parse", "--path-format=absolute", "--git-path", "index"],
            None,
        )?;
        let real = PathBuf::from(real.trim());
        if real.is_file() {
            std::fs::copy(&real, &path).map_err(|e| e.to_string())?;
        }
        Ok(Self(path))
    }
}

impl Drop for TempIndex {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        let _ = std::fs::remove_file(self.0.with_extension("idx.lock"));
    }
}

/// The tree of everything in `dir`'s checkout right now: tracked changes and untracked files
/// that are not ignored.
fn working_tree(dir: &Path) -> Result<String, String> {
    let index = TempIndex::of(dir)?;
    run(dir, &["add", "-A", "--", "."], Some(&index.0))?;
    let top = run(dir, &["rev-parse", "--show-toplevel"], None)?;
    // `add -A -- .` from a subfolder only adds that subfolder; add the whole checkout.
    run(Path::new(top.trim()), &["add", "-A"], Some(&index.0))?;
    Ok(run(dir, &["write-tree"], Some(&index.0))?
        .trim()
        .to_string())
}

/// The id of the tree holding `dir`'s checkout as it is now; equal ids mean equal contents.
pub fn tree_of(dir: &Path) -> Result<String, String> {
    working_tree(dir)
}

/// A commit holding `root`'s checkout as it is now (on top of `HEAD`), without touching its
/// branch, index or files. Workers start from it, so they see the user's uncommitted work.
pub fn snapshot(root: &Path) -> Result<String, String> {
    let tree = working_tree(root)?;
    let head = run(root, &["rev-parse", "--verify", "-q", "HEAD"], None)
        .ok()
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty());
    let mut args = vec![
        "commit-tree".to_string(),
        tree,
        "-m".into(),
        "oxi: worker base".into(),
    ];
    if let Some(head) = head {
        args.push("-p".into());
        args.push(head);
    }
    // commit-tree needs an identity; repositories without one still get a snapshot.
    let mut cmd = system::command(&git_path(), root, &args);
    cmd.env("GIT_AUTHOR_NAME", "oxi")
        .env("GIT_AUTHOR_EMAIL", "oxi@localhost")
        .env("GIT_COMMITTER_NAME", "oxi")
        .env("GIT_COMMITTER_EMAIL", "oxi@localhost");
    let output = cmd
        .output()
        .map_err(|e| format!("Could not run git: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// What changed in `dir`'s checkout since commit `base`, for reading: a `--stat` summary, or the
/// full patch (bytes that are not UTF-8 are shown as replacement characters; [`patch_since`]
/// keeps them).
pub fn diff_since(dir: &Path, base: &str, stat: bool) -> Result<String, String> {
    let tree = working_tree(dir)?;
    let mut args = vec!["diff", "--no-color", "--no-ext-diff", "--find-renames"];
    args.push(if stat { "--stat=120" } else { "--binary" });
    args.extend([base, tree.as_str()]);
    run(dir, &args, None)
}

/// A checkout's changes since a commit, byte for byte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Patch {
    pub bytes: Vec<u8>,
    /// The tree the changes lead to (see [`tree_of`]).
    pub tree: String,
}

/// The changes made in `dir`'s checkout since commit `base`, as an exact binary patch.
pub fn patch_since(dir: &Path, base: &str) -> Result<Patch, String> {
    let tree = working_tree(dir)?;
    let bytes = run_bytes(
        dir,
        &[
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--find-renames",
            "--binary",
            base,
            tree.as_str(),
        ],
        None,
        None,
    )?;
    Ok(Patch { bytes, tree })
}

/// [`diff_since`]'s `--stat` reduced to its last line, e.g. "3 files changed, 40 insertions(+)".
pub fn short_stat(dir: &Path, base: &str) -> Option<String> {
    let tree = working_tree(dir).ok()?;
    let out = run(dir, &["diff", "--shortstat", base, tree.as_str()], None).ok()?;
    Some(out.trim().to_string()).filter(|s| !s.is_empty())
}

/// Paths with unresolved conflicts in `dir`'s index (or in index file `index`).
fn unmerged(dir: &Path, index: Option<&Path>) -> Result<Vec<String>, String> {
    let out = run_bytes(dir, &["ls-files", "-u", "-z"], index, None)?;
    let mut paths: Vec<String> = Vec::new();
    for entry in out.split(|b| *b == 0).filter(|e| !e.is_empty()) {
        // "<mode> <object> <stage>\t<path>"
        let Some(tab) = entry.iter().position(|b| *b == b'\t') else {
            continue;
        };
        let path = String::from_utf8_lossy(&entry[tab + 1..]).into_owned();
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    Ok(paths)
}

/// The files a patch touches, from `git apply --numstat -z` (renames give both names).
fn touched_files(numstat: &[u8]) -> Vec<String> {
    let mut files = Vec::new();
    let mut tokens = numstat.split(|b| *b == 0);
    while let Some(token) = tokens.next() {
        let token = token.strip_prefix(b"\n").unwrap_or(token);
        if token.is_empty() {
            continue;
        }
        // "<added>\t<deleted>\t<path>", or "<added>\t<deleted>\t" then the old and new paths.
        let mut parts = token.splitn(3, |b| *b == b'\t');
        let (Some(_), Some(_), Some(path)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let mut push = |p: &[u8]| {
            let p = String::from_utf8_lossy(p).into_owned();
            if !p.is_empty() && !files.contains(&p) {
                files.push(p);
            }
        };
        if path.is_empty() {
            for p in [tokens.next(), tokens.next()].into_iter().flatten() {
                push(p);
            }
        } else {
            push(path);
        }
    }
    files
}

/// Result of [`apply_onto`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Applied {
    /// Files the changes touched.
    pub files: Vec<String>,
    /// Files left with conflict markers.
    pub conflicts: Vec<String>,
    /// Conflicting files that got no conflict markers (binary files, a deletion against an
    /// edit): the workspace keeps its version, the worker's is only in its work tree.
    pub unresolved: Vec<String>,
    /// The worker's tree that was applied (see [`tree_of`]): its work tree may only go while it
    /// still holds exactly this.
    pub tree: String,
}

/// Apply the changes made in the checkout at `from` since `base` to the checkout at `onto`, as
/// uncommitted changes. When they do not apply cleanly (the main checkout changed the same
/// lines meanwhile), a three-way merge leaves conflict markers in the files it could not merge.
/// The user's index is never changed. A checkout with unresolved conflicts is refused, and when
/// the changes cannot be applied at all nothing is changed and an error comes back.
pub fn apply_onto(onto: &Path, from: &Path, base: &str) -> Result<Applied, String> {
    // Patch paths are relative to the repository's top folder.
    let top = run(onto, &["rev-parse", "--show-toplevel"], None)?;
    let onto = Path::new(top.trim());
    let pending = unmerged(onto, None)?;
    if !pending.is_empty() {
        return Err(format!(
            "The workspace has unresolved conflicts ({}); resolve them before merging a worker.",
            pending.join(", ")
        ));
    }
    let patch = patch_since(from, base)?;
    let mut applied = if patch.bytes.iter().all(u8::is_ascii_whitespace) {
        Applied::default()
    } else {
        apply_patch(onto, &patch.bytes)?
    };
    applied.tree = patch.tree;
    Ok(applied)
}

fn apply_patch(onto: &Path, patch: &[u8]) -> Result<Applied, String> {
    let numstat = run_bytes(onto, &["apply", "--numstat", "-z", "-"], None, Some(patch))?;
    let files = touched_files(&numstat);
    // Plain `git apply` is all or nothing, and leaves the index alone.
    if run_bytes(onto, &["apply", "--binary", "-"], None, Some(patch)).is_ok() {
        return Ok(Applied {
            files,
            ..Default::default()
        });
    }
    // `--3way` works on an index, which must match the files it touches: a copy of the user's,
    // with the touched files staged as they are now. The user's own index stays as it was.
    let index = TempIndex::of(onto)?;
    let (present, absent): (Vec<&String>, Vec<&String>) =
        files.iter().partition(|f| onto.join(f).exists());
    if !present.is_empty() {
        let mut args: Vec<&str> = vec!["add", "-f", "--"];
        args.extend(present.iter().map(|f| f.as_str()));
        run(onto, &args, Some(&index.0))?;
    }
    if !absent.is_empty() {
        let mut args: Vec<&str> = vec!["rm", "--cached", "-q", "--ignore-unmatch", "--"];
        args.extend(absent.iter().map(|f| f.as_str()));
        run(onto, &args, Some(&index.0))?;
    }
    // `--3way` fails when some files conflict, after applying the rest; when the patch cannot
    // be applied at all it changes nothing.
    let three_way = run_bytes(
        onto,
        &["apply", "--3way", "--binary", "-"],
        Some(&index.0),
        Some(patch),
    );
    // The checkout had no conflicts before, so every one is this patch's.
    let conflicts = unmerged(onto, Some(&index.0)).unwrap_or_default();
    if let Err(error) = three_way
        && conflicts.is_empty()
    {
        return Err(format!("The changes do not apply: {error}"));
    }
    let (conflicts, unresolved): (Vec<String>, Vec<String>) = conflicts
        .into_iter()
        .partition(|f| has_conflict_markers(&onto.join(f)));
    Ok(Applied {
        files,
        conflicts,
        unresolved,
        tree: String::new(),
    })
}

fn has_conflict_markers(path: &Path) -> bool {
    std::fs::read(path).is_ok_and(|bytes| {
        bytes.windows(8).any(|w| w == b"<<<<<<< ") && bytes.windows(8).any(|w| w == b">>>>>>> ")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempRepo(PathBuf);

    impl Drop for TempRepo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn git(dir: &Path, args: &[&str]) {
        run(dir, args, None).unwrap();
    }

    fn repo() -> TempRepo {
        let root = crate::fsutil::unique_temp_path(&std::env::temp_dir(), "oxi-handoff", "repo");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q"]);
        git(&root, &["config", "user.name", "Oxi Test"]);
        git(&root, &["config", "user.email", "oxi@example.com"]);
        git(&root, &["config", "core.autocrlf", "false"]);
        std::fs::write(root.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-q", "-m", "init"]);
        TempRepo(root)
    }

    #[test]
    fn a_worker_copy_starts_from_uncommitted_work_and_its_changes_come_back_unstaged() {
        let main = repo();
        // The user's uncommitted edit and a new file.
        std::fs::write(main.0.join("a.txt"), "one\ntwo\nthree\nfour\n").unwrap();
        std::fs::write(main.0.join("notes.md"), "draft\n").unwrap();
        let base = snapshot(&main.0).unwrap();
        git(&main.0, &["status", "--short"]);
        assert!(
            run(&main.0, &["diff", "--cached", "--name-only"], None)
                .unwrap()
                .is_empty(),
            "the user's index is untouched"
        );

        // The worker's copy, as a linked work tree at the snapshot.
        let wt = TempRepo(crate::fsutil::unique_temp_path(
            &std::env::temp_dir(),
            "oxi-handoff",
            "wt",
        ));
        let wt_arg = wt.0.to_string_lossy().into_owned();
        git(
            &main.0,
            &["worktree", "add", "-q", "--detach", &wt_arg, &base],
        );
        assert_eq!(
            std::fs::read_to_string(wt.0.join("notes.md")).unwrap(),
            "draft\n"
        );
        std::fs::write(wt.0.join("a.txt"), "ONE\ntwo\nthree\nfour\n").unwrap();
        std::fs::write(wt.0.join("new.rs"), "fn main() {}\n").unwrap();
        assert!(
            short_stat(&wt.0, &base)
                .unwrap()
                .starts_with("2 files changed")
        );

        // Meanwhile the user edits another line of the same file.
        std::fs::write(main.0.join("a.txt"), "one\ntwo\nTHREE\nfour\n").unwrap();
        let applied = apply_onto(&main.0, &wt.0, &base).unwrap();
        assert!(applied.conflicts.is_empty(), "{applied:?}");
        assert_eq!(applied.files.len(), 2, "{applied:?}");
        assert_eq!(
            std::fs::read_to_string(main.0.join("a.txt")).unwrap(),
            "ONE\ntwo\nTHREE\nfour\n"
        );
        assert!(main.0.join("new.rs").is_file());
        assert!(
            run(&main.0, &["diff", "--cached", "--name-only"], None)
                .unwrap()
                .is_empty(),
            "the result stays unstaged"
        );
        let _ = run(&main.0, &["worktree", "remove", "--force", &wt_arg], None);
    }

    #[test]
    fn clashing_changes_leave_conflict_markers() {
        let main = repo();
        let base = snapshot(&main.0).unwrap();
        let wt = TempRepo(crate::fsutil::unique_temp_path(
            &std::env::temp_dir(),
            "oxi-handoff",
            "wt",
        ));
        let wt_arg = wt.0.to_string_lossy().into_owned();
        git(
            &main.0,
            &["worktree", "add", "-q", "--detach", &wt_arg, &base],
        );
        std::fs::write(wt.0.join("a.txt"), "one\nworker\nthree\n").unwrap();
        std::fs::write(main.0.join("a.txt"), "one\nuser\nthree\n").unwrap();
        let applied = apply_onto(&main.0, &wt.0, &base).unwrap();
        assert_eq!(applied.conflicts, vec!["a.txt".to_string()]);
        let text = std::fs::read_to_string(main.0.join("a.txt")).unwrap();
        assert!(
            text.contains("<<<<<<<") && text.contains("worker"),
            "{text}"
        );
        assert!(applied.unresolved.is_empty());
        assert!(
            run(&main.0, &["diff", "--cached", "--name-only"], None)
                .unwrap()
                .is_empty(),
            "the conflict stays out of the user's index"
        );
        let _ = run(&main.0, &["worktree", "remove", "--force", &wt_arg], None);
    }

    /// A linked work tree of `main` at `base`, removed on drop.
    struct Worker<'a> {
        main: &'a Path,
        dir: TempRepo,
    }

    impl Drop for Worker<'_> {
        fn drop(&mut self) {
            let arg = self.dir.0.to_string_lossy().into_owned();
            let _ = run(self.main, &["worktree", "remove", "--force", &arg], None);
        }
    }

    fn worker<'a>(main: &'a TempRepo, base: &str) -> Worker<'a> {
        let dir = TempRepo(crate::fsutil::unique_temp_path(
            &std::env::temp_dir(),
            "oxi-handoff",
            "wt",
        ));
        let arg = dir.0.to_string_lossy().into_owned();
        git(&main.0, &["worktree", "add", "-q", "--detach", &arg, base]);
        Worker { main: &main.0, dir }
    }

    fn write(path: PathBuf, bytes: &[u8]) {
        crate::fsutil::write_atomic(&path, bytes).unwrap();
    }

    fn staged(dir: &Path, path: &str) -> Vec<u8> {
        run_bytes(dir, &["show", &format!(":{path}")], None, None).unwrap()
    }

    #[test]
    fn a_three_way_merge_keeps_what_the_user_staged() {
        let main = repo();
        let base = snapshot(&main.0).unwrap();
        let wt = worker(&main, &base);
        write(wt.dir.0.join("a.txt"), b"ONE\ntwo\nthree\n");
        // The user staged one edit of the same file and kept another unstaged.
        write(main.0.join("a.txt"), b"one\ntwo\nstaged\n");
        git(&main.0, &["add", "a.txt"]);
        write(main.0.join("a.txt"), b"one\ntwo\nstaged\nunstaged\n");
        // A text that does not apply as is (the context moved), so the merge is three-way.
        write(wt.dir.0.join("a.txt"), b"one\nworker\nthree\n");
        write(main.0.join("a.txt"), b"one\nuser\nstaged\nunstaged\n");
        let index_before = staged(&main.0, "a.txt");
        let applied = apply_onto(&main.0, &wt.dir.0, &base).unwrap();
        assert_eq!(applied.conflicts, vec!["a.txt".to_string()], "{applied:?}");
        assert_eq!(
            staged(&main.0, "a.txt"),
            index_before,
            "the staged version stays"
        );
        assert_eq!(index_before, b"one\ntwo\nstaged\n");
        assert!(
            run(&main.0, &["ls-files", "-u"], None).unwrap().is_empty(),
            "no conflict stages in the user's index"
        );
        assert!(!applied.tree.is_empty());
        assert_eq!(tree_of(&wt.dir.0).unwrap(), applied.tree);
    }

    #[test]
    fn a_clean_merge_keeps_a_partially_staged_file_s_index_entry() {
        let main = repo();
        write(main.0.join("b.txt"), b"1\n2\n3\n4\n5\n6\n7\n8\n9\n");
        git(&main.0, &["add", "b.txt"]);
        git(&main.0, &["commit", "-q", "-m", "b"]);
        let base = snapshot(&main.0).unwrap();
        let wt = worker(&main, &base);
        write(wt.dir.0.join("b.txt"), b"1\n2\n3\n4\n5\n6\n7\n8\nnine\n");
        write(main.0.join("b.txt"), b"one\n2\n3\n4\n5\n6\n7\n8\n9\n");
        git(&main.0, &["add", "b.txt"]);
        write(main.0.join("b.txt"), b"one\n2\n3\nfour\n5\n6\n7\n8\n9\n");
        let applied = apply_onto(&main.0, &wt.dir.0, &base).unwrap();
        assert!(applied.conflicts.is_empty(), "{applied:?}");
        assert_eq!(
            std::fs::read(main.0.join("b.txt")).unwrap(),
            b"one\n2\n3\nfour\n5\n6\n7\n8\nnine\n"
        );
        assert_eq!(staged(&main.0, "b.txt"), b"one\n2\n3\n4\n5\n6\n7\n8\n9\n");
    }

    #[test]
    fn a_workspace_with_unresolved_conflicts_is_refused() {
        let main = repo();
        write(main.0.join("other.txt"), b"base\n");
        git(&main.0, &["add", "other.txt"]);
        git(&main.0, &["commit", "-q", "-m", "other"]);
        git(&main.0, &["checkout", "-q", "-b", "side"]);
        write(main.0.join("other.txt"), b"side\n");
        git(&main.0, &["commit", "-q", "-am", "side"]);
        git(&main.0, &["checkout", "-q", "-"]);
        write(main.0.join("other.txt"), b"main\n");
        git(&main.0, &["commit", "-q", "-am", "main"]);
        assert!(run(&main.0, &["merge", "-q", "side"], None).is_err());
        let base = snapshot(&main.0).unwrap();
        let wt = worker(&main, &base);
        write(wt.dir.0.join("a.txt"), b"changed\n");
        let before = std::fs::read(main.0.join("a.txt")).unwrap();
        let err = apply_onto(&main.0, &wt.dir.0, &base).unwrap_err();
        assert!(err.contains("unresolved conflicts (other.txt)"), "{err}");
        assert_eq!(std::fs::read(main.0.join("a.txt")).unwrap(), before);
    }

    #[test]
    fn a_binary_conflict_is_reported_unresolved_and_the_worker_s_version_kept() {
        let main = repo();
        write(main.0.join("img.bin"), &[0u8, 1, 2, 3, 0, 255]);
        git(&main.0, &["add", "img.bin"]);
        git(&main.0, &["commit", "-q", "-m", "bin"]);
        let base = snapshot(&main.0).unwrap();
        let wt = worker(&main, &base);
        write(wt.dir.0.join("img.bin"), &[0u8, 9, 9, 9, 0, 255]);
        write(wt.dir.0.join("a.txt"), b"one\ntwo\nthree\nworker\n");
        write(main.0.join("img.bin"), &[0u8, 7, 7, 7, 0, 255]);
        match apply_onto(&main.0, &wt.dir.0, &base) {
            Ok(applied) => {
                assert_eq!(
                    applied.unresolved,
                    vec!["img.bin".to_string()],
                    "{applied:?}"
                );
                assert!(applied.conflicts.is_empty(), "{applied:?}");
            }
            // Nothing applied is fine too, as long as nothing was lost.
            Err(e) => assert!(e.contains("do not apply"), "{e}"),
        }
        assert_eq!(
            std::fs::read(main.0.join("img.bin")).unwrap(),
            [0u8, 7, 7, 7, 0, 255],
            "the user's version stays"
        );
        assert_eq!(
            std::fs::read(wt.dir.0.join("img.bin")).unwrap(),
            [0u8, 9, 9, 9, 0, 255],
            "the worker's version stays in its work tree"
        );
        assert!(run(&main.0, &["ls-files", "-u"], None).unwrap().is_empty());
    }

    #[test]
    fn patches_keep_bytes_that_are_not_utf8() {
        let main = repo();
        write(main.0.join("latin1.txt"), b"caf\xe9\nna\xefve\n");
        git(&main.0, &["add", "latin1.txt"]);
        git(&main.0, &["commit", "-q", "-m", "latin1"]);
        let base = snapshot(&main.0).unwrap();
        let wt = worker(&main, &base);
        write(
            wt.dir.0.join("latin1.txt"),
            b"caf\xe9\nna\xefve\nd\xe9j\xe0 vu\n",
        );
        let patch = patch_since(&wt.dir.0, &base).unwrap();
        assert!(
            patch.bytes.windows(2).any(|w| w == b"d\xe9"),
            "raw bytes in the patch"
        );
        let applied = apply_onto(&main.0, &wt.dir.0, &base).unwrap();
        assert_eq!(applied.files, vec!["latin1.txt".to_string()]);
        assert_eq!(
            std::fs::read(main.0.join("latin1.txt")).unwrap(),
            b"caf\xe9\nna\xefve\nd\xe9j\xe0 vu\n"
        );
    }

    #[test]
    fn numstat_lists_both_names_of_a_rename() {
        assert_eq!(
            touched_files(b"1\t0\ta.txt\x000\t0\t\x00old.rs\x00new.rs\x00"),
            vec!["a.txt".to_string(), "old.rs".into(), "new.rs".into()]
        );
    }
}
