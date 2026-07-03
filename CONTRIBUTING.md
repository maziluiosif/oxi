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

Start from the [Architecture](README.md#architecture) section of the README for the
module map. A few conventions worth knowing before you dive in:

- Tool implementations live under `src/agent/tools/`; path-based tools must go through
  `paths::resolve_under_cwd`/`resolve_under_cwd_for_create` so they can't escape the
  workspace root — reuse those helpers rather than resolving paths by hand.
- Mutating tools (`bash`, `write`, `edit`) are gated by `src/agent/approval.rs`'s
  `ApprovalGate`; if you add a new mutating tool, register it in
  `tool_requires_approval` rather than assuming it's safe to skip.
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
- The release process (`.github/workflows/release.yml`) generates `CHANGELOG.md`
  automatically from commit history on `master`; you don't need to hand-edit the
  changelog yourself.
