#!/usr/bin/env bash
set -euo pipefail

raw_os="${TOGI_OS:-$(uname -s)}"
raw_arch="${TOGI_ARCH:-$(uname -m)}"
os_lc=$(printf '%s' "$raw_os" | tr '[:upper:]' '[:lower:]')
arch_lc=$(printf '%s' "$raw_arch" | tr '[:upper:]' '[:lower:]')

case "$os_lc" in
  linux*) os="linux" ;;
  darwin*) os="macos" ;;
  mingw* | msys* | cygwin* | windows*) os="windows" ;;
  *)
    echo "unsupported OS: ${raw_os}" >&2
    exit 1
    ;;
esac

case "$arch_lc" in
  x86_64 | amd64) arch="x86_64" ;;
  arm64 | aarch64) arch="arm64" ;;
  *)
    echo "unsupported architecture: ${raw_arch}" >&2
    exit 1
    ;;
esac

case "${os}-${arch}" in
  linux-x86_64 | macos-arm64)
    ext="tar.gz"
    binary="togi"
    ;;
  windows-x86_64)
    ext="zip"
    binary="togi.exe"
    ;;
  *)
    echo "unsupported release target: ${os}-${arch}" >&2
    exit 1
    ;;
esac

asset_name="togi-${os}-${arch}"
printf 'TOGI_ARCHIVE=%q\n' "${asset_name}.${ext}"
printf 'TOGI_BINARY=%q\n' "${binary}"
