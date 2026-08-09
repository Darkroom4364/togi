#!/usr/bin/env bash
set -uo pipefail

if [[ -z "${TOGI_BIN:-}" ]]; then
  echo "TOGI_BIN must name the concrete installed Togi binary" >&2
  exit 2
fi
if [[ -z "${TOGI_EXPECTED_VERSION:-}" ]]; then
  echo "TOGI_EXPECTED_VERSION must name the installed Togi release tag" >&2
  exit 2
fi

togi_bin="$TOGI_BIN"
expected_tag="$TOGI_EXPECTED_VERSION"
if [[ ! "$expected_tag" =~ ^v[0-9]+[.][0-9]+[.][0-9]+$ ]]; then
  echo "TOGI_EXPECTED_VERSION must be an immutable vX.Y.Z tag" >&2
  exit 2
fi

run_togi() (
  unset TOGI_BASE TOGI_TIMEOUT TOGI_FORMAT TOGI_TEST_CMD TOGI_REPORT_PATH TOGI_BIN TOGI_EXPECTED_VERSION
  "$togi_bin" "$@"
)

if ! version_output="$(run_togi --version)"; then
  echo "Could not verify the installed Togi binary version." >&2
  exit 2
fi
if [[ "$version_output" != "togi ${expected_tag#v}" ]]; then
  echo "Installed Togi version does not match expected ${expected_tag}: ${version_output}" >&2
  exit 2
fi

if ! check_help="$(run_togi help check 2>&1)"; then
  echo "Could not verify that the installed Togi binary supports --json-report." >&2
  exit 2
fi
if [[ "$check_help" != *"--json-report"* ]]; then
  echo "The installed Togi binary does not support --json-report." >&2
  exit 2
fi

if ! command -v jq >/dev/null 2>&1; then
  echo "jq is required to generate Action report outputs" >&2
  exit 2
fi

report_output_path="${TOGI_REPORT_PATH:-${RUNNER_TEMP:-.}/togi-report.json}"
report_path="$report_output_path"
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

if [[ -n "${TOGI_BASE:-}" ]]; then
  review_args+=(--base "$TOGI_BASE")
fi

if [[ -n "${TOGI_TIMEOUT:-}" ]]; then
  review_args+=(--timeout "$TOGI_TIMEOUT")
fi

if [[ -n "${TOGI_FORMAT:-}" ]]; then
  review_args+=(--format "$TOGI_FORMAT")
fi

if [[ -n "${TOGI_TEST_CMD:-}" ]]; then
  review_args+=(--test-cmd "$TOGI_TEST_CMD")
fi
review_args+=(--json-report "$report_path")

run_togi "${review_args[@]}"
review_status=$?
case "$review_status" in
  0|1)
    ;;
  *)
    rm -f "$report_path"
    exit "$review_status"
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
  printf 'report-path=%s\n' "$report_output_path"
  printf 'mutation-score=%s\n' "$mutation_score"
  printf 'survivor-count=%s\n' "$survivor_count"
} >>"$GITHUB_OUTPUT"; then
  echo "Could not write Action outputs" >&2
  rm -f "$report_path"
  exit 2
fi

exit "$review_status"
