# oxi app icon

The app icon uses linked code brackets in copper orange on a graphite tile,
matching the application's warm accent. The exterior is transparent.

- `app-icon.png`: 1024 × 1024 source, embedded by the native app and used by
  `scripts/bundle-macos.sh` to build the macOS icon.
- `windows/app-icon-256.png`: 256 × 256 PNG.
- `windows/app-icon.ico`: PNG frames at 16, 24, 32, 48, 64, 128 and 256 pixels,
  embedded by `build.rs` on Windows.

Generated with the built-in ImageGen tool; platform sizes were exported with
macOS `sips`. The ICO container preserves the PNG frames and their alpha.

## Generation prompt

Create one premium native desktop app icon for oxi, a lightweight local-first
coding agent built in Rust, with a dark UI and warm copper-orange accent.
A nearly black graphite rounded-square tile with a transparent exterior.
Center two broad opposing code chevrons, linked as a folded ribbon with a
diagonal stroke. Use copper-orange faces, apricot highlights, burnt-orange
facets, restrained satin depth, crisp front-facing geometry and bold strokes
that remain legible at small sizes. No lettering, ring, robot, gears, circuit
lines, sparkle, tiny details, excessive glow, perspective or watermark.
