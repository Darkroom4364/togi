#!/usr/bin/env bash
# Exercises the documented Linux x86_64 released-binary first-success path.
set -euo pipefail

: "${TOGI_VERSION:?TOGI_VERSION is required}"
: "${GITHUB_WORKSPACE:?GITHUB_WORKSPACE is required}"
: "${GITHUB_PATH:?GITHUB_PATH is required}"
: "${RUNNER_TEMP:?RUNNER_TEMP is required}"

case "$(uname -m)" in
  x86_64 | amd64) ;;
  *)
    echo "Released-binary smoke requires x86_64, got $(uname -m)" >&2
    exit 1
    ;;
esac

if command -v togi >/dev/null 2>&1; then
  echo "Released-binary smoke requires no preinstalled togi: $(command -v togi)" >&2
  exit 1
fi

eval "$(TOGI_OS=Linux TOGI_ARCH=x86_64 bash "${GITHUB_WORKSPACE}/.github/scripts/resolve-togi-asset.sh")"
if [ "$TOGI_ARCHIVE" != "togi-linux-x86_64.tar.gz" ] || [ "$TOGI_BINARY" != "togi" ]; then
  echo "Unexpected Linux release asset: ${TOGI_ARCHIVE} (${TOGI_BINARY})" >&2
  exit 1
fi

release_base="https://github.com/Darkroom4364/togi/releases/download/${TOGI_VERSION}"
archive_path="${RUNNER_TEMP}/${TOGI_ARCHIVE}"
checksums_path="${RUNNER_TEMP}/togi-release-checksums.txt"
curl -fsSLo "$archive_path" "${release_base}/${TOGI_ARCHIVE}"
curl -fsSLo "$checksums_path" "${release_base}/checksums.txt"
expected_sha=$(awk -v file="$TOGI_ARCHIVE" '$2 == file || $2 == "./" file { print $1 }' "$checksums_path")
if [ -z "$expected_sha" ]; then
  echo "No checksum found for ${TOGI_ARCHIVE}" >&2
  exit 1
fi
actual_sha=$(sha256sum "$archive_path" | awk '{print $1}')
if [ "$actual_sha" != "$expected_sha" ]; then
  echo "Checksum mismatch for ${TOGI_ARCHIVE}" >&2
  exit 1
fi

TOGI_ARCHIVE="$TOGI_ARCHIVE" TOGI_BINARY="$TOGI_BINARY" TOGI_ARCHIVE_PATH="$archive_path" \
  bash "${GITHUB_WORKSPACE}/.github/scripts/install-togi-archive.sh"
install_dir="${RUNNER_TEMP}/togi-bin"
export PATH="${install_dir}:${PATH}"
if [ "$(command -v togi)" != "${install_dir}/togi" ]; then
  echo "Released-binary smoke did not select the installed togi" >&2
  exit 1
fi
version_output=$(togi --version)
printf '%s\n' "$version_output"
if [ "$version_output" != "togi ${TOGI_VERSION#v}" ]; then
  echo "Unexpected togi version: ${version_output}" >&2
  exit 1
fi

fixture_dir="${GITHUB_WORKSPACE}/tests/fixtures/go"
project_root=$(mktemp -d "${RUNNER_TEMP}/togi-released-binary-fixture.XXXXXX")
trap 'rm -rf "$project_root"' EXIT
cp "$fixture_dir/go.mod" "$fixture_dir/calc.go" "$fixture_dir/calc_test.go" "$project_root/"
git -C "$project_root" init -q
git -C "$project_root" config user.name "Togi Released Binary Smoke"
git -C "$project_root" config user.email "togi-smoke@example.invalid"
sed -i 's/if n > 0/if n < 0/' "$project_root/calc.go"
git -C "$project_root" add go.mod calc.go calc_test.go
git -C "$project_root" commit -qm "Seed fixture"
sed -i 's/if n < 0/if n > 0/' "$project_root/calc.go"
git -C "$project_root" add calc.go
git -C "$project_root" commit -qm "Restore positive comparison"
git -C "$project_root" rev-parse --verify HEAD~1 >/dev/null

export GOFLAGS=-count=1
export GOCACHE="${RUNNER_TEMP}/togi-released-binary-go-cache"
rm -rf "$GOCACHE"
go -C "$project_root" test ./...

run_output="${RUNNER_TEMP}/togi-released-binary-smoke-output.txt"
set +e
(
  cd "$project_root"
  togi check --base HEAD~1
) >"$run_output" 2>&1
status=$?
set -e

case "$status" in
  1)
    ;;
  0)
    echo "Expected the weak fixture to produce a surviving mutant" >&2
    cat "$run_output" >&2
    exit 1
    ;;
  *)
    echo "Released-binary mutation run failed with exit code ${status}" >&2
    cat "$run_output" >&2
    exit "$status"
    ;;
esac

if ! grep -Eq '^Results: [0-9]+ killed, [1-9][0-9]* survived, 0 timeout, 0 build errors$' "$run_output"; then
  echo "Released-binary mutation run did not prove healthy survivor results" >&2
  cat "$run_output" >&2
  exit 1
fi
if ! grep -Eq '^Execution: [1-9][0-9]* freshly tested$' "$run_output"; then
  echo "Released-binary mutation run did not prove fresh execution" >&2
  cat "$run_output" >&2
  exit 1
fi
if grep -q '^Partial:' "$run_output"; then
  echo "Released-binary mutation run was partial" >&2
  cat "$run_output" >&2
  exit 1
fi
git -C "$project_root" diff --exit-code
