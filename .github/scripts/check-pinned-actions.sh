#!/usr/bin/env bash
set -euo pipefail

if [[ "$(git rev-parse --is-inside-work-tree 2>/dev/null || true)" != "true" ]]; then
  echo "check-pinned-actions.sh must run inside a Git work tree" >&2
  exit 1
fi

repo_root="$(git rev-parse --show-toplevel)"
script_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
manifests=()

while IFS= read -r -d '' manifest; do
  manifests+=("$manifest")
done < <(
  git -C "$repo_root" ls-files -z --cached -- \
    ':(glob).github/workflows/**/*.yml' \
    ':(glob).github/workflows/**/*.yaml' \
    ':(glob)**/action.yml' \
    ':(glob)**/action.yaml'
)

CARGO_TARGET_DIR="${TOGI_PINNED_ACTIONS_TARGET_DIR:-${TMPDIR:-/tmp}/togi-pinned-actions-target}" \
  exec cargo run --locked --quiet --manifest-path "$script_root/Cargo.toml" \
    --example check-pinned-actions -- "$repo_root" "${manifests[@]}"
