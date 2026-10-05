#!/bin/sh
# Installs the latest oxi release on macOS (Apple Silicon) and Linux (x86_64).
#
#   curl -fsSL https://maziluiosif.github.io/oxi/install.sh | sh
#
# Files fetched with curl never get macOS's quarantine attribute, so the
# ad-hoc signed app opens without the `xattr` step a browser download needs.
#
# Environment overrides:
#   OXI_VERSION      release tag to install (e.g. v1.8.0); defaults to the latest
#   OXI_BIN_DIR      where the `oxi` command goes; defaults to ~/.local/bin
#   OXI_APP_DIR      macOS only: where oxi.app goes; defaults to /Applications
#                    when writable, ~/Applications otherwise

set -eu

REPO="maziluiosif/oxi"

say() { printf 'oxi: %s\n' "$*"; }
fail() { printf 'oxi: error: %s\n' "$*" >&2; exit 1; }

need() {
    command -v "$1" >/dev/null 2>&1 || fail "'$1' is required but was not found"
}

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d ' ' -f 1
    else
        shasum -a 256 "$1" | cut -d ' ' -f 1
    fi
}

main() {
    need curl
    need tar
    command -v sha256sum >/dev/null 2>&1 || need shasum

    os=$(uname -s)
    arch=$(uname -m)
    case "$os/$arch" in
        Darwin/arm64) asset="oxi-macos-arm64.tar.gz" ;;
        Linux/x86_64 | Linux/amd64) asset="oxi-linux-x86_64.tar.gz" ;;
        Darwin/*) fail "prebuilt macOS builds are Apple Silicon only; build from source on $arch: https://github.com/$REPO#build-and-run-from-source" ;;
        *) fail "no prebuilt build for $os/$arch; build from source: https://github.com/$REPO#build-and-run-from-source" ;;
    esac

    version="${OXI_VERSION:-}"
    if [ -n "$version" ]; then
        case "$version" in v*) ;; *) version="v$version" ;; esac
        base="https://github.com/$REPO/releases/download/$version"
    else
        base="https://github.com/$REPO/releases/latest/download"
    fi

    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT INT TERM

    say "downloading $asset${version:+ ($version)}"
    curl -fL --proto '=https' --tlsv1.2 --progress-bar -o "$tmp/$asset" "$base/$asset" \
        || fail "download failed: $base/$asset"
    curl -fsSL --proto '=https' --tlsv1.2 -o "$tmp/SHA256SUMS" "$base/SHA256SUMS" \
        || fail "download failed: $base/SHA256SUMS"

    expected=$(awk -v f="$asset" '$2 == f || $2 == "*" f { print $1 }' "$tmp/SHA256SUMS")
    [ -n "$expected" ] || fail "$asset is missing from SHA256SUMS"
    actual=$(sha256_of "$tmp/$asset")
    [ "$expected" = "$actual" ] || fail "checksum mismatch for $asset (expected $expected, got $actual)"
    say "checksum verified"

    mkdir -p "$tmp/extract"
    tar -xzf "$tmp/$asset" -C "$tmp/extract"

    bin_dir="${OXI_BIN_DIR:-$HOME/.local/bin}"
    mkdir -p "$bin_dir"

    if [ "$os" = "Darwin" ]; then
        app_dir="${OXI_APP_DIR:-}"
        if [ -z "$app_dir" ]; then
            if [ -w /Applications ]; then app_dir="/Applications"; else app_dir="$HOME/Applications"; fi
        fi
        mkdir -p "$app_dir"
        app="$app_dir/oxi.app"
        rm -rf "$app"
        mv "$tmp/extract/oxi.app" "$app"
        # Not needed for a curl download, but clears the flag if the app came
        # from somewhere that set it (e.g. a proxy or an earlier browser install).
        xattr -dr com.apple.quarantine "$app" 2>/dev/null || true
        ln -sf "$app/Contents/MacOS/oxi" "$bin_dir/oxi"
        say "installed $app"
    else
        install -m 755 "$tmp/extract/oxi" "$bin_dir/oxi"
    fi
    say "installed the oxi command at $bin_dir/oxi"

    case ":$PATH:" in
        *":$bin_dir:"*) ;;
        *)
            say "$bin_dir is not on your PATH; add this line to your shell profile:"
            printf '\n    export PATH="%s:$PATH"\n\n' "$bin_dir"
            ;;
    esac

    if [ "$os" = "Darwin" ]; then
        say "done. Open oxi from Launchpad, or run \`oxi\` in a project directory."
    else
        say "done. Run \`oxi\` in a project directory to open it as the first workspace."
    fi
}

# Everything runs from main() so a truncated download cannot execute half a script.
main "$@"
