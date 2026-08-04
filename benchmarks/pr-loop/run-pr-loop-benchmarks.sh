#!/usr/bin/env bash
# PR-loop benchmark harness for togi (issue #487-A).
#
# Copies tests/fixtures/go into a disposable temp project, initializes a local
# Git history, applies one fixed working-tree patch, and runs the workloads
# declared in manifest.json through `togi check --base HEAD` so every
# measurement represents a PR diff, never --all.
#
# Semantic/provenance invariant failures fail the harness (exit 1). Timing is
# recorded for observation only; nothing here compares against a baseline.
#
# Usage: run-pr-loop-benchmarks.sh [--output DIR] [--keep-workspace]
# Env:   TOGI_BIN  path to the togi binary (default: target/release/togi)
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
REPO_ROOT=$(cd "$SCRIPT_DIR/../.." && pwd)
MANIFEST=${BENCH_MANIFEST:-"$SCRIPT_DIR/manifest.json"}

OUT_DIR=""
KEEP_WORKSPACE=0

usage() {
  echo "Usage: $0 [--output DIR] [--keep-workspace]" >&2
  echo "  --output DIR       directory for raw reports and the normalized result" >&2
  echo "                     (default: a fresh dir under RUNNER_TEMP/TMPDIR)" >&2
  echo "  --keep-workspace   keep the disposable temp project for debugging" >&2
  echo "Env: TOGI_BIN (default: <repo>/target/release/togi)" >&2
  echo "     BENCH_MANIFEST (default: <repo>/benchmarks/pr-loop/manifest.json)" >&2
  echo "Requires: bash, git, go, jq, sed, sha256sum|shasum, and python3" >&2
  echo "          (python3 provides the monotonic high-resolution clock)" >&2
}

while [ $# -gt 0 ]; do
  case "$1" in
    --output)
      if [ $# -lt 2 ]; then
        echo "--output requires a directory argument" >&2
        usage
        exit 2
      fi
      case "$2" in
        -*)
          echo "--output requires a directory argument, got option '$2'" >&2
          usage
          exit 2
          ;;
      esac
      OUT_DIR=$2
      shift 2
      ;;
    --keep-workspace)
      KEEP_WORKSPACE=1
      shift
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage
      exit 2
      ;;
  esac
done

for tool in git go jq sed; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "$tool is required for the PR-loop benchmarks" >&2
    exit 1
  fi
done

# python3 supplies the monotonic high-resolution clock used for wall timings.
if ! command -v python3 >/dev/null 2>&1; then
  echo "python3 is required for monotonic wall timing in the PR-loop benchmarks" >&2
  exit 1
fi

if command -v sha256sum >/dev/null 2>&1; then
  SHA256_CMD="sha256sum"
elif command -v shasum >/dev/null 2>&1; then
  SHA256_CMD="shasum -a 256"
else
  echo "sha256sum or shasum is required for the PR-loop benchmarks" >&2
  exit 1
fi

sha256_stdin() {
  $SHA256_CMD | cut -d ' ' -f 1
}

sha256_file() {
  $SHA256_CMD "$1" | cut -d ' ' -f 1
}

# Milliseconds from python3's monotonic high-resolution clock. Portable
# across GNU and BSD userlands; wall-clock date math is not.
now_ms() {
  python3 -c 'import time; print(time.monotonic_ns() // 1_000_000)'
}

TOGI_BIN=${TOGI_BIN:-"$REPO_ROOT/target/release/togi"}
case "$TOGI_BIN" in
  /*) ;;
  *) TOGI_BIN="$REPO_ROOT/$TOGI_BIN" ;;
esac
if [ ! -x "$TOGI_BIN" ]; then
  echo "togi binary not found at $TOGI_BIN" >&2
  echo "build it with 'cargo build --locked --release' or set TOGI_BIN" >&2
  exit 1
fi

if [ ! -f "$MANIFEST" ]; then
  echo "manifest not found at $MANIFEST" >&2
  exit 2
fi

MANIFEST_SCHEMA=$(jq -r '.schema_version // "missing"' "$MANIFEST")
if [ "$MANIFEST_SCHEMA" != "1" ]; then
  echo "unsupported manifest schema_version $MANIFEST_SCHEMA (expected 1)" >&2
  exit 2
fi

# Full manifest shape validation, checked directly before any setup so a
# malformed or empty manifest can never yield a zero-workload success.
if ! jq -e '
  (.name | type == "string" and length > 0)
  and (.fixture | type == "object")
  and (.fixture.source_dir | type == "string" and length > 0)
  and (.fixture.patch_file | type == "string" and length > 0)
  and (.fixture.patch_sha256 | type == "string" and test("^[0-9a-f]{64}$"))
  and (.fixture.changed_file | type == "string" and length > 0)
  and (.fixture.changed_line_range | type == "array" and length == 2
       and ([.[] | type == "number"] | all)
       and (.[0] <= .[1]))
  and (.fixture.base_ref | type == "string" and length > 0)
  and (.fixture.expected_mutation_count | type == "number" and . > 0 and floor == .)
  and (.togi | type == "object")
  and (.togi.common_args | type == "array" and length > 0
       and ([.[] | type == "string"] | all))
' "$MANIFEST" >/dev/null; then
  echo "manifest $MANIFEST failed schema/provenance validation" >&2
  echo "required: name, fixture.{source_dir, patch_file, patch_sha256 (64-hex)," >&2
  echo "  changed_file, changed_line_range [min,max], base_ref," >&2
  echo "  expected_mutation_count > 0}, togi.common_args [non-empty strings]" >&2
  exit 2
fi

# Keep the known-invariant list here in sync with invariant_filter().
if ! jq -e '
  (.workloads | type == "array" and length == 4)
  and ([.workloads[] |
        ((.name | type == "string" and length > 0)
         and ((.cache // "") == "fresh" or (.cache // "") == "reuse")
         and (.extra_args | type == "array" and ([.[] | type == "string"] | all))
         and (.invariants | type == "array" and length > 0
              and (index("report-well-formed") != null)
              and ([.[] | type == "string"
                      and (. as $i | ["report-well-formed", "full-fresh-execution",
                                      "full-exact-cache-reuse",
                                      "schemata-fast-path-and-fallback",
                                      "pr-diff-targeting"] | index($i) != null)]
                   | all)))]
       | all)
' "$MANIFEST" >/dev/null; then
  echo "manifest $MANIFEST must declare exactly 4 workloads, each with name," >&2
  echo "  cache (fresh|reuse), extra_args [strings], and non-empty invariants" >&2
  echo "  drawn from the known list, always including report-well-formed" >&2
  exit 2
fi

if ! jq -e '
  ([.workloads[].name]
     == ["cold-regular", "warm-exact-cache", "cold-schemata", "pr-diff-default"])
  and (.workloads[0].cache == "fresh" and .workloads[0].seeds_cache == true)
  and (.workloads[1].cache == "reuse"
       and (.workloads[1].expects_cache_from // "") == "cold-regular")
' "$MANIFEST" >/dev/null; then
  echo "manifest $MANIFEST must order workloads cold-regular, warm-exact-cache," >&2
  echo "  cold-schemata, pr-diff-default, with warm-exact-cache reusing the cache" >&2
  echo "  seeded by cold-regular" >&2
  exit 2
fi

# PR-diff invocation contract: the benchmark only measures `check --base
# HEAD` on a working-tree diff. Every workload's effective argv (common_args
# + extra_args) is validated, so selector flags injected through either list
# are rejected before any run.
if ! jq -e '
  . as $m
  | ($m.fixture.base_ref == "HEAD")
    and ([range(0; ($m.workloads | length)) as $w
          | ($m.togi.common_args + $m.workloads[$w].extra_args) as $argv
          | ($argv[0] == "check")
            and ([$argv[] | select(. == "--all")] | length == 0)
            and ([$argv[] | select(. == "--base")] | length == 1)
            and ([$argv[] | select(startswith("--base="))] | length == 0)
            and ((($argv | index("--base")) + 1) as $i
                 | $i < ($argv | length) and $argv[$i] == "HEAD")
         ] | all)
' "$MANIFEST" >/dev/null; then
  echo "manifest $MANIFEST violates the PR-diff invocation contract:" >&2
  echo "  fixture.base_ref must be HEAD, and every workload's effective argv" >&2
  echo "  (togi.common_args + workloads[].extra_args) must start with 'check'," >&2
  echo "  pass exactly one split '--base HEAD' pair (no --base= form, no" >&2
  echo "  duplicates), and must not contain '--all'" >&2
  exit 2
fi

EXPECTED_MUTATIONS=$(jq -r '.fixture.expected_mutation_count' "$MANIFEST")
CHANGED_FILE=$(jq -r '.fixture.changed_file' "$MANIFEST")
LINE_MIN=$(jq -r '.fixture.changed_line_range[0]' "$MANIFEST")
LINE_MAX=$(jq -r '.fixture.changed_line_range[1]' "$MANIFEST")
FIXTURE_SOURCE_DIR=$(jq -r '.fixture.source_dir' "$MANIFEST")
EXPECTED_PATCH_SHA=$(jq -r '.fixture.patch_sha256' "$MANIFEST")
PATCH_FILE="$REPO_ROOT/$(jq -r '.fixture.patch_file' "$MANIFEST")"
WORKLOAD_COUNT=$(jq -r '.workloads | length' "$MANIFEST")

ACTUAL_PATCH_SHA=$(sha256_file "$PATCH_FILE")
if [ "$ACTUAL_PATCH_SHA" != "$EXPECTED_PATCH_SHA" ]; then
  echo "fixture patch digest mismatch:" >&2
  echo "  manifest: $EXPECTED_PATCH_SHA" >&2
  echo "  actual:   $ACTUAL_PATCH_SHA" >&2
  echo "update manifest.json deliberately if the patch changed" >&2
  exit 1
fi

COMMON_ARGS=()
while IFS= read -r arg; do
  COMMON_ARGS+=("$arg")
done < <(jq -r '.togi.common_args[]' "$MANIFEST")

if [ -z "$OUT_DIR" ]; then
  OUT_DIR=$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/togi-pr-loop-benchmarks.XXXXXX")
fi
mkdir -p "$OUT_DIR/raw"
OUT_DIR=$(cd "$OUT_DIR" && pwd)

WORK_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/togi-pr-loop-work.XXXXXX")
PROJECT_DIR="$WORK_ROOT/project"
cleanup() {
  if [ "$KEEP_WORKSPACE" = "1" ]; then
    echo "workspace kept: $WORK_ROOT" >&2
  else
    rm -rf "$WORK_ROOT"
  fi
}
trap cleanup EXIT

# Disposable temp project: fixture copy, local Git history, one fixed
# working-tree patch applied on top of the base commit.
mkdir -p "$PROJECT_DIR"
cp -R "$REPO_ROOT/$FIXTURE_SOURCE_DIR/." "$PROJECT_DIR/"
git -C "$PROJECT_DIR" -c init.defaultBranch=main init -q
git -C "$PROJECT_DIR" config user.email "togi-pr-loop-bench@example.invalid"
git -C "$PROJECT_DIR" config user.name "togi-pr-loop-bench"
git -C "$PROJECT_DIR" config commit.gpgsign false
git -C "$PROJECT_DIR" add .
git -C "$PROJECT_DIR" commit -qm "fixture base"
git -C "$PROJECT_DIR" apply "$PATCH_FILE"
BASE_REVISION=$(git -C "$PROJECT_DIR" rev-parse HEAD)

TOGI_VERSION=$("$TOGI_BIN" --version)
GO_VERSION=$(go version)
GIT_VERSION=$(git --version)
OS_NAME=$(uname -s)
ARCH_NAME=$(uname -m)
STARTED_AT=$(date -u +%Y-%m-%dT%H:%M:%SZ)

# jq filter for each named invariant. Manifest values are injected as
# $expected_mutations, $changed_file, $line_min, and $line_max.
invariant_filter() {
  case "$1" in
    report-well-formed)
      cat <<'EOF'
(.kind == "mutation_report")
and (.schema_version == 1)
and ((.generator // "") | startswith("togi/"))
and (.partial == false)
and (.planned_total == .total)
and (.total == $expected_mutations)
and (.mutations | type == "array")
and ((.mutations | length) == .total)
and (.timeout == 0)
and (.build_errors == 0)
and (.duration_ms > 0)
and ((.test_command // []) == ["go", "test", "./..."])
EOF
      ;;
    full-fresh-execution)
      cat <<'EOF'
(.tested == .total)
and ((.exact_cache_reused // 0) == 0)
and ((.incremental_history_reused // 0) == 0)
and ([.mutations[].execution.state == "executed"] | all)
EOF
      ;;
    full-exact-cache-reuse)
      cat <<'EOF'
(.tested == 0)
and ((.exact_cache_reused // 0) == .total)
and ([.mutations[].execution.state == "exact_cache"] | all)
EOF
      ;;
    schemata-fast-path-and-fallback)
      cat <<'EOF'
(.schemata != null)
and (.schemata.fast_path >= 1)
and (.schemata.fallback >= 1)
and ((.schemata.fast_path + .schemata.fallback) == .total)
EOF
      ;;
    pr-diff-targeting)
      cat <<'EOF'
(([.mutations[].file] | unique) == [$changed_file])
and ([.mutations[].line | (. >= $line_min and . <= $line_max)] | all)
EOF
      ;;
    *)
      return 1
      ;;
  esac
}

FRAGMENTS=()
FAILURES=()
REFERENCE_DIGEST=""
DIGESTS_CONSISTENT=1
OVERALL_OK=1

while IFS= read -r workload; do
  WL_NAME=$(jq -r '.name' <<<"$workload")
  CACHE_POLICY=$(jq -r '.cache' <<<"$workload")

  EXTRA_ARGS=()
  while IFS= read -r arg; do
    EXTRA_ARGS+=("$arg")
  done < <(jq -r '.extra_args[]' <<<"$workload")

  CMD=("$TOGI_BIN")
  if [ ${#COMMON_ARGS[@]} -gt 0 ]; then
    CMD+=("${COMMON_ARGS[@]}")
  fi
  if [ ${#EXTRA_ARGS[@]} -gt 0 ]; then
    CMD+=("${EXTRA_ARGS[@]}")
  fi

  case "$CACHE_POLICY" in
    fresh)
      rm -rf "$PROJECT_DIR/.togi-cache"
      ;;
    reuse)
      ;;
    *)
      echo "unknown cache policy '$CACHE_POLICY' for workload $WL_NAME" >&2
      exit 2
      ;;
  esac

  RAW_STDOUT="$OUT_DIR/raw/$WL_NAME.stdout"
  RAW_STDERR="$OUT_DIR/raw/$WL_NAME.stderr"
  REPORT_JSON="$OUT_DIR/raw/$WL_NAME.report.json"

  echo "running workload $WL_NAME" >&2
  START_MS=$(now_ms)
  set +e
  ( cd "$PROJECT_DIR" && "${CMD[@]}" ) >"$RAW_STDOUT" 2>"$RAW_STDERR"
  STATUS=$?
  set -e
  WALL_MS=$(( $(now_ms) - START_MS ))

  REPORT_OK=0
  if [ "$STATUS" != "0" ] && [ "$STATUS" != "1" ]; then
    echo "workload $WL_NAME: togi exited with unexpected status $STATUS" >&2
  elif sed -n '/^{/,$p' "$RAW_STDOUT" > "$REPORT_JSON" \
    && [ -s "$REPORT_JSON" ] \
    && jq -e . "$REPORT_JSON" >/dev/null 2>&1; then
    REPORT_OK=1
  else
    echo "workload $WL_NAME: could not extract a JSON report from togi stdout" >&2
  fi

  SEMANTICS="null"
  DIGEST=""
  if [ "$REPORT_OK" = "1" ]; then
    SEMANTICS=$(jq -c '{
      total, planned_total, tested, killed, survived, timeout, build_errors,
      uncovered: (.uncovered // 0),
      subsumed: (.subsumed // 0),
      exact_cache_reused: (.exact_cache_reused // 0),
      incremental_history_reused: (.incremental_history_reused // 0),
      partial,
      reported_duration_ms: .duration_ms,
      schemata: (.schemata // null),
      mutation_count: (.mutations | length)
    }' "$REPORT_JSON")
    DIGEST=$(jq -r '[.mutations[]
      | "\(.file):\(.line):\(.column // 0):\(.operator):\(.original // "")->\(.replacement // "")"]
      | sort | .[]' "$REPORT_JSON" | sha256_stdin)
    if [ -z "$REFERENCE_DIGEST" ]; then
      REFERENCE_DIGEST=$DIGEST
    elif [ "$DIGEST" != "$REFERENCE_DIGEST" ]; then
      DIGESTS_CONSISTENT=0
      FAILURES+=("$WL_NAME:mutation-identity-drift")
    fi
  fi

  INVARIANTS_JSON="[]"
  WL_OK=1
  while IFS= read -r invariant; do
    if ! FILTER=$(invariant_filter "$invariant"); then
      echo "unknown invariant '$invariant' declared by workload $WL_NAME" >&2
      exit 2
    fi
    INV_OK="false"
    if [ "$REPORT_OK" = "1" ] \
      && jq -e \
        --argjson expected_mutations "$EXPECTED_MUTATIONS" \
        --arg changed_file "$CHANGED_FILE" \
        --argjson line_min "$LINE_MIN" \
        --argjson line_max "$LINE_MAX" \
        "$FILTER" "$REPORT_JSON" >/dev/null; then
      INV_OK="true"
    else
      WL_OK=0
      FAILURES+=("$WL_NAME:$invariant")
    fi
    INVARIANTS_JSON=$(jq -c --arg name "$invariant" --argjson ok "$INV_OK" \
      '. + [{name: $name, ok: $ok}]' <<<"$INVARIANTS_JSON")
  done < <(jq -r '.invariants[]' <<<"$workload")

  if [ "$REPORT_OK" != "1" ]; then
    WL_OK=0
    FAILURES+=("$WL_NAME:report-extraction")
  fi
  if [ "$WL_OK" != "1" ]; then
    OVERALL_OK=0
  fi

  CMD_JSON=$(printf '%s\n' "${CMD[@]}" | jq -Rn '[inputs]')

  FRAGMENT="$WORK_ROOT/workload-$WL_NAME.json"
  jq -n \
    --arg name "$WL_NAME" \
    --arg cache_policy "$CACHE_POLICY" \
    --argjson command "$CMD_JSON" \
    --argjson exit_status "$STATUS" \
    --argjson wall_ms "$WALL_MS" \
    --argjson report_ok "$REPORT_OK" \
    --argjson semantics "$SEMANTICS" \
    --arg digest "$DIGEST" \
    --argjson invariants "$INVARIANTS_JSON" \
    --argjson ok "$WL_OK" \
    --arg raw_stdout "raw/$WL_NAME.stdout" \
    --arg raw_stderr "raw/$WL_NAME.stderr" \
    --arg report "raw/$WL_NAME.report.json" \
    '{
      name: $name,
      cache_policy: $cache_policy,
      command: $command,
      exit_status: $exit_status,
      timing: {
        wall_ms: $wall_ms,
        reported_duration_ms: (
          if $report_ok == 1 then $semantics.reported_duration_ms else null end
        )
      },
      semantics: (
        if $report_ok == 1
        then $semantics + {mutation_identity_sha256: $digest}
        else null
        end
      ),
      artifacts: {
        raw_stdout: $raw_stdout,
        raw_stderr: $raw_stderr,
        report: (if $report_ok == 1 then $report else null end)
      },
      invariants: $invariants,
      ok: ($ok == 1)
    }' > "$FRAGMENT"
  FRAGMENTS+=("$FRAGMENT")
done < <(jq -c '.workloads[]' "$MANIFEST")

# Guard against any loop truncation the pre-flight validation cannot see:
# a result with fewer workloads than the manifest declares is never success.
if [ "${#FRAGMENTS[@]}" -ne "$WORKLOAD_COUNT" ]; then
  echo "harness error: ran ${#FRAGMENTS[@]} of $WORKLOAD_COUNT declared workloads" >&2
  exit 2
fi

if [ "$DIGESTS_CONSISTENT" != "1" ]; then
  OVERALL_OK=0
fi
if [ "$DIGESTS_CONSISTENT" = "1" ]; then
  DIGESTS_CONSISTENT_JSON="true"
else
  DIGESTS_CONSISTENT_JSON="false"
fi
if [ "$OVERALL_OK" = "1" ]; then
  OVERALL_OK_JSON="true"
else
  OVERALL_OK_JSON="false"
fi

FAILURES_JSON="[]"
if [ ${#FAILURES[@]} -gt 0 ]; then
  FAILURES_JSON=$(printf '%s\n' "${FAILURES[@]}" | jq -Rn '[inputs]')
fi

META_JSON="$WORK_ROOT/meta.json"
jq -n \
  --arg togi_version "$TOGI_VERSION" \
  --arg togi_bin "$TOGI_BIN" \
  --arg go_version "$GO_VERSION" \
  --arg git_version "$GIT_VERSION" \
  --arg os "$OS_NAME" \
  --arg arch "$ARCH_NAME" \
  --arg started_at "$STARTED_AT" \
  --arg fixture_source "$FIXTURE_SOURCE_DIR" \
  --arg patch_sha "$ACTUAL_PATCH_SHA" \
  --arg base_revision "$BASE_REVISION" \
  '{
    kind: "togi_pr_loop_benchmark_result",
    schema_version: 1,
    timing_policy: "observational-only",
    manifest: {
      name: "togi-pr-loop-benchmarks",
      schema_version: 1,
      path: "benchmarks/pr-loop/manifest.json"
    },
    provenance: {
      togi_version: $togi_version,
      togi_binary: $togi_bin,
      report_kind: "mutation_report",
      report_schema_version: 1,
      os: $os,
      arch: $arch,
      go_version: $go_version,
      git_version: $git_version,
      fixture_source_dir: $fixture_source,
      fixture_patch: "benchmarks/pr-loop/fixture-change.patch",
      fixture_patch_sha256: $patch_sha,
      fixture_base_revision: $base_revision,
      started_at_utc: $started_at
    }
  }' > "$META_JSON"

RESULT_JSON="$OUT_DIR/pr-loop-benchmark-result.json"
jq -s \
  --arg digest "$REFERENCE_DIGEST" \
  --argjson consistent "$DIGESTS_CONSISTENT_JSON" \
  --argjson ok "$OVERALL_OK_JSON" \
  --argjson failures "$FAILURES_JSON" \
  '.[0] + {
    workloads: .[1:],
    cross_workload: {
      mutation_identity_consistent: $consistent,
      mutation_identity_sha256: (if $consistent then $digest else null end)
    },
    ok: $ok,
    failures: $failures
  }' "$META_JSON" "${FRAGMENTS[@]}" > "$RESULT_JSON"

jq -r '.workloads[]
  | "\(.name): exit=\(.exit_status) wall=\(.timing.wall_ms)ms reported=\(.timing.reported_duration_ms // 0)ms ok=\(.ok)"' \
  "$RESULT_JSON"
echo "normalized result: $RESULT_JSON"
echo "raw reports: $OUT_DIR/raw"

if [ "$OVERALL_OK" != "1" ]; then
  echo "PR-loop benchmark invariants failed:" >&2
  jq -r '.failures[] | "  - \(.)"' "$RESULT_JSON" >&2
  exit 1
fi
echo "all PR-loop benchmark invariants passed"
