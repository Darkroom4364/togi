#!/usr/bin/env bash
set -euo pipefail

: "${TOGI_ARCHIVE:?TOGI_ARCHIVE is required}"
: "${TOGI_BINARY:?TOGI_BINARY is required}"
: "${GITHUB_PATH:?GITHUB_PATH is required}"

TEMP_ROOT="${RUNNER_TEMP:-.}"
if command -v cygpath >/dev/null 2>&1; then
  TEMP_ROOT=$(cygpath -u "$TEMP_ROOT")
fi
INSTALL_DIR="${TEMP_ROOT}/togi-bin"
EXTRACT_DIR="${TEMP_ROOT}/togi-extract"
ARCHIVE_PATH="${TOGI_ARCHIVE_PATH:-${TEMP_ROOT}/${TOGI_ARCHIVE}}"
rm -rf "$INSTALL_DIR" "$EXTRACT_DIR"
mkdir -p "$INSTALL_DIR" "$EXTRACT_DIR"

validate_archive_entries() {
  awk '
    {
      path = $0
      gsub(/\\/, "/", path)
      if (path == "" || path ~ /^\// || path ~ /(^|\/)\.\.($|\/)/) {
        printf "Unsafe archive entry: %s\n", $0 > "/dev/stderr"
        exit 1
      }
    }
  '
}

validate_tar_entry_types() {
  awk '
    substr($0, 1, 1) == "l" || substr($0, 1, 1) == "h" {
      printf "Unsafe archive link entry: %s\n", $0 > "/dev/stderr"
      exit 1
    }
  '
}

validate_zip_entry_types() {
  awk '
    /^[dlh-][rwxStTs-]{9}/ {
      type = substr($0, 1, 1)
      if (type == "l" || type == "h") {
        printf "Unsafe archive link entry: %s\n", $0 > "/dev/stderr"
        exit 1
      }
    }
  '
}

case "$TOGI_ARCHIVE" in
  *.tar.gz)
    tar tzf "$ARCHIVE_PATH" | validate_archive_entries
    tar tvzf "$ARCHIVE_PATH" | validate_tar_entry_types
    tar xzf "$ARCHIVE_PATH" -C "$EXTRACT_DIR"
    ;;
  *.zip)
    unzip -Z1 "$ARCHIVE_PATH" | validate_archive_entries
    if command -v zipinfo >/dev/null 2>&1; then
      zipinfo -l "$ARCHIVE_PATH" | validate_zip_entry_types
    else
      unzip -Z -l "$ARCHIVE_PATH" | validate_zip_entry_types
    fi
    unzip -q "$ARCHIVE_PATH" -d "$EXTRACT_DIR"
    ;;
  *)
    echo "Unsupported Togi archive format: ${TOGI_ARCHIVE}" >&2
    exit 1
    ;;
esac

TOGI_SOURCE=$(find "$EXTRACT_DIR" -type f -name "$TOGI_BINARY" -print -quit)
if [ -z "$TOGI_SOURCE" ]; then
  echo "Downloaded ${TOGI_ARCHIVE}, but ${TOGI_BINARY} was not found" >&2
  exit 1
fi
cp "$TOGI_SOURCE" "$INSTALL_DIR/$TOGI_BINARY"
chmod +x "$INSTALL_DIR/$TOGI_BINARY"

PATH_ENTRY="$INSTALL_DIR"
if command -v cygpath >/dev/null 2>&1; then
  PATH_ENTRY=$(cygpath -w "$INSTALL_DIR")
fi
echo "$PATH_ENTRY" >> "$GITHUB_PATH"
