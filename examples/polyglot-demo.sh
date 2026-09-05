#!/usr/bin/env bash
# Demo: togi on a polyglot change — one run, one report, one score gate.
# Requires: go, python3, and either cargo or an executable TOGI_BIN.
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
FIXTURE="$ROOT/tests/fixtures/polyglot"

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

echo "=== togi demo: one mutation run across Go + Rust + Python ==="
echo ""
echo "The fixture is one PR-sized change touching three languages, each"
echo "with deliberately weak tests. togi.toml carries per-language test"
echo "commands, so a single 'togi check' runs each mutant against its own"
echo "language's suite and reports one unified result with one score gate."
echo ""

# Work in a temp copy so we don't pollute the fixture
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
cp -r "$FIXTURE"/* "$WORK/"
cd "$WORK"
REPORT="$WORK/togi-report.json"

git init -q
git config user.email "togi-demo@example.invalid"
git config user.name "togi demo"
git commit --allow-empty -q -m "empty"
git add -A
git commit -q -m "add calc helpers in go, rust, and python"

echo "Running: togi check --base HEAD~1 --jobs 1"
echo ""

# GOWORK=off avoids Go complaining about workspace in temp dirs.
# A clean survivor report is expected; reject timeout and build-error reports.
if GOWORK=off "$TOGI" check --base HEAD~1 --timeout 60 --jobs 1 --json-report "$REPORT"; then
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

echo ""
echo "=== one run, one report, one gate — no per-language glue required ==="
