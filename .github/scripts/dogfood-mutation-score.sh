#!/usr/bin/env bash
# Runs a bounded dogfood mutation check and writes a shields.io endpoint JSON
# with the resulting mutation score. Used by the dogfood-badge workflow.
set -euo pipefail

output_path="${1:-mutation-score.json}"
togi_bin="${TOGI_BIN:-./target/release/togi}"

if ! command -v jq >/dev/null 2>&1; then
  echo "jq is required for the dogfood badge update" >&2
  exit 1
fi

report_path="${RUNNER_TEMP:-/tmp}/togi-dogfood-score-report.json"
raw_report_path="${RUNNER_TEMP:-/tmp}/togi-dogfood-score-report.raw"

set +e
"$togi_bin" check \
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
      cat "$raw_report_path" >&2
    fi
    exit "$status"
    ;;
esac

sed -n '/^{/,$p' "$raw_report_path" >"$report_path"

if ! jq -e '.tested > 0 and .timeout == 0 and .build_errors == 0 and .partial == false' "$report_path" >/dev/null; then
  echo "Dogfood run produced an invalid report:" >&2
  cat "$report_path" >&2
  exit 1
fi

score="$(jq -r '.mutation_score | round' "$report_path")"
tested="$(jq -r '.tested' "$report_path")"

jq -n --argjson score "$score" --argjson tested "$tested" '{
  schemaVersion: 1,
  label: "mutation score (dogfood: src/report/json.rs)",
  message: "\($score)% (\($tested) tested)",
  color: "brightgreen"
}' >"$output_path"

python3 -m json.tool "$output_path" >/dev/null

echo "Dogfood mutation score: ${score}% (${tested} tested) -> ${output_path}"
