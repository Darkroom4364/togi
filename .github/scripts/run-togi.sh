#!/usr/bin/env bash
set -euo pipefail

args=(
  check
  --base "${TOGI_BASE:-origin/main}"
  --timeout "${TOGI_TIMEOUT:-30}"
  --format "${TOGI_FORMAT:-terminal}"
)

if [[ -n "${TOGI_TEST_CMD:-}" ]]; then
  args+=(--test-cmd "$TOGI_TEST_CMD")
fi

"${TOGI_BIN:-togi}" "${args[@]}"
