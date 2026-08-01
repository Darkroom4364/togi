#!/usr/bin/env bash
# Runs exactly the approved Mitigrid external-dogfood case and preserves its evidence.
set -euo pipefail

readonly APPROVAL_COMMENT_ID=5150807894
readonly APPROVAL_AUTHOR=Darkroom4364
readonly APPROVAL_URL="https://github.com/Darkroom4364/Mitigrid/pull/125#issuecomment-5150807894"
readonly APPROVAL_API_URL="https://api.github.com/repos/Darkroom4364/Mitigrid/issues/comments/${APPROVAL_COMMENT_ID}"
readonly APPROVAL_BODY="$(cat <<'EOF'
I authorize one no-secret, read-only external-dogfood run of released Darkroom4364/togi v0.4.1 on Darkroom4364/Mitigrid commit f5f3f57c92fdb3405b92eca7c9b6a6d3d704c1e8, base 16e7c9e49f353fd7f4254276b3a7ece99c6dedf6, scoped to crates/opencem-cli/src/commands/pack.rs, using cargo test --locked --workspace and cargo check --locked --workspace. The run may publish its bounded raw stdout/stderr, JSON reports, environment metadata, and checksums in Darkroom4364/togi. It must use no secrets and must not modify Mitigrid.
EOF
)"
readonly TOGI_VERSION=v0.4.1
readonly TOGI_ARCHIVE=togi-linux-x86_64.tar.gz
readonly TOGI_ARCHIVE_SHA256=6be7bf55d3c84a539cdaa4e60e5b5ef212ddb0e2575cd6b85ceae50218abce5c
readonly TARGET_REPOSITORY=https://github.com/Darkroom4364/Mitigrid.git
readonly TARGET_REVISION=f5f3f57c92fdb3405b92eca7c9b6a6d3d704c1e8
readonly TARGET_BASE=16e7c9e49f353fd7f4254276b3a7ece99c6dedf6
readonly TARGET_PATH=crates/opencem-cli/src/commands/pack.rs
readonly GENERATED_MUTANT_CEILING=20
readonly MUTATION_TIMEOUT_SECONDS=120
readonly MUTATION_JOBS=2
readonly OUTER_TIMEOUT_SECONDS=2100

usage() {
    cat <<'EOF'
Usage: run-external-dogfood.sh OUTPUT_DIRECTORY

Runs the one approved external-dogfood case. OUTPUT_DIRECTORY must be absent or
an empty absolute directory. EXPECTED_WORKFLOW_SHA, GITHUB_SHA, and
GITHUB_WORKSPACE are required from the workflow environment.
EOF
}

die() {
    echo "external dogfood: $*" >&2
    exit 1
}

require_command() {
    command -v "$1" >/dev/null 2>&1 || die "required command is unavailable: $1"
}

validate_archive_entries() {
    tar tzf "$1" | awk '
        {
            path = $0
            gsub(/\\/, "/", path)
            if (path == "" || path ~ /^\// || path ~ /(^|\/)\.\.($|\/)/) {
                printf "unsafe archive entry: %s\n", $0 > "/dev/stderr"
                exit 1
            }
        }
    '
}

validate_tar_entry_types() {
    tar tvzf "$1" | awk '
        {
            kind = substr($0, 1, 1)
            if (kind == "l" || kind == "h") {
                printf "unsafe archive link entry: %s\n", $0 > "/dev/stderr"
                exit 1
            }
        }
    '
}

validate_single_json_document() {
    jq -e -s 'length == 1' "$1" >/dev/null
}

if [[ "${1:-}" == "--help" ]]; then
    usage
    exit 0
fi
if [[ $# -ne 1 ]]; then
    usage >&2
    exit 2
fi

output_dir=$1
case "$output_dir" in
    /*) ;;
    *) die "output directory must be absolute" ;;
esac
if [[ -e "$output_dir" ]]; then
    [[ -d "$output_dir" ]] || die "output path is not a directory"
    [[ -z "$(find "$output_dir" -mindepth 1 -print -quit)" ]] || die "output directory is not empty"
else
    mkdir -p "$output_dir"
fi

: "${EXPECTED_WORKFLOW_SHA:?EXPECTED_WORKFLOW_SHA is required}"
: "${GITHUB_SHA:?GITHUB_SHA is required}"
: "${GITHUB_WORKSPACE:?GITHUB_WORKSPACE is required}"
[[ "$EXPECTED_WORKFLOW_SHA" =~ ^[0-9a-f]{40}$ ]] || die "EXPECTED_WORKFLOW_SHA is not a full lowercase commit SHA"
[[ "$GITHUB_SHA" =~ ^[0-9a-f]{40}$ ]] || die "GITHUB_SHA is not a full lowercase commit SHA"
[[ "$EXPECTED_WORKFLOW_SHA" == "$GITHUB_SHA" ]] || die "workflow dispatch SHA does not match GITHUB_SHA"
workflow_head=$(git -C "$GITHUB_WORKSPACE" rev-parse HEAD)
[[ "$workflow_head" == "$EXPECTED_WORKFLOW_SHA" ]] || die "checked-out Togi revision does not match expected workflow SHA"

for command in awk cargo curl find git jq nproc sha256sum sort tar timeout; do
    require_command "$command"
done
[[ "$(uname -s)" == Linux ]] || die "this protocol requires Linux"
[[ "$(uname -m)" == x86_64 ]] || die "this protocol requires x86_64"

work_root=$(mktemp -d "${RUNNER_TEMP:-/tmp}/togi-external-dogfood.XXXXXX")
cleanup() {
    rm -rf "$work_root"
}
trap cleanup EXIT

approval_response="$work_root/approval-response.json"
curl -fsSL --proto '=https' --tlsv1.2 --retry 3 --retry-delay 1 \
    "$APPROVAL_API_URL" >"$approval_response"
[[ "$(jq -r '.id' "$approval_response")" == "$APPROVAL_COMMENT_ID" ]] || die "approval comment id did not match"
[[ "$(jq -r '.user.login' "$approval_response")" == "$APPROVAL_AUTHOR" ]] || die "approval comment author did not match"
[[ "$(jq -r '.html_url' "$approval_response")" == "$APPROVAL_URL" ]] || die "approval comment URL did not match"
[[ "$(jq -r '.body' "$approval_response")" == "$APPROVAL_BODY" ]] || die "approval comment body did not match"
jq -S '{url: .html_url, id, author: .user.login, created_at, body}' "$approval_response" >"$output_dir/approval.json"

jq -n -S \
    --arg workflow_source_revision "$EXPECTED_WORKFLOW_SHA" \
    --arg approval_url "$APPROVAL_URL" \
    --arg approval_body "$APPROVAL_BODY" \
    --arg release_tag "$TOGI_VERSION" \
    --arg archive "$TOGI_ARCHIVE" \
    --arg archive_sha256 "$TOGI_ARCHIVE_SHA256" \
    --arg repository "$TARGET_REPOSITORY" \
    --arg revision "$TARGET_REVISION" \
    --arg base "$TARGET_BASE" \
    --arg path "$TARGET_PATH" \
    --argjson ceiling "$GENERATED_MUTANT_CEILING" \
    --argjson mutation_timeout "$MUTATION_TIMEOUT_SECONDS" \
    --argjson jobs "$MUTATION_JOBS" \
    --argjson outer_timeout "$OUTER_TIMEOUT_SECONDS" \
    '{schema_version: 1,
      case: "mitigrid-v0.4.1-pack",
      workflow_source_revision: $workflow_source_revision,
      approval: {url: $approval_url, id: 5150807894, author: "Darkroom4364", body: $approval_body},
      release: {tag: $release_tag, archive: $archive, archive_sha256: $archive_sha256},
      target: {repository: $repository, revision: $revision, base: $base, path: $path,
               test_command: ["cargo", "test", "--locked", "--workspace"],
               build_command: ["cargo", "check", "--locked", "--workspace"],
               togi_toml_max_per_run: 0},
      limits: {generated_mutant_ceiling: $ceiling, per_mutation_timeout_seconds: $mutation_timeout,
               jobs: $jobs, outer_timeout_seconds: $outer_timeout}}' >"$output_dir/case.json"

release_dir="$work_root/release"
extract_dir="$work_root/extract"
install_dir="$work_root/bin"
mkdir -p "$release_dir" "$extract_dir" "$install_dir"
archive_path="$release_dir/$TOGI_ARCHIVE"
release_base="https://github.com/Darkroom4364/togi/releases/download/${TOGI_VERSION}"
curl -fsSL --proto '=https' --tlsv1.2 --retry 3 --retry-delay 1 \
    -o "$archive_path" "$release_base/$TOGI_ARCHIVE"
curl -fsSL --proto '=https' --tlsv1.2 --retry 3 --retry-delay 1 \
    -o "$output_dir/release-checksums.txt" "$release_base/checksums.txt"
manifest_matches=()
while IFS= read -r manifest_match; do
    manifest_matches+=("$manifest_match")
done < <(awk -v file="$TOGI_ARCHIVE" '$2 == file || $2 == "./" file { print $1 }' "$output_dir/release-checksums.txt")
[[ ${#manifest_matches[@]} -eq 1 ]] || die "release manifest must contain exactly one archive entry"
[[ "${manifest_matches[0]}" == "$TOGI_ARCHIVE_SHA256" ]] || die "release manifest checksum did not match the approved checksum"
archive_sha256=$(sha256sum "$archive_path" | awk '{print $1}')
[[ "$archive_sha256" == "$TOGI_ARCHIVE_SHA256" ]] || die "downloaded archive checksum did not match the approved checksum"
validate_archive_entries "$archive_path"
validate_tar_entry_types "$archive_path"
tar xzf "$archive_path" -C "$extract_dir"
binaries=()
while IFS= read -r binary; do
    binaries+=("$binary")
done < <(find "$extract_dir" -type f -name togi -print)
[[ ${#binaries[@]} -eq 1 ]] || die "release archive must contain exactly one togi binary"
install -m 0755 "${binaries[0]}" "$install_dir/togi"
togi_bin="$install_dir/togi"
togi_version="$($togi_bin --version)"
[[ "$togi_version" == "togi 0.4.1" ]] || die "released binary version did not match v0.4.1"
printf '%s\n' "$togi_version" >"$output_dir/togi-version.txt"
jq -n -S \
    --arg tag "$TOGI_VERSION" \
    --arg archive "$TOGI_ARCHIVE" \
    --arg expected_sha256 "$TOGI_ARCHIVE_SHA256" \
    --arg actual_sha256 "$archive_sha256" \
    --arg version "$togi_version" \
    '{tag: $tag, archive: $archive, expected_sha256: $expected_sha256,
      actual_sha256: $actual_sha256, version: $version}' >"$output_dir/release-verification.json"

target_dir="$work_root/target"
GIT_TERMINAL_PROMPT=0 git clone --filter=blob:none --no-checkout "$TARGET_REPOSITORY" "$target_dir"
GIT_TERMINAL_PROMPT=0 git -C "$target_dir" checkout --detach "$TARGET_REVISION"
[[ "$(git -C "$target_dir" rev-parse HEAD)" == "$TARGET_REVISION" ]] || die "target checkout revision did not match"
git -C "$target_dir" cat-file -e "${TARGET_BASE}^{commit}"
[[ "$(git -C "$target_dir" rev-parse "${TARGET_REVISION}^")" == "$TARGET_BASE" ]] || die "approved base is not the target revision's direct parent"
git -C "$target_dir" merge-base --is-ancestor "$TARGET_BASE" "$TARGET_REVISION"
[[ -f "$target_dir/$TARGET_PATH" ]] || die "approved target path is absent"
git -C "$target_dir" diff --binary "$TARGET_BASE" "$TARGET_REVISION" -- "$TARGET_PATH" >"$output_dir/target.patch"
[[ -s "$output_dir/target.patch" ]] || die "approved target path has no diff from its base"
[[ -f "$target_dir/togi.toml" ]] || die "target togi.toml is absent"
awk '
    /^\[mutations\]/{ in_mutations = 1; next }
    /^\[/{ in_mutations = 0 }
    in_mutations && /^[[:space:]]*max_per_run[[:space:]]*=[[:space:]]*0[[:space:]]*(#.*)?$/ { matches++ }
    END { exit matches == 1 ? 0 : 1 }
' "$target_dir/togi.toml" || die "target togi.toml must set mutations.max_per_run = 0"
cp "$target_dir/togi.toml" "$output_dir/target-togi.toml"
git -C "$target_dir" status --porcelain --untracked-files=all >"$output_dir/target-before-status.txt"
[[ ! -s "$output_dir/target-before-status.txt" ]] || die "target worktree was not clean before execution"

original_home=${HOME:?HOME is required}
toolchain_home=${RUSTUP_HOME:-"$original_home/.rustup"}
[[ -d "$toolchain_home" ]] || die "Rust toolchain location is unavailable"
mkdir -p "$work_root/home" "$work_root/cargo-home"
runtime_env=(
    "PATH=$PATH"
    "HOME=$work_root/home"
    "CARGO_HOME=$work_root/cargo-home"
    "RUSTUP_HOME=$toolchain_home"
    "TZ=UTC"
    "LC_ALL=C"
)

set +e
(
    cd "$target_dir"
    env -i "${runtime_env[@]}" cargo fetch --locked
) >"$output_dir/cargo-fetch.stdout" 2>"$output_dir/cargo-fetch.stderr"
fetch_status=$?
set -e
printf '%s\n' "$fetch_status" >"$output_dir/cargo-fetch-status.txt"
[[ "$fetch_status" -eq 0 ]] || die "cargo fetch failed"

set +e
(
    cd "$target_dir"
    env -i "${runtime_env[@]}" CARGO_NET_OFFLINE=true cargo test --locked --workspace
) >"$output_dir/preflight.stdout" 2>"$output_dir/preflight.stderr"
preflight_status=$?
set -e
printf '%s\n' "$preflight_status" >"$output_dir/preflight-status.txt"
[[ "$preflight_status" -eq 0 ]] || die "target preflight failed"

printf '%s\n' \
    'preflight: cargo test --locked --workspace' \
    "dry-run: togi check --dry-run --format json --base $TARGET_BASE --path $TARGET_PATH --test-cmd 'cargo test --locked --workspace' --build-cmd 'cargo check --locked --workspace'" \
    "execution: timeout --preserve-status 35m togi check --format json --base $TARGET_BASE --path $TARGET_PATH --test-cmd 'cargo test --locked --workspace' --build-cmd 'cargo check --locked --workspace' --timeout $MUTATION_TIMEOUT_SECONDS --jobs $MUTATION_JOBS --force-rerun --no-incremental-history" \
    >"$output_dir/commands.txt"

set +e
(
    cd "$target_dir"
    env -i "${runtime_env[@]}" CARGO_NET_OFFLINE=true "$togi_bin" check \
        --dry-run --format json --base "$TARGET_BASE" --path "$TARGET_PATH" \
        --test-cmd "cargo test --locked --workspace" \
        --build-cmd "cargo check --locked --workspace"
) >"$output_dir/dry-run.stdout" 2>"$output_dir/dry-run.stderr"
dry_run_status=$?
set -e
printf '%s\n' "$dry_run_status" >"$output_dir/dry-run-status.txt"
[[ "$dry_run_status" -eq 0 ]] || die "dry run failed"
validate_single_json_document "$output_dir/dry-run.stdout"
cp "$output_dir/dry-run.stdout" "$output_dir/dry-run.json"
jq -e --argjson ceiling "$GENERATED_MUTANT_CEILING" '
    def natural: type == "number" and . >= 0 and floor == .;
    type == "object" and .kind == "dry_run" and .schema_version == 1 and .generator == "togi/0.4.1" and
    .dry_run == true and (.planned_total | natural) and (.mutations | type == "array") and
    .planned_total == (.mutations | length) and .planned_total >= 1 and .planned_total <= $ceiling
' "$output_dir/dry-run.json" >/dev/null || die "dry run did not produce the approved bounded v0.4.1 plan"

start_ns=$(date -u +%s%N)
set +e
(
    cd "$target_dir"
    env -i "${runtime_env[@]}" CARGO_NET_OFFLINE=true timeout --preserve-status 35m "$togi_bin" check \
        --format json --base "$TARGET_BASE" --path "$TARGET_PATH" \
        --test-cmd "cargo test --locked --workspace" \
        --build-cmd "cargo check --locked --workspace" \
        --timeout "$MUTATION_TIMEOUT_SECONDS" --jobs "$MUTATION_JOBS" \
        --force-rerun --no-incremental-history
) >"$output_dir/togi.stdout" 2>"$output_dir/togi.stderr"
togi_status=$?
set -e
end_ns=$(date -u +%s%N)
printf '%s\n' "$togi_status" >"$output_dir/togi-exit-status.txt"
case "$togi_status" in
    0|1) ;;
    *) die "Togi execution failed with nonpublishable status $togi_status" ;;
esac
validate_single_json_document "$output_dir/togi.stdout"
cp "$output_dir/togi.stdout" "$output_dir/report.json"
wall_time_ms=$(((end_ns - start_ns) / 1000000))
jq -n -S --argjson wall_time_ms "$wall_time_ms" '{wall_time_ms: $wall_time_ms}' >"$output_dir/wall-time.json"

set +e
(
    cd "$target_dir"
    env -i "${runtime_env[@]}" CARGO_NET_OFFLINE=true "$togi_bin" clean
) >"$work_root/togi-clean.stdout" 2>"$work_root/togi-clean.stderr"
clean_status=$?
set -e
[[ "$clean_status" -eq 0 ]] || die "Togi clean failed"
git -C "$target_dir" status --porcelain --untracked-files=all >"$output_dir/target-after-status.txt"
[[ ! -s "$output_dir/target-after-status.txt" ]] || die "target worktree was not clean after execution"

jq -n -S \
    --arg workflow_source_revision "$EXPECTED_WORKFLOW_SHA" \
    --arg runner_os "${RUNNER_OS:-Linux}" \
    --arg runner_image "${ImageOS:-unknown}" \
    --arg runner_image_version "${ImageVersion:-unknown}" \
    --arg uname "$(uname -srm)" \
    --arg arch "$(uname -m)" \
    --arg cpu_count "$(nproc)" \
    --arg cargo_version "$(cargo --version)" \
    --arg rustc_version "$(rustc --version)" \
    --arg git_version "$(git --version)" \
    --arg togi_version "$togi_version" \
    --arg locale "C" \
    --arg timezone "UTC" \
    --arg cargo_lock_sha256 "$(sha256sum "$target_dir/Cargo.lock" | awk '{print $1}')" \
    --arg togi_toml_sha256 "$(sha256sum "$target_dir/togi.toml" | awk '{print $1}')" \
    '{schema_version: 1, workflow_source_revision: $workflow_source_revision,
      runner: {os: $runner_os, image: $runner_image, image_version: $runner_image_version,
               uname: $uname, arch: $arch, cpu_count: $cpu_count},
      versions: {cargo: $cargo_version, rustc: $rustc_version, git: $git_version, togi: $togi_version},
      locale: $locale, timezone: $timezone,
      target: {cargo_lock_sha256: $cargo_lock_sha256, togi_toml_sha256: $togi_toml_sha256}}' \
    >"$output_dir/environment.json"

validator="$GITHUB_WORKSPACE/.github/scripts/verify-external-dogfood-evidence.sh"
[[ -f "$validator" ]] || die "offline evidence validator is unavailable"
bash "$validator" --generate "$output_dir" >"$output_dir/validation.txt"
checksum_files=(
    approval.json case.json cargo-fetch-status.txt cargo-fetch.stderr cargo-fetch.stdout commands.txt
    dry-run-status.txt dry-run.json dry-run.stderr dry-run.stdout environment.json
    preflight-status.txt preflight.stderr preflight.stdout release-checksums.txt release-verification.json
    report.json target-after-status.txt target-before-status.txt target-togi.toml target.patch
    togi-exit-status.txt togi-version.txt togi.stderr togi.stdout validation.txt wall-time.json metrics.json
)
(
    cd "$output_dir"
    printf '%s\n' "${checksum_files[@]}" | LC_ALL=C sort | while IFS= read -r file; do
        sha256sum "$file"
    done
) >"$output_dir/SHA256SUMS"
bash "$validator" --verify "$output_dir"
