#!/usr/bin/env bash
#
# Render the product demo from the real UI: the website video, its poster, the README GIF,
# and the screenshots used on the website.
#
# The UI is drawn offscreen (no window, no screen recording) by the ignored `record_demo`
# test in src/app/demo_recording.rs. The agent's turn is scripted, but its tools run for real
# against a generated sample project. It uses a throwaway HOME and an in-memory keychain, so
# your settings, chats and keys are never touched.
#
# Usage: scripts/render-demo.sh
# Requires ffmpeg (brew install ffmpeg).
# Keep this script LF-only: Bash treats a CR after `pipefail` as part of the option name.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
command -v ffmpeg >/dev/null || { echo "ffmpeg is required (brew install ffmpeg)" >&2; exit 1; }

cd "$ROOT"
echo "Rendering frames…"
OXI_DEMO_FRAMES="$WORK/frames" OXI_DEMO_STILLS="$WORK/stills" \
  cargo test --release record_demo -- --ignored --nocapture

FRAMES="$WORK/frames/frame_%05d.png"
DEMO="$ROOT/assets/demo"
SHOTS="$ROOT/assets/screenshots"
# The website serves the video from its own folder: GitHub Pages sends it with the right MIME
# type, while raw.githubusercontent.com does not.
SITE="$ROOT/docs/media"
mkdir -p "$SITE"

echo "Encoding video…"
# 1600 px wide keeps text crisp on the site's ~1080 px frame at 2x, at a fraction of 4K's size.
ffmpeg -v error -y -framerate 20 -i "$FRAMES" \
  -vf "scale=1600:-2:flags=lanczos,format=yuv420p" \
  -c:v libx264 -preset slow -crf 22 -tune animation -movflags +faststart \
  "$SITE/demo.mp4"

echo "Encoding GIF…"
# GitHub READMEs cannot autoplay video, so the README keeps a GIF: smaller and at 12 fps.
ffmpeg -v error -y -framerate 20 -i "$FRAMES" \
  -vf "fps=12,scale=960:-1:flags=lanczos,split[a][b];[a]palettegen=max_colors=128:stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=5:diff_mode=rectangle" \
  "$DEMO/demo.gif"

echo "Writing poster and screenshots…"
ffmpeg -v error -y -i "$WORK/stills/agent-run.png" -vf "scale=1600:-2:flags=lanczos" "$SITE/poster.jpg"
for still in "$WORK"/stills/*.png; do
  ffmpeg -v error -y -i "$still" -vf "scale=1600:-2:flags=lanczos" \
    "$SHOTS/$(basename "$still")"
done

ls -la "$DEMO" "$SITE"
echo "Done."
