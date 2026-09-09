#!/usr/bin/env bash
# Demo: togi finding test gaps in a Go project with weak tests
# Requires: go and either cargo or an executable TOGI_BIN.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
TARGET_DIR="${CARGO_TARGET_DIR:-"$ROOT/target"}"
case "$TARGET_DIR" in
  /*) ;;
  *) TARGET_DIR="$ROOT/$TARGET_DIR" ;;
esac
TOGI="${TOGI_BIN:-"$TARGET_DIR/debug/togi"}"
case "$TOGI" in
  /*) ;;
  *) TOGI="$ROOT/$TOGI" ;;
esac
FIXTURE="$ROOT/tests/fixtures/go"

is_clean_survivor_report() {
  local report survived timeout build_errors pattern

  [[ -s "$1" ]] || return 1
  report=$(<"$1")

  pattern='"survived"[[:space:]]*:[[:space:]]*([0-9]+)'
  [[ "$report" =~ $pattern ]] || return 1
  survived="${BASH_REMATCH[1]}"
  pattern='"timeout"[[:space:]]*:[[:space:]]*([0-9]+)'
  [[ "$report" =~ $pattern ]] || return 1
  timeout="${BASH_REMATCH[1]}"
  pattern='"build_errors"[[:space:]]*:[[:space:]]*([0-9]+)'
  [[ "$report" =~ $pattern ]] || return 1
  build_errors="${BASH_REMATCH[1]}"

  (( survived > 0 && timeout == 0 && build_errors == 0 ))
}

if [[ ! -x "$TOGI" ]]; then
  if [[ -n "${TOGI_BIN:-}" ]]; then
    echo "togi binary not found at $TOGI" >&2
    exit 1
  fi
  echo "Building togi..."
  cargo build --manifest-path "$ROOT/Cargo.toml" --target-dir "$TARGET_DIR"
fi

echo "=== togi demo: finding test gaps ==="
echo ""
echo "The fixture has 4 Go functions with deliberately weak tests:"
echo "  - TestAdd:        only tests Add(2,3)"
echo "  - TestIsPositive: only tests IsPositive(1), misses 0 and negatives"
echo "  - TestMax:        only tests Max(3,5), misses a>b case"
echo "  - TestAbs:        MISSING entirely"
echo ""

# Work in a temp copy so we don't pollute the fixture
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
cp -r "$FIXTURE"/* "$WORK/"
cd "$WORK"
REPORT="$WORK/togi-report.json"

# Set up git history: empty commit, then add all files
git init -q
git commit --allow-empty -q -m "empty"
git add -A
git commit -q -m "add calc module"

echo "Running: togi check --base HEAD~1 --test-cmd 'go test ./...' --jobs 1"
echo ""

# GOWORK=off avoids Go complaining about workspace in temp dirs
# A clean survivor report is expected; reject timeout and build-error reports.
if GOWORK=off "$TOGI" check --base HEAD~1 --test-cmd "go test ./..." --timeout 30 --jobs 1 --json-report "$REPORT"; then
  status=0
else
  status=$?
fi
if [[ "$status" != 0 && "$status" != 1 ]]; then
  exit "$status"
fi
if ! is_clean_survivor_report "$REPORT"; then
  echo "Expected a JSON report with survived > 0, timeout == 0, and build_errors == 0." >&2
  exit 1
fi
