# Subagent conversations

ACP subagents and Claude Workflow members have transcripts linked to the parent chat.
Use the compact **Agents** menu above the parent transcript or nested sidebar rows to open
them. Workflow branches start folded; their detail view groups member status in one list.
Delegation tools link only to their own child. The breadcrumb returns to the workflow or
parent chat; transcript widths match the normal conversation.
Opening a sub-chat reads its transcript; it does not launch another process or send a prompt.
The provider owns the child conversation and its shared workspace.

The view includes prompts, answers, reasoning/tool details, model, reported context, state,
and workflow phases/member status when available. Child tool identifiers and context usage
are isolated from the parent and from other children. Approvals use the parent's policy and
approval channel, including plan-mode restrictions. Background updates continue after the
parent's prompt ends without creating an empty assistant turn in the parent chat.

## Adapter contracts

- Oxi advertises `clientCapabilities.subagents: {}` and accepts the draft ACP RFD
  `subagent_update`, `session_message`, child `state_update` and ordinary updates addressed
  to announced children. Capability changes merge; `cancel: null` revokes cancellation.
- Claude's installed adapter also needs the documented AIR `asyncTasks` extension for
  background workflows. Oxi negotiates this for Claude, and accepts its corresponding
  `subagent_spawned` / `subagent_state_update` form as well. In this form the adapter currently
  grants no per-child controls. Do not infer cancel/message/close support from the provider's
  underlying SDK: only advertised controls appear.
- Codex uses the RFD contract and dynamically grants/revokes child cancellation. A stop
  targets that child session on the existing connection; it does not stop the parent.
- Workflow/task stop uses `_session/async_task/stop` only for tasks with `canStop: true`.
  Stopping a child waiting on approval cancels its pending approval and lets the parent proceed.
- The installed Claude adapter does **not** stream individual Workflow members over ACP.
  Oxi follows the current native session's workflow metadata and member JSONL files under
  `CLAUDE_CONFIG_DIR` (default `~/.claude`). Reads run off the UI/RPC threads, stay inside that
  session's directory, validate run/agent identifiers, ignore incomplete trailing lines and
  never execute scripts. Remote SSH reads happen on the server running the adapter.
  Poll interval: one second. Per-file read cap: 16 MiB per poll; a single JSONL record over that
  cap cannot be imported. Each file resumes at its last complete line.

## Persistence and frontends

Child conversations live in an additional `delegated_chats` record inside the parent's atomic
session save. Existing chats need no migration; older readers ignore this record. It does not
become part of the parent's model prompt/history or change the parent's transcript hash.
Reconnection replaces only histories actually replayed by the adapter; metadata-only replay
keeps the saved transcript. Restored running states are shown as saved/disconnected until
fresh provider updates arrive.

The headless/remote protocol adds `delegated` to `chat.get`, a `delegated` agent event, and
`subagent.cancel` (`root`, `chat`, `sessionId`, `child`, `task`). The desktop SSH client mirrors
these transcripts and controls. Existing editor/mobile clients continue receiving their
ordinary parent messages; they can ignore the additional fields. No new Android or Sublime
sub-chat screen is added in this change.

## Validation

The test suite includes an actual JSON-RPC subprocess fixture covering announcements during
setup, child permissions, cancellation while waiting for approval, an unaffected parent and
late child updates. Reducer/store tests cover nesting, tool-id isolation, capability revocation,
message deduplication and save/reload. Workflow reader tests cover partial lines, incremental
reads, terminal member failures, and path containment.

Visual gallery (offscreen rendering, no provider calls):

```sh
CARGO_INCREMENTAL=0 OXI_SUBAGENT_REVIEW=1 OXI_GALLERY=/tmp/oxi-subagents \
  cargo test --locked --lib render_gallery -- --ignored --nocapture
```

Optionally set `OXI_SUBAGENT_SAVED_ROOT` to a native Claude session directory to review real
saved workflow/member transcripts. This does not prove current live-provider behavior; a
fresh Claude/Codex run is a separate smoke test.
