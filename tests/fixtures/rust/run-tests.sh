#!/usr/bin/env bash
set -euo pipefail

target_dir="$(mktemp -d)"
trap 'rm -rf "$target_dir"' EXIT

export CARGO_TARGET_DIR="$target_dir"
cargo test --quiet --locked
