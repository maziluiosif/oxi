#!/usr/bin/env bash
#
# Build a macOS `.app` bundle for oxi from a compiled binary and the app icon.
#
# A bare Unix executable, when double-clicked in Finder, is opened through
# Terminal (so a terminal window appears, it carries the terminal icon, and
# quitting the terminal kills the app). Wrapping the binary in a proper `.app`
# bundle makes Finder launch the GUI directly, with the right icon.
#
# Usage: scripts/bundle-macos.sh <path-to-oxi-binary> <output-dir>
# Env:   OXI_VERSION  override the version string (defaults to Cargo.toml).
#
# Requires macOS tools `sips` and `iconutil` (present on GitHub macos runners).
set -euo pipefail

BIN="${1:?usage: bundle-macos.sh <oxi-binary> <output-dir>}"
OUT_DIR="${2:?usage: bundle-macos.sh <oxi-binary> <output-dir>}"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ICON_PNG="$REPO_ROOT/assets/app-icon.png"
APP="$OUT_DIR/oxi.app"

if [[ ! -f "$BIN" ]]; then
  echo "error: binary not found: $BIN" >&2
  exit 1
fi
if [[ ! -f "$ICON_PNG" ]]; then
  echo "error: icon not found: $ICON_PNG" >&2
  exit 1
fi

VERSION="${OXI_VERSION:-$(grep -m1 '^version' "$REPO_ROOT/Cargo.toml" | sed -E 's/.*"(.*)".*/\1/')}"

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

cp "$BIN" "$APP/Contents/MacOS/oxi"
chmod +x "$APP/Contents/MacOS/oxi"

# Build oxi.icns from the 1024x1024 PNG. iconutil only accepts the canonical
# iconset names below, so generate exactly those sizes.
ICONSET="$(mktemp -d)/oxi.iconset"
mkdir -p "$ICONSET"
for size in 16 32 128 256 512; do
  sips -z "$size" "$size" "$ICON_PNG" --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
  retina=$((size * 2))
  sips -z "$retina" "$retina" "$ICON_PNG" --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/oxi.icns"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key>
  <string>oxi</string>
  <key>CFBundleDisplayName</key>
  <string>oxi</string>
  <key>CFBundleIdentifier</key>
  <string>com.maziluiosif.oxi</string>
  <key>CFBundleVersion</key>
  <string>${VERSION}</string>
  <key>CFBundleShortVersionString</key>
  <string>${VERSION}</string>
  <key>CFBundleExecutable</key>
  <string>oxi</string>
  <key>CFBundleIconFile</key>
  <string>oxi</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>LSMinimumSystemVersion</key>
  <string>10.15</string>
  <key>NSHighResolutionCapable</key>
  <true/>
</dict>
</plist>
PLIST

echo "Built $APP (version $VERSION)"
