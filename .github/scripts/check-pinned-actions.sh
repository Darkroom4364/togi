#!/usr/bin/env bash
set -euo pipefail

failed=0

while IFS= read -r line; do
  file="${line%%:*}"
  rest="${line#*:}"
  line_no="${rest%%:*}"
  match="${line##*uses: }"
  ref="${match%%[[:space:]]*}"

  case "$ref" in
    ./*|docker://*|"")
      continue
      ;;
    actions/*)
      continue
      ;;
  esac

  if [[ ! "$ref" =~ @[0-9a-f]{40}$ ]]; then
    echo "$file:$line_no: external action must be pinned to a full commit SHA: $ref" >&2
    failed=1
  fi
done < <(rg -n -o 'uses:\s+\S+' .github/workflows action.yml)

exit "$failed"
