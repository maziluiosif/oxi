# Contributing to oxi

`oxi` is a local desktop coding-agent chat app built in Rust with egui/eframe. See
[README.md](README.md) for what it does and how it's laid out; this file covers the
mechanics of sending a change.

## Requirements

- A stable Rust toolchain (`rustup toolchain install stable`) with the `rustfmt` and
  `clippy` components.
- A desktop environment supported by `eframe` (the app doesn't run headless).

## Building and running

```bash
cargo run --release
```

Debug builds work too (`cargo run`), but the release profile is closer to what CI/release
artifacts ship and is noticeably more responsive for UI work.

Published binaries are built with `cargo build --profile dist`: the release profile plus thin
LTO and a single codegen unit, about 11% smaller. It takes several minutes longer to build, so
it is kept out of the everyday `--release` loop.

Diagnostics go to `oxi.log` next to `settings.json` (Settings → About → Open log); set
`OXI_LOG=debug` for more detail. Panics land in `crash.log` in the same folder.

## Before opening a PR

CI (`.github/workflows/ci.yml`) runs these three checks on every push and PR; run them
locally first so you're not waiting on CI to find a formatting nit:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

`cargo clippy` is run with `-D warnings`, so any new lint warning fails CI — fix it or, if
it's a deliberate false positive, silence it locally with a `#[allow(...)]` and a short
comment explaining why.

CI also runs `cargo audit` against `Cargo.lock` to catch known RustSec advisories in
dependencies. If you add or bump a dependency and `cargo audit` flags it, either pick a
version without the advisory or (if there's genuinely no fix yet) leave a note in the PR
explaining why it's acceptable.

## Code layout

| Path | What lives there |
| --- | --- |
| `src/app/` | The egui app: `OxiApp` and its state (`state.rs`), one module per surface (composer, sidebar, conversation, editor in `file_explorer/`, git panel, settings) and the per-frame drains of background work. |
| `src/ui/` | Reusable widgets and painters: chrome, chat messages, the diff view, markdown. |
| `src/agent/` | Agent runs: provider loops (`openai.rs`, `anthropic.rs`, `codex_responses.rs`), tools, approvals, MCP and ACP clients. |
| `src/settings/`, `src/secrets.rs` | `settings.json` and the OS keychain. |
| `src/session_store/` | Chat transcripts on disk (JSONL). |
| `src/git/`, `src/git.rs` | libgit2 operations, snapshots of agent turns, hunk staging. |
| `src/compute/` | SSH connections and tunnels to remote model runtimes. |
| `src/router/` | Router (auto) provider: task classification, quotas, spend ledger. |
| `src/fsutil.rs` | Crash-safe file replacement for oxi's own files and the agent's file tools; prefer it over `fs::write` for app state. |
| `src/logging.rs` | `oxi.log` and `crash.log`. Use `log::warn!` / `log::error!` rather than `eprintln!`. |
| `src/os_open.rs` | Opening files and folders with the OS; never through a shell. |

A few conventions worth knowing before you dive in:

- Tool implementations live under `src/agent/tools/`; path-based tools must go through
  `paths::resolve_under_cwd`/`resolve_under_cwd_for_create` so they can't escape the
  workspace root — reuse those helpers rather than resolving paths by hand.
- Mutating tools (`bash`, `write`, `edit`, …) are gated by `src/agent/approval.rs`'s
  `ApprovalGate`; if you add a new mutating tool, classify it in
  `tools::tool_side_effect` rather than assuming it's safe to skip. Unknown names are
  treated as external and always ask.
- Async work runs on the shared runtime in `src/runtime.rs`; don't build another
  `tokio::runtime::Runtime`. Blocking calls made from async code (tool runs, approval
  waits) go through `spawn_blocking` or `runtime::block_in_place`.
- Secrets (provider API keys, OAuth tokens, SSH passwords) go through `src/secrets.rs`,
  which wraps the OS keychain. Don't add new plaintext-JSON credential storage — follow
  the pattern in `src/oauth/store.rs` or `src/compute/store.rs` instead.

## Tests

Most modules keep their tests in an inline `#[cfg(test)] mod tests` block next to the
code under test; a few larger areas (e.g. `src/agent/tools/tests.rs`) use a dedicated
file. Match whichever convention the file you're touching already uses.

A handful of tests are marked `#[ignore]` because they exercise the real OS keychain
(`src/secrets.rs`) and aren't safe to run unattended in CI/sandboxed environments. Run
them explicitly when touching that code:

```bash
cargo test -- --ignored
```

## Commit / PR conventions

- Keep PRs focused — one logical change per PR is easier to review and bisect than a
  bundle of unrelated fixes.
- To try a change before it is released, add the `build` label to its pull request (or run
  **Actions → Preview build** for any branch). macOS, Linux and Windows builds are attached
  to the run, and the pull request gets a comment linking them.
- To cut a release, run **Actions → Prepare release**. It opens (or refreshes) the
  `dev` → `master` pull request, titled with the version it will publish and carrying a
  preview of the release notes.
- Merging into `master` starts the release workflow. It generates release notes and a
  SemVer bump from commits since the latest `v*` tag, builds every supported platform,
  then commits `CHANGELOG.md`, `Cargo.toml`, and `Cargo.lock`, tags and publishes the
  release with a `SHA256SUMS` file, then opens a `master` → `dev` sync PR and
  auto-merges it after CI passes.
- CI runs on Linux (fmt, clippy, tests), macOS (clippy, tests) and Windows (clippy), plus
  `cargo audit`. A daily audit also opens an issue when a new RustSec advisory affects
  `dev`.
- Use Conventional Commit prefixes where possible (`feat:`, `fix:`, `security:`, and
  `type!:` for breaking changes). They provide deterministic release notes and version
  selection if the optional LLM changelog service is unavailable. You normally should
  not edit `CHANGELOG.md` or bump the crate version by hand.

## Licensing of contributions

oxi is released under the [MIT License](LICENSE). Contributions are accepted under the
same license.

### Sign your commits

By submitting a pull request you certify the
[Developer Certificate of Origin](https://developercertificate.org/): that you wrote the
contribution yourself, or otherwise have the right to submit it under the project's
license. Certify it by adding a `Signed-off-by` line to every commit, which `git commit -s`
adds for you:

```
Signed-off-by: Your Name <your.email@example.com>
```

The name and email in the sign-off must match the commit author.

### Grant

You keep the copyright to your contribution. Alongside the MIT license, you also grant the
maintainer a perpetual, worldwide, irrevocable, royalty-free right to use, reproduce,
modify, distribute and sublicense it, including under license terms that differ from the
MIT License.

This exists so the project can be relicensed or dual-licensed in the future without having
to track down every past contributor, which is a practical dead end once a project has any
real number of them. It does not let anyone take away what is already published: every
release made under the MIT License stays MIT, permanently, and anyone may keep using and
forking those releases under those terms.
