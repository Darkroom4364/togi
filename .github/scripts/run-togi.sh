#!/usr/bin/env bash
set -euo pipefail

args=(check)

if [[ -n "${TOGI_BASE:-}" ]]; then
  args+=(--base "$TOGI_BASE")
fi

if [[ -n "${TOGI_TIMEOUT:-}" ]]; then
  args+=(--timeout "$TOGI_TIMEOUT")
fi

if [[ -n "${TOGI_FORMAT:-}" ]]; then
  args+=(--format "$TOGI_FORMAT")
fi

if [[ -n "${TOGI_TEST_CMD:-}" ]]; then
  args+=(--test-cmd "$TOGI_TEST_CMD")
fi

"${TOGI_BIN:-togi}" "${args[@]}"
