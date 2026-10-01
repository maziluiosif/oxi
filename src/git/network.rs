//! Git remote authentication and fetch/pull/push integration.
//!
//! Network operations use libgit2 with the stored GitHub token by default. When
//! `AppSettings::git_use_system_cli` is on, fetch and push run the installed `git` executable
//! instead (see `system.rs`); merging after a fetch always stays in libgit2.

use git2::{
    BranchType, Cred, CredentialType, FetchOptions, PushOptions, RemoteCallbacks, Repository,
    ResetType, build::CheckoutBuilder,
};

use std::path::{Path, PathBuf};

use super::{author_signature, current_branch, err, repo_root, system};

/// The `git` executable to use for network operations, or `None` for libgit2 + token auth.
fn system_git() -> Option<PathBuf> {
    let settings = crate::settings::AppSettings::load();
    settings
        .git_use_system_cli
        .then(|| system::resolve_executable(&settings.git_executable))
}

fn github_credentials() -> (String, String) {
    let settings = crate::settings::AppSettings::load();
    let secrets = crate::secrets::load_unified();
    (
        settings.github_username.trim().to_owned(),
        secrets.github_token.trim().to_owned(),
    )
}

fn remote_callbacks() -> RemoteCallbacks<'static> {
    let (configured_username, token) = github_credentials();
    let mut callbacks = RemoteCallbacks::new();
    callbacks.credentials(move |url, username_url, allowed| {
        // GitHub accepts a PAT as the HTTP password and requires a non-empty username. Use the
        // configured account name when available; otherwise derive it from an HTTPS GitHub URL.
        let url_username = url::Url::parse(url)
            .ok()
            .map(|u| u.username().to_owned())
            .filter(|u| !u.is_empty());
        let username = if !configured_username.is_empty() {
            configured_username.as_str()
        } else {
            username_url
                .or(url_username.as_deref())
                .unwrap_or("x-access-token")
        };
        if allowed.contains(CredentialType::USER_PASS_PLAINTEXT) && !token.is_empty() {
            return Cred::userpass_plaintext(username, &token);
        }
        if allowed.contains(CredentialType::SSH_KEY) {
            return Cred::ssh_key_from_agent(username_url.unwrap_or("git"));
        }
        if allowed.contains(CredentialType::USERNAME) {
            return Cred::username(username);
        }
        Cred::default()
    });
    callbacks
}

fn remote_url(repo: &Repository, name: &str) -> String {
    repo.find_remote(name)
        .ok()
        .and_then(|r| r.url().ok().map(str::to_owned))
        .unwrap_or_default()
}

fn remote_error(repo: &Repository, remote: &str, operation: &str, error: git2::Error) -> String {
    let url = remote_url(repo, remote);
    let is_github_https =
        url.starts_with("https://github.com/") || url.starts_with("http://github.com/");
    let is_403 = error.code() == git2::ErrorCode::Auth || error.message().contains("403");
    if is_github_https && is_403 {
        let (_, token) = github_credentials();
        if token.is_empty() {
            return format!(
                "GitHub rejected {operation}: no token is configured. Open Settings → GitHub and save a personal access token with repository Contents: Read and write permission."
            );
        }
        return format!(
            "GitHub rejected {operation} with HTTP 403. Authentication reached GitHub, but this token cannot write to the repository. For a fine-grained token, grant access to this repository and Repository permissions → Contents: Read and write. For an organization repository, also authorize the token for SSO and check that the organization approved it. Remote: {url}"
        );
    }
    format!("{operation} failed for {url}: {error}")
}

pub(super) fn system_fetch(repo: &Repository, git: &Path) -> Result<(), String> {
    let output = system::run(git, repo_root(repo)?, &system::fetch_args())?;
    if output.success {
        Ok(())
    } else {
        Err(format!(
            "fetch failed for {}: {}",
            remote_url(repo, "origin"),
            output.failure_message()
        ))
    }
}

pub(super) fn fetch(repo: &Repository) -> Result<(), String> {
    if let Some(git) = system_git() {
        return system_fetch(repo, &git);
    }
    let mut remote = repo.find_remote("origin").map_err(err)?;
    let mut options = FetchOptions::new();
    options.remote_callbacks(remote_callbacks());
    remote
        .fetch(&[] as &[&str], Some(&mut options), None)
        .map_err(|e| remote_error(repo, "origin", "fetch", e))
}

/// Integrate the current branch's upstream after a fetch. Fast-forwards when possible and
/// creates a regular two-parent merge commit when local and remote histories diverged.
///
/// A dirty worktree is rejected before integration. If libgit2 detects merge conflicts, the
/// attempted merge is rolled back to the original HEAD so a network button can never leave the
/// user's repository half-merged.
fn integrate_upstream(repo: &Repository) -> Result<(), String> {
    let branch = current_branch(repo);
    if branch.is_empty() || branch == "HEAD" {
        return Err("Cannot pull while HEAD is detached".into());
    }
    let local = repo.find_branch(&branch, BranchType::Local).map_err(err)?;
    let upstream = local
        .upstream()
        .map_err(|_| "The current branch has no upstream".to_string())?;
    let upstream_name = upstream
        .name()
        .ok()
        .flatten()
        .unwrap_or("upstream")
        .to_owned();
    let upstream_ref = upstream.get();
    let upstream_oid = upstream_ref.target().ok_or("Upstream has no target")?;
    let annotated = repo
        .reference_to_annotated_commit(upstream_ref)
        .map_err(err)?;
    let (analysis, _) = repo.merge_analysis(&[&annotated]).map_err(err)?;
    if analysis.is_up_to_date() {
        return Ok(());
    }

    let refname = format!("refs/heads/{branch}");
    if analysis.is_fast_forward() {
        // Update the worktree to the fetched commit *before* moving the branch reference, using a
        // safe checkout. While HEAD still points at the old commit, libgit2 uses it as the merge
        // base: files the fast-forward does not touch keep their local modifications (matching
        // `git pull`, which does not require a clean tree), and a locally-modified file that the
        // fast-forward would overwrite aborts the checkout instead of being clobbered. Only once
        // the worktree is in place do we advance the reference, so an unrelated dirty file never
        // blocks the pull and a relevant one is never lost.
        //
        // (Moving the reference first would force a checkout: HEAD would already match the target,
        // so a safe checkout would treat the still-old index as staged changes and preserve it
        // instead of completing the fast-forward — which is why this must run in this order.)
        let target = repo.find_object(upstream_oid, None).map_err(err)?;
        let mut checkout = CheckoutBuilder::new();
        checkout.safe();
        repo.checkout_tree(&target, Some(&mut checkout))
            .map_err(|e| {
                format!(
                    "Cannot pull: your local changes to files updated by {upstream_name} would be overwritten. Commit, stash, or discard them first. ({e})"
                )
            })?;
        repo.reference(&refname, upstream_oid, true, "pull: fast-forward")
            .map_err(err)?;
        return repo.set_head(&refname).map_err(err);
    }

    let original = repo.head().and_then(|h| h.peel_to_commit()).map_err(err)?;
    let remote_commit = repo.find_commit(upstream_oid).map_err(err)?;
    // Resolve identity before changing index/worktree, so a missing author config cannot leave a
    // merge in progress.
    let signature = author_signature(repo)?;
    let mut checkout = CheckoutBuilder::new();
    checkout.safe();
    if let Err(error) = repo.merge(&[&annotated], None, Some(&mut checkout)) {
        // A safe merge checkout aborts if a locally-modified file would be overwritten. Nothing
        // was committed, but libgit2 may have recorded merge state; clear it so the repository is
        // left exactly as it was found.
        let _ = repo.cleanup_state();
        return Err(format!(
            "Cannot pull: your local changes to files updated by {upstream_name} would be overwritten. Commit, stash, or discard them first. ({error})"
        ));
    }

    let mut index = repo.index().map_err(err)?;
    if index.has_conflicts() {
        // Restore both index and worktree. Conflict resolution UI is intentionally not forced on
        // the user as a side effect of pressing Push.
        let object = original.as_object();
        let rollback = repo.reset(object, ResetType::Hard, None).map_err(err);
        let _ = repo.cleanup_state();
        rollback?;
        return Err(format!(
            "Local and {upstream_name} have conflicting changes. Automatic sync was rolled back safely; merge the branches manually or resolve the conflicting commits before pushing."
        ));
    }

    let merge_result = (|| -> Result<(), String> {
        let tree_oid = index.write_tree().map_err(err)?;
        let tree = repo.find_tree(tree_oid).map_err(err)?;
        let message = format!("Merge {upstream_name} into {branch}");
        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            &message,
            &tree,
            &[&original, &remote_commit],
        )
        .map_err(err)?;
        repo.cleanup_state().map_err(err)
    })();
    if let Err(error) = merge_result {
        let rollback = repo
            .reset(original.as_object(), ResetType::Hard, None)
            .map_err(err);
        let _ = repo.cleanup_state();
        rollback?;
        return Err(format!(
            "Automatic sync failed and was rolled back safely: {error}"
        ));
    }
    Ok(())
}

pub(super) fn pull(repo: &Repository) -> Result<(), String> {
    fetch(repo)?;
    integrate_upstream(repo)
}

fn push_once(repo: &Repository, branch: &str) -> Result<(), git2::Error> {
    let mut remote = repo.find_remote("origin")?;
    let mut options = PushOptions::new();
    options.remote_callbacks(remote_callbacks());
    let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
    remote.push(&[&refspec], Some(&mut options))
}

fn libgit2_push(repo: &Repository, branch: &str) -> Result<(), String> {
    match push_once(repo, branch) {
        Ok(()) => Ok(()),
        Err(error) if error.code() == git2::ErrorCode::NotFastForward => {
            // Match the useful part of `git push`: refresh the remote tracking branch, safely
            // integrate it, then retry. Never force-push and never overwrite remote history.
            fetch(repo)?;
            integrate_upstream(repo)?;
            push_once(repo, branch).map_err(|e| remote_error(repo, "origin", "push", e))
        }
        Err(error) => Err(remote_error(repo, "origin", "push", error)),
    }
}

/// System Git counterpart of [`libgit2_push`], with the same fetch-integrate-retry behaviour on
/// a non-fast-forward rejection.
pub(super) fn system_push(repo: &Repository, git: &Path, branch: &str) -> Result<(), String> {
    let root = repo_root(repo)?;
    let args = system::push_args(branch);
    let output = system::run(git, root, &args)?;
    if output.success {
        return Ok(());
    }
    if !system::push_rejected_non_fast_forward(&output.stdout) {
        return Err(format!(
            "push failed for {}: {}",
            remote_url(repo, "origin"),
            output.failure_message()
        ));
    }
    system_fetch(repo, git)?;
    integrate_upstream(repo)?;
    let retry = system::run(git, root, &args)?;
    if retry.success {
        Ok(())
    } else {
        Err(format!(
            "push failed for {}: {}",
            remote_url(repo, "origin"),
            retry.failure_message()
        ))
    }
}

pub(super) fn push(repo: &Repository) -> Result<(), String> {
    let branch = current_branch(repo);
    if branch.is_empty() || branch == "HEAD" {
        return Err("Cannot push while HEAD is detached".into());
    }

    match system_git() {
        Some(git) => system_push(repo, &git, &branch)?,
        None => libgit2_push(repo, &branch)?,
    }

    let mut local = repo.find_branch(&branch, BranchType::Local).map_err(err)?;
    if local.upstream().is_err() {
        local
            .set_upstream(Some(&format!("origin/{branch}")))
            .map_err(err)?;
    }
    Ok(())
}
