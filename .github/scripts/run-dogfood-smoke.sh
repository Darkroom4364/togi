#!/usr/bin/env bash
set -euo pipefail

if ! command -v jq >/dev/null 2>&1; then
  echo "jq is required for the dogfood smoke test" >&2
  exit 1
fi

report_path="${RUNNER_TEMP:-/tmp}/togi-dogfood-report.json"
raw_report_path="${RUNNER_TEMP:-/tmp}/togi-dogfood-report.raw"

set +e
./target/release/togi check \
  --all \
  --path src/report/json.rs \
  --test-cmd "cargo test --locked" \
  --calibrate-timeout \
  --timeout-multiplier 4 \
  --timeout-slack 2 \
  --format json \
  >"$raw_report_path"
status=$?
set -e

case "$status" in
  0|1)
    ;;
  *)
    echo "Dogfood run failed unexpectedly with exit code $status" >&2
    if [[ -f "$raw_report_path" ]]; then
      cat "$raw_report_path"
    fi
    exit "$status"
    ;;
esac

sed -n '/^{/,$p' "$raw_report_path" >"$report_path"

if ! jq -e '.tested > 0 and .timeout == 0 and .build_errors == 0 and .partial == false' "$report_path" >/dev/null; then
  echo "Dogfood run produced an invalid report:" >&2
  cat "$report_path"
  exit 1
fi

jq -r '"Dogfood summary: \(.killed) killed, \(.survived) survived, \(.timeout) timeout, \(.build_errors) build errors"' "$report_path"
