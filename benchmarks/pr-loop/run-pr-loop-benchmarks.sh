#!/usr/bin/env bash
# PR-loop benchmark harness for togi (issue #487-A).
#
# Copies tests/fixtures/go into one disposable temp project per scenario,
# initializes a local Git history, applies the scenario's fixed working-tree
# patch, and runs the workloads declared in manifest.json through
# `togi check --base HEAD` so every measurement represents a PR diff, never
# --all.
#
# Semantic/provenance invariant failures fail the harness (exit 1). Timing is
# recorded for observation only; nothing here compares against a baseline.
#
# Go build cache provenance: when BENCH_GO_BUILD_CACHE_STATE is warmup or
# primed, the harness requires an absolute GOCACHE exactly matching
# `go env GOCACHE` (exit 2 otherwise) and records the resolved path plus the
# job-private-explicit-gocache policy in provenance. The default unclassified
# state makes no cache requirement and stays observational.
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
  echo "  --keep-workspace   keep the disposable temp projects for debugging" >&2
  echo "Env: TOGI_BIN (default: <repo>/target/release/togi)" >&2
  echo "     BENCH_MANIFEST (default: <repo>/benchmarks/pr-loop/manifest.json)" >&2
  echo "     BENCH_GO_BUILD_CACHE_STATE (unclassified|warmup|primed;" >&2
  echo "          warmup/primed require an absolute GOCACHE matching" >&2
  echo "          'go env GOCACHE' and an existing cache directory)" >&2
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
if [ "$MANIFEST_SCHEMA" != "2" ]; then
  echo "unsupported manifest schema_version $MANIFEST_SCHEMA (expected 2)" >&2
  exit 2
fi

# Full manifest shape validation, checked directly before any setup so a
# malformed or empty manifest can never yield a zero-workload success.
if ! jq -e '
  (.name | type == "string" and length > 0)
  and (.fixture | type == "object")
  and (.fixture.source_dir | type == "string" and length > 0)
  and (.fixture.base_ref | type == "string" and length > 0)
  and (.scenarios | type == "array" and length > 0
       and (([.[].name] | unique | length) == length)
       and ([.[] |
             ((.name | type == "string" and length > 0)
              and (.patch_file | type == "string" and length > 0)
              and (.patch_sha256 | type == "string" and test("^[0-9a-f]{64}$"))
              and (.changed_files | type == "array" and length > 0
                   and ([.[] |
                        ((.path | type == "string" and length > 0)
                         and (.line_range | type == "array" and length == 2
                              and ([.[] | type == "number"] | all)
                              and (.[0] <= .[1])))]
                       | all))
              and ((has("requires_mutation_per_changed_file") | not) or (.requires_mutation_per_changed_file | type == "boolean"))
              and (.expected_mutation_count | type == "number" and . > 0 and floor == .))]
            | all))
  and (.togi | type == "object")
  and (.togi.common_args | type == "array" and length > 0
       and ([.[] | type == "string"] | all))
' "$MANIFEST" >/dev/null; then
  echo "manifest $MANIFEST failed schema/provenance validation" >&2
  echo "  [{path, line_range [min,max]}], optional requires_mutation_per_changed_file," >&2
  echo "  expected_mutation_count > 0}]," >&2
  echo "  togi.common_args [non-empty strings]" >&2
  exit 2
fi

# Keep the known-invariant list here in sync with invariant_filter().
if ! jq -e '
  (.workloads | type == "array" and length == 6)
  and ([.workloads[] |
        ((.name | type == "string" and length > 0)
         and (.scenario | type == "string" and length > 0)
         and ((.runner_mode // "") == "regular"
              or (.runner_mode // "") == "schemata"
              or (.runner_mode // "") == "default")
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
  and (. as $m
       | [.workloads[].scenario]
       | all(. as $s | [$m.scenarios[].name] | index($s) != null))
' "$MANIFEST" >/dev/null; then
  echo "manifest $MANIFEST must declare exactly 6 workloads, each with name," >&2
  echo "  scenario (a declared scenario), runner_mode (regular|schemata|default)," >&2
  echo "  cache (fresh|reuse), extra_args [strings], and non-empty invariants" >&2
  echo "  drawn from the known list, always including report-well-formed" >&2
  exit 2
fi

if ! jq -e '
  ([.scenarios[].name] == ["single-file", "multi-file"])
  and ([.workloads[].name]
       == ["cold-regular", "warm-exact-cache", "cold-schemata", "pr-diff-default",
           "multi-file-regular", "multi-file-default"])
  and ([.workloads[].scenario]
       == ["single-file", "single-file", "single-file", "single-file",
           "multi-file", "multi-file"])
  and ([.workloads[].runner_mode]
       == ["regular", "regular", "schemata", "default", "regular", "default"])
  and (.workloads[0].cache == "fresh" and .workloads[0].seeds_cache == true)
  and (.workloads[1].cache == "reuse"
       and (.workloads[1].expects_cache_from // "") == "cold-regular")
  and (.workloads[4].cache == "fresh" and .workloads[4].seeds_cache == true)
' "$MANIFEST" >/dev/null; then
  echo "manifest $MANIFEST must order workloads cold-regular, warm-exact-cache," >&2
  echo "  cold-schemata, pr-diff-default (single-file), then multi-file-regular," >&2
  echo "  multi-file-default (multi-file), with warm-exact-cache reusing the" >&2
  echo "  cache seeded by cold-regular inside the single-file scenario" >&2
  exit 2
fi

# Cache dependencies never cross scenarios: expects_cache_from may only name
# an earlier workload in the same scenario that seeds its cache.
if ! jq -e '
  . as $m
  | [.workloads | to_entries[]
     | select(.value.expects_cache_from != null)
     | . as $edge
     | ([$m.workloads[0:$edge.key][]
         | select(.name == $edge.value.expects_cache_from
                  and .scenario == $edge.value.scenario
                  and (.seeds_cache // false))]
        | length == 1)]
  | all
' "$MANIFEST" >/dev/null; then
  echo "manifest $MANIFEST violates the cache dependency contract:" >&2
  echo "  expects_cache_from must name exactly one earlier workload in the" >&2
  echo "  same scenario that declares seeds_cache" >&2
  exit 2
fi

# runner_mode must match the effective argv of every workload: regular pins
# --no-schemata, schemata pins --schemata, default pins neither, and the
# selected test command must come from exactly one --test-cmd pair.
if ! jq -e '
  . as $m
  | ([$m.togi.common_args[] | select(. == "--test-cmd")] | length == 1)
    and (([$m.togi.common_args | to_entries[] | select(.value == "--test-cmd") | .key][0]) as $i
         | ($i + 1) < ($m.togi.common_args | length)
           and ($m.togi.common_args[$i + 1] | length > 0))
    and ([range(0; ($m.workloads | length)) | . as $w
          | ($m.togi.common_args + $m.workloads[$w].extra_args) as $argv
          | ([$argv[] | select(. == "--test-cmd")] | length) as $cmds
          | ([$argv[] | select(. == "--no-schemata")] | length) as $no
          | ([$argv[] | select(. == "--schemata")] | length) as $yes
          | ($cmds == 1)
            and (if $m.workloads[$w].runner_mode == "regular"
                 then ($no == 1 and $yes == 0)
                 elif $m.workloads[$w].runner_mode == "schemata"
                 then ($yes == 1 and $no == 0)
                 else ($yes == 0 and $no == 0) end)]
         | all)
' "$MANIFEST" >/dev/null; then
  echo "manifest $MANIFEST violates the runner_mode/test-command contract:" >&2
  echo "  regular requires exactly one --no-schemata and no --schemata," >&2
  echo "  schemata requires exactly one --schemata and no --no-schemata," >&2
  echo "  default requires neither, and every effective argv must carry" >&2
  echo "  exactly one split --test-cmd pair" >&2
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

FIXTURE_SOURCE_DIR=$(jq -r '.fixture.source_dir' "$MANIFEST")
WORKLOAD_COUNT=$(jq -r '.workloads | length' "$MANIFEST")
SCENARIO_COUNT=$(jq -r '.scenarios | length' "$MANIFEST")

# The selected test command every workload must resolve to, derived from the
# single --test-cmd pair in common_args.
TEST_CMD_INDEX=$(jq -r '[.togi.common_args | to_entries[] | select(.value == "--test-cmd") | .key][0]' "$MANIFEST")
TEST_CMD_VALUE=$(jq -r --argjson i "$TEST_CMD_INDEX" '.togi.common_args[$i + 1]' "$MANIFEST")
EXPECTED_TEST_COMMAND_JSON=$(printf '%s' "$TEST_CMD_VALUE" | tr ' ' '\n' | sed '/^$/d' | jq -Rn '[inputs]')

# Scenario state is kept in parallel indexed arrays (bash 3.2 compatible);
# scenario_index maps a scenario name to its slot.
declare -a SCENARIO_ORDER=()
declare -a SCENARIO_PATCH_FILE=()
declare -a SCENARIO_PATCH_SHA=()
declare -a SCENARIO_REQUIRE_PER_CHANGED_FILE=()
declare -a SCENARIO_EXPECTED=()
declare -a SCENARIO_CHANGED_FILES=()

scenario_index() {
  local i
  for i in "${!SCENARIO_ORDER[@]}"; do
    if [ "${SCENARIO_ORDER[$i]}" = "$1" ]; then
      printf '%s\n' "$i"
      return 0
    fi
  done
  return 1
}

while IFS= read -r scenario; do
  S_NAME=$(jq -r '.name' <<<"$scenario")
  SCENARIO_ORDER+=("$S_NAME")
  SCENARIO_PATCH_FILE+=("$REPO_ROOT/$(jq -r '.patch_file' <<<"$scenario")")
  SCENARIO_PATCH_SHA+=($(jq -r '.patch_sha256' <<<"$scenario"))
  SCENARIO_REQUIRE_PER_CHANGED_FILE+=($(jq -r '.requires_mutation_per_changed_file // false' <<<"$scenario"))
  SCENARIO_EXPECTED+=($(jq -r '.expected_mutation_count' <<<"$scenario"))
  SCENARIO_CHANGED_FILES+=("$(jq -c '.changed_files' <<<"$scenario")")
done < <(jq -c '.scenarios[]' "$MANIFEST")

# Every scenario patch digest is verified before any run.
for i in "${!SCENARIO_ORDER[@]}"; do
  ACTUAL_PATCH_SHA=$(sha256_file "${SCENARIO_PATCH_FILE[$i]}")
  if [ "$ACTUAL_PATCH_SHA" != "${SCENARIO_PATCH_SHA[$i]}" ]; then
    echo "fixture patch digest mismatch for scenario ${SCENARIO_ORDER[$i]}:" >&2
    echo "  manifest: ${SCENARIO_PATCH_SHA[$i]}" >&2
    echo "  actual:   $ACTUAL_PATCH_SHA" >&2
    echo "update manifest.json deliberately if the patch changed" >&2
    exit 1
  fi
done

COMMON_ARGS=()
while IFS= read -r arg; do
  COMMON_ARGS+=("$arg")
done < <(jq -r '.togi.common_args[]' "$MANIFEST")

# Go build cache provenance contract. warmup and primed measurements must
# run against a job-private explicit GOCACHE; unclassified local runs make
# no cache requirement and stay observational.
BENCH_GO_BUILD_CACHE_STATE=${BENCH_GO_BUILD_CACHE_STATE:-unclassified}
case "$BENCH_GO_BUILD_CACHE_STATE" in
  warmup|primed)
    if [ -z "${GOCACHE:-}" ]; then
      echo "BENCH_GO_BUILD_CACHE_STATE=$BENCH_GO_BUILD_CACHE_STATE requires GOCACHE to be set" >&2
      exit 2
    fi
    case "$GOCACHE" in
      /*) ;;
      *)
        echo "GOCACHE must be an absolute path for $BENCH_GO_BUILD_CACHE_STATE measurements, got '$GOCACHE'" >&2
        exit 2
        ;;
    esac
    if [ ! -d "$GOCACHE" ]; then
      echo "GOCACHE directory $GOCACHE does not exist for $BENCH_GO_BUILD_CACHE_STATE measurements" >&2
      exit 2
    fi
    RESOLVED_GOCACHE=$(go env GOCACHE) || {
      echo "go env GOCACHE failed for $BENCH_GO_BUILD_CACHE_STATE measurements" >&2
      exit 2
    }
    if [ "$RESOLVED_GOCACHE" != "$GOCACHE" ]; then
      echo "GOCACHE ($GOCACHE) does not match 'go env GOCACHE' ($RESOLVED_GOCACHE)" >&2
      exit 2
    fi
    GO_BUILD_CACHE_POLICY="job-private-explicit-gocache"
    GO_BUILD_CACHE_PATH_JSON=$(jq -n --arg path "$RESOLVED_GOCACHE" '$path')
    ;;
  unclassified)
    GO_BUILD_CACHE_POLICY="unenforced"
    GO_BUILD_CACHE_PATH_JSON="null"
    ;;
  *)
    echo "unknown BENCH_GO_BUILD_CACHE_STATE '$BENCH_GO_BUILD_CACHE_STATE'" >&2
    echo "expected unclassified, warmup, or primed" >&2
    exit 2
    ;;
esac

if [ -z "$OUT_DIR" ]; then
  OUT_DIR=$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/togi-pr-loop-benchmarks.XXXXXX")
fi
mkdir -p "$OUT_DIR/raw"
OUT_DIR=$(cd "$OUT_DIR" && pwd)

WORK_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/togi-pr-loop-work.XXXXXX")
cleanup() {
  if [ "$KEEP_WORKSPACE" = "1" ]; then
    echo "workspace kept: $WORK_ROOT" >&2
  else
    rm -rf "$WORK_ROOT"
  fi
}
trap cleanup EXIT

# One disposable temp project per scenario: fixture copy, local Git history,
# the scenario's fixed working-tree patch applied on top of the base commit.
declare -a SCENARIO_PROJECT=()
declare -a SCENARIO_BASE_REVISION=()
for i in "${!SCENARIO_ORDER[@]}"; do
  S_PROJECT="$WORK_ROOT/project-${SCENARIO_ORDER[$i]}"
  mkdir -p "$S_PROJECT"
  cp -R "$REPO_ROOT/$FIXTURE_SOURCE_DIR/." "$S_PROJECT/"
  git -C "$S_PROJECT" -c init.defaultBranch=main init -q
  git -C "$S_PROJECT" config user.email "togi-pr-loop-bench@example.invalid"
  git -C "$S_PROJECT" config user.name "togi-pr-loop-bench"
  git -C "$S_PROJECT" config commit.gpgsign false
  git -C "$S_PROJECT" add .
  git -C "$S_PROJECT" commit -qm "fixture base"
  git -C "$S_PROJECT" apply "${SCENARIO_PATCH_FILE[$i]}"
  SCENARIO_PROJECT+=("$S_PROJECT")
  SCENARIO_BASE_REVISION+=($(git -C "$S_PROJECT" rev-parse HEAD))
done

TOGI_VERSION=$("$TOGI_BIN" --version)
GO_VERSION=$(go version)
GIT_VERSION=$(git --version)
OS_NAME=$(uname -s)
ARCH_NAME=$(uname -m)
STARTED_AT=$(date -u +%Y-%m-%dT%H:%M:%SZ)
BENCH_RUNNER_LABEL=${BENCH_RUNNER_LABEL:-local}
if command -v getconf >/dev/null 2>&1; then
  LOGICAL_CPU_COUNT=$(getconf _NPROCESSORS_ONLN)
elif command -v sysctl >/dev/null 2>&1; then
  LOGICAL_CPU_COUNT=$(sysctl -n hw.logicalcpu)
else
  LOGICAL_CPU_COUNT=$(nproc)
fi
KERNEL_RELEASE=$(uname -r)
IMAGE_OS=${ImageOS:-}
IMAGE_VERSION=${ImageVersion:-}

# jq filter for each named invariant. Scenario values are injected as
# $expected_mutations, $changed_files, and $requires_per_changed_file; the
# manifest's selected test command is injected as $expected_test_command.
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
and ((.test_command // []) == $expected_test_command)
and ([.mutations[] | ((.test_selection? // {"mode": "full"}).mode == "full")] | all)
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
([.mutations[]
  | .file as $file
  | .line as $line
  | ([$changed_files[]
      | select(.path == $file
               and $line >= .line_range[0]
               and $line <= .line_range[1])]
     | length >= 1)]
 | all)
and (if $requires_per_changed_file then
       ([.mutations[].file] | unique | sort)
       == ([$changed_files[].path] | unique | sort)
     else true end)
EOF
      ;;
    *)
      return 1
      ;;
  esac
}

FRAGMENTS=()
FAILURES=()
declare -a SCENARIO_REF_DIGEST=()
declare -a SCENARIO_CONSISTENT=()
for i in "${!SCENARIO_ORDER[@]}"; do
  SCENARIO_REF_DIGEST+=("")
  SCENARIO_CONSISTENT+=(1)
done
OVERALL_OK=1

while IFS= read -r workload; do
  WL_NAME=$(jq -r '.name' <<<"$workload")
  WL_SCENARIO=$(jq -r '.scenario' <<<"$workload")
  RUNNER_MODE=$(jq -r '.runner_mode' <<<"$workload")
  CACHE_POLICY=$(jq -r '.cache' <<<"$workload")
  WL_SCENARIO_INDEX=$(scenario_index "$WL_SCENARIO") || {
    echo "workload $WL_NAME names undeclared scenario '$WL_SCENARIO'" >&2
    exit 2
  }
  PROJECT_DIR="${SCENARIO_PROJECT[$WL_SCENARIO_INDEX]}"
  EXPECTED_MUTATIONS="${SCENARIO_EXPECTED[$WL_SCENARIO_INDEX]}"
  CHANGED_FILES_JSON="${SCENARIO_CHANGED_FILES[$WL_SCENARIO_INDEX]}"

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

  echo "running workload $WL_NAME (scenario $WL_SCENARIO)" >&2
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
  RUNNER_MODE_OK=1
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
      selected_test_command: (.test_command // null),
      test_selection: {
        mode: (if ([.mutations[] | ((.test_selection? // {"mode": "full"}).mode == "full")] | all) then "full-suite" else "narrowed" end),
        full_suite_mutation_count: ([.mutations[] | select((.test_selection? // {"mode": "full"}).mode == "full")] | length),
        narrowed_mutation_count: ([.mutations[] | select((.test_selection? // {"mode": "full"}).mode != "full")] | length)
      },
      mutation_count: (.mutations | length)
    }' "$REPORT_JSON")
    DIGEST=$(jq -r '[.mutations[]
      | "\(.file):\(.line):\(.column // 0):\(.operator):\(.original // "")->\(.replacement // "")"]
      | sort | .[]' "$REPORT_JSON" | sha256_stdin)
    if [ -z "${SCENARIO_REF_DIGEST[$WL_SCENARIO_INDEX]}" ]; then
      SCENARIO_REF_DIGEST[$WL_SCENARIO_INDEX]=$DIGEST
    elif [ "$DIGEST" != "${SCENARIO_REF_DIGEST[$WL_SCENARIO_INDEX]}" ]; then
      SCENARIO_CONSISTENT[$WL_SCENARIO_INDEX]=0
      FAILURES+=("$WL_NAME:mutation-identity-drift")
    fi
    # runner_mode is manifest-declared and argv-validated; the report's
    # schemata evidence must agree with it. Default mode makes no assertion:
    # it deliberately inherits togi's own defaults.
    if [ "$RUNNER_MODE" = "schemata" ]; then
      if ! jq -e '.schemata != null' "$REPORT_JSON" >/dev/null; then
        RUNNER_MODE_OK=0
      fi
    elif [ "$RUNNER_MODE" = "regular" ]; then
      if ! jq -e '.schemata == null' "$REPORT_JSON" >/dev/null; then
        RUNNER_MODE_OK=0
      fi
    fi
    if [ "$RUNNER_MODE_OK" != "1" ]; then
      FAILURES+=("$WL_NAME:runner-mode-consistency")
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
        --argjson changed_files "$CHANGED_FILES_JSON" \
        --argjson requires_per_changed_file "${SCENARIO_REQUIRE_PER_CHANGED_FILE[$WL_SCENARIO_INDEX]}" \
        --argjson expected_test_command "$EXPECTED_TEST_COMMAND_JSON" \
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
  if [ "$RUNNER_MODE_OK" != "1" ]; then
    WL_OK=0
  fi
  if [ "$WL_OK" != "1" ]; then
    OVERALL_OK=0
  fi

  CMD_JSON=$(printf '%s\n' "${CMD[@]}" | jq -Rn '[inputs]')

  FRAGMENT="$WORK_ROOT/workload-$WL_NAME.json"
  jq -n \
    --arg name "$WL_NAME" \
    --arg scenario "$WL_SCENARIO" \
    --arg runner_mode "$RUNNER_MODE" \
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
      scenario: $scenario,
      runner_mode: $runner_mode,
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

# Per-scenario mutation identity: workloads of one scenario must agree, and
# scenarios legitimately differ from each other.
SCENARIOS_JSON="{}"
for i in "${!SCENARIO_ORDER[@]}"; do
  if [ "${SCENARIO_CONSISTENT[$i]}" != "1" ]; then
    OVERALL_OK=0
    S_CONSISTENT="false"
  else
    S_CONSISTENT="true"
  fi
  SCENARIOS_JSON=$(jq \
    --arg name "${SCENARIO_ORDER[$i]}" \
    --argjson consistent "$S_CONSISTENT" \
    --arg digest "${SCENARIO_REF_DIGEST[$i]}" \
    '. + {($name): {
      mutation_identity_consistent: $consistent,
      mutation_identity_sha256: (if $consistent and $digest != "" then $digest else null end)
    }}' <<<"$SCENARIOS_JSON")
done

if [ "$OVERALL_OK" = "1" ]; then
  OVERALL_OK_JSON="true"
else
  OVERALL_OK_JSON="false"
fi

FAILURES_JSON="[]"
if [ ${#FAILURES[@]} -gt 0 ]; then
  FAILURES_JSON=$(printf '%s\n' "${FAILURES[@]}" | jq -Rn '[inputs]')
fi

# Per-scenario fixture provenance: patch identity and base revision of each
# disposable project.
FIXTURE_SCENARIOS_JSON="{}"
for i in "${!SCENARIO_ORDER[@]}"; do
  FIXTURE_SCENARIOS_JSON=$(jq \
    --arg name "${SCENARIO_ORDER[$i]}" \
    --arg patch_file "${SCENARIO_PATCH_FILE[$i]#"$REPO_ROOT"/}" \
    --arg patch_sha "${SCENARIO_PATCH_SHA[$i]}" \
    --arg base_revision "${SCENARIO_BASE_REVISION[$i]}" \
    '. + {($name): {
      patch_file: $patch_file,
      patch_sha256: $patch_sha,
      base_revision: $base_revision
    }}' <<<"$FIXTURE_SCENARIOS_JSON")
done

META_JSON="$WORK_ROOT/meta.json"
jq -n \
  --arg togi_version "$TOGI_VERSION" \
  --arg togi_bin "$TOGI_BIN" \
  --arg go_version "$GO_VERSION" \
  --arg git_version "$GIT_VERSION" \
  --arg os "$OS_NAME" \
  --arg arch "$ARCH_NAME" \
  --arg runner_label "$BENCH_RUNNER_LABEL" \
  --argjson logical_cpu_count "$LOGICAL_CPU_COUNT" \
  --arg kernel_release "$KERNEL_RELEASE" \
  --arg image_os "$IMAGE_OS" \
  --arg image_version "$IMAGE_VERSION" \
  --arg go_build_cache_state "$BENCH_GO_BUILD_CACHE_STATE" \
  --arg go_build_cache_policy "$GO_BUILD_CACHE_POLICY" \
  --argjson go_build_cache_path "$GO_BUILD_CACHE_PATH_JSON" \
  --arg started_at "$STARTED_AT" \
  --arg fixture_source "$FIXTURE_SOURCE_DIR" \
  --argjson fixture_scenarios "$FIXTURE_SCENARIOS_JSON" \
  '{
    kind: "togi_pr_loop_benchmark_result",
    schema_version: 2,
    timing_policy: "observational-only",
    manifest: {
      name: "togi-pr-loop-benchmarks",
      schema_version: 2,
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
      runner_label: $runner_label,
      logical_cpu_count: $logical_cpu_count,
      kernel_release: $kernel_release,
      image_os: (if $image_os == "" then null else $image_os end),
      image_version: (if $image_version == "" then null else $image_version end),
      git_version: $git_version,
      go_build_cache_state: $go_build_cache_state,
      go_build_cache_policy: $go_build_cache_policy,
      go_build_cache_path: $go_build_cache_path,
      fixture_source_dir: $fixture_source,
      fixture_scenarios: $fixture_scenarios,
      started_at_utc: $started_at
    }
  }' > "$META_JSON"

RESULT_JSON="$OUT_DIR/pr-loop-benchmark-result.json"
jq -s \
  --argjson scenarios "$SCENARIOS_JSON" \
  --argjson ok "$OVERALL_OK_JSON" \
  --argjson failures "$FAILURES_JSON" \
  '.[0] + {
    workloads: .[1:],
    cross_workload: {
      scenarios: $scenarios
    },
    ok: $ok,
    failures: $failures
  }' "$META_JSON" "${FRAGMENTS[@]}" > "$RESULT_JSON"

jq -r '.workloads[]
  | "\(.name) [\(.scenario)]: exit=\(.exit_status) wall=\(.timing.wall_ms)ms reported=\(.timing.reported_duration_ms // 0)ms ok=\(.ok)"' \
  "$RESULT_JSON"
echo "normalized result: $RESULT_JSON"
echo "raw reports: $OUT_DIR/raw"

if [ "$OVERALL_OK" != "1" ]; then
  echo "PR-loop benchmark invariants failed:" >&2
  jq -r '.failures[] | "  - \(.)"' "$RESULT_JSON" >&2
  exit 1
fi
echo "all PR-loop benchmark invariants passed"
