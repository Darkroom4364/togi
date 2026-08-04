#!/usr/bin/env bash
# Downloads one public Togi release archive over unauthenticated HTTPS and
# verifies it against the release's checksums.txt. Fails on missing, renamed,
# duplicate, malformed, or checksum-mismatched asset entries.
set -euo pipefail

: "${TOGI_VERSION:?TOGI_VERSION is required}"
: "${TOGI_ARCHIVE:?TOGI_ARCHIVE is required}"

fetch_dir="${TOGI_FETCH_DIR:-${RUNNER_TEMP:-.}}"
if command -v cygpath >/dev/null 2>&1; then
  fetch_dir=$(cygpath -u "$fetch_dir")
fi
mkdir -p "$fetch_dir"

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  elif command -v certutil >/dev/null 2>&1; then
    certutil -hashfile "$1" SHA256 | sed -n '2p' | tr -d '[:space:]' | tr '[:upper:]' '[:lower:]'
  else
    echo "No SHA-256 tool found for release verification" >&2
    exit 1
  fi
}

release_base="https://github.com/Darkroom4364/togi/releases/download/${TOGI_VERSION}"
archive_path="${fetch_dir}/${TOGI_ARCHIVE}"
checksums_path="${fetch_dir}/togi-release-checksums.txt"
curl -fsSLo "$archive_path" "${release_base}/${TOGI_ARCHIVE}"
curl -fsSLo "$checksums_path" "${release_base}/checksums.txt"

match_count=$(awk -v file="$TOGI_ARCHIVE" '
  $2 == file || $2 == "./" file { count++ }
  END { print count + 0 }
' "$checksums_path")
if [ "$match_count" -eq 0 ]; then
  echo "No checksum found for ${TOGI_ARCHIVE} (asset missing or renamed)" >&2
  exit 1
fi
if [ "$match_count" -gt 1 ]; then
  echo "Duplicate checksum entries for ${TOGI_ARCHIVE}" >&2
  exit 1
fi
expected_sha=$(awk -v file="$TOGI_ARCHIVE" '$2 == file || $2 == "./" file { print $1; exit }' "$checksums_path")
if ! printf '%s\n' "$expected_sha" | grep -Eq '^[0-9a-f]{64}$'; then
  echo "Malformed checksum for ${TOGI_ARCHIVE}" >&2
  exit 1
fi
actual_sha=$(sha256_file "$archive_path")
if [ "$actual_sha" != "$expected_sha" ]; then
  echo "Checksum mismatch for ${TOGI_ARCHIVE}" >&2
  exit 1
fi

printf 'TOGI_ARCHIVE_PATH=%q\n' "$archive_path"
