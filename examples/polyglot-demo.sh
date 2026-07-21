#!/usr/bin/env bash
# Demo: togi on a polyglot change — one run, one report, one score gate.
# Requires: go, cargo, python3, and a built togi (or cargo install togi).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$SCRIPT_DIR/.."
TOGI="$ROOT/target/debug/togi"
FIXTURE="$ROOT/tests/fixtures/polyglot"

if [[ ! -x "$TOGI" ]]; then
  echo "Building togi..."
  cargo build --manifest-path "$ROOT/Cargo.toml"
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

git init -q
git commit --allow-empty -q -m "empty"
git add -A
git commit -q -m "add calc helpers in go, rust, and python"

echo "Running: togi check --base HEAD~1 --jobs 1"
echo ""

# GOWORK=off avoids Go complaining about workspace in temp dirs.
# Exit code is non-zero while mutants survive — that is the score gate at work.
GOWORK=off "$TOGI" check --base HEAD~1 --timeout 60 --jobs 1 || true

echo ""
echo "=== one run, one report, one gate — no per-language glue required ==="
