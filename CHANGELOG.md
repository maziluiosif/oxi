# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/maziluiosif/oxi/compare/v0.5.0...HEAD
[0.5.0]: https://github.com/maziluiosif/oxi/compare/v0.4.1...v0.5.0
[0.4.1]: https://github.com/maziluiosif/oxi/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/maziluiosif/oxi/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/maziluiosif/oxi/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/maziluiosif/oxi/releases/tag/v0.2.0
