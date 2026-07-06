# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.11.1] - 2026-07-06

### Changed
- Updated README with refreshed screenshots


## [0.11.0] - 2026-07-06

### Added
- Context compaction: /compact command summarizes older conversation turns into a single collapsible summary message to manage long conversations
- /new command to start a fresh chat from the composer
- Auto-compaction runs pre-send when context reaches 85% of the window, with deferred message auto-sending
- Per-session context estimation calibrated from real provider token usage counts
- SSH host key pinning (trust-on-first-use) for remote compute targets to prevent MITM attacks
- Test connection panel surfaces SSH key mismatches with fingerprints and "Accept new key" button
- Edit tool `replaceAll` option to replace all occurrences of a pattern instead of requiring exact single matches
- Configurable bash timeout cap via `bash_timeout_cap_secs` setting (default 300s, range 5-3600s) in Settings → Agent

### Changed
- Optimized file diff rendering to avoid large LCS allocations
- File reads now streamed instead of loading entire files into memory
- Last turn usage remains visible while idle
- Sessions and settings now saved via synced temp files for atomic writes
- Wire-history cache invalidated after transcript mutations during compaction

### Fixed
- Preserve state and write files atomically


## [0.10.0] - 2026-07-06

### Added
- Reasoning effort configuration with provider-specific overrides for GPT, Codex, and Claude
- Codex model discovery support
- Context usage indicator
- History reuse for cache-friendly follow-up turns

### Changed
- Improved context trimming and tool output limits
- Enhanced read output line number handling


## [0.9.2] - 2026-07-05

### Fixed
- Fix credential persistence on macOS and Windows by working around keyring v1 default-store initialization bug that caused saved secrets to vanish on restart
- Fix Linux CI compilation by enabling crypto-rust feature on secret-service dependency


## [0.9.1] - 2026-07-05

### Changed
- Migrate to Rust edition 2024 with MSRV 1.92
- Update egui/eframe/egui_extras to 0.35 with UI API changes
- Update reqwest to 0.13 with feature renames and cert verification changes
- Update vt100 to 0.16 with scrollback buffer improvements
- Update rand to 0.10
- Update dirs, rfd, russh, portable-pty, pulldown-cmark, and sha2 to latest versions

### Removed
- Drop unused arboard dependency

### Security
- Clear RUSTSEC advisories for ttf-parser, quick-xml, paste, zbus, and rand dependencies


## [0.9.0] - 2026-07-04

### Added
- Settings now includes Prompts and About tabs for better organization
- App displays its version in the About tab
- Automatic update checker runs on startup and can be triggered from About tab; sidebar shows a quiet indicator when updates are available
- View release button in About tab to access the latest GitHub release

### Changed
- Settings reorganized: agent system prompt and commit-message generator moved to Prompts tab; Agent tab now focuses on tools, approval, and web search
- Provider configuration simplified from multiple profiles per provider to one config per provider kind
- API keys now keychain-keyed by provider slug instead of profile ID
- settings.json automatically migrated on first load with immediate rewrite to ensure migration runs exactly once
- Commit-message generation now pins provider and model directly instead of profile ID


## [0.8.0] - 2026-07-03

### Added
- Move provider API keys, OAuth tokens, and SSH passwords into the OS keychain (macOS Keychain Services, Windows Credential Manager, Linux Secret Service)
- Add crash logging to `<config_dir>/oxi/crash.log`
- Add end-to-end agent-loop integration tests with wiremock
- Add regression tests for bash deny-list obfuscation handling
- Add session file corruption/malformation test coverage
- Add CONTRIBUTING.md with build, test, and code convention guidance
- Add cargo-audit and rust-cache to CI

### Changed
- Harden bash deny-list normalization to strip quote and backslash characters
- Restrict settings.json permissions to 0600 on Unix
- Refactor large modules into submodules: ui/messages.rs, app/settings_ui.rs, app/git_panel.rs, markdown.rs, settings.rs, theme.rs (no behavior change)

### Fixed
- Fix CI release pipeline JSON parsing when LLM response contains unescaped backslashes
- Resolve cargo-audit vulnerabilities by updating rustls-webpki and quinn-proto; document exceptions for quick-xml and rsa

### Security
- Credentials now stored in OS keychain instead of plaintext JSON files
- Harden bash command validation to catch obfuscations like `s\udo` and `'sudo'`


### Added
- Panic hook that logs crashes to `<config_dir>/oxi/crash.log` before the default handler
  prints to stderr, so failures in background threads leave a trace to report

### Security
- Move provider API keys, OAuth tokens, and SSH passwords into the OS keychain (macOS
  Keychain Services, Windows Credential Manager, Linux Secret Service) instead of
  plaintext JSON on disk; existing `settings.json`/`oauth.json`/`ssh_credentials.json`
  values are migrated in automatically and the plaintext is removed
- Restrict `settings.json` to owner-only permissions (`0600`) on Unix when saving, as
  defense in depth for its remaining (non-secret) contents

## [0.7.0] - 2026-07-03

### Added
- Track streaming duration with `started_at` and `worked_duration` fields on chat messages
- Collapsible activity summary showing 'Worked for…' duration (Cursor-style)
- Web search backend selection (Bing, DuckDuckGo, SearXNG) with Bing as default
- Zero-config DuckDuckGo search fallback when no SearXNG URL is configured
- Delete button on chat row hover (replaces timestamp on hover)
- Hover feedback and pointing-hand cursor on all interactive buttons
- Click-to-expand tool pills showing raw output/diff

### Changed
- Improved accent color consistency across app
- Improved chat entry spacing
- Improved git functionality across multiple workspaces
- New sidebar design
- Redesigned composer model selector with provider and model dropdowns
- Unified button rendering with consistent icon button styling
- Persist assistant `worked_duration` across reload
- Fold thinking blocks by visual row count to reduce streaming flicker

### Fixed
- Composer dropdowns, thinking block folding, and reload state
- UI thinking blocks display
- Icon-font vertical centering and hover accent-color transitions
- `tint_on_panels` blend order and dark-theme badge colors
- Clippy lints and code formatting


## [0.6.1] - 2026-07-03

### Changed
- Centralize theme tokens with shared corner-radius constants (`RADIUS_ROW`, `RADIUS_BUTTON`, `RADIUS_CHIP`, `RADIUS_PANEL`) replacing hardcoded pixel values throughout UI
- Rewrite git panel with improved layout: constrain diff viewer width to stay centered in chat column, make scrollbars always visible, reserve fixed space for action buttons on narrow panels
- Align message-status pill to fixed 28px height for visual consistency with header buttons
- Fix local-default URLs and hostnames in settings and documentation (`mac-mini` → `localhost`)

### Removed
- Delete autonomous Claude Code agent loop system (`.claude/loops/` directory)


## [0.6.0] - 2026-07-02

### Added
- Ollama provider with OpenAI-compatible /v1 API support
- SSH remote compute targets for LM Studio and Ollama with password authentication and connection testing
- LM Studio provider with local LAN model support and self-signed TLS
- LLM-powered commit message generator with streaming completion
- Resizable bottom terminal panel with embedded PTY shell, 256/RGB colors, mouse support, and scrollback persistence
- Stream-level retry logic with exponential backoff for transient HTTP failures (408/429/5xx)
- In-band SSE error recovery without losing progress from completed rounds
- Nerd Font icon constants and icon-driven UI widgets throughout the app
- Git diff as full-area overlay with caching and keyboard dismissal (Esc/✕)
- Automatic upstream detection and `--set-upstream` fallback for new branch pushes
- Rust/copper brand accent across all built-in palettes with derived semantic colors
- Settings reorganization into Providers/Agent/Appearance tabs with improved layout

### Changed
- Tightened client timeouts (connect=30s, read=60-180s, tcp_keepalive=60s) to prevent long turns from being killed
- Replaced per-provider inline error handling with shared `send_with_retry` wrapper in new `agent/net.rs` module
- Running tool pills now use neutral surface with spinner and badge only carrying state, reducing visual noise
- Provider tabs reordered to lead with Ollama and LM Studio (local runtimes)
- Git panel UI refactored to use icon-driven widgets and inline error display
- Terminal scrollback offset clamped to prevent vt300 underflow panic
- Live thinking bubble tail-preview truncated to keep newest reasoning lines visible while streaming

### Fixed
- Worker disconnect handling while waiting for response to avoid hanging sessions on panics
- `AgentEvent::StreamRetry` prevents duplicate text/thinking when generation is re-sent
- Conversation header layout no longer overflows with right button cluster
- Cargo fmt and clippy warnings unblocked CI


### Added
- Ollama provider profile (OpenAI-compatible `/v1` API, no-auth by default, defaults to
  `http://localhost:11434/v1`)
- Remote SSH compute target for LM Studio / Ollama profiles: point a profile at a host
  reached over SSH (e.g. a Mac mini) and oxi tunnels the connection so the runtime only
  needs to listen on `127.0.0.1` there. Password auth, "Test connection" button in
  Settings → Providers. SSH passwords are stored separately in `ssh_credentials.json`,
  never in `settings.json`.

## [0.5.0] - 2026-06-27

### Added
- Web search and web fetch tools for agents to query SearXNG instances and fetch URLs
- Text size (UI density) setting with Compact, Normal, and Comfortable options
- Full Noto Emoji font for improved emoji rendering

### Changed
- Unified typography scale across UI with consistent font sizes (H1 20 / H2 17 / H3 15 / body 14 / small 12.5 / tiny 11.5 / code 13)
- Inline code now uses surrounding prose font and size for proper baseline alignment
- User and assistant body text now use the same font size
- Consistent line-height (1.35) for all flowing prose including list items
- Tool configuration migrated from fixed array to Vec<bool> for extensibility
- egui text styles overridden to match unified typography scale


## [0.4.1] - 2026-06-25

### Fixed
- macOS release now ships as a proper .app bundle with icon instead of requiring Terminal


## [0.4.0] - 2026-06-24

### Added
- Theme-aware palette system with light, dark, and midnight themes plus custom theme support


## [0.3.0] - 2026-06-24

### Added
- Persist sidebar width across sessions
- Application icon

### Changed
- Update app icon transparency
- Remove chat placeholder

### Removed
- Unused dead code


## [0.2.0] - 2026-06-24

### Added
- **Approval mode** for mutating tools: the agent now pauses and asks for explicit
  confirmation before running `bash`, `write`, or `edit`. The prompt offers
  *Approve*, *Approve rest* (auto-approve the remaining tools in the run), and *Deny*
  (the model is told the user declined). Controlled by a new
  "Ask before running bash / write / edit" toggle in Settings → Tools (on by default).
- CI status badge in the README.
- Unit tests for the agent runner (`opencode_go_model_uses_anthropic`, profile key
  resolution) and the approval gate.

### Changed
- Context-budget trimming serializes each message only once instead of up to twice,
  reducing per-request work on long histories.

### Removed
- **GitHub Copilot** provider and its OAuth (device-flow) support, including the
  `COPILOT_GITHUB_TOKEN` / `GH_TOKEN` / `GITHUB_TOKEN` fallbacks. **(breaking)**

### Fixed
- Test suite no longer compiled after an earlier refactor (`model.rs` referenced a
  removed function and a dropped enum field), which left CI red.
- `clippy::collapsible_if` warning in the sidebar that broke `cargo clippy -D warnings`.
- Approval prompt is rendered at the tail of the transcript (above the floating
  composer) so it stays visible while a run is paused; previously it could be scrolled
  out of view at the top.

## [0.1.0]

- Initial release: local desktop coding-agent chat app (Rust + egui/eframe) with
  streaming LLM responses, built-in workspace tools, per-workspace session
  persistence, configurable provider profiles, and OAuth for Codex.

[Unreleased]: https://github.com/maziluiosif/oxi/compare/v0.11.1...HEAD
[0.11.1]: https://github.com/maziluiosif/oxi/compare/v0.11.0...v0.11.1
[0.11.0]: https://github.com/maziluiosif/oxi/compare/v0.10.0...v0.11.0
[0.10.0]: https://github.com/maziluiosif/oxi/compare/v0.9.2...v0.10.0
[0.9.2]: https://github.com/maziluiosif/oxi/compare/v0.9.1...v0.9.2
[0.9.1]: https://github.com/maziluiosif/oxi/compare/v0.9.0...v0.9.1
[0.9.0]: https://github.com/maziluiosif/oxi/compare/v0.8.0...v0.9.0
[0.8.0]: https://github.com/maziluiosif/oxi/compare/v0.7.0...v0.8.0
[0.7.0]: https://github.com/maziluiosif/oxi/compare/v0.6.1...v0.7.0
[0.6.1]: https://github.com/maziluiosif/oxi/compare/v0.6.0...v0.6.1
[0.6.0]: https://github.com/maziluiosif/oxi/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/maziluiosif/oxi/compare/v0.4.1...v0.5.0
[0.4.1]: https://github.com/maziluiosif/oxi/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/maziluiosif/oxi/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/maziluiosif/oxi/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/maziluiosif/oxi/releases/tag/v0.2.0
