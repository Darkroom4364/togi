#!/usr/bin/env bash
# Fails closed unless this runner natively matches the required target: the
# Rust host triple must equal TOGI_EXPECTED_TARGET and the normalized runner
# architecture must equal TOGI_EXPECTED_ARCH. A runner-image change fails the
# leg here instead of silently proving the wrong target.
set -euo pipefail

: "${TOGI_EXPECTED_TARGET:?TOGI_EXPECTED_TARGET is required}"
: "${TOGI_EXPECTED_ARCH:?TOGI_EXPECTED_ARCH is required}"

host=$(rustc -vV | awk '/^host:/ { print $2 }')
if [ "$host" != "$TOGI_EXPECTED_TARGET" ]; then
  echo "Rust host ${host} does not match required target ${TOGI_EXPECTED_TARGET}" >&2
  exit 1
fi

machine=$(uname -m)
case "$machine" in
  x86_64 | amd64) machine=x86_64 ;;
  arm64 | aarch64) machine=arm64 ;;
  *)
    echo "Unsupported runner architecture: ${machine}" >&2
    exit 1
    ;;
esac
if [ "$machine" != "$TOGI_EXPECTED_ARCH" ]; then
  echo "Runner architecture ${machine} does not match expected ${TOGI_EXPECTED_ARCH}" >&2
  exit 1
fi
