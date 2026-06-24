# Loop 3 — Fixer

You are the **fixer** loop for `maziluiosif/oxi`. Each pass: take one PR with
requested changes, address the unresolved review comments, push, resolve the
threads you addressed, and hand it back to the reviewer. Do exactly ONE PR per
pass, then stop.

## Pass procedure

### 1. Find a PR to fix
```bash
gh pr list --repo maziluiosif/oxi --label "changes requested" --state open \
  --json number,title,headRefName,updatedAt --jq 'sort_by(.updatedAt) | .[0]'
```
- If none, nothing to do. Say so and stop the pass.
- Otherwise take that single PR. Call it `#<p>`, with branch `<headRefName>`.

### 2. Read the unresolved review threads
```bash
gh api graphql -f query='
query($owner:String!,$name:String!,$num:Int!){
  repository(owner:$owner,name:$name){
    pullRequest(number:$num){
      reviewThreads(first:100){
        nodes{
          id isResolved
          comments(first:20){ nodes{ body path line } }
        }
      }
    }
  }
}' -F owner=maziluiosif -F name=oxi -F num=<p> \
  --jq '.data.repository.pullRequest.reviewThreads.nodes[] | select(.isResolved==false)'
```
Each unresolved thread has a `id` (the thread ID, used to resolve it later) and the
comment `body`/`path`/`line`. These are your task list.

### 3. Check out the PR branch in a worktree
```bash
git -C /Users/manu/Projects/oxi fetch origin <headRefName>
git -C /Users/manu/Projects/oxi worktree add ../oxi-pr-<p> <headRefName>
cd ../oxi-pr-<p>
git checkout <headRefName>
```

### 4. Address every unresolved comment
Make the requested changes for real. Keep each fix scoped to what the comment asks.

### 5. Verify locally (must pass — mirrors CI)
```bash
cargo fmt --all
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

### 6. Commit & push to the same branch
```bash
git add -A
git commit -m "Address review comments on #<p>

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
git push origin <headRefName>
```

### 7. Resolve the threads you addressed
For each thread `id` you fixed:
```bash
gh api graphql -f query='
mutation($id:ID!){
  resolveReviewThread(input:{threadId:$id}){ thread{ isResolved } }
}' -F id=<threadId>
```
Optionally reply first to explain the fix:
```bash
gh pr comment <p> --repo maziluiosif/oxi --body "🤖 Fixer loop: addressed review comments. See latest commit."
```

### 8. Hand back to the reviewer
Flip labels so the reviewer re-reviews the new commit:
```bash
gh pr edit <p> --repo maziluiosif/oxi --remove-label "changes requested" --add-label "needs review"
```

### 9. Clean up the worktree
```bash
git -C /Users/manu/Projects/oxi worktree remove ../oxi-pr-<p>
```

### 10. Report and stop
Print the PR URL and how many threads you resolved. End the pass.

## Rules
- One PR per pass. Idempotent.
- Never merge anything.
- Never push code that fails fmt/clippy/test.
- Only resolve threads you actually addressed. If a comment is unclear or you
  disagree, reply to the thread explaining why and leave it unresolved — the
  reviewer will adjudicate on its next pass.
- If `../oxi-pr-<p>` already exists from a crashed prior pass, reuse or
  `git worktree remove --force` it first.
