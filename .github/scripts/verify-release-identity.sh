#!/usr/bin/env bash
# Post-publication release identity check per docs/PUBLISHING.md: re-resolve
# the triggering tag to its peeled commit, require it to equal the successful
# release-workflow head, and verify the public GitHub release association for
# that exact tag. target_commitish is recorded as metadata only; the peeled
# commit and workflow head establish source identity. All network access is
# unauthenticated and read-only; no credentials are read or emitted.
set -euo pipefail

: "${TOGI_VERSION:?TOGI_VERSION is required}"
: "${TOGI_WORKFLOW_HEAD:?TOGI_WORKFLOW_HEAD is required}"

repo="Darkroom4364/togi"

if [[ ! "$TOGI_VERSION" =~ ^v[0-9]+[.][0-9]+[.][0-9]+([-+][A-Za-z0-9._-]+)?$ ]]; then
  echo "Invalid release tag: ${TOGI_VERSION}" >&2
  exit 1
fi
if [[ ! "$TOGI_WORKFLOW_HEAD" =~ ^[0-9a-f]{40}$ ]]; then
  echo "Invalid workflow head SHA: ${TOGI_WORKFLOW_HEAD}" >&2
  exit 1
fi

tag_ref="refs/tags/${TOGI_VERSION}"
ls_remote=$(git ls-remote "https://github.com/${repo}.git" "$tag_ref" "${tag_ref}^{}")
tag_sha=$(awk -v ref="$tag_ref" '$2 == ref { print $1 }' <<<"$ls_remote")
peeled_sha=$(awk -v ref="${tag_ref}^{}" '$2 == ref { print $1 }' <<<"$ls_remote")
if [ -z "$tag_sha" ]; then
  echo "Release tag ${TOGI_VERSION} does not resolve on ${repo}" >&2
  exit 1
fi
commit_sha="${peeled_sha:-$tag_sha}"
printf 'tag %s resolves to commit %s\n' "$TOGI_VERSION" "$commit_sha"

if [ "$commit_sha" != "$TOGI_WORKFLOW_HEAD" ]; then
  echo "Release tag ${TOGI_VERSION} commit ${commit_sha} does not match workflow head ${TOGI_WORKFLOW_HEAD}" >&2
  exit 1
fi

release_json=$(curl -fsSL "https://api.github.com/repos/${repo}/releases/tags/${TOGI_VERSION}")
tag_name=$(jq -er '.tag_name' <<<"$release_json")
if [ "$tag_name" != "$TOGI_VERSION" ]; then
  echo "GitHub release tag ${tag_name} does not match ${TOGI_VERSION}" >&2
  exit 1
fi
if [ "$(jq -r '.draft' <<<"$release_json")" != "false" ]; then
  echo "GitHub release for ${TOGI_VERSION} is a draft" >&2
  exit 1
fi
if [ "$(jq -r '.prerelease' <<<"$release_json")" != "false" ]; then
  echo "GitHub release for ${TOGI_VERSION} is a prerelease" >&2
  exit 1
fi
target_commitish=$(jq -r '.target_commitish' <<<"$release_json")
printf 'release association verified for %s (target_commitish metadata: %s)\n' \
  "$TOGI_VERSION" "$target_commitish"
