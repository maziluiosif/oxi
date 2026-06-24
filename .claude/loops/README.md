# Loop Engineering for oxi

Three autonomous Claude Code loops that take a tagged GitHub issue all the way to
a PR that's ready for *your* final review. You act as the **agentic operator**: you
write issues and do the final human review. The loops do the rest.

```
  YOU            create issue ──tag: ready─────────────┐
  ────                                                 │
  LOOP 1   implementer  ── picks `ready` issue          │
                        ── worktree → code → fmt/clippy/test
                        ── push branch → open PR (tag: needs review)
  LOOP 2   reviewer     ── reviews `needs review` PRs
                        ── leaves blocking inline comments (tag: changes requested)
                        ── re-reviews when new commits land
                        ── converged & clean → tag: ready for human review
  LOOP 3   fixer        ── fixes `changes requested` PRs
                        ── resolves addressed threads → tag back: needs review
                                                                  │
  YOU            review `ready for human review` PR ◄────────────┘
```

The reviewer ↔ fixer ping-pong (`needs review` ↔ `changes requested`) runs until
convergence, at which point the reviewer tags the PR `ready for human review`.

## Repo facts the loops rely on

- Repo: `maziluiosif/oxi`
- Base branch for all PRs: **`dev`** (the default branch)
- CI / required checks (must pass locally before any push):
  - `cargo fmt --all -- --check`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo test`

## Labels

| Label | Meaning |
|-------|---------|
| `ready` | Issue is ready for the implementer to pick up |
| `in progress` | Implementer is actively working it |
| `needs review` | PR is waiting for the reviewer loop |
| `changes requested` | Reviewer left blocking comments; fixer's turn |
| `ready for human review` | Loops converged; **your** turn |

## How to run

### Quick start (macOS)

The `start-loops.sh` launcher opens one Terminal window per loop, each starting a
Claude Code session already seeded with the right `/loop` prompt. Requires macOS
and the [`claude`](https://claude.com/claude-code) CLI on your `PATH`.

```bash
# self-paced loops (each loop picks its own cadence)
.claude/loops/start-loops.sh

# fixed cadence — every loop ticks on the given interval
.claude/loops/start-loops.sh 10m

# print what would launch without opening anything
.claude/loops/start-loops.sh --dry-run
```

That's it — three windows titled `oxi · implementer`, `oxi · reviewer`, and
`oxi · fixer` start up. Then feed the system (see below).

### Manual (any platform)

Or open **three** Claude Code terminals/sessions in this repo yourself, one per
loop, and run the matching `/loop` command in each. Pick an interval that suits you
(these examples are self-paced; you can also pass an interval like `/loop 10m ...`).

```
# Terminal 1 — implementer
/loop Read .claude/loops/implementer.md and execute exactly one full pass, then stop until the next tick.

# Terminal 2 — reviewer
/loop Read .claude/loops/reviewer.md and execute exactly one full pass, then stop until the next tick.

# Terminal 3 — fixer
/loop Read .claude/loops/fixer.md and execute exactly one full pass, then stop until the next tick.
```

To feed the system, create an issue and tag it `ready`:

```
gh issue create --title "Add X" --body "Detailed spec..." --label ready
```

Then watch the labels move. When a PR hits `ready for human review`, you take over.

## Safety notes

- Each loop is **idempotent per pass**: it picks one unit of work, does it, stops.
- Implementer works in a git **worktree** (`../oxi-issue-<n>`) so loops never
  collide on the working tree.
- Loops never merge to `dev` — only a human merges the final PR.
