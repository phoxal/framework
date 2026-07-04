#!/usr/bin/env bash
set -euo pipefail

# THROWAWAY plan-01 activation helper.
#
# Delete this script after the artifact git_only release scope is activated and
# the reviewer has run it once. The earlier plan-01 seed used retired
# phoxal-runtime-* names, so this re-seeds the current artifact package tags:
#
#   <package>-v<current-version>
#
# Tags are created at the current HEAD commit only when missing. Existing tags
# are left untouched. This script never pushes; it prints the exact push command
# for review.

usage() {
  cat <<'USAGE'
Usage: scripts/reseed-artifact-tags.sh [--dry-run]

Creates missing <package>-v<current-version> tags for discovered official
artifact crates at the current HEAD commit. With --dry-run, only prints what
would happen.
USAGE
}

dry_run=false
if [[ "${1:-}" == "--dry-run" ]]; then
  dry_run=true
  shift
fi

if [[ "$#" -ne 0 ]]; then
  usage >&2
  exit 2
fi

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

head_sha="$(git rev-parse HEAD)"
discover_json="$(cargo xtask release discover --json)"

missing_tags=()
while IFS=$'\t' read -r package version; do
  tag="${package}-v${version}"

  if git rev-parse --quiet --verify "refs/tags/${tag}" >/dev/null; then
    tag_sha="$(git rev-list -n 1 "${tag}")"
    echo "exists: ${tag} -> ${tag_sha}"
    continue
  fi

  missing_tags+=("${tag}")
  if [[ "$dry_run" == true ]]; then
    echo "would create: ${tag} -> ${head_sha}"
  else
    git tag "${tag}" "${head_sha}"
    echo "created: ${tag} -> ${head_sha}"
  fi
done < <(
  python3 -c '
import json
import sys

for artifact in json.load(sys.stdin):
    print("{}\t{}".format(artifact["package_name"], artifact["version"]))
' <<<"$discover_json"
)

if [[ "${#missing_tags[@]}" -eq 0 ]]; then
  echo "no missing artifact tags"
else
  printf 'push after review with: git push origin'
  printf ' %q' "${missing_tags[@]}"
  printf '\n'
fi
