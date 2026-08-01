#!/usr/bin/env bash
# Generates and verifies only the approved Mitigrid external-dogfood evidence directory.
set -euo pipefail

readonly APPROVAL_COMMENT_ID=5150807894
readonly APPROVAL_AUTHOR=Darkroom4364
readonly APPROVAL_URL="https://github.com/Darkroom4364/Mitigrid/pull/125#issuecomment-5150807894"
readonly APPROVAL_BODY="$(cat <<'EOF'
I authorize one no-secret, read-only external-dogfood run of released Darkroom4364/togi v0.4.1 on Darkroom4364/Mitigrid commit f5f3f57c92fdb3405b92eca7c9b6a6d3d704c1e8, base 16e7c9e49f353fd7f4254276b3a7ece99c6dedf6, scoped to crates/opencem-cli/src/commands/pack.rs, using cargo test --locked --workspace and cargo check --locked --workspace. The run may publish its bounded raw stdout/stderr, JSON reports, environment metadata, and checksums in Darkroom4364/togi. It must use no secrets and must not modify Mitigrid.
EOF
)"
readonly TOGI_ARCHIVE=togi-linux-x86_64.tar.gz
readonly TOGI_ARCHIVE_SHA256=6be7bf55d3c84a539cdaa4e60e5b5ef212ddb0e2575cd6b85ceae50218abce5c
readonly TARGET_REPOSITORY=https://github.com/Darkroom4364/Mitigrid.git
readonly TARGET_REVISION=f5f3f57c92fdb3405b92eca7c9b6a6d3d704c1e8
readonly TARGET_BASE=16e7c9e49f353fd7f4254276b3a7ece99c6dedf6
readonly TARGET_PATH=crates/opencem-cli/src/commands/pack.rs
readonly DEFAULT_WORKFLOW_REF=refs/heads/main
readonly GENERATED_MUTANT_CEILING=20
readonly MUTATION_TIMEOUT_SECONDS=120
readonly MUTATION_JOBS=2
readonly OUTER_TIMEOUT_SECONDS=2100

usage() {
    cat <<'EOF'
Usage: verify-external-dogfood-evidence.sh --generate CASE_DIRECTORY
       verify-external-dogfood-evidence.sh --verify CASE_DIRECTORY

--generate validates the complete run inputs and raw reports, then writes a
canonical metrics.json. --verify is offline-only: it repeats the validation,
checks metrics.json, and checks every required artifact digest in SHA256SUMS.
EOF
}

die() {
    echo "external dogfood evidence: $*" >&2
    exit 1
}

require_command() {
    command -v "$1" >/dev/null 2>&1 || die "required command is unavailable: $1"
}

require_file() {
    [[ -f "$case_dir/$1" ]] || die "required artifact is missing: $1"
}

validate_single_json_document() {
    jq -e -s 'length == 1' "$1" >/dev/null || die "not exactly one JSON document: $1"
}

validate_togi_config() {
    awk '
        /^\[mutations\]/{ in_mutations = 1; next }
        /^\[/{ in_mutations = 0 }
        in_mutations && /^[[:space:]]*max_per_run[[:space:]]*=[[:space:]]*0[[:space:]]*(#.*)?$/ { matches++ }
        END { exit matches == 1 ? 0 : 1 }
    ' "$case_dir/target-togi.toml" || die "target-togi.toml does not set mutations.max_per_run = 0"
}

core_files=(
    approval.json case.json cargo-fetch-status.txt cargo-fetch.stderr cargo-fetch.stdout commands.txt
    dry-run-status.txt dry-run.json dry-run.stderr dry-run.stdout environment.json
    preflight-status.txt preflight.stderr preflight.stdout release-checksums.txt release-verification.json
    report.json target-after-status.txt target-before-status.txt target-changed-paths.txt target-togi.toml target.patch
    togi-exit-status.txt togi-version.txt togi.stderr togi.stdout wall-time.json
)
hashed_files=("${core_files[@]}" metrics.json validation.txt)

if [[ "${1:-}" == "--help" ]]; then
    usage
    exit 0
fi
if [[ $# -ne 2 || ( "$1" != "--generate" && "$1" != "--verify" ) ]]; then
    usage >&2
    exit 2
fi
mode=$1
case_dir=$2
[[ -d "$case_dir" ]] || die "case directory does not exist"
for command in awk cmp jq sha256sum sort; do
    require_command "$command"
done
for file in "${core_files[@]}"; do
    require_file "$file"
done

validate_single_json_document "$case_dir/approval.json"
validate_single_json_document "$case_dir/case.json"
validate_single_json_document "$case_dir/environment.json"
validate_single_json_document "$case_dir/release-verification.json"
validate_single_json_document "$case_dir/dry-run.stdout"
validate_single_json_document "$case_dir/dry-run.json"
validate_single_json_document "$case_dir/togi.stdout"
validate_single_json_document "$case_dir/report.json"
validate_single_json_document "$case_dir/wall-time.json"
cmp -s "$case_dir/dry-run.stdout" "$case_dir/dry-run.json" || die "dry-run JSON is not a byte-for-byte copy of stdout"
cmp -s "$case_dir/togi.stdout" "$case_dir/report.json" || die "report JSON is not a byte-for-byte copy of stdout"

jq -e \
    --arg workflow_sha "$(jq -r '.workflow_source_revision' "$case_dir/case.json")" \
    --arg workflow_ref "$DEFAULT_WORKFLOW_REF" \
    --arg approval_url "$APPROVAL_URL" \
    --arg approval_body "$APPROVAL_BODY" \
    --arg archive "$TOGI_ARCHIVE" \
    --arg archive_sha256 "$TOGI_ARCHIVE_SHA256" \
    --arg repository "$TARGET_REPOSITORY" \
    --arg revision "$TARGET_REVISION" \
    --arg base "$TARGET_BASE" \
    --arg path "$TARGET_PATH" \
    --argjson ceiling "$GENERATED_MUTANT_CEILING" \
    --argjson mutation_timeout "$MUTATION_TIMEOUT_SECONDS" \
    --argjson jobs "$MUTATION_JOBS" \
    --argjson outer_timeout "$OUTER_TIMEOUT_SECONDS" '
    .schema_version == 1 and .case == "mitigrid-v0.4.1-pack" and
    (.workflow_source_revision | test("^[0-9a-f]{40}$")) and .workflow_source_revision == $workflow_sha and
    .workflow_source_ref == $workflow_ref and
    .approval == {url: $approval_url, id: 5150807894, author: "Darkroom4364", body: $approval_body} and
    .release == {tag: "v0.4.1", archive: $archive, archive_sha256: $archive_sha256} and
    .target == {repository: $repository, revision: $revision, base: $base, mutation_scope: $path,
                direct_parent_changed_paths: [".togi-baseline", $path, "docs/governance/mutation-baseline-v0.1.md",
                                              "docs/governance/public-readiness.md", "docs/governance/release-policy.md"],
                test_command: ["cargo", "test", "--locked", "--workspace"],
                build_command: ["cargo", "check", "--locked", "--workspace"],
                togi_toml_max_per_run: 0} and
    .limits == {generated_mutant_ceiling: $ceiling, per_mutation_timeout_seconds: $mutation_timeout,
                jobs: $jobs, outer_timeout_seconds: $outer_timeout}
' "$case_dir/case.json" >/dev/null || die "case metadata does not describe the one approved case"
workflow_sha=$(jq -r '.workflow_source_revision' "$case_dir/case.json")

jq -e --arg url "$APPROVAL_URL" --arg body "$APPROVAL_BODY" '
    .url == $url and .id == 5150807894 and .author == "Darkroom4364" and .body == $body and
    (.created_at | type == "string" and length > 0)
' "$case_dir/approval.json" >/dev/null || die "approval provenance is invalid"

jq -e --arg workflow_sha "$workflow_sha" --arg workflow_ref "$DEFAULT_WORKFLOW_REF" '
    def sha256: type == "string" and test("^[0-9a-f]{64}$");
    .schema_version == 1 and .workflow_source_revision == $workflow_sha and .workflow_source_ref == $workflow_ref and
    .runner.os == "Linux" and .runner.arch == "x86_64" and
    (.runner.image | type == "string") and (.runner.image_version | type == "string") and
    (.runner.uname | type == "string") and (.runner.cpu_count | type == "string" and test("^[1-9][0-9]*$")) and
    .versions.togi == "togi 0.4.1" and (.versions.cargo | type == "string") and
    (.versions.rustc | type == "string") and (.versions.git | type == "string") and
    .locale == "C" and .timezone == "UTC" and
    (.target.cargo_lock_sha256 | sha256) and (.target.togi_toml_sha256 | sha256)
' "$case_dir/environment.json" >/dev/null || die "environment metadata is invalid"

[[ "$(cat "$case_dir/togi-version.txt")" == "togi 0.4.1" ]] || die "released Togi version evidence is invalid"
jq -e --arg archive "$TOGI_ARCHIVE" --arg sha256 "$TOGI_ARCHIVE_SHA256" '
    .tag == "v0.4.1" and .archive == $archive and .expected_sha256 == $sha256 and
    .actual_sha256 == $sha256 and .version == "togi 0.4.1"
' "$case_dir/release-verification.json" >/dev/null || die "released archive verification is invalid"
manifest_matches=()
while IFS= read -r manifest_match; do
    manifest_matches+=("$manifest_match")
done < <(awk -v file="$TOGI_ARCHIVE" '$2 == file || $2 == "./" file { print $1 }' "$case_dir/release-checksums.txt")
[[ ${#manifest_matches[@]} -eq 1 && "${manifest_matches[0]}" == "$TOGI_ARCHIVE_SHA256" ]] || die "release manifest evidence is invalid"

[[ "$(cat "$case_dir/cargo-fetch-status.txt")" == 0 ]] || die "cargo fetch did not succeed"
[[ "$(cat "$case_dir/preflight-status.txt")" == 0 ]] || die "target preflight did not succeed"
[[ "$(cat "$case_dir/dry-run-status.txt")" == 0 ]] || die "dry run did not succeed"
togi_status=$(cat "$case_dir/togi-exit-status.txt")
[[ "$togi_status" == 0 || "$togi_status" == 1 ]] || die "Togi execution status is not publishable"
[[ ! -s "$case_dir/target-before-status.txt" ]] || die "target was dirty before execution"
[[ ! -s "$case_dir/target-after-status.txt" ]] || die "target was dirty after execution"
[[ -s "$case_dir/target.patch" ]] || die "target patch is empty"
expected_changed_paths=$(printf '%s\n' ".togi-baseline" "$TARGET_PATH" "docs/governance/mutation-baseline-v0.1.md" "docs/governance/public-readiness.md" "docs/governance/release-policy.md")
[[ "$(cat "$case_dir/target-changed-paths.txt")" == "$expected_changed_paths" ]] || die "direct-parent changed paths are invalid"

expected_commands=$(cat <<EOF
preflight: cargo test --locked --workspace
dry-run: togi check --dry-run --format json --base $TARGET_BASE --test-cmd 'cargo test --locked --workspace' --build-cmd 'cargo check --locked --workspace'
execution: timeout --preserve-status 35m togi check --format json --base $TARGET_BASE --test-cmd 'cargo test --locked --workspace' --build-cmd 'cargo check --locked --workspace' --timeout $MUTATION_TIMEOUT_SECONDS --jobs $MUTATION_JOBS --force-rerun --no-incremental-history
EOF
)
[[ "$(cat "$case_dir/commands.txt")" == "$expected_commands" ]] || die "recorded commands are not the approved commands"

jq -e --argjson ceiling "$GENERATED_MUTANT_CEILING" --arg target_path "$TARGET_PATH" '
    def natural: type == "number" and . >= 0 and floor == .;
    type == "object" and (keys | sort) == ["dry_run", "kind", "mutations", "planned_total"] and
    .kind == "dry_run" and .dry_run == true and (.planned_total | natural) and (.mutations | type == "array") and
    .planned_total == (.mutations | length) and .planned_total >= 1 and .planned_total <= $ceiling and
    (.mutations | all(.[]; (keys | sort) == ["column", "description", "file", "id", "line", "operator", "original", "replacement"] and
        (.id | natural) and (.file == $target_path) and (.line | natural) and (.column | natural) and
        (.operator | type == "string") and (.description | type == "string") and
        (.original | type == "string") and (.replacement | type == "string")))
' "$case_dir/dry-run.json" >/dev/null || die "dry run is not the approved bounded v0.4.1 plan"
dry_run_planned_total=$(jq -r '.planned_total' "$case_dir/dry-run.json")

jq -e \
    --arg revision "$TARGET_REVISION" \
    --arg target_path "$TARGET_PATH" \
    --argjson dry_run_planned_total "$dry_run_planned_total" '
    def natural: type == "number" and . >= 0 and floor == .;
    def result_count($result): [.mutations[] | select(.result == $result)] | length;
    def execution_count($state): [.mutations[] | select(.execution.state == $state)] | length;
    . as $report |
    ($report.mutations | length) as $mutation_length |
    ($report | result_count("killed")) as $killed |
    ($report | result_count("survived")) as $survived |
    ($report | result_count("timeout")) as $timeout |
    ($report | result_count("build_error")) as $build_errors |
    ($report | result_count("uncovered")) as $uncovered |
    ($report | result_count("subsumed")) as $subsumed |
    ($report | execution_count("executed")) as $executed |
    type == "object" and .kind == "mutation_report" and .schema_version == 1 and .generator == "togi/0.4.1" and
    .source_revision == $revision and .test_command == ["cargo", "test", "--locked", "--workspace"] and
    .build_command == ["cargo", "check", "--locked", "--workspace"] and (.planned_total | natural) and
    ([.total, .tested, .killed, .survived, .timeout, .build_errors, (.executed_killed // 0),
      (.exact_cache_reused // 0), (.incremental_history_reused // 0), (.uncovered // 0), (.subsumed // 0),
      .duration_ms] | all(.[]; natural)) and
    (.mutation_score | type == "number" and . >= 0) and
    .planned_total == $dry_run_planned_total and .total == .planned_total and .total == $mutation_length and
    .tested == .total and .tested == $executed and .tested > 0 and
    .killed == $killed and .survived == $survived and .timeout == $timeout and .build_errors == $build_errors and
    (.uncovered // 0) == $uncovered and (.subsumed // 0) == $subsumed and
    (.executed_killed // .killed) == .killed and (.exact_cache_reused // 0) == 0 and
    (.incremental_history_reused // 0) == 0 and (.uncovered // 0) == 0 and (.subsumed // 0) == 0 and
    .timeout == 0 and .build_errors == 0 and .partial == false and (.early_stop_reason? // null) == null and
    (.mutations | all(.[]; (.id | natural) and (.file == $target_path) and
        (.result == "killed" or .result == "survived") and
        (.execution | type == "object") and .execution.state == "executed")) and
    (((.killed / .tested * 100) - .mutation_score) | fabs) < 0.000001
' "$case_dir/report.json" >/dev/null || die "report does not contain a complete fresh v0.4.1 execution"

jq -e '
    .wall_time_ms | type == "number" and . >= 0 and floor == .
' "$case_dir/wall-time.json" >/dev/null || die "wall-time evidence is invalid"

generate_metrics() {
    jq -S --argjson execution_exit_status "$togi_status" \
        --slurpfile dry_run "$case_dir/dry-run.json" \
        --slurpfile wall_time "$case_dir/wall-time.json" '
        . as $report |
        {schema_version: 1,
         source_revision: $report.source_revision,
         dry_run_planned_total: $dry_run[0].planned_total,
         planned_total: $report.planned_total,
         total: $report.total,
         tested: $report.tested,
         killed: $report.killed,
         survived: $report.survived,
         timeout: $report.timeout,
         build_errors: $report.build_errors,
         executed_killed: ($report.executed_killed // $report.killed),
         exact_cache_reused: ($report.exact_cache_reused // 0),
         incremental_history_reused: ($report.incremental_history_reused // 0),
         uncovered: ($report.uncovered // 0),
         subsumed: ($report.subsumed // 0),
         partial: $report.partial,
         early_stop_reason: ($report.early_stop_reason // null),
         mutation_score: $report.mutation_score,
         duration_ms: $report.duration_ms,
         execution_exit_status: $execution_exit_status,
         wall_time_ms: $wall_time[0].wall_time_ms}
    ' "$case_dir/report.json"
}

if [[ "$mode" == "--generate" ]]; then
    generate_metrics >"$case_dir/metrics.json"
    echo "generated metrics.json from the validated v0.4.1 report"
    exit 0
fi

require_file metrics.json
require_file validation.txt
require_file SHA256SUMS
metrics_tmp=$(mktemp)
trap 'rm -f "$metrics_tmp"' EXIT
generate_metrics >"$metrics_tmp"
cmp -s "$metrics_tmp" "$case_dir/metrics.json" || die "metrics.json is not the deterministic report-derived metrics"

expected_hash_files=$(printf '%s\n' "${hashed_files[@]}" | LC_ALL=C sort)
actual_hash_files=$(awk '{print $2}' "$case_dir/SHA256SUMS" | LC_ALL=C sort)
[[ "$actual_hash_files" == "$expected_hash_files" ]] || die "SHA256SUMS does not cover exactly the required artifacts"
(
    cd "$case_dir"
    sha256sum --check --status SHA256SUMS
) || die "artifact checksum validation failed"
echo "verified complete external-dogfood evidence offline"
