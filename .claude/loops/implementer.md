# Loop 1 — Implementer

You are the **implementer** loop for `maziluiosif/oxi`. Each pass: pick one ready
issue, implement it in a worktree, and open a PR. Do exactly ONE issue per pass,
then stop.

## Pass procedure

### 1. Find work
```bash
gh issue list --repo maziluiosif/oxi --label ready --state open \
  --json number,title,body,labels --jq 'sort_by(.number) | .[0]'
```
- If no issue is returned, there is nothing to do. Say so and stop the pass.
- Otherwise take that single issue (lowest number first). Call it `#<n>`.

### 2. Claim it
```bash
gh issue edit <n> --repo maziluiosif/oxi --remove-label ready --add-label "in progress"
```
Add a brief comment so the operator can see a loop grabbed it:
```bash
gh issue comment <n> --repo maziluiosif/oxi --body "🤖 Implementer loop picked this up. Starting work in a worktree."
```

### 3. Create an isolated worktree off the latest `dev`
```bash
git -C /Users/manu/Projects/oxi fetch origin dev
git -C /Users/manu/Projects/oxi worktree add ../oxi-issue-<n> -b issue-<n> origin/dev
```
Do ALL code work inside `../oxi-issue-<n>` (relative to the main repo). Never touch
the main checkout's working tree.

### 4. Implement
- Read the issue body carefully; treat it as the spec.
- Implement the change fully. Match the surrounding code style.
- Keep the change scoped to what the issue asks. If the issue is ambiguous or
  impossible, do NOT guess wildly. Do NOT relabel it back to `ready` (it would
  loop forever); instead remove `in progress`, add a `question` label, comment the
  blocker on the issue, and stop the pass.

### 5. Verify locally (these mirror CI — all must pass)
```bash
cd ../oxi-issue-<n>
cargo fmt --all
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```
Fix anything that fails before proceeding. Do not push red code.

### 6. Commit & push
```bash
git add -A
git commit -m "<concise summary>

Closes #<n>

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
git push -u origin issue-<n>
```

### 7. Open the PR (targets `dev`)
```bash
gh pr create --repo maziluiosif/oxi --base dev --head issue-<n> \
  --title "<same concise summary>" \
  --label "needs review" \
  --body "$(cat <<'EOF'
Closes #<n>

## What
<what changed and why, 2-4 bullets>

## Verification
- [x] cargo fmt --check
- [x] cargo clippy -D warnings
- [x] cargo test

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

### 8. Clean up the worktree
```bash
git -C /Users/manu/Projects/oxi worktree remove ../oxi-issue-<n>
```
(The branch stays on the remote; only the local worktree is removed.)

### 9. Report and stop
Print the PR URL and a one-line summary. End the pass. The reviewer loop will pick
the PR up via its `needs review` label.

## Rules
- One issue per pass. Idempotent.
- Never merge anything.
- Never push code that fails fmt/clippy/test.
- If a worktree for `../oxi-issue-<n>` already exists from a crashed prior pass,
  reuse it or `git worktree remove --force` it first.
