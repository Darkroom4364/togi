#!/usr/bin/env bash
# Tier 2 post-publication smoke: the published release archive for this
# runner's platform must download from the public release URL, verify against
# checksums.txt, install, and report the tagged version. No mutation run.
set -euo pipefail

: "${TOGI_VERSION:?TOGI_VERSION is required}"
: "${GITHUB_PATH:?GITHUB_PATH is required}"
: "${RUNNER_TEMP:?RUNNER_TEMP is required}"

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

if command -v togi >/dev/null 2>&1; then
  echo "Released-archive install smoke requires no preinstalled togi: $(command -v togi)" >&2
  exit 1
fi

binding=$(bash "${script_dir}/assert-release-target.sh")
eval "$binding"

fetched=$(TOGI_VERSION="$TOGI_VERSION" TOGI_ARCHIVE="$TOGI_ARCHIVE" \
  bash "${script_dir}/fetch-togi-release-asset.sh")
eval "$fetched"

TOGI_ARCHIVE="$TOGI_ARCHIVE" TOGI_BINARY="$TOGI_BINARY" TOGI_ARCHIVE_PATH="$TOGI_ARCHIVE_PATH" \
  bash "${script_dir}/install-togi-archive.sh"

temp_root="${RUNNER_TEMP}"
if command -v cygpath >/dev/null 2>&1; then
  temp_root=$(cygpath -u "$temp_root")
fi
install_dir="${temp_root}/togi-bin"
export PATH="${install_dir}:${PATH}"
if [ "$(command -v "$TOGI_BINARY")" != "${install_dir}/${TOGI_BINARY}" ]; then
  echo "Released-archive install smoke did not select the installed ${TOGI_BINARY}" >&2
  exit 1
fi
version_output=$("$TOGI_BINARY" --version)
printf '%s\n' "$version_output"
if [ "$version_output" != "togi ${TOGI_VERSION#v}" ]; then
  echo "Unexpected togi version: ${version_output}" >&2
  exit 1
fi
