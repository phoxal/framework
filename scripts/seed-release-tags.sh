#!/usr/bin/env bash
# One-time bootstrap of per-package release tags for plan #01 (per-artifact
# release).
#
# Before plan #01 the workspace released under a single shared tag (`v<version>`).
# release-plz now tracks each crate by its OWN tag `<crate>-v<version>` (set by
# git_tag_name in release-plz.toml). The 18 runtime crates are git_only
# (publish = false), so release-plz uses their git tags - not a registry - as the
# "last released" baseline. With no per-package tags present, the FIRST release-plz
# run would treat every runtime as an initial release and build all 18 images.
#
# Run this ONCE at activation (after merging the #01-B PR, before the next push to
# main) to seed `<crate>-v<version>` for every workspace member at the last shared
# release commit, so release-plz's first run no-ops for unchanged crates.
#
# Idempotent: skips tags that already exist; safe to re-run.
#
# Usage:
#   scripts/seed-release-tags.sh [BASE_REF]   # BASE_REF defaults to v0.19.1
#   REMOTE=upstream scripts/seed-release-tags.sh
set -euo pipefail

base_ref="${1:-v0.19.1}"
remote="${REMOTE:-origin}"

if ! git rev-parse -q --verify "${base_ref}^{commit}" >/dev/null; then
  echo "error: base ref '${base_ref}' not found (expected the last shared release tag)" >&2
  exit 1
fi
base_commit="$(git rev-parse "${base_ref}^{commit}")"
echo "seeding per-package tags at ${base_ref} (${base_commit})"

created=()
# cargo metadata -> "<name> <version>" for every workspace member.
while read -r name version; do
  tag="${name}-v${version}"
  if git rev-parse -q --verify "refs/tags/${tag}" >/dev/null; then
    echo "  skip ${tag} (exists)"
    continue
  fi
  git tag "${tag}" "${base_commit}"
  created+=("${tag}")
  echo "  tag  ${tag} -> ${base_commit}"
done < <(
  cargo metadata --format-version 1 --no-deps \
    | python3 -c 'import json, sys
meta = json.load(sys.stdin)
members = set(meta["workspace_members"])
for pkg in meta["packages"]:
    if pkg["id"] in members:
        print(pkg["name"], pkg["version"])'
)

if [[ ${#created[@]} -eq 0 ]]; then
  echo "nothing to do: all per-package tags already exist"
  exit 0
fi

echo "pushing ${#created[@]} new tag(s) to ${remote}..."
git push "${remote}" "${created[@]}"
echo "done: seeded ${#created[@]} per-package tag(s)."
