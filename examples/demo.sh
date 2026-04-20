#!/usr/bin/env bash
# Demo: togi finding test gaps in a Go project with weak tests
# Requires: go, cargo build (or cargo install togi)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$SCRIPT_DIR/.."
TOGI="$ROOT/target/debug/togi"
FIXTURE="$ROOT/tests/fixtures/go"

if [[ ! -x "$TOGI" ]]; then
  echo "Building togi..."
  cargo build --manifest-path "$ROOT/Cargo.toml"
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

# Set up git history: empty commit, then add all files
git init -q
git commit --allow-empty -q -m "empty"
git add -A
git commit -q -m "add calc module"

echo "Running: togi check --base HEAD~1 --test-cmd 'go test ./...' --jobs 1"
echo ""

# GOWORK=off avoids Go complaining about workspace in temp dirs
GOWORK=off "$TOGI" check --base HEAD~1 --test-cmd "go test ./..." --timeout 30 --jobs 1 || true
