# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/maziluiosif/oxi/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/maziluiosif/oxi/releases/tag/v0.2.0
