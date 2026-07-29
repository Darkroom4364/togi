#!/usr/bin/env bash
set -uo pipefail

if ! command -v jq >/dev/null 2>&1; then
  echo "jq is required to generate Action report outputs" >&2
  exit 2
fi

report_path="${TOGI_REPORT_PATH:-${RUNNER_TEMP:-.}/togi-report.json}"
if command -v cygpath >/dev/null 2>&1; then
  report_path=$(cygpath -u "$report_path")
fi
if ! mkdir -p "$(dirname "$report_path")"; then
  echo "Could not create report directory for ${report_path}" >&2
  exit 2
fi
if ! rm -f "$report_path"; then
  echo "Could not remove stale report at ${report_path}" >&2
  exit 2
fi

review_args=(check)
json_args=(check)

if [[ -n "${TOGI_BASE:-}" ]]; then
  review_args+=(--base "$TOGI_BASE")
  json_args+=(--base "$TOGI_BASE")
fi

if [[ -n "${TOGI_TIMEOUT:-}" ]]; then
  review_args+=(--timeout "$TOGI_TIMEOUT")
  json_args+=(--timeout "$TOGI_TIMEOUT")
fi

if [[ -n "${TOGI_FORMAT:-}" ]]; then
  review_args+=(--format "$TOGI_FORMAT")
fi
json_args+=(--format json)

if [[ -n "${TOGI_TEST_CMD:-}" ]]; then
  review_args+=(--test-cmd "$TOGI_TEST_CMD")
  json_args+=(--test-cmd "$TOGI_TEST_CMD")
fi

togi_bin="${TOGI_BIN:-togi}"

if [[ "${TOGI_FORMAT:-}" == "json" ]]; then
  "$togi_bin" "${review_args[@]}" | tee "$report_path"
  statuses=("${PIPESTATUS[@]}")
  review_status=${statuses[0]}
  tee_status=${statuses[1]}
  if (( tee_status != 0 )); then
    echo "Could not write report to ${report_path}" >&2
    rm -f "$report_path"
    exit 2
  fi
  json_status=$review_status
else
  "$togi_bin" "${review_args[@]}"
  review_status=$?
  case "$review_status" in
    0|1)
      ;;
    *)
      rm -f "$report_path"
      exit "$review_status"
      ;;
  esac

  "$togi_bin" "${json_args[@]}" >"$report_path"
  json_status=$?
fi

case "$json_status" in
  0|1)
    ;;
  *)
    rm -f "$report_path"
    exit "$json_status"
    ;;
esac

summary=$(
  jq -er '
    if .kind != "mutation_report" then
      error("expected a normal mutation report")
    elif (.schema_version | type) != "number" then
      error("report schema version is missing or invalid")
    elif (.schema_version < 1 or (.schema_version | floor) != .schema_version) then
      error("report schema version is missing or invalid")
    elif (.mutation_score | type) != "number" then
      error("report mutation score is missing or invalid")
    elif (.survived | type) != "number" then
      error("report survivor count is missing or invalid")
    elif (.survived < 0 or (.survived | floor) != .survived) then
      error("report survivor count is missing or invalid")
    else
      [.mutation_score, .survived] | @tsv
    end
  ' "$report_path"
) || {
  echo "Togi did not produce a valid replayable mutation report" >&2
  rm -f "$report_path"
  exit 2
}

IFS=$'\t' read -r mutation_score survivor_count <<<"$summary"
if [[ -z "${GITHUB_OUTPUT:-}" ]]; then
  echo "GITHUB_OUTPUT is required to publish Action outputs" >&2
  rm -f "$report_path"
  exit 2
fi
if ! {
  printf 'report-path=%s\n' "$report_path"
  printf 'mutation-score=%s\n' "$mutation_score"
  printf 'survivor-count=%s\n' "$survivor_count"
} >>"$GITHUB_OUTPUT"; then
  echo "Could not write Action outputs" >&2
  rm -f "$report_path"
  exit 2
fi

exit "$review_status"
