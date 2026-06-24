#!/usr/bin/env bash
#
# start-loops.sh — launch the three oxi loop-engineering loops (macOS).
#
# Opens one macOS Terminal window per loop (implementer, reviewer, fixer),
# each starting a Claude Code session seeded with the matching /loop prompt.
# After this, you only write issues and do the final human review.
#
# Usage:
#   .claude/loops/start-loops.sh             # self-paced loops
#   .claude/loops/start-loops.sh 10m         # fixed 10-minute interval for every loop
#   .claude/loops/start-loops.sh --dry-run   # print what would launch, don't open anything
#
# Requires: macOS (Terminal.app + osascript) and the `claude` CLI on PATH.

set -euo pipefail

# --- locate the repo root (two levels up from .claude/loops/) ----------------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"

# --- parse args --------------------------------------------------------------
DRY_RUN=0
INTERVAL=""
for arg in "$@"; do
  case "$arg" in
    --dry-run) DRY_RUN=1 ;;
    -h|--help)
      grep -E '^#( |$)' "$0" | sed -E 's/^# ?//'
      exit 0
      ;;
    *)
      if [[ "$arg" =~ ^[0-9]+[smhd]$ ]]; then
        INTERVAL="$arg "
      else
        echo "Unknown argument: $arg (expected an interval like 10m, or --dry-run)" >&2
        exit 2
      fi
      ;;
  esac
done

# --- preflight ---------------------------------------------------------------
if [[ "$(uname)" != "Darwin" ]]; then
  echo "This launcher uses macOS Terminal.app + osascript and only runs on macOS." >&2
  echo "On other platforms, open three terminals and run the /loop prompts manually" >&2
  echo "(see .claude/loops/README.md)." >&2
  exit 1
fi

if ! command -v claude >/dev/null 2>&1; then
  echo "The 'claude' CLI was not found on PATH. Install Claude Code first." >&2
  exit 1
fi

# --- the three loops ---------------------------------------------------------
# name|title|loop-definition file
LOOPS=(
  "implementer|oxi · implementer|implementer.md"
  "reviewer|oxi · reviewer|reviewer.md"
  "fixer|oxi · fixer|fixer.md"
)

launch() {
  local title="$1" file="$2"
  local prompt="/loop ${INTERVAL}Read .claude/loops/${file} and execute exactly one full pass, then stop until the next tick."
  # Command run inside the new Terminal window. Single-quote the prompt so the
  # shell passes it to claude verbatim; the prompt contains no single quotes.
  local shell_cmd="cd ${REPO_DIR} && claude '${prompt}'"

  if [[ "$DRY_RUN" -eq 1 ]]; then
    echo "[${title}]"
    echo "  ${shell_cmd}"
    return
  fi

  osascript \
    -e 'tell application "Terminal"' \
    -e '  activate' \
    -e "  set t to do script \"${shell_cmd}\"" \
    -e "  set custom title of (first window whose tabs contains t) to \"${title}\"" \
    -e 'end tell' >/dev/null
}

echo "Repo: ${REPO_DIR}"
if [[ -n "$INTERVAL" ]]; then
  echo "Cadence: every ${INTERVAL% }"
else
  echo "Cadence: self-paced"
fi
echo "Launching loops: implementer, reviewer, fixer"
echo

for entry in "${LOOPS[@]}"; do
  IFS='|' read -r _name title file <<< "$entry"
  launch "$title" "$file"
done

if [[ "$DRY_RUN" -eq 0 ]]; then
  echo "Three Terminal windows are starting (one per loop)."
  echo "Feed the system with:  gh issue create --title \"...\" --body \"...\" --label ready"
fi
