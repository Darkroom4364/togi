#!/usr/bin/env bash
# Asserts this runner matches the expected release target and prints the
# resolved asset binding (TOGI_ARCHIVE, TOGI_BINARY). Fails closed when the
# host architecture or the resolver-selected asset differs from the expected
# target, so a moving runner label cannot verify the wrong archive.
set -euo pipefail

: "${TOGI_EXPECTED_ARCH:?TOGI_EXPECTED_ARCH is required}"
: "${TOGI_EXPECTED_ARCHIVE:?TOGI_EXPECTED_ARCHIVE is required}"
: "${TOGI_EXPECTED_BINARY:?TOGI_EXPECTED_BINARY is required}"

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

host_arch=$(uname -m)
case "$host_arch" in
  x86_64 | amd64) host_arch=x86_64 ;;
  arm64 | aarch64) host_arch=arm64 ;;
  *)
    echo "Unsupported runner architecture: ${host_arch}" >&2
    exit 1
    ;;
esac
if [ "$host_arch" != "$TOGI_EXPECTED_ARCH" ]; then
  echo "Runner architecture ${host_arch} does not match expected ${TOGI_EXPECTED_ARCH}" >&2
  exit 1
fi

resolved=$(bash "${script_dir}/resolve-togi-asset.sh")
eval "$resolved"
if [ "$TOGI_ARCHIVE" != "$TOGI_EXPECTED_ARCHIVE" ]; then
  echo "Resolved archive ${TOGI_ARCHIVE} does not match expected ${TOGI_EXPECTED_ARCHIVE}" >&2
  exit 1
fi
if [ "$TOGI_BINARY" != "$TOGI_EXPECTED_BINARY" ]; then
  echo "Resolved binary ${TOGI_BINARY} does not match expected ${TOGI_EXPECTED_BINARY}" >&2
  exit 1
fi

printf 'TOGI_ARCHIVE=%q\n' "$TOGI_ARCHIVE"
printf 'TOGI_BINARY=%q\n' "$TOGI_BINARY"
